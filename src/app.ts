import rateLimit from '@fastify/rate-limit';
import websocket from '@fastify/websocket';
import Fastify, { LogController } from 'fastify';
import authPlugin from './auth/plugin.js';
import { config } from './config.js';
import { docsSpec, docsUi } from './docs/plugin.js';
import { ProxyError, RiotError } from './errors.js';
import { logger } from './logger.js';
import { requestsTotal } from './metrics.js';
import { ANON_QUOTA_PER_MIN } from './quotas.js';
import { redis } from './redis.js';
import adminRoutes from './routes/admin.js';
import dashboardRoutes from './routes/dashboard.js';
import debugRoutes from './routes/debug.js';
import devUiRoutes from './routes/dev-ui.js';
import healthRoutes from './routes/health.js';
import lolRoutes from './routes/lol.js';
import playerRoutes from './routes/players.js';
import riotRoutes from './routes/riot.js';
import { sharedSchemas } from './routes/schemas.js';
import staticRoutes from './routes/static.js';
import { metricsBroadcaster } from './stats/broadcaster.js';
import { metricsHistory } from './stats/history.js';
import wsRoutes from './ws/index.js';

export async function buildApp() {
  const app = Fastify({
    loggerInstance: logger,
    trustProxy: true,
    // Fastify's own request/response lines are suppressed; §13 wants one
    // structured line per request, emitted in the onResponse hook below.
    logController: new LogController({ disableRequestLogging: true }),
    bodyLimit: 1_048_576,
    ajv: { customOptions: { removeAdditional: 'all', coerceTypes: true, useDefaults: true } },
  });

  await app.register(websocket, {
    options: { maxPayload: 64 * 1024 },
  });

  /**
   * §12.1 / FR-13 — per-consumer quotas, independent of Riot's limits. Backed
   * by Redis so the quota is shared across api replicas.
   */
  await app.register(rateLimit, {
    global: true,
    redis,
    nameSpace: 'q:',
    // The default is a fallback for unauthenticated routes; authenticated
    // requests get the consumer's own `quota_per_min` (§7.1).
    max: (request) => request.consumer?.quotaPerMin ?? ANON_QUOTA_PER_MIN,
    timeWindow: '1 minute',
    keyGenerator: (request) => request.consumer?.id ?? request.ip,
    // Health and metrics must stay reachable when a consumer is over quota.
    allowList: (request) => Boolean(request.routeOptions.config?.public),
    // The plugin *throws* whatever this returns rather than sending it as a
    // body, so it has to be the error itself. Returning `.toEnvelope()` handed
    // the error handler a bare object, which `toProxyError` cannot recognise —
    // and every over-quota request came back as a 500.
    errorResponseBuilder: (_request, context) => {
      const retryAfter = Math.ceil(Number(context.ttl) / 1000);
      return new ProxyError('QUOTA_EXCEEDED', `Quota of ${context.max}/min exceeded`, {
        retryAfter,
      });
    },
  });

  await app.register(authPlugin);

  /**
   * §6.1 — one error envelope for everything the service returns.
   *
   * Registered *before* the route plugins: a child context captures the
   * parent's error handler at registration time, so setting it afterwards
   * would leave every routed request falling back to Fastify's default.
   */
  app.setErrorHandler((error, request, reply) => {
    const proxyError = toProxyError(error);

    if (proxyError.retryAfter !== undefined) {
      reply.header('Retry-After', String(proxyError.retryAfter));
    }

    // 5xx is our bug; 4xx is the caller's. Log accordingly.
    const level = proxyError.statusCode >= 500 ? 'error' : 'warn';
    logger[level](
      {
        err: error,
        code: proxyError.code,
        route: request.routeOptions.url,
        consumer: request.consumer?.name,
      },
      'request failed',
    );

    void reply.code(proxyError.statusCode).send(proxyError.toEnvelope());
  });

  app.setNotFoundHandler((request, reply) => {
    void reply
      .code(404)
      .send(
        new ProxyError('NOT_FOUND', `No route for ${request.method} ${request.url}`).toEnvelope(),
      );
  });

  /**
   * §12.3's parameters and the §6.1 error envelope, registered once (#61).
   * Must come before the route plugins: a route referencing `ErrorResponse#`
   * cannot compile its validator until the reference resolves.
   */
  for (const schema of sharedSchemas) app.addSchema(schema);

  /**
   * Before the route plugins, and it has to be: `@fastify/swagger` collects
   * routes through an `onRoute` hook, which only fires for routes registered
   * after it (#64).
   */
  if (config.docsUi) await app.register(docsSpec);

  await app.register(healthRoutes);
  await app.register(riotRoutes);
  await app.register(lolRoutes);
  await app.register(playerRoutes);
  await app.register(staticRoutes);
  await app.register(adminRoutes);
  await app.register(debugRoutes);
  await app.register(wsRoutes);
  // The `metrics` topic's clock. Idle while nothing is subscribed, and stopped
  // with the app so tests and write-spec runs never leave a timer behind. The
  // history recorder is its always-on sibling: one compact point a minute,
  // recorded whether or not a dashboard is open.
  metricsBroadcaster.start();
  metricsHistory.start();
  app.addHook('onClose', async () => {
    metricsBroadcaster.stop();
    metricsHistory.stop();
  });
  // Development-only browser client (§ none — it is a tool, not a contract).
  if (config.devUi) await app.register(devUiRoutes);
  // The operational dashboard — on in production, unlike the dev UI.
  if (config.dashboardUi) await app.register(dashboardRoutes);
  // After the routes: the page can only describe what is already registered.
  if (config.docsUi) await app.register(docsUi);

  /** §13 — one structured line per request. */
  app.addHook('onResponse', async (request, reply) => {
    const route = request.routeOptions.url ?? request.url;

    /**
     * Except the reference's own assets. Scalar's bundle is 3.7 MB across
     * several requests and one page load would otherwise bury a minute of real
     * traffic. `logLevel: 'silent'` on the plugin does not cover this: that
     * setting governs the route's own Fastify logger, and this line is ours —
     * emitted from a hook that calls the module-level logger directly. The
     * metric is still incremented, so the traffic remains visible in aggregate.
     */
    const isDocsAsset = route.startsWith('/docs');
    const cacheState = (reply.getHeader('X-Cache') as string | undefined)?.toLowerCase() ?? 'none';

    requestsTotal.inc({
      route,
      status: String(reply.statusCode),
      cache: normaliseCacheLabel(cacheState),
    });

    if (isDocsAsset) return;

    logger.info(
      {
        consumer: request.consumer?.name,
        route,
        method: request.method,
        status: reply.statusCode,
        cacheState,
        totalMs: Math.round(reply.elapsedTime),
      },
      'request',
    );
  });

  return app;
}

export type App = Awaited<ReturnType<typeof buildApp>>;

function normaliseCacheLabel(state: string): string {
  if (state === 'hit-neg') return 'neg';
  if (state === 'hit' || state === 'miss' || state === 'stale') return state;
  return 'none';
}

/**
 * Fastify's schema validation errors and any stray exception are folded into
 * the same envelope; upstream detail is never leaked (§12.2).
 */
function toProxyError(error: unknown): ProxyError {
  if (error instanceof ProxyError) return error;
  if (error instanceof RiotError) return error.toProxyError();

  const candidate = error as { validation?: unknown; statusCode?: number; message?: string };

  if (candidate?.validation) {
    // A bad platform/region reaches us as a schema enum failure; §6.1 wants
    // BAD_REGION for those specifically.
    const message = candidate.message ?? 'Request validation failed';
    const code = /platform|region/i.test(message) ? 'BAD_REGION' : 'VALIDATION';
    return new ProxyError(code, message);
  }

  if (candidate?.statusCode === 429) {
    return new ProxyError('QUOTA_EXCEEDED', 'Quota exceeded');
  }

  if (config.isProduction) {
    return new ProxyError('INTERNAL', 'Internal server error');
  }
  return new ProxyError('INTERNAL', candidate?.message ?? 'Internal server error');
}

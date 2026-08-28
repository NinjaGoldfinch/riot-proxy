import { Pool, type Dispatcher } from 'undici';
import { config } from '../config.js';
import { RiotError } from '../errors.js';
import { logger } from '../logger.js';
import { rl429Total, upstreamLatency, upstreamRequestsTotal } from '../metrics.js';
import type { BuiltRequest } from './endpoints.js';
import { limiter, delay, type Priority, type RateLimiter } from './limiter.js';

/** §5.5 — 5xx gets two retries with the documented jittered backoff. */
const SERVER_ERROR_RETRIES = 2;
const SERVER_ERROR_BACKOFF_MS = [250, 750];

/** §9.4 — service-limited 429s (no type header) back off separately. */
const SERVICE_429_MAX_TRIES = 3;
const SERVICE_429_BASE_MS = 500;
const SERVICE_429_MAX_MS = 8000;

export interface RiotResponse<T> {
  data: T;
  status: number;
  headers: Record<string, string | undefined>;
  /** Round-trip milliseconds, excluding limiter wait. */
  upstreamMs: number;
}

export interface FetchOptions {
  priority?: Priority;
  waitBudgetMs?: number;
  signal?: AbortSignal;
}

/**
 * §4.1 / Phase 1 — one pooled undici client per Riot host. Roughly 20 hosts
 * exist; pools are created lazily so a single-region deployment holds one.
 */
export interface RiotClientOptions {
  rateLimiter?: RateLimiter;
  /**
   * Overrides how a dispatcher is built for a host. Exists so tests can point
   * the client at a local plain-HTTP server; production always uses the
   * default HTTPS pool.
   */
  poolFactory?: (host: string) => Dispatcher;
}

export class RiotClient {
  private readonly pools = new Map<string, Dispatcher>();
  private readonly limiter: RateLimiter;
  private readonly poolFactory: (host: string) => Dispatcher;

  constructor(options: RiotClientOptions = {}) {
    this.limiter = options.rateLimiter ?? limiter;
    this.poolFactory = options.poolFactory ?? defaultPoolFactory;
  }

  private pool(host: string): Dispatcher {
    let pool = this.pools.get(host);
    if (!pool) {
      pool = this.poolFactory(host);
      this.pools.set(host, pool);
    }
    return pool;
  }

  /**
   * Perform one upstream call, applying the limiter (§9) and the status policy
   * (§5.5). Throws `RiotError` for every non-2xx outcome the caller must handle.
   */
  async request<T>(req: BuiltRequest, opts: FetchOptions = {}): Promise<RiotResponse<T>> {
    const priority = opts.priority ?? 'interactive';
    let serverRetries = 0;
    let serviceTries = 0;

    for (;;) {
      await this.limiter.acquire(req.scope, req.method, {
        priority,
        ...(opts.waitBudgetMs !== undefined ? { waitBudgetMs: opts.waitBudgetMs } : {}),
      });

      const started = performance.now();
      let res: Dispatcher.ResponseData;
      try {
        res = await this.pool(req.host).request({
          method: 'GET',
          path: buildPath(req),
          headers: {
            // §5.2 — key goes in the header, never the query string.
            'X-Riot-Token': config.RIOT_API_KEY,
            'User-Agent': config.RIOT_USER_AGENT,
            Accept: 'application/json',
          },
          ...(opts.signal ? { signal: opts.signal } : {}),
        });
      } catch {
        const upstreamMs = performance.now() - started;
        upstreamRequestsTotal.inc({ region: req.scope, method: req.method, status: 'network' });
        upstreamLatency.observe({ region: req.scope, method: req.method }, upstreamMs / 1000);

        if (serverRetries < SERVER_ERROR_RETRIES) {
          await delay(jitter(SERVER_ERROR_BACKOFF_MS[serverRetries] ?? 750));
          serverRetries += 1;
          continue;
        }
        throw new RiotError(0, req.method, req.host, 'Network failure contacting Riot API', {});
      }

      const upstreamMs = performance.now() - started;
      const headers = normaliseHeaders(res.headers);
      const status = res.statusCode;

      upstreamRequestsTotal.inc({ region: req.scope, method: req.method, status: String(status) });
      upstreamLatency.observe({ region: req.scope, method: req.method }, upstreamMs / 1000);

      // Feed the limiter on every response, including errors — the headers are
      // authoritative and a 429 body still carries current counts (§9.1).
      void this.limiter.observeHeaders(req.scope, req.method, headers).catch((err: unknown) => {
        logger.warn({ err, scope: req.scope }, 'failed to sync limiter from headers');
      });

      if (status >= 200 && status < 300) {
        const data = (await res.body.json()) as T;
        return { data, status, headers, upstreamMs };
      }

      // Drain the body so the connection returns to the pool.
      const body = await safeText(res);

      if (status === 404) {
        throw new RiotError(404, req.method, req.host, 'Not found');
      }

      if (status === 401 || status === 403) {
        // §5.5 — never retry, alert loudly: the key is expired, revoked or
        // blacklisted, and every further request wastes time.
        logger.error(
          { status, method: req.method, host: req.host },
          'RIOT KEY REJECTED — check RIOT_API_KEY (dev keys expire every 24h)',
        );
        throw new RiotError(status, req.method, req.host, 'Upstream rejected credentials');
      }

      if (status === 429) {
        const type = headers['x-rate-limit-type'];
        const retryAfter = Number(headers['retry-after'] ?? '');
        rl429Total.inc({ region: req.scope, type: type ?? 'service' });

        if (type && Number.isFinite(retryAfter)) {
          // Our own bucket accounting was wrong — §9.4 says treat as a bug.
          if (type === 'application' || type === 'method') {
            logger.error(
              { scope: req.scope, method: req.method, type, retryAfter, headers },
              'accountable 429 — limiter accounting is out of sync',
            );
          }
          await this.limiter.freeze(req.scope, retryAfter, type);
          continue; // acquire() will now block on the frozen scope
        }

        // §5.4(4)/(5) — no type header means an underlying service limited us.
        // Our buckets are fine; back off with jitter and do not touch them.
        if (serviceTries < SERVICE_429_MAX_TRIES) {
          const backoff = Math.min(SERVICE_429_BASE_MS * 2 ** serviceTries, SERVICE_429_MAX_MS);
          serviceTries += 1;
          logger.warn(
            { scope: req.scope, method: req.method, try: serviceTries, backoff },
            'service-type 429, backing off',
          );
          await delay(jitter(backoff));
          continue;
        }

        throw new RiotError(429, req.method, req.host, 'Upstream rate limited', {
          ...(Number.isFinite(retryAfter) ? { retryAfter } : { retryAfter: 1 }),
          ...(type ? { rateLimitType: type } : {}),
        });
      }

      if (status >= 500) {
        if (serverRetries < SERVER_ERROR_RETRIES) {
          const backoff = SERVER_ERROR_BACKOFF_MS[serverRetries] ?? 750;
          serverRetries += 1;
          logger.warn({ status, method: req.method, try: serverRetries }, 'upstream 5xx, retrying');
          await delay(jitter(backoff));
          continue;
        }
        throw new RiotError(status, req.method, req.host, 'Upstream server error');
      }

      logger.warn(
        { status, method: req.method, body: body.slice(0, 200) },
        'unexpected upstream status',
      );
      throw new RiotError(status, req.method, req.host, 'Unexpected upstream status');
    }
  }

  async close(): Promise<void> {
    await Promise.allSettled([...this.pools.values()].map((p) => p.close()));
    this.pools.clear();
  }
}

function defaultPoolFactory(host: string): Dispatcher {
  return new Pool(`https://${host}`, {
    connections: 32,
    keepAliveTimeout: 60_000,
    keepAliveMaxTimeout: 300_000,
    headersTimeout: 10_000,
    bodyTimeout: 15_000,
  });
}

export function buildPath(req: BuiltRequest): string {
  const entries = Object.entries(req.query).filter(
    ([, v]) => v !== undefined && v !== null && v !== '',
  );
  if (entries.length === 0) return req.path;
  const params = new URLSearchParams();
  for (const [k, v] of entries) params.append(k, String(v));
  return `${req.path}?${params.toString()}`;
}

function normaliseHeaders(
  headers: Record<string, string | string[] | undefined>,
): Record<string, string | undefined> {
  const out: Record<string, string | undefined> = {};
  for (const [k, v] of Object.entries(headers)) {
    out[k.toLowerCase()] = Array.isArray(v) ? v.join(',') : v;
  }
  return out;
}

async function safeText(res: Dispatcher.ResponseData): Promise<string> {
  try {
    return await res.body.text();
  } catch {
    return '';
  }
}

/** ±20 % jitter so retries from N instances do not synchronise. */
function jitter(ms: number): number {
  return Math.round(ms * (0.8 + Math.random() * 0.4));
}

export const riotClient = new RiotClient();

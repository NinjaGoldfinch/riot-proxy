import swagger from '@fastify/swagger';
import scalar from '@scalar/fastify-api-reference';
import type { FastifyPluginAsync } from 'fastify';
import type { OpenAPIV3_1 } from 'openapi-types';
import fp from 'fastify-plugin';
import { ENDPOINTS, type EndpointSpec } from '../riot/endpoints.js';
import { CODE_SAMPLES, ERROR_EXAMPLES, RIOT_EXAMPLES } from './examples.js';
import { openApiDocument } from './openapi.js';

/**
 * Serving the reference (#64), in two halves because the two have opposite
 * ordering requirements.
 *
 * `docsSpec` must be registered **before** the route plugins: `@fastify/swagger`
 * works by adding an `onRoute` hook, and a hook only sees routes registered
 * after it. `docsUi` must be registered **after** them, so the `onRoute` hook it
 * installs to mark its own asset routes public does not also fire for every
 * real route in the service.
 */

/**
 * Which Riot method backs which route, so the badges below can quote the
 * endpoint's real TTL instead of a number typed into a description. Only the
 * passthrough routes appear: a composite fans out to several methods and has no
 * single TTL to report.
 */
const METHOD_BY_ROUTE: Record<string, EndpointSpec['id']> = {
  '/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}': 'account.byRiotId',
  '/v1/riot/accounts/by-puuid/{region}/{puuid}': 'account.byPuuid',
  '/v1/lol/summoners/by-puuid/{platform}/{puuid}': 'summoner.byPuuid',
  '/v1/lol/league/entries/by-puuid/{platform}/{puuid}': 'league.entriesByPuuid',
  '/v1/lol/matches/ids/{region}/{puuid}': 'match.idsByPuuid',
  '/v1/lol/matches/{region}/{matchId}': 'match.byId',
  '/v1/lol/matches/{region}/{matchId}/timeline': 'match.timeline',
  '/v1/lol/spectator/active/{platform}/{puuid}': 'spectator.activeGame',
  '/v1/lol/mastery/by-puuid/{platform}/{puuid}': 'mastery.byPuuid',
  '/v1/lol/rotations/{platform}': 'platform.championRotations',
  '/v1/lol/status/{platform}': 'status.platformData',
};

const ttlBadge = (s: number): string => {
  if (!Number.isFinite(s)) return 'Immutable · archived';
  if (s % 86_400 === 0) return `Cache ${s / 86_400} d`;
  if (s % 3600 === 0) return `Cache ${s / 3600} h`;
  if (s % 60 === 0) return `Cache ${s / 60} m`;
  return `Cache ${s} s`;
};

interface MediaType {
  schema?: Record<string, unknown>;
  example?: unknown;
  examples?: Record<string, { summary: string; value: unknown }>;
}

interface Operation {
  tags?: string[];
  security?: unknown[];
  'x-badges'?: { name: string }[];
  'x-codeSamples'?: { lang: string; label: string; source: string }[];
  responses?: Record<string, { content?: Record<string, MediaType> }>;
}

/**
 * Examples are attached here rather than on the routes, so that describing a
 * body and constraining one stay separable (#63). Riot's `200`s keep their
 * empty schema and gain an example; nothing below can reach a validator or a
 * serialiser.
 */
function attachExamples(path: string, op: Operation): void {
  const json = (status: string) => op.responses?.[status]?.content?.['application/json'];

  const riot = RIOT_EXAMPLES[path];
  if (riot !== undefined) {
    const media = json('200');
    if (media) media.example = riot;
  }

  for (const [status, example] of Object.entries(ERROR_EXAMPLES)) {
    const media = json(status);
    if (media) media.examples = { [example.summary]: example };
  }
}

/**
 * What a passthrough proxy's reference should say and a generic one cannot:
 * how long an answer is cached, whether it costs Riot budget at all, and what
 * it costs the caller. All of it read from `ENDPOINTS` at build time, so it
 * cannot drift from what the cache actually does.
 */
function badgesFor(path: string, op: Operation): string[] {
  const tags = op.tags ?? [];
  const badges: string[] = [];

  const methodId = METHOD_BY_ROUTE[path];
  if (methodId) {
    const spec = ENDPOINTS.find((e) => e.id === methodId);
    if (spec) badges.push(ttlBadge(spec.ttlSeconds));
  }

  if (tags.includes('players')) badges.push('Composite');
  if (tags.includes('static')) badges.push('No upstream call');
  if (tags.includes('ops')) badges.push('Public · no key');
  if (tags.includes('admin')) badges.push('Admin scope');

  return badges;
}

/**
 * Half one: the document. Registered from `buildApp()` before the route
 * plugins.
 */
export const docsSpec: FastifyPluginAsync = fp(async (fastify) => {
  await fastify.register(swagger, {
    openapi: openApiDocument as unknown as OpenAPIV3_1.Document,
    /**
     * Name the shared components after their `$id` instead of the default
     * `def-0`, `def-1`, … . Without this every reference registered in stage 2
     * lands in `components/schemas` under a serial number, which is worse than
     * inlining it was: the sidebar gets eight entries a reader cannot tell
     * apart, and a `$ref` in the document points at nothing they can name.
     */
    refResolver: {
      buildLocalReference: (json, _baseUri, _fragment, i) =>
        typeof json.$id === 'string' && json.$id.length > 0 ? json.$id : `def-${i}`,
    },
    /**
     * `transformObject` rather than `transform`: the badges and the public-route
     * `security` override are properties of the finished operation object, and
     * `transform` only gets to rewrite a route's schema on the way in.
     */
    transformObject: (documentObject) => {
      if (!('openapiObject' in documentObject)) return documentObject.swaggerObject;
      const doc = documentObject.openapiObject as unknown as {
        paths?: Record<string, Record<string, Operation>>;
      };
      for (const [path, operations] of Object.entries(doc.paths ?? {})) {
        for (const op of Object.values(operations)) {
          if (typeof op !== 'object' || op === null) continue;

          // The `ops` routes are `config.public`: the auth hook skips them and
          // the rate limiter's allowList exempts them. An empty `security`
          // stops the page demanding a key to try `/healthz`.
          if (op.tags?.includes('ops')) op.security = [];

          const badges = badgesFor(path, op);
          if (badges.length > 0) op['x-badges'] = badges.map((name) => ({ name }));

          attachExamples(path, op);

          const samples = CODE_SAMPLES[path];
          if (samples) op['x-codeSamples'] = samples;
        }
      }
      return documentObject.openapiObject;
    },
  });
});

/**
 * Half two: the page and the document's public URLs. Registered from
 * `buildApp()` *after* the route plugins, and deliberately not wrapped in
 * `fastify-plugin` — it needs its own encapsulation context so the `onRoute`
 * hook below applies to Scalar's routes and to nothing else.
 */
export const docsUi: FastifyPluginAsync = async (fastify) => {
  /**
   * A reference you need a key to read is a reference nobody reads, so every
   * route registered in this context is marked public — which exempts it from
   * both the auth hook and the per-consumer quota. Without this the reader
   * spends 60 requests of their own allowance loading a 3.7 MB bundle, and an
   * anonymous reader gets a 401 instead of a page.
   *
   * The consequence is deliberate and worth stating: the admin surface becomes
   * publicly enumerable. The routes themselves are scope- and IP-gated, and the
   * README already described them. If that is ever judged wrong, the lever is
   * `x-internal: true` on the `admin` tag rather than gating the whole page.
   */
  fastify.addHook('onRoute', (routeOptions) => {
    routeOptions.config = { ...routeOptions.config, public: true };
  });

  await fastify.register(scalar, {
    routePrefix: '/docs',
    // Scalar's asset routes must not each produce a §13 request line. This is
    // not sufficient on its own — see the note in `app.ts`.
    logLevel: 'silent',
    configuration: {
      title: 'riot-proxy',
      // The reader's own system preference, rather than a theme we picked.
      darkMode: undefined,
    },
  });

  /**
   * At the root as well as under `/docs`, because a client generator wants the
   * document without the page. They share `DOCS_UI` rather than getting a flag
   * each: two flags is two states nobody tests, and a deployment that wants the
   * spec but not the page is not a case that has come up.
   */
  fastify.get('/openapi.json', { schema: { hide: true } }, async () => fastify.swagger());
  fastify.get('/openapi.yaml', { schema: { hide: true } }, async (_request, reply) => {
    void reply.type('text/yaml; charset=utf-8');
    return fastify.swagger({ yaml: true });
  });
};

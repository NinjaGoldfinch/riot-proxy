import { config } from '../config.js';
import { ENDPOINTS, type EndpointSpec } from '../riot/endpoints.js';
import { REFRESH_COOLDOWN_S } from '../routes/players.js';

/**
 * The document's own content (#62) — everything that is not derived from a
 * route's schema.
 *
 * The rule this file follows: generate, never type. Every number below is read
 * from the running configuration, so a `CACHE_TTL_OVERRIDES` change or a
 * different `BULK_USAGE_CEILING` is reflected the next time the document is
 * built. A hand-written reference states these as prose and is wrong after the
 * first deployment that tunes one.
 */

const seconds = (s: number): string => {
  if (!Number.isFinite(s)) return 'never';
  if (s === 0) return 'off';
  if (s % 86_400 === 0) return `${s / 86_400} d`;
  if (s % 3600 === 0) return `${s / 3600} h`;
  if (s % 60 === 0) return `${s / 60} m`;
  return `${s} s`;
};

/** §8.2 as a table, from `ENDPOINTS` rather than from memory. */
function cacheTable(): string {
  const row = (e: EndpointSpec) =>
    `| \`${e.id}\` | ${e.host} | ${seconds(e.ttlSeconds)} | ${seconds(e.negTtlSeconds)} | ` +
    `${e.immutable ? 'archived' : '—'} |`;
  return [
    '| Method | Host | Cache TTL | Negative TTL | Storage |',
    '| --- | --- | --- | --- | --- |',
    ...ENDPOINTS.map(row),
  ].join('\n');
}

const overridden = config.CACHE_TTL_OVERRIDES
  ? `\n\nThis deployment overrides some of those: \`${config.CACHE_TTL_OVERRIDES}\`.`
  : '';

/**
 * `info.description` — the consumer guide. This is the reference's front page,
 * and deliberately the only place the header and error tables live.
 */
const description = `
A self-hosted middleman between [Riot's public API](https://developer.riotgames.com/)
and everything downstream of it. It holds one Riot key, spends it carefully, and
gives its consumers their own keys and their own quotas.

Three things it does that calling Riot directly does not:

- **It stays inside Riot's rate limits for you.** Limits are read from Riot's own
  response headers rather than assumed, and user-invoked requests are served
  ahead of background work — bulk jobs stop taking tokens once a bucket is
  ${Math.round(config.BULK_USAGE_CEILING * 100)} % full, so the rest stays free
  for a request someone is waiting on.
- **It caches and archives.** Repeated reads are served from Redis; matches are
  immutable, so they are archived in Postgres and re-reading a player's history
  costs no quota at all.
- **It composes.** The endpoints under \`players\` fan out to several Riot calls
  and return one document, which is what a profile page actually needs.

## Authentication

Every route outside \`ops\` needs a consumer key, as a bearer token:

\`\`\`
Authorization: Bearer rpx_…
\`\`\`

Keys are issued by an operator (\`POST /v1/admin/consumers\`) and shown exactly
once — only a SHA-256 of the key is stored, so a lost key is reissued, never
recovered. Each key carries its own per-minute quota, independent of Riot's
limits; the default is 600/min and unauthenticated callers get 60/min.

## Response headers

| Header | Meaning |
| --- | --- |
| \`X-Cache\` | \`HIT\`, \`MISS\`, \`STALE\`, or \`HIT-NEG\` for a cached 404 |
| \`X-Cache-Age\` | Age **of the content**, in seconds — not of the last fetch |
| \`Retry-After\` | On 429 and 503, how long to wait before retrying |

\`X-Cache: MISS\` with a large \`X-Cache-Age\` is not a contradiction. The age
describes the payload, so a match served from the Postgres archive is a MISS
against Redis and legitimately days old. That is the archive working.

## Cache TTLs

${cacheTable()}

Negative caching (§8.3) stores upstream 404s briefly so a mistyped Riot ID
cannot be used to hammer Riot. \`never\` means immutable: the payload is
archived in Postgres and re-read from there forever.${overridden}

## Errors

One envelope, always:

\`\`\`json
{ "error": { "code": "NOT_FOUND", "message": "…", "retryAfter": 30 } }
\`\`\`

| Code | Status | Means |
| --- | --- | --- |
| \`VALIDATION\` | 400 | The request failed this proxy's own schema |
| \`BAD_REGION\` | 400 | Unknown platform or region |
| \`UNAUTHORIZED\` | 401 | Missing or unrecognised consumer key |
| \`FORBIDDEN\` | 403 | Key lacks the scope, or the admin IP allowlist rejected it |
| \`NOT_FOUND\` | 404 | Riot has no such resource, or we do not serve it |
| \`QUOTA_EXCEEDED\` | 429 | **Your** per-minute quota. \`Retry-After\` is the window |
| \`UPSTREAM_ERROR\` | 502 | Riot answered, unusably |
| \`UPSTREAM_UNAVAILABLE\` | 503 | Riot is down, or our limiter is shedding load |
| \`INTERNAL\` | 500 | Ours |

The two throttles are worth telling apart, because they are not the same
problem. A 429 is your own quota and is fixed by slowing down. A 503 is the
limiter refusing to spend Riot budget it does not have, and affects every
consumer at once.

## Key scope

Riot encrypts PUUIDs per API key. A PUUID from another key will not resolve
here, and rotating this deployment's key strands every stored PUUID — which is
why \`keyScope\` appears in \`/readyz\` and \`/v1/admin/stats\`. Match IDs are
**not** encrypted, so the archive survives a rotation intact.

## Costs

- The composite endpoints accept \`?refresh=true\` to spend quota on re-reading
  rather than serving cache. It is metered at one refresh per player per
  ${REFRESH_COOLDOWN_S} s; the response says whether your claim won
  (\`refreshed\`) and when the next one is available (\`refreshAvailableIn\`).
- The first time anyone looks a player up, the proxy queues a walk of their
  whole match history${
    config.LOOKUP_BACKFILL_LIMIT > 0
      ? ` — up to ${config.LOOKUP_BACKFILL_LIMIT.toLocaleString('en-US')} matches`
      : ' (disabled on this deployment)'
  }. It runs at bulk priority, out of your way, and it is why the second page
  view is free.
`.trim();

/**
 * §11 as prose. OpenAPI 3.1 cannot express a WebSocket — `webhooks` is the
 * wrong shape too, since we are the server and the transport is a persistent
 * socket rather than a callback — so `/v1/ws` is absent as an operation and the
 * protocol is documented here instead. Scalar renders tag descriptions as full
 * markdown, so the samples survive. If this ever needs to be machine-readable
 * the answer is a separate AsyncAPI document, not a contortion of this one.
 */
const wsDescription = `
\`\`\`
GET /v1/ws?token=rpx_…
\`\`\`

A WebSocket. Auth is the same consumer key, in the query string rather than a
header, because a browser \`WebSocket\` cannot set one.

**Client → server**

\`\`\`json
{ "op": "subscribe",   "topics": ["player:<puuid>", "patch"] }
{ "op": "unsubscribe", "topics": ["player:<puuid>"] }
{ "op": "ping" }
\`\`\`

**Server → client** — \`ready\` on connect, \`subscribed\` acknowledging a
change, \`pong\`, \`error\`, and the events themselves:

\`\`\`json
{ "event": "game.started",   "topic": "player:…", "at": 1756000000000,
  "data": { "puuid": "…", "platform": "euw1", "gameId": 1234567890,
            "championId": 64, "queueId": 420 } }
{ "event": "game.ended",     "data": { "puuid": "…", "gameId": 1234567890 } }
{ "event": "rank.changed",   "data": { "puuid": "…", "queue": "RANKED_SOLO_5x5",
            "before": { "tier": "GOLD", "rank": "I", "lp": 98 },
            "after":  { "tier": "PLATINUM", "rank": "IV", "lp": 12 } } }
{ "event": "match.archived", "data": { "puuid": "…", "matchId": "EUW1_7381937461" } }
{ "event": "patch.new",      "data": { "version": "15.16.1" } }
\`\`\`

Two topics exist: \`player:<puuid>\` and \`patch\`.

**A player topic only ever fires for a _tracked_ player.** Subscribing to an
untracked PUUID is accepted and then silent — nothing polls them, so there is
nothing to report. Tracking is an operator action
(\`POST /v1/admin/tracked-players\`).

The server pings every 30 s and drops a connection that misses two consecutive
pongs.
`.trim();

const tags = [
  {
    name: 'players',
    description:
      'Composite endpoints: one call, several Riot requests, one document. **Start here.** ' +
      'A profile is four upstream calls and a match page is eleven; these spend one request ' +
      'of your quota instead, fan out concurrently, and return the parts that succeeded with a ' +
      '`warnings[]` array naming any that did not — mastery timing out does not fail the ' +
      'profile. The passthrough routes below are the escape hatch, not the front door.',
  },
  {
    name: 'riot',
    description:
      'account-v1, proxied. The body is Riot\'s, verbatim — the authority on its fields is ' +
      '[Riot\'s own reference](https://developer.riotgames.com/apis). These are regional ' +
      'endpoints. Note `sea`: account-v1 has no `sea` host, so the proxy sends those calls to ' +
      '`asia` for you, which is the single most surprising thing about this API.',
  },
  {
    name: 'lol',
    description:
      'The League endpoints, proxied — summoner, league, mastery, spectator, match, status, ' +
      'rotations. Bodies are Riot\'s, verbatim. Match documents are immutable, so once one has ' +
      'been fetched it is archived and served from Postgres at no upstream cost.',
  },
  {
    name: 'static',
    description:
      'The local Data Dragon mirror — champions, items, runes, summoner spells, profile icons, ' +
      'maps. No upstream call and no Riot budget spent, so render freely.',
  },
  { name: 'ws', description: wsDescription },
  {
    name: 'ops',
    description:
      'Liveness, readiness and Prometheus. Public: no consumer key, and exempt from the quota ' +
      'so health stays answerable when a caller is over it.',
  },
  {
    name: 'admin',
    description:
      'Operator surface. Requires the `admin` scope, and — when `ADMIN_IP_ALLOWLIST` is set — ' +
      'a source address on that list. Listed here because the shape of the operator API is ' +
      'part of the contract; the routes themselves are gated.',
  },
];

const tagGroups = [
  { name: 'Player data', tags: ['players', 'riot', 'lol'] },
  { name: 'Static data', tags: ['static'] },
  { name: 'Realtime', tags: ['ws'] },
  { name: 'Operations', tags: ['ops'] },
  { name: 'Administration', tags: ['admin'] },
];

export const openApiDocument = {
  openapi: '3.1.0',
  info: {
    title: 'riot-proxy',
    version: '1.0.0',
    description,
    license: { name: 'MIT' },
  },
  /**
   * Ordering matters: the request console targets the first entry by default,
   * so localhost leads. The production entry is templated rather than named, so
   * pointing the console at a real deployment — and spending its quota and its
   * Riot budget — is something the reader opts into by filling it in.
   */
  servers: [
    { url: `http://localhost:${config.PORT}`, description: 'Local development' },
    {
      url: '{scheme}://{host}',
      description: 'Your deployment',
      variables: {
        scheme: { enum: ['https', 'http'], default: 'https' },
        host: { default: 'riot-proxy.example.com' },
      },
    },
  ],
  components: {
    securitySchemes: {
      bearerAuth: {
        type: 'http',
        scheme: 'bearer',
        description:
          'A consumer key, `rpx_…`, issued by `POST /v1/admin/consumers` and shown exactly ' +
          'once. Only its SHA-256 is stored.',
      },
      tokenQuery: {
        type: 'apiKey',
        in: 'query',
        name: 'token',
        description:
          '**WebSocket handshake only.** A browser `WebSocket` cannot set an `Authorization` ' +
          'header, so `/v1/ws` takes the same key as `?token=`. Do not use this on the HTTP ' +
          'routes — a key in a query string ends up in access logs.',
      },
    },
  },
  security: [{ bearerAuth: [] }],
  tags,
  'x-tagGroups': tagGroups,
} as const;

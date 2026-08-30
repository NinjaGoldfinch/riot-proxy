/**
 * Type-only, for its side effect: `@fastify/swagger` augments `FastifySchema`
 * with the documentation keys — `tags`, `summary`, `description`, `hide`,
 * `operationId`. Without it in the program those keys are not typed at all, and
 * the `tags` already scattered through the route files only compiled because
 * TypeScript skips the excess-property check when `response` is also present.
 * A schema with `hide` and nothing else does not get that reprieve. Imported
 * here because every route file imports this one.
 */
import type {} from '@fastify/swagger';
import { Type } from '@sinclair/typebox';
import { ERROR_CODES } from '../errors.js';
import { PLATFORMS, REGIONS } from '../riot/routing.js';
import { MatchSummarySchema } from './match-summary.js';

/**
 * §12.3 — closed enums for platform/region, clamped counts, and Riot ID length
 * bounds, all enforced before any upstream call is made.
 */

export const PlatformParam = Type.Unsafe<string>({
  type: 'string',
  enum: [...PLATFORMS],
  description: 'Riot platform host (e.g. euw1, na1, kr)',
});

export const RegionParam = Type.Unsafe<string>({
  type: 'string',
  enum: [...REGIONS],
  description: 'Riot regional routing value',
});

/**
 * Riot ID bounds per §12.3. The maxima are Riot's documented hard caps, so a
 * longer value cannot name a real account and is worth rejecting for free. The
 * minima are only "non-empty": Riot's 3-character floor is an account-creation
 * rule, not an API contract, and legacy and staff accounts sit below it (the
 * 2-character region tag lines handed out during the Riot ID rollout, for one).
 * Rejecting those here would 400 a real player to save a request we can afford.
 * See NinjaGoldfinch/riot-proxy#11.
 */
export const GAME_NAME_MIN = 1;
export const GAME_NAME_MAX = 16;
export const TAG_LINE_MIN = 1;
export const TAG_LINE_MAX = 5;

export const GameNameParam = Type.String({
  minLength: GAME_NAME_MIN,
  maxLength: GAME_NAME_MAX,
  description: 'Riot ID game name (the part before the #)',
});

export const TagLineParam = Type.String({
  minLength: TAG_LINE_MIN,
  maxLength: TAG_LINE_MAX,
  description: 'Riot ID tag line (the part after the #)',
});

/** PUUIDs are 78-character encrypted strings, but length varies by key era. */
export const PuuidParam = Type.String({
  minLength: 60,
  maxLength: 128,
  pattern: '^[A-Za-z0-9_-]+$',
  description: "Encrypted PUUID (valid only for the proxy's current Riot key)",
});

export const MatchIdParam = Type.String({
  minLength: 6,
  maxLength: 40,
  pattern: '^[A-Za-z0-9]+_[0-9]+$',
  description: 'Match ID, e.g. EUW1_7381937461',
});

/** §6.2 — match id list: `count` clamped to 1–100 by Riot's own limit. */
export const MatchIdsQuery = Type.Object({
  start: Type.Optional(Type.Integer({ minimum: 0, maximum: 10_000, default: 0 })),
  count: Type.Optional(Type.Integer({ minimum: 1, maximum: 100, default: 20 })),
  queue: Type.Optional(Type.Integer({ minimum: 0, maximum: 5000 })),
  type: Type.Optional(
    Type.Unsafe<string>({ type: 'string', enum: ['ranked', 'normal', 'tourney', 'tutorial'] }),
  ),
  startTime: Type.Optional(Type.Integer({ minimum: 0 })),
  endTime: Type.Optional(Type.Integer({ minimum: 0 })),
});

/**
 * §6.3 — the composite match page. `count` is clamped far below match-v5's own
 * 100 because every id on the page becomes its own upstream call; 20 is the
 * most a single request should be allowed to fan out to.
 */
export const MatchPageQuery = Type.Object({
  platform: Type.Optional(PlatformParam),
  start: Type.Optional(Type.Integer({ minimum: 0, maximum: 10_000, default: 0 })),
  count: Type.Optional(Type.Integer({ minimum: 1, maximum: 20, default: 10 })),
  queue: Type.Optional(Type.Integer({ minimum: 0, maximum: 5000 })),
  type: Type.Optional(
    Type.Unsafe<string>({ type: 'string', enum: ['ranked', 'normal', 'tourney', 'tutorial'] }),
  ),
  /** Spend quota to re-read the id list; rate limited per player. */
  refresh: Type.Optional(Type.Boolean({ default: false })),
});

/**
 * The composite match page is the one response that is not a Riot payload: it
 * is a document we assemble, and the matches on it are summaries (§6.3, and the
 * rationale in `match-summary.ts`). So unlike every other route it declares a
 * real response schema — which documents the shape and, because Fastify
 * serialises against it, guarantees a full match payload cannot find its way
 * back onto this endpoint.
 */
export const MatchPageResponse = Type.Object(
  {
    puuid: Type.String(),
    platform: Type.String(),
    region: Type.String(),
    start: Type.Integer(),
    count: Type.Integer(),
    matchIds: Type.Array(Type.String()),
    matches: Type.Array(MatchSummarySchema),
    hasMore: Type.Boolean(),
    matchIdsAgeSeconds: Type.Number(),
    backfill: Type.Union([
      Type.Object({
        jobId: Type.String(),
        status: Type.Unsafe<string>({ type: 'string', enum: ['queued', 'already-queued'] }),
        limit: Type.Integer(),
      }),
      Type.Null(),
    ]),
    refreshed: Type.Boolean(),
    refreshAvailableIn: Type.Number(),
    warnings: Type.Array(Type.String()),
  },
  { $id: 'MatchPage' },
);

/**
 * §9 — `/v1/admin/limits/:scope` reports one bucket, and a bucket is keyed by
 * whichever host serves the endpoint: platform hosts for the game APIs, region
 * hosts for account-v1 and match-v5. So the param is the union of both, not a
 * platform — passing `europe` here is as valid as passing `euw1`.
 */
export const ScopeParam = Type.Unsafe<string>({
  type: 'string',
  enum: [...PLATFORMS, ...REGIONS],
  description: 'A rate-limit bucket: either a platform host (euw1) or a region host (europe)',
});

/** An ISO-8601 timestamp column that the row may not have set yet. */
const NullableTimestamp = Type.Union([Type.String({ format: 'date-time' }), Type.Null()]);

/**
 * The proxy's own payloads, not Riot's — so unlike the passthrough routes these
 * are real schemas. Fastify serialises against them, which is the point: a
 * `keyHash` added to the consumers table can never reach this endpoint by
 * accident, because a field this object does not name is not emitted.
 *
 * Both are `db.select()` on the whole table, so every column belongs here. A
 * column added to `consumers` or `players` and not added here disappears from
 * the response silently — the tests in `test/docs-schemas.test.ts` assert the
 * full body for exactly that reason.
 */
export const ConsumerSummary = Type.Object(
  {
    id: Type.String({ format: 'uuid' }),
    name: Type.String(),
    scopes: Type.Array(Type.Unsafe<string>({ type: 'string', enum: ['read', 'admin'] })),
    quotaPerMin: Type.Integer({ description: 'Requests per minute allowed to this consumer' }),
    createdAt: Type.String({ format: 'date-time' }),
    disabledAt: {
      ...NullableTimestamp,
      description: 'Set when the key was revoked; revoked consumers are kept, never deleted',
    },
  },
  { $id: 'ConsumerSummary' },
);

export const PlayerSummary = Type.Object(
  {
    puuid: Type.String(),
    keyScope: Type.String({
      description: 'Fingerprint of the Riot key this PUUID was encrypted for',
    }),
    platform: Type.String(),
    gameName: Type.Union([Type.String(), Type.Null()]),
    tagLine: Type.Union([Type.String(), Type.Null()]),
    tracked: Type.Boolean(),
    lastSeenMatchId: {
      ...Type.Union([Type.String(), Type.Null()]),
      description: 'Cursor the match poller resumes from (#46)',
    },
    historyBackfillStartedAt: {
      ...NullableTimestamp,
      description: 'Set with historyBackfilledAt still null means a walk that died mid-way',
    },
    historyBackfilledAt: NullableTimestamp,
    historyBackfillDepth: Type.Union([Type.Integer(), Type.Null()]),
    updatedAt: Type.String({ format: 'date-time' }),
  },
  { $id: 'PlayerSummary' },
);

export const ConsumerListResponse = Type.Object({ consumers: Type.Array(ConsumerSummary) });

export const PlayerListResponse = Type.Object({ players: Type.Array(PlayerSummary) });

export const AdminStatsResponse = Type.Object({
  keyScope: Type.String(),
  archivedMatches: Type.Integer(),
  trackedPlayers: Type.Integer(),
});

export const LimitsResponse = Type.Object({
  scope: Type.String(),
  usage: Type.Unsafe<unknown>({ description: 'Per-bucket token usage as the limiter sees it' }),
  frozenMs: Type.Union([Type.Number(), Type.Null()], {
    description: 'Milliseconds until a 429-induced freeze on this bucket lifts, or null',
  }),
});

/** §6.2 / FR-12 — liveness. */
export const HealthResponse = Type.Object({ ok: Type.Boolean() });

/**
 * §6.2 — readiness. The 503 is the interesting one: it carries the same shape
 * as the 200, so the individual `redis` / `postgres` booleans say which
 * dependency is the reason the service is not ready.
 */
export const ReadyResponse = Type.Object({
  ok: Type.Boolean(),
  redis: Type.Boolean(),
  postgres: Type.Boolean(),
  keyScope: Type.String(),
});

export const MasteryQuery = Type.Object({
  top: Type.Optional(Type.Integer({ minimum: 1, maximum: 200 })),
});

export const ErrorResponse = Type.Object(
  {
    error: Type.Object({
      code: Type.Unsafe<string>({ type: 'string', enum: [...ERROR_CODES] }),
      message: Type.String(),
      retryAfter: Type.Optional(Type.Number()),
    }),
  },
  { $id: 'ErrorResponse' },
);

/**
 * Riot payloads pass through unmodified (§6.1), so response bodies are declared
 * as unconstrained — validating Riot's schema here would break the proxy every
 * time Riot adds a field.
 */
export const PassthroughResponse = Type.Unsafe<unknown>({});

export const errorResponses = {
  400: ErrorResponse,
  401: ErrorResponse,
  403: ErrorResponse,
  404: ErrorResponse,
  429: ErrorResponse,
  502: ErrorResponse,
  503: ErrorResponse,
};

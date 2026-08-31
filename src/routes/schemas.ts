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
import { Type, type Static } from '@sinclair/typebox';
import { ERROR_CODES } from '../errors.js';
import { CRAWL_PHASES, CRAWL_STATUSES } from '../db/ladder.js';
import { APEX_TIERS, DIVISIONS, PAGED_TIERS, RANKED_QUEUES, TIERS } from '../riot/ladder.js';
import { PLATFORMS, REGIONS } from '../riot/routing.js';
import {
  MetricsHistoryPoint,
  MetricsSnapshot,
  type MetricsHistoryPointData,
  type MetricsSnapshotData,
} from '../stats/schema.js';
import { MatchSummarySchema } from './match-summary.js';

/**
 * §12.3 — closed enums for platform/region, clamped counts, and Riot ID length
 * bounds, all enforced before any upstream call is made.
 */

/**
 * §12.3's parameters are declared once and referenced everywhere (#61). The
 * exported name is the `$ref`; the definition it points at is registered on the
 * instance by `sharedSchemas` below. Route files are unaffected — they keep
 * using `PlatformParam` and get a reference instead of a copy — and the emitted
 * document describes a PUUID once rather than eleven times.
 */
export const PlatformParamSchema = Type.Unsafe<string>({
  $id: 'PlatformParam',
  type: 'string',
  enum: [...PLATFORMS],
  title: 'Platform',
  description:
    'Riot platform host (e.g. `euw1`, `na1`, `kr`). Platform endpoints — summoner, spectator, ' +
    'league, mastery, rotations, status — bucket their rate limit by this value.',
});
export const PlatformParam = Type.Unsafe<string>({ $ref: 'PlatformParam#' });

export const RegionParamSchema = Type.Unsafe<string>({
  $id: 'RegionParam',
  type: 'string',
  enum: [...REGIONS],
  title: 'Region',
  description:
    'Riot regional routing value (`americas`, `europe`, `asia`, `sea`). account-v1 and match-v5 ' +
    'are regional, not per-platform. Note `sea`: account-v1 has no `sea` host, so the proxy ' +
    'routes those calls to `asia` on your behalf.',
});
export const RegionParam = Type.Unsafe<string>({ $ref: 'RegionParam#' });

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

export const GameNameParamSchema = Type.String({
  $id: 'GameNameParam',
  minLength: GAME_NAME_MIN,
  maxLength: GAME_NAME_MAX,
  title: 'Riot ID game name',
  description: 'The part of a Riot ID before the `#`.',
});
export const GameNameParam = Type.Unsafe<string>({ $ref: 'GameNameParam#' });

export const TagLineParamSchema = Type.String({
  $id: 'TagLineParam',
  minLength: TAG_LINE_MIN,
  maxLength: TAG_LINE_MAX,
  title: 'Riot ID tag line',
  description: 'The part of a Riot ID after the `#`.',
});
export const TagLineParam = Type.Unsafe<string>({ $ref: 'TagLineParam#' });

/** PUUIDs are 78-character encrypted strings, but length varies by key era. */
export const PuuidParamSchema = Type.String({
  $id: 'PuuidParam',
  minLength: 60,
  maxLength: 128,
  pattern: '^[A-Za-z0-9_-]+$',
  title: 'PUUID',
  description:
    'Encrypted player UUID. Riot encrypts these per API key, so a PUUID is only meaningful to ' +
    "the key that produced it — one obtained elsewhere will not resolve here, and the proxy's " +
    'own stored PUUIDs are stranded by a key rotation (§7.4).',
});
export const PuuidParam = Type.Unsafe<string>({ $ref: 'PuuidParam#' });

export const MatchIdParamSchema = Type.String({
  $id: 'MatchIdParam',
  minLength: 6,
  maxLength: 40,
  pattern: '^[A-Za-z0-9]+_[0-9]+$',
  title: 'Match ID',
  description:
    'Platform-prefixed match identifier, e.g. `EUW1_7381937461`. Unlike a PUUID this is not ' +
    'encrypted, so the match archive survives a key rotation.',
});
export const MatchIdParam = Type.Unsafe<string>({ $ref: 'MatchIdParam#' });

/**
 * league-v4's ladder parameters. The two tier enums are deliberately not one:
 * the paged entries route 400s on an apex tier, so a schema that accepted
 * `MASTER` there would document a request Riot refuses.
 */
export const RankedQueueParamSchema = Type.Unsafe<string>({
  $id: 'RankedQueueParam',
  type: 'string',
  enum: [...RANKED_QUEUES],
  title: 'Ranked queue',
  description: 'Ranked ladder to read: `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`.',
});
export const RankedQueueParam = Type.Unsafe<string>({ $ref: 'RankedQueueParam#' });

export const ApexTierParamSchema = Type.Unsafe<string>({
  $id: 'ApexTierParam',
  type: 'string',
  enum: [...APEX_TIERS],
  title: 'Apex tier',
  description:
    'A tier served whole by its own league endpoint — `MASTER`, `GRANDMASTER`, `CHALLENGER`. ' +
    'One request returns every entry, so these are not paged.',
});
export const ApexTierParam = Type.Unsafe<string>({ $ref: 'ApexTierParam#' });

export const LadderTierParamSchema = Type.Unsafe<string>({
  $id: 'LadderTierParam',
  type: 'string',
  enum: [...PAGED_TIERS],
  title: 'Ladder tier',
  description:
    'A tier walked page by page — `IRON` through `DIAMOND`. The apex tiers are not valid here; ' +
    'read them from the apex league route instead.',
});
export const LadderTierParam = Type.Unsafe<string>({ $ref: 'LadderTierParam#' });

export const DivisionParamSchema = Type.Unsafe<string>({
  $id: 'DivisionParam',
  type: 'string',
  enum: [...DIVISIONS],
  title: 'Division',
  description: 'Division within a tier, `I` (highest) to `IV`.',
});
export const DivisionParam = Type.Unsafe<string>({ $ref: 'DivisionParam#' });

/**
 * Pages are 1-based and ~205 entries wide. A page past the end of a division
 * is an empty array, not a 404, so paging off the end is the documented way to
 * learn where a ladder stops. The maximum is a sanity bound, well clear of the
 * ~7 000 pages the largest division runs to.
 */
export const LadderPageQuery = Type.Object({
  page: Type.Optional(Type.Integer({ minimum: 1, maximum: 100_000, default: 1 })),
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
 * The composite bodies (#63). These are the proxy's own documents rather than
 * Riot's, so §6.1's argument for leaving responses unconstrained does not reach
 * them: they get real schemas, Fastify serialises against them, and the types
 * in `players.ts` are derived from them with `Static<>` so the two cannot drift.
 *
 * The Riot-shaped parts inside them stay unconstrained. `account`, `summoner`,
 * `league`, `mastery` and the match documents behind the summaries are still
 * Riot's payloads, and validating their shape here would break the proxy the
 * next time Riot adds a field.
 */

/** A part of a composite that failed is `null`, not absent. */
const NullableAge = Type.Union([Type.Number(), Type.Null()]);

/**
 * A Riot payload embedded in one of our documents. `{}` rather than a typed
 * schema: fast-json-stringify passes an empty schema through untouched, which
 * is exactly the passthrough guarantee, and the alternative would strip every
 * field Riot adds.
 */
const RiotPart = Type.Unsafe<unknown>({
  description: "Riot's payload, verbatim, or `null` if that part failed — see `warnings`.",
});

export const BackfillNoticeSchema = Type.Object(
  {
    jobId: Type.String(),
    status: Type.Unsafe<string>({
      type: 'string',
      enum: ['queued', 'already-queued'],
      description: '`already-queued` means another request got there first, not that it failed.',
    }),
    limit: Type.Integer({ description: 'How far back the walk will go, in matches.' }),
  },
  {
    $id: 'BackfillNotice',
    description:
      "Present when this lookup queued the player's history for archiving — the first time " +
      'anyone asks for a player. The walk runs at bulk priority, out of the way of interactive ' +
      'traffic, and is why the second page view costs no quota.',
  },
);
export const BackfillNotice = Type.Unsafe<Static<typeof BackfillNoticeSchema>>({
  $ref: 'BackfillNotice#',
});

export const ProfileResponseSchema = Type.Object(
  {
    puuid: Type.String(),
    platform: Type.String(),
    region: Type.String(),
    account: RiotPart,
    summoner: RiotPart,
    league: RiotPart,
    mastery: RiotPart,
    /**
     * Modelled as four named fields rather than an open map, because that is
     * what it is — and an open `Record<string, number | null>` is the shape
     * fast-json-stringify quietly drops the nulls out of.
     */
    ageSeconds: Type.Object(
      {
        account: NullableAge,
        summoner: NullableAge,
        league: NullableAge,
        mastery: NullableAge,
      },
      {
        description:
          'Per part, how long its content has been unchanged. `X-Cache-Age` is the stalest of ' +
          'these — the right answer for a cache header and the wrong one for labelling four ' +
          'independent sections, since an account untouched for a day would otherwise make a ' +
          'rank that moved a minute ago look a day old.',
      },
    ),
    refreshed: Type.Boolean({
      description: 'Whether this request won the refresh window and went upstream.',
    }),
    refreshAvailableIn: Type.Number({
      description: 'Seconds until another `?refresh=true` is allowed for this player; 0 when now.',
    }),
    warnings: Type.Array(Type.String(), {
      description:
        'Names each part that could not be fetched. A composite returns what it has rather than ' +
        'failing whole, so this is how a caller tells a missing section from an empty one.',
    }),
  },
  { $id: 'ProfileBody' },
);
export const ProfileResponse = Type.Unsafe<Static<typeof ProfileResponseSchema>>({
  $ref: 'ProfileBody#',
});

export const MatchPageResponseSchema = Type.Object(
  {
    puuid: Type.String(),
    platform: Type.String(),
    region: Type.String(),
    start: Type.Integer(),
    count: Type.Integer(),
    matchIds: Type.Array(Type.String(), {
      description: 'The id page as Riot returned it, including ids no summary could be built for.',
    }),
    matches: Type.Array(MatchSummarySchema, {
      description:
        "One summary per id that resolved — the requesting player's line in each game, not the " +
        'game. Shorter than `matchIds` when a match could not be fetched.',
    }),
    hasMore: Type.Boolean({
      description: 'A full page came back, so there is probably another behind it. Page on this.',
    }),
    matchIdsAgeSeconds: Type.Number({
      description:
        'How long the id list has been unchanged. The matches behind it are immutable, so this ' +
        'is the only age on the page that can mean anything.',
    }),
    backfill: Type.Union([BackfillNotice, Type.Null()]),
    refreshed: Type.Boolean(),
    refreshAvailableIn: Type.Number(),
    warnings: Type.Array(Type.String()),
  },
  { $id: 'MatchPage' },
);
export const MatchPageResponse = Type.Unsafe<Static<typeof MatchPageResponseSchema>>({
  $ref: 'MatchPage#',
});

/**
 * §9 — `/v1/admin/limits/:scope` reports one bucket, and a bucket is keyed by
 * whichever host serves the endpoint: platform hosts for the game APIs, region
 * hosts for account-v1 and match-v5. So the param is the union of both, not a
 * platform — passing `europe` here is as valid as passing `euw1`.
 */
export const ScopeParamSchema = Type.Unsafe<string>({
  $id: 'ScopeParam',
  type: 'string',
  enum: [...PLATFORMS, ...REGIONS],
  title: 'Rate-limit scope',
  description: 'A rate-limit bucket: either a platform host (`euw1`) or a region host (`europe`).',
});
export const ScopeParam = Type.Unsafe<string>({ $ref: 'ScopeParam#' });

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

/** One crawl run, as the admin listing and the trigger route report it. */
export const LadderCrawlSummary = Type.Object(
  {
    id: Type.String({ format: 'uuid' }),
    platform: Type.String(),
    queue: Type.String(),
    tierFloor: {
      ...Type.String(),
      description: 'How far down the ladder this run was told to enumerate',
    },
    status: Type.Unsafe<string>({ type: 'string', enum: [...CRAWL_STATUSES] }),
    phase: {
      ...Type.Unsafe<string>({ type: 'string', enum: [...CRAWL_PHASES] }),
      description:
        'Which stage a running crawl is in. `enumerate` walks the ladder, `collect` gathers ' +
        'every discovered player’s match ids, and `archive` fetches the matches behind them — ' +
        'in that order, so a match shared by ten players is fetched once.',
    },
    startedAt: Type.String({ format: 'date-time' }),
    finishedAt: NullableTimestamp,
    pagesFetched: Type.Integer(),
    entriesSeen: Type.Integer(),
    playersDiscovered: Type.Integer(),
    backfillsEnqueued: {
      ...Type.Integer(),
      description: 'Players whose match history the collect stage was asked to walk',
    },
    matchIdsSeen: {
      ...Type.Integer(),
      description: 'Distinct matches those players have played, after de-duplication',
    },
    matchesQueued: {
      ...Type.Integer(),
      description: 'How many of those were not already archived, and so cost a fetch',
    },
    pendingLegs: {
      ...Type.Integer(),
      description:
        'Legs of the current stage still outstanding — apex leagues and (tier, division) ' +
        'walks, then match-id batches, then the archive hand-off. 0 for a finished run.',
    },
  },
  { $id: 'LadderCrawlSummary' },
);

export const LadderCrawlListResponse = Type.Object({ crawls: Type.Array(LadderCrawlSummary) });

/**
 * `tierFloor` bounds what the crawl enumerates. It is the one knob that turns
 * three requests into twenty thousand, so it is spelled out rather than left
 * to the reader to infer from the tier list.
 */
export const LadderCrawlBody = Type.Object({
  platform: Type.Optional(PlatformParam),
  queue: Type.Optional(RankedQueueParam),
  tierFloor: Type.Optional({
    ...Type.Unsafe<string>({ type: 'string', enum: [...TIERS] }),
    description:
      'Lowest tier to enumerate, inclusive. Defaults to LADDER_TIER_FLOOR. `MASTER` and above ' +
      'is three requests per queue; `IRON` is the whole ladder, ~15–20 k pages.',
  }),
});

/**
 * What the dashboard's start form needs and cannot work out: the ladders this
 * deployment can crawl, and what it would do if the form were left alone.
 *
 * Served rather than hardcoded in the page because both halves have a source
 * of truth in the repo — the tier and queue lists are league-v4's value space,
 * and the defaults are `LADDER_*` — and a form that drifts from either offers
 * a crawl the service will refuse.
 */
export const LadderOptionsResponse = Type.Object(
  {
    platforms: Type.Array(Type.Object({ id: Type.String(), label: Type.String() }), {
      description: 'Every platform a crawl can name, with its human label',
    }),
    queues: Type.Array(Type.Unsafe<string>({ type: 'string', enum: [...RANKED_QUEUES] })),
    tiers: {
      ...Type.Array(Type.Unsafe<string>({ type: 'string', enum: [...TIERS] })),
      description: 'Tier floors, ascending — `IRON` is the whole ladder, `CHALLENGER` is one call',
    },
    defaults: Type.Object({
      platform: Type.String(),
      queue: Type.String(),
      tierFloor: { ...Type.String(), description: 'LADDER_TIER_FLOOR — how far down to enumerate' },
      backfillTierFloor: {
        ...Type.String(),
        description: 'LADDER_BACKFILL_TIER_FLOOR — how far down to walk match histories',
      },
      backfillLimit: {
        ...Type.Integer(),
        description: 'LADDER_BACKFILL_LIMIT — matches per discovered player. 0 walks nobody.',
      },
    }),
  },
  { $id: 'LadderOptions' },
);

export const LadderCrawlStartedResponse = Type.Object({
  crawlId: Type.String({ format: 'uuid' }),
  status: Type.Unsafe<string>({ type: 'string', enum: ['started', 'already-running'] }),
  /**
   * Which ladder the id names. One live crawl is enforced per
   * `(key_scope, platform, queue)`, so `already-running` is always about the
   * ladder that was asked for — and saying so is what makes that legible:
   * without it a caller watching one ladder start while another is running
   * has an id and no way to tell the two apart.
   */
  platform: Type.String(),
  queue: Type.String(),
  legs: {
    ...Type.Integer(),
    description: 'Jobs fanned out — one per apex league, one per (tier, division) below them',
  },
});

/**
 * A champion's line in one slice of the aggregate.
 *
 * `share` is this champion's fraction of the picks in the slice, not Riot's
 * pick rate: a match is not played "in" a tier — its ten participants can sit
 * in ten different ones — so the denominator a true pick rate needs (games in
 * this tier) is not a number this table has. The share is well defined, is
 * what a chart of the slice actually plots, and does not pretend otherwise.
 */
export const ChampionStatEntry = Type.Object(
  {
    championId: Type.Integer(),
    tier: Type.String(),
    patch: Type.String(),
    games: Type.Integer(),
    wins: Type.Integer(),
    winRate: Type.Number({ minimum: 0, maximum: 1 }),
    share: {
      ...Type.Number({ minimum: 0, maximum: 1 }),
      description: "This champion's games as a fraction of the slice's games",
    },
  },
  { $id: 'ChampionStatEntry' },
);

export const ChampionStatsResponse = Type.Object({
  platform: Type.String(),
  queue: Type.String(),
  tier: {
    ...Type.Union([Type.String(), Type.Null()]),
    description: 'Null when the slice spans every tier the crawl reached',
  },
  patch: {
    ...Type.Union([Type.String(), Type.Null()]),
    description: '`gameVersion` major.minor. Null when nothing has been aggregated yet',
  },
  computedAt: {
    ...NullableTimestamp,
    description: 'When this slice was last recomputed from the archive',
  },
  totalGames: {
    ...Type.Integer(),
    description: 'Games in the slice — the denominator behind `share`',
  },
  champions: Type.Array(
    Type.Unsafe<Static<typeof ChampionStatEntry>>({
      $ref: 'ChampionStatEntry#',
    }),
  ),
});

export const ChampionStatsQuery = Type.Object({
  platform: Type.Optional(PlatformParam),
  queue: Type.Optional(RankedQueueParam),
  tier: Type.Optional(Type.Unsafe<string>({ type: 'string', enum: [...TIERS] })),
  patch: Type.Optional(
    Type.String({
      minLength: 3,
      maxLength: 8,
      pattern: '^[0-9]+\\.[0-9]+$',
      description: 'Defaults to the newest patch this deployment has aggregated',
    }),
  ),
  limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 500, default: 200 })),
});

export const AdminStatsResponse = Type.Object({
  keyScope: Type.String(),
  archivedMatches: Type.Integer(),
  trackedPlayers: Type.Integer(),
});

/**
 * The full operational snapshot — defined in `src/stats/schema.ts` because the
 * `metrics` WebSocket topic publishes the identical document and the event
 * catalogue derives its type from the same schema. Referenced rather than
 * inlined so the reference names the shape once (#61).
 */
export const MetricsResponse = Type.Unsafe<MetricsSnapshotData>({ $ref: 'MetricsSnapshot#' });

export const MetricsHistoryResponse = Type.Object({
  intervalS: Type.Integer({
    description: 'Seconds between points (`METRICS_HISTORY_INTERVAL_S`)',
  }),
  maxPoints: Type.Integer({
    description: 'Retention cap — the list holds at most this many points, oldest dropped first',
  }),
  points: Type.Array(Type.Unsafe<MetricsHistoryPointData>({ $ref: 'MetricsHistoryPoint#' }), {
    description: 'Oldest first',
  }),
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

/**
 * The envelope, referenced rather than copied. Attaching `ErrorResponse` itself
 * to seven statuses on twenty-six routes put 182 copies of the same object in
 * the emitted document and left `components/schemas` empty, because a `$id`
 * alone does not register anything — `fastify.addSchema` does (#61).
 */
const errorRef = Type.Unsafe<Static<typeof ErrorResponse>>({ $ref: 'ErrorResponse#' });

const VALIDATION_ERRORS = { 400: errorRef } as const;
const AUTH_ERRORS = { 401: errorRef, 403: errorRef } as const;
const QUOTA_ERROR = { 429: errorRef } as const;
const NOT_FOUND_ERROR = { 404: errorRef } as const;
const UPSTREAM_ERRORS = { 502: errorRef, 503: errorRef } as const;

/**
 * Which failures a route can actually produce. Attaching all seven statuses to
 * everything was convenient and dishonest: `/v1/static/{file}` never calls Riot
 * and cannot return 502, and a reference that lists impossible failures teaches
 * callers to handle noise.
 *
 * The lists are drawn from `toProxyError` in `app.ts` and from the rate-limit
 * plugin's `errorResponseBuilder`, not from intuition. Two consequences of that
 * are easy to get backwards:
 *
 *   - 429 belongs on every route the auth hook covers, including the ones that
 *     never leave the process. The quota is the plugin's, applied to anything
 *     not marked `config.public`, so the static mirror and the admin bookkeeping
 *     routes can all exhaust it.
 *   - 404 belongs on the local routes too. It is ours as often as it is Riot's
 *     — an unknown Data Dragon file, a consumer id that does not exist.
 */

/** Routes that reach Riot: everything can happen, including the upstream's own. */
export const upstreamErrors = {
  ...VALIDATION_ERRORS,
  ...AUTH_ERRORS,
  ...NOT_FOUND_ERROR,
  ...QUOTA_ERROR,
  ...UPSTREAM_ERRORS,
};

/**
 * Routes served entirely from this process — the Data Dragon mirror, admin
 * bookkeeping. Authenticated and quota-bearing, but with no upstream to fail.
 */
export const localErrors = {
  ...VALIDATION_ERRORS,
  ...AUTH_ERRORS,
  ...NOT_FOUND_ERROR,
  ...QUOTA_ERROR,
};

/**
 * There is deliberately no third set for the `config.public` routes. The plan
 * called for one, but there is nothing to put in it: `/healthz`, `/readyz` and
 * `/metrics` take no parameters, so they cannot fail validation; the auth hook
 * does not run on them and the rate limiter's allowList exempts them, so no
 * 401, 403 or 429; and none of them reaches Riot. They produce no error
 * envelope at all, and declaring one would be the same dishonesty at a smaller
 * scale. `/readyz`'s 503 is a success-path response carrying the ready body,
 * not an error, and is declared on the route itself.
 */

/**
 * Registered on the instance in `buildApp()` before the route plugins, so both
 * the validator and the emitted document resolve the references above.
 */
export const sharedSchemas = [
  ErrorResponse,
  MetricsSnapshot,
  MetricsHistoryPoint,
  PlatformParamSchema,
  RegionParamSchema,
  GameNameParamSchema,
  TagLineParamSchema,
  PuuidParamSchema,
  MatchIdParamSchema,
  RankedQueueParamSchema,
  ApexTierParamSchema,
  LadderTierParamSchema,
  DivisionParamSchema,
  ScopeParamSchema,
  ChampionStatEntry,
  BackfillNoticeSchema,
  ProfileResponseSchema,
  MatchPageResponseSchema,
  LadderCrawlSummary,
];

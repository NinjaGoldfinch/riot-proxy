import { sql } from 'drizzle-orm';
import {
  bigint,
  boolean,
  index,
  integer,
  jsonb,
  pgTable,
  primaryKey,
  smallint,
  text,
  timestamp,
  uniqueIndex,
  uuid,
} from 'drizzle-orm/pg-core';
import { DEFAULT_QUOTA_PER_MIN } from '../quotas.js';

/** §7.1 — downstream projects. Only the sha256 of the bearer token is stored. */
export const consumers = pgTable('consumers', {
  id: uuid('id').primaryKey().defaultRandom(),
  name: text('name').notNull(),
  keyHash: text('key_hash').notNull().unique(),
  scopes: text('scopes')
    .array()
    .notNull()
    .default(sql`'{read}'`),
  quotaPerMin: integer('quota_per_min').notNull().default(DEFAULT_QUOTA_PER_MIN),
  createdAt: timestamp('created_at', { withTimezone: true }).notNull().defaultNow(),
  disabledAt: timestamp('disabled_at', { withTimezone: true }),
});

/**
 * §7.4 — PUUIDs are encrypted per API key, so the primary key is
 * (key_scope, puuid). Rotating the Riot key strands the old rows instead of
 * silently poisoning lookups.
 */
export const players = pgTable(
  'players',
  {
    puuid: text('puuid').notNull(),
    keyScope: text('key_scope').notNull(),
    platform: text('platform').notNull(),
    gameName: text('game_name'),
    tagLine: text('tag_line'),
    tracked: boolean('tracked').notNull().default(false),
    lastSeenMatchId: text('last_seen_match_id'),
    /**
     * Whether anyone has ever walked this player's history (#44). The archive
     * cannot answer that question: matches are shared, so a player's games can
     * be stored entirely because a teammate was walked. Only a row here means
     * "we did this player".
     *
     * `startedAt` set with `backfilledAt` still null is a walk that is running
     * or died mid-way — distinguishable from one that never ran, so a failed
     * walk retries instead of counting as done. `depth` is how far back the
     * walk actually got, so a `LOOKUP_BACKFILL_LIMIT` raised later can be told
     * apart from a history that simply ended.
     */
    historyBackfillStartedAt: timestamp('history_backfill_started_at', { withTimezone: true }),
    historyBackfilledAt: timestamp('history_backfilled_at', { withTimezone: true }),
    historyBackfillDepth: integer('history_backfill_depth'),
    updatedAt: timestamp('updated_at', { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [
    primaryKey({ columns: [t.keyScope, t.puuid] }),
    index('players_tracked_idx').on(t.tracked, t.keyScope),
  ],
);

/**
 * Match IDs (`EUW1_7381937461`) are *not* encrypted, so the archive survives
 * key rotation and needs no key_scope (§7.4).
 */
export const matches = pgTable(
  'matches',
  {
    matchId: text('match_id').primaryKey(),
    region: text('region').notNull(),
    queueId: integer('queue_id').generatedAlwaysAs(sql`((data->'info'->>'queueId')::int)`),
    gameEndTs: bigint('game_end_ts', { mode: 'number' }).generatedAlwaysAs(
      sql`((data->'info'->>'gameEndTimestamp')::bigint)`,
    ),
    /**
     * `gameVersion` major.minor (#109) — Data Dragon's own version list is a
     * different numbering (its third component is a Data Dragon build, not a
     * game one), so this stays independent of the mirror. Generated + stored
     * so the recompute (`recomputeChampionStats`) and its future
     * `AGGREGATE_PATCH_LIMIT` bound never open `data` for it. Null
     * `gameVersion` yields a null patch, same exclusion the aggregate applied
     * before this column existed.
     */
    patch: text('patch').generatedAlwaysAs(
      sql`(split_part(data->'info'->>'gameVersion', '.', 1) || '.' || split_part(data->'info'->>'gameVersion', '.', 2))`,
    ),
    /**
     * Seconds (#109) — every `gameVersion` this service can have archived
     * postdates Riot's 11.20 switch away from milliseconds, so no unit
     * branch is needed here.
     */
    gameDuration: integer('game_duration').generatedAlwaysAs(
      sql`((data->'info'->>'gameDuration')::int)`,
    ),
    data: jsonb('data').notNull(),
    timeline: jsonb('timeline'),
    fetchedAt: timestamp('fetched_at', { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [
    index('matches_game_end_idx').on(t.gameEndTs),
    index('matches_queue_patch_idx').on(t.queueId, t.patch),
    /**
     * Declared to match `0000_init.sql`, which created this index directly —
     * `schema.ts` didn't declare it, so a `drizzle-kit generate` would have
     * emitted a DROP for a live index (#109).
     */
    index('matches_participants_gin').using('gin', sql`(data->'metadata'->'participants')`),
  ],
);

/**
 * One row per participant. `(champion_id, win)` was the whole table through L5
 * (#90); the rest are facts C2 (#110) extracts once, at archive time, so every
 * aggregate that needs them reads a column instead of opening `matches.data`.
 *
 * All nullable and best-effort: `extractParticipants` (`src/db/matches.ts`)
 * writes whatever a participant object actually has, and Riot's own payload
 * omits fields by patch, queue and game mode (Arena's `placement`/`subteam_id`
 * exist nowhere else; ARAM has no `team_position`). Absent stays null rather
 * than a guessed default, the same rule `match-summary.ts` follows for the same
 * payload.
 */
export const matchParticipants = pgTable(
  'match_participants',
  {
    matchId: text('match_id')
      .notNull()
      .references(() => matches.matchId, { onDelete: 'cascade' }),
    puuid: text('puuid').notNull(),
    championId: integer('champion_id'),
    win: boolean('win'),
    teamId: smallint('team_id'),
    /** `''` in ARAM/Arena, per Riot; absent entirely on very old archives. */
    teamPosition: text('team_position'),
    kills: smallint('kills'),
    deaths: smallint('deaths'),
    assists: smallint('assists'),
    /** `totalMinionsKilled + neutralMinionsKilled`. */
    cs: integer('cs'),
    gold: integer('gold'),
    damage: integer('damage'),
    vision: integer('vision'),
    item0: integer('item0'),
    item1: integer('item1'),
    item2: integer('item2'),
    item3: integer('item3'),
    item4: integer('item4'),
    item5: integer('item5'),
    /** The trinket slot (`item6`) is never a build choice, so it is skipped. */
    keystoneId: integer('keystone_id'),
    /** The secondary rune *tree*, not a specific rune — `perks.styles[1].style`. */
    subStyleId: integer('sub_style_id'),
    spell1: integer('spell1'),
    spell2: integer('spell2'),
    /** Arena (queue 1700) only. */
    placement: smallint('placement'),
    /** Arena's team-of-two id. Riot's field is `playerSubteamId`, not `subteamId`. */
    subteamId: smallint('subteam_id'),
  },
  (t) => [
    primaryKey({ columns: [t.matchId, t.puuid] }),
    index('match_participants_puuid_idx').on(t.puuid),
  ],
);

/**
 * `info.teams[].bans[]`, one row per ban (#110). `championId: -1` — no pick
 * made in that slot — is skipped at extraction; a row here always names a real
 * champion.
 *
 * No `key_scope`, matching `match_participants`: a ban is a fact about the
 * match, not about a player, so nothing here is encrypted (§7.4).
 */
export const matchBans = pgTable(
  'match_bans',
  {
    matchId: text('match_id')
      .notNull()
      .references(() => matches.matchId, { onDelete: 'cascade' }),
    teamId: smallint('team_id').notNull(),
    pickTurn: smallint('pick_turn').notNull(),
    championId: integer('champion_id').notNull(),
  },
  // The PK's leading column gives "every ban in this match" its index for free.
  (t) => [primaryKey({ columns: [t.matchId, t.teamId, t.pickTurn] })],
);

/**
 * One row per crawl run (#87) — the unit of observability and resumability.
 * Per-(tier, division) page cursors live in Redis while a crawl is running,
 * because they churn on every page; this row is the durable summary.
 *
 * Key-scoped like `players`: a crawl enumerates PUUIDs, and those are only
 * meaningful to the key that produced them (§7.4).
 */
export const ladderCrawls = pgTable(
  'ladder_crawls',
  {
    id: uuid('id').primaryKey().defaultRandom(),
    keyScope: text('key_scope').notNull(),
    platform: text('platform').notNull(),
    queue: text('queue').notNull(),
    /** The tier the crawl was told to enumerate down to, recorded per run so
     * a ladder read later can tell "nobody below Master" from "we never
     * looked below Master". */
    tierFloor: text('tier_floor').notNull(),
    status: text('status').notNull().default('running'),
    /**
     * Which of the three stages a running crawl is in: `enumerate` walks the
     * ladder, `collect` asks every discovered player for their match ids, and
     * `archive` fetches the matches behind them.
     *
     * They are stages rather than one interleaved pipeline because a match is
     * shared by ten players. Fetching a player's matches the moment they are
     * discovered means the ten participants of one game are discovered at ten
     * different times, and whichever of them is walked first pays for the
     * match the other nine would have found in the archive — but only if their
     * walks happen *after* it landed. Holding every id until they are all in
     * makes that a single de-duplicated set, so a match is fetched once.
     */
    phase: text('phase').notNull().default('enumerate'),
    startedAt: timestamp('started_at', { withTimezone: true }).notNull().defaultNow(),
    finishedAt: timestamp('finished_at', { withTimezone: true }),
    pagesFetched: integer('pages_fetched').notNull().default(0),
    entriesSeen: integer('entries_seen').notNull().default(0),
    playersDiscovered: integer('players_discovered').notNull().default(0),
    backfillsEnqueued: integer('backfills_enqueued').notNull().default(0),
    /** Distinct match ids the collect stage gathered, after de-duplication. */
    matchIdsSeen: integer('match_ids_seen').notNull().default(0),
    /** How many of those were not already archived, and so cost a fetch. */
    matchesQueued: integer('matches_queued').notNull().default(0),
  },
  (t) => [
    /**
     * "One live crawl per (key_scope, platform, queue)" is the rule #88's
     * trigger route enforces, and a rule enforced by a read-then-insert in a
     * job processor is a rule two workers can both pass. A partial unique
     * index makes the database say no instead, so the second trigger loses a
     * race it cannot see rather than starting a duplicate crawl.
     */
    uniqueIndex('ladder_crawls_live_idx')
      .on(t.keyScope, t.platform, t.queue)
      .where(sql`status = 'running'`),
    index('ladder_crawls_recent_idx').on(t.keyScope, t.platform, t.queue, t.startedAt),
  ],
);

/**
 * The ladder itself — latest state, not history. A rank timeseries is
 * unbounded growth with no retention story, and is a non-goal until it has
 * one (§10 of the plan).
 *
 * `last_seen_crawl_id` is what makes that enough: rows stamped with the newest
 * completed crawl are the current ladder, and the rows it did not touch are
 * the players who dropped, decayed or were never re-seen. Both questions are
 * one indexed predicate rather than a table of deltas.
 *
 * No foreign key to `ladder_crawls` on purpose. The crawl rows are a run log
 * that a retention policy may one day prune, and the ladder has to outlive its
 * log — a cascade there would delete the data for the sake of the bookkeeping.
 */
export const leagueEntries = pgTable(
  'league_entries',
  {
    keyScope: text('key_scope').notNull(),
    platform: text('platform').notNull(),
    queue: text('queue').notNull(),
    puuid: text('puuid').notNull(),
    tier: text('tier').notNull(),
    /** Riot's `rank` field. Named `division` here because `rank` reads as the
     * whole standing, and apex entries report `I` for everyone. */
    division: text('division').notNull(),
    leaguePoints: integer('league_points').notNull(),
    wins: integer('wins').notNull(),
    losses: integer('losses').notNull(),
    veteran: boolean('veteran').notNull().default(false),
    inactive: boolean('inactive').notNull().default(false),
    freshBlood: boolean('fresh_blood').notNull().default(false),
    hotStreak: boolean('hot_streak').notNull().default(false),
    firstSeenCrawlId: uuid('first_seen_crawl_id').notNull(),
    lastSeenCrawlId: uuid('last_seen_crawl_id').notNull(),
    updatedAt: timestamp('updated_at', { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [
    primaryKey({ columns: [t.keyScope, t.platform, t.queue, t.puuid] }),
    /** Leaderboard order: one tier and division, best first. */
    index('league_entries_ladder_idx').on(
      t.keyScope,
      t.platform,
      t.queue,
      t.tier,
      t.division,
      t.leaguePoints,
    ),
    /** "Everything this crawl saw", and its complement. */
    index('league_entries_last_seen_idx').on(t.lastSeenCrawlId),
    /** One player's standing across queues — the PK cannot serve this, since
     * puuid is its last column. */
    index('league_entries_puuid_idx').on(t.keyScope, t.puuid),
  ],
);

/**
 * What the archive says, once the ladder gives it a tier (#90).
 *
 * Recomputed from `matches` × `match_participants` × `league_entries` rather
 * than accumulated on archive-write: the archive is immutable, so a recompute
 * is idempotent and needs no reconciliation, and the ladder underneath it
 * moves — a player promoted since the last crawl should count in their new
 * tier, which an incremental counter could never go back and fix.
 *
 * A table rather than a materialized view because the unit of work is one
 * (platform, queue) — the thing a crawl finishes — and `REFRESH MATERIALIZED
 * VIEW` has no `WHERE`: a Korean crawl completing would recompute EUW as well.
 * The table also carries `computed_at`, which a view has nowhere to put.
 *
 * `patch` is `gameVersion`'s major.minor. Data Dragon's own version list is a
 * different numbering — its third component is a Data Dragon build, not a game
 * one — so mapping onto it would still group by major.minor and would make the
 * aggregate depend on the mirror being present.
 */
export const championStats = pgTable(
  'champion_stats',
  {
    keyScope: text('key_scope').notNull(),
    platform: text('platform').notNull(),
    queue: text('queue').notNull(),
    tier: text('tier').notNull(),
    patch: text('patch').notNull(),
    championId: integer('champion_id').notNull(),
    games: integer('games').notNull(),
    wins: integer('wins').notNull(),
    computedAt: timestamp('computed_at', { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [
    primaryKey({
      columns: [t.keyScope, t.platform, t.queue, t.tier, t.patch, t.championId],
    }),
    /**
     * The read route's shape: a slice is one (platform, queue, patch) and
     * optionally one tier, ordered by how often a champion was played. The
     * primary key cannot serve it — `patch` sits behind `tier` there, and the
     * common question is "this patch, all tiers".
     */
    index('champion_stats_slice_idx').on(t.keyScope, t.platform, t.queue, t.patch, t.tier, t.games),
  ],
);

export type Consumer = typeof consumers.$inferSelect;
export type NewConsumer = typeof consumers.$inferInsert;
export type Player = typeof players.$inferSelect;
export type NewPlayer = typeof players.$inferInsert;
export type LadderCrawl = typeof ladderCrawls.$inferSelect;
export type NewLadderCrawl = typeof ladderCrawls.$inferInsert;
export type LeagueEntry = typeof leagueEntries.$inferSelect;
export type NewLeagueEntry = typeof leagueEntries.$inferInsert;
export type ChampionStat = typeof championStats.$inferSelect;

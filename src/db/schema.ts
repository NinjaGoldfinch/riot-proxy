import { sql } from 'drizzle-orm';
import {
  bigint,
  boolean,
  index,
  integer,
  jsonb,
  pgTable,
  primaryKey,
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
    data: jsonb('data').notNull(),
    timeline: jsonb('timeline'),
    fetchedAt: timestamp('fetched_at', { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [index('matches_game_end_idx').on(t.gameEndTs)],
);

/** Optional denormalisation for "matches where this player appeared" queries. */
export const matchParticipants = pgTable(
  'match_participants',
  {
    matchId: text('match_id')
      .notNull()
      .references(() => matches.matchId, { onDelete: 'cascade' }),
    puuid: text('puuid').notNull(),
    championId: integer('champion_id'),
    win: boolean('win'),
  },
  (t) => [
    primaryKey({ columns: [t.matchId, t.puuid] }),
    index('match_participants_puuid_idx').on(t.puuid),
  ],
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
    startedAt: timestamp('started_at', { withTimezone: true }).notNull().defaultNow(),
    finishedAt: timestamp('finished_at', { withTimezone: true }),
    pagesFetched: integer('pages_fetched').notNull().default(0),
    entriesSeen: integer('entries_seen').notNull().default(0),
    playersDiscovered: integer('players_discovered').notNull().default(0),
    backfillsEnqueued: integer('backfills_enqueued').notNull().default(0),
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

export type Consumer = typeof consumers.$inferSelect;
export type NewConsumer = typeof consumers.$inferInsert;
export type Player = typeof players.$inferSelect;
export type NewPlayer = typeof players.$inferInsert;
export type LadderCrawl = typeof ladderCrawls.$inferSelect;
export type NewLadderCrawl = typeof ladderCrawls.$inferInsert;
export type LeagueEntry = typeof leagueEntries.$inferSelect;
export type NewLeagueEntry = typeof leagueEntries.$inferInsert;

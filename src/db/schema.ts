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
  uuid,
} from 'drizzle-orm/pg-core';

/** §7.1 — downstream projects. Only the sha256 of the bearer token is stored. */
export const consumers = pgTable('consumers', {
  id: uuid('id').primaryKey().defaultRandom(),
  name: text('name').notNull(),
  keyHash: text('key_hash').notNull().unique(),
  scopes: text('scopes')
    .array()
    .notNull()
    .default(sql`'{read}'`),
  quotaPerMin: integer('quota_per_min').notNull().default(600),
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

export type Consumer = typeof consumers.$inferSelect;
export type NewConsumer = typeof consumers.$inferInsert;
export type Player = typeof players.$inferSelect;
export type NewPlayer = typeof players.$inferInsert;
export type MatchRow = typeof matches.$inferSelect;

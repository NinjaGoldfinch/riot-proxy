import { and, eq, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import { db } from './index.js';
import { players, type Player } from './schema.js';

/**
 * Every read and write here is scoped by KEY_SCOPE (§7.4) — PUUIDs from a
 * different Riot key are a different namespace, not the same player.
 */

export interface UpsertPlayerInput {
  puuid: string;
  platform: string;
  gameName?: string | null;
  tagLine?: string | null;
  tracked?: boolean;
  lastSeenMatchId?: string | null;
}

/**
 * Absent fields are left as they are rather than overwritten. The lookup path
 * calls this with nothing but a PUUID and a platform, and must not blank the
 * Riot ID an admin track put there — so only what the caller actually passed
 * is written. Passing an explicit `null` still clears the column.
 */
export async function upsertPlayer(input: UpsertPlayerInput): Promise<Player> {
  const provided = {
    ...(input.gameName !== undefined ? { gameName: input.gameName } : {}),
    ...(input.tagLine !== undefined ? { tagLine: input.tagLine } : {}),
    ...(input.tracked !== undefined ? { tracked: input.tracked } : {}),
    ...(input.lastSeenMatchId !== undefined ? { lastSeenMatchId: input.lastSeenMatchId } : {}),
  };

  const [row] = await db
    .insert(players)
    .values({
      puuid: input.puuid,
      keyScope: KEY_SCOPE,
      platform: input.platform,
      ...provided,
      updatedAt: new Date(),
    })
    .onConflictDoUpdate({
      target: [players.keyScope, players.puuid],
      set: { platform: input.platform, ...provided, updatedAt: new Date() },
    })
    .returning();

  if (!row) throw new Error('failed to upsert player');
  return row;
}

export async function getPlayer(puuid: string): Promise<Player | undefined> {
  const rows = await db
    .select()
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)))
    .limit(1);
  return rows[0];
}

export async function listTrackedPlayers(): Promise<Player[]> {
  return db
    .select()
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.tracked, true)));
}

export async function listPlayers(): Promise<Player[]> {
  return db.select().from(players).where(eq(players.keyScope, KEY_SCOPE));
}

/**
 * How many players this key scope is tracking. `listPlayers().filter(...)`
 * answered the same question by pulling every row across the wire to produce
 * one number; `countArchivedMatches` next door already had the right shape.
 */
export async function countTrackedPlayers(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(players)
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.tracked, true)));
  return rows[0]?.n ?? 0;
}

/** Every player ever seen under this key scope, tracked or not. */
export async function countPlayers(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(players)
    .where(eq(players.keyScope, KEY_SCOPE));
  return rows[0]?.n ?? 0;
}

export async function setTracked(puuid: string, tracked: boolean): Promise<boolean> {
  const rows = await db
    .update(players)
    .set({ tracked, updatedAt: new Date() })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)))
    .returning({ puuid: players.puuid });
  return rows.length > 0;
}

export async function setLastSeenMatch(puuid: string, matchId: string): Promise<void> {
  await db
    .update(players)
    .set({ lastSeenMatchId: matchId, updatedAt: new Date() })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)));
}

/**
 * #44 — a walk is starting. Upserts, because the admin backfill route can name
 * a player nobody has looked up yet, and the stamp needs a row to live on.
 *
 * Recording the start rather than only the finish is what makes a walk that
 * died mid-way distinguishable from one that never ran.
 */
export async function markBackfillStarted(puuid: string, platform: string): Promise<void> {
  const now = new Date();
  await db
    .insert(players)
    .values({
      puuid,
      keyScope: KEY_SCOPE,
      platform,
      historyBackfillStartedAt: now,
      updatedAt: now,
    })
    .onConflictDoUpdate({
      target: [players.keyScope, players.puuid],
      set: { historyBackfillStartedAt: now, updatedAt: now },
    });
}

/**
 * The walk reached the end of the history (or its limit). `depth` is how many
 * matches back it actually got, so a limit raised later can be told apart from
 * a history that simply ran out.
 */
export async function markBackfillComplete(puuid: string, depth: number): Promise<void> {
  const now = new Date();
  await db
    .update(players)
    .set({ historyBackfilledAt: now, historyBackfillDepth: depth, updatedAt: now })
    .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid)));
}

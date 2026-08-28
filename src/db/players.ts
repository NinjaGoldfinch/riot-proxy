import { and, eq } from 'drizzle-orm';
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

export async function upsertPlayer(input: UpsertPlayerInput): Promise<Player> {
  const values = {
    puuid: input.puuid,
    keyScope: KEY_SCOPE,
    platform: input.platform,
    gameName: input.gameName ?? null,
    tagLine: input.tagLine ?? null,
    ...(input.tracked !== undefined ? { tracked: input.tracked } : {}),
    ...(input.lastSeenMatchId !== undefined ? { lastSeenMatchId: input.lastSeenMatchId } : {}),
    updatedAt: new Date(),
  };

  const [row] = await db
    .insert(players)
    .values(values)
    .onConflictDoUpdate({
      target: [players.keyScope, players.puuid],
      set: {
        platform: values.platform,
        gameName: values.gameName,
        tagLine: values.tagLine,
        ...(input.tracked !== undefined ? { tracked: input.tracked } : {}),
        ...(input.lastSeenMatchId !== undefined ? { lastSeenMatchId: input.lastSeenMatchId } : {}),
        updatedAt: values.updatedAt,
      },
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

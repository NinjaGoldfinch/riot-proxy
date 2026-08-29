import { eq, inArray, sql as raw } from 'drizzle-orm';
import { archivedMatchesTotal } from '../metrics.js';
import { db } from './index.js';
import { matchParticipants, matches } from './schema.js';

/**
 * §7.3 / §8.2 — completed matches are immutable. Riot should be hit once per
 * match, ever; after that the archive answers.
 */

interface MatchInfoParticipant {
  puuid?: string;
  championId?: number;
  win?: boolean;
}

export interface RiotMatch {
  metadata?: { matchId?: string; participants?: string[] };
  info?: { participants?: MatchInfoParticipant[]; gameEndTimestamp?: number; queueId?: number };
}

export async function getArchivedMatch(matchId: string): Promise<unknown | undefined> {
  const rows = await db
    .select({ data: matches.data })
    .from(matches)
    .where(eq(matches.matchId, matchId))
    .limit(1);
  return rows[0]?.data;
}

export async function getArchivedTimeline(matchId: string): Promise<unknown | undefined> {
  const rows = await db
    .select({ timeline: matches.timeline })
    .from(matches)
    .where(eq(matches.matchId, matchId))
    .limit(1);
  return rows[0]?.timeline ?? undefined;
}

export async function hasArchivedMatch(matchId: string): Promise<boolean> {
  const rows = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .where(eq(matches.matchId, matchId))
    .limit(1);
  return rows.length > 0;
}

/**
 * Idempotent (§10): archiving the same match twice is a no-op apart from
 * refreshing `fetched_at`. A timeline arriving later fills the null column
 * without disturbing the match body.
 */
export async function archiveMatch(
  matchId: string,
  region: string,
  data: RiotMatch,
): Promise<void> {
  await db
    .insert(matches)
    .values({ matchId, region, data })
    .onConflictDoUpdate({
      target: matches.matchId,
      set: { data, region, fetchedAt: new Date() },
    });

  archivedMatchesTotal.inc();

  const participants = extractParticipants(data);
  if (participants.length === 0) return;

  await db
    .insert(matchParticipants)
    .values(participants.map((p) => ({ matchId, ...p })))
    .onConflictDoNothing();
}

export async function archiveTimeline(
  matchId: string,
  region: string,
  timeline: unknown,
  matchData?: RiotMatch,
): Promise<void> {
  // The timeline can arrive before the match body (a direct timeline request).
  // Insert a placeholder row only when we actually have the match data —
  // `data` is NOT NULL, so a timeline-only archive must wait for the match.
  if (matchData) {
    await db
      .insert(matches)
      .values({ matchId, region, data: matchData, timeline })
      .onConflictDoUpdate({
        target: matches.matchId,
        set: { timeline, region, fetchedAt: new Date() },
      });
    return;
  }

  await db.update(matches).set({ timeline }).where(eq(matches.matchId, matchId));
}

function extractParticipants(data: RiotMatch): {
  puuid: string;
  championId: number | null;
  win: boolean | null;
}[] {
  const infoParticipants = data.info?.participants ?? [];
  if (infoParticipants.length > 0) {
    return infoParticipants
      .filter((p): p is MatchInfoParticipant & { puuid: string } => typeof p.puuid === 'string')
      .map((p) => ({
        puuid: p.puuid,
        championId: typeof p.championId === 'number' ? p.championId : null,
        win: typeof p.win === 'boolean' ? p.win : null,
      }));
  }
  // Fall back to metadata when only IDs are present.
  return (data.metadata?.participants ?? []).map((puuid) => ({
    puuid,
    championId: null,
    win: null,
  }));
}

export async function countArchivedMatches(): Promise<number> {
  const rows = await db.select({ n: raw<number>`count(*)::int` }).from(matches);
  return rows[0]?.n ?? 0;
}

/** Which of these match IDs are already archived — used to skip backfill work. */
export async function filterUnarchived(matchIds: string[]): Promise<string[]> {
  if (matchIds.length === 0) return [];
  // `sql\`… = ANY(${array})\`` expands a JS array to a parenthesised
  // placeholder list — `= ANY(($1, $2, …))` — which Postgres reads as a row
  // constructor and rejects. `inArray` emits a plain `in (…)` instead.
  // Callers batch at 100 ids, well inside the bind-parameter limit.
  const rows = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .where(inArray(matches.matchId, matchIds));
  const known = new Set(rows.map((r) => r.matchId));
  return matchIds.filter((id) => !known.has(id));
}

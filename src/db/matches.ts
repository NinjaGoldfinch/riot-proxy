import { eq, gt, inArray, sql as raw } from 'drizzle-orm';
import { archivedMatchesTotal } from '../metrics.js';
import { db } from './index.js';
import { matchBans, matchParticipants, matches } from './schema.js';

/**
 * §7.3 / §8.2 — completed matches are immutable. Riot should be hit once per
 * match, ever; after that the archive answers.
 */

interface MatchInfoPerks {
  styles?: { description?: string; style?: number; selections?: { perk?: number }[] }[];
}

interface MatchInfoParticipant {
  puuid?: string;
  championId?: number;
  win?: boolean;
  teamId?: number;
  teamPosition?: string;
  kills?: number;
  deaths?: number;
  assists?: number;
  totalMinionsKilled?: number;
  neutralMinionsKilled?: number;
  goldEarned?: number;
  visionScore?: number;
  totalDamageDealtToChampions?: number;
  item0?: number;
  item1?: number;
  item2?: number;
  item3?: number;
  item4?: number;
  item5?: number;
  summoner1Id?: number;
  summoner2Id?: number;
  perks?: MatchInfoPerks;
  placement?: number;
  /** Arena's field name — not `subteamId` (verified against real payloads in match-summary.ts). */
  playerSubteamId?: number;
}

interface MatchInfoBan {
  championId?: number;
  pickTurn?: number;
}

interface MatchInfoTeam {
  teamId?: number;
  bans?: MatchInfoBan[];
}

export interface RiotMatch {
  metadata?: { matchId?: string; participants?: string[] };
  info?: {
    participants?: MatchInfoParticipant[];
    teams?: MatchInfoTeam[];
    gameEndTimestamp?: number;
    queueId?: number;
  };
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

/**
 * The archived bodies for a page of match ids, keyed by id and missing the ones
 * that are not stored.
 *
 * `getArchivedMatch` answers for one match, which is right for `Fetcher`: it
 * looks a single request up in the archive before spending quota on it. The
 * composite match page is the one caller that turns a single request into
 * twenty of them, and a fully archived page — the case the archive exists to
 * make cheap — then issued twenty single-row queries against a pool of ten, so
 * half of them waited on the other half before the page could be assembled
 * (#54). Same bind-parameter reasoning as `filterUnarchived`; the page's
 * `count` is capped at 20.
 */
export async function getArchivedMatches(matchIds: string[]): Promise<Map<string, unknown>> {
  if (matchIds.length === 0) return new Map();
  const rows = await db
    .select({ matchId: matches.matchId, data: matches.data })
    .from(matches)
    .where(inArray(matches.matchId, matchIds));
  return new Map(rows.map((row) => [row.matchId, row.data]));
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
  await applyMatchFacts(matchId, data);
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

const num = (value: unknown): number | null => (typeof value === 'number' ? value : null);
const str = (value: unknown): string | null => (typeof value === 'string' ? value : null);

/**
 * `perks.styles` is an array of two entries — primary and sub — and all a fact
 * row needs from it is the keystone (the primary style's first selection) and
 * the sub style's tree id, which is the pairing `champion_runes` groups on
 * (§7.2 of the plan). Not a specific rune: `sub_style_id` names the tree.
 */
function extractPerks(perks: MatchInfoPerks | undefined): {
  keystoneId: number | null;
  subStyleId: number | null;
} {
  const styles = perks?.styles ?? [];
  const primary = styles.find((s) => s.description === 'primaryStyle');
  const sub = styles.find((s) => s.description === 'subStyle');
  return {
    keystoneId: num(primary?.selections?.[0]?.perk),
    subStyleId: num(sub?.style),
  };
}

/** `totalMinionsKilled + neutralMinionsKilled`, null only when both are absent. */
function extractCs(p: MatchInfoParticipant): number | null {
  const lane = typeof p.totalMinionsKilled === 'number' ? p.totalMinionsKilled : undefined;
  const jungle = typeof p.neutralMinionsKilled === 'number' ? p.neutralMinionsKilled : undefined;
  if (lane === undefined && jungle === undefined) return null;
  return (lane ?? 0) + (jungle ?? 0);
}

interface ParticipantRow {
  puuid: string;
  championId: number | null;
  win: boolean | null;
  teamId: number | null;
  teamPosition: string | null;
  kills: number | null;
  deaths: number | null;
  assists: number | null;
  cs: number | null;
  gold: number | null;
  damage: number | null;
  vision: number | null;
  item0: number | null;
  item1: number | null;
  item2: number | null;
  item3: number | null;
  item4: number | null;
  item5: number | null;
  keystoneId: number | null;
  subStyleId: number | null;
  spell1: number | null;
  spell2: number | null;
  placement: number | null;
  subteamId: number | null;
}

/** Every fact column null — the shape a metadata-only fallback row still needs. */
const EMPTY_FACTS = {
  championId: null,
  win: null,
  teamId: null,
  teamPosition: null,
  kills: null,
  deaths: null,
  assists: null,
  cs: null,
  gold: null,
  damage: null,
  vision: null,
  item0: null,
  item1: null,
  item2: null,
  item3: null,
  item4: null,
  item5: null,
  keystoneId: null,
  subStyleId: null,
  spell1: null,
  spell2: null,
  placement: null,
  subteamId: null,
} as const satisfies Omit<ParticipantRow, 'puuid'>;

function extractParticipants(data: RiotMatch): ParticipantRow[] {
  const infoParticipants = data.info?.participants ?? [];
  if (infoParticipants.length > 0) {
    return infoParticipants
      .filter((p): p is MatchInfoParticipant & { puuid: string } => typeof p.puuid === 'string')
      .map((p) => {
        const { keystoneId, subStyleId } = extractPerks(p.perks);
        return {
          puuid: p.puuid,
          championId: num(p.championId),
          win: typeof p.win === 'boolean' ? p.win : null,
          teamId: num(p.teamId),
          teamPosition: str(p.teamPosition),
          kills: num(p.kills),
          deaths: num(p.deaths),
          assists: num(p.assists),
          cs: extractCs(p),
          gold: num(p.goldEarned),
          damage: num(p.totalDamageDealtToChampions),
          vision: num(p.visionScore),
          item0: num(p.item0),
          item1: num(p.item1),
          item2: num(p.item2),
          item3: num(p.item3),
          item4: num(p.item4),
          item5: num(p.item5),
          keystoneId,
          subStyleId,
          spell1: num(p.summoner1Id),
          spell2: num(p.summoner2Id),
          placement: num(p.placement),
          subteamId: num(p.playerSubteamId),
        };
      });
  }
  // Fall back to metadata when only IDs are present — every fact column stays
  // null, the same shape a pre-C2 archive wrote.
  return (data.metadata?.participants ?? []).map((puuid) => ({ puuid, ...EMPTY_FACTS }));
}

interface BanRow {
  teamId: number;
  pickTurn: number;
  championId: number;
}

/** `info.teams[].bans[]`. `championId: -1` is an empty slot, not a ban. */
function extractBans(data: RiotMatch): BanRow[] {
  const bans: BanRow[] = [];
  for (const team of data.info?.teams ?? []) {
    if (typeof team.teamId !== 'number') continue;
    for (const ban of team.bans ?? []) {
      if (typeof ban.championId !== 'number' || ban.championId === -1) continue;
      if (typeof ban.pickTurn !== 'number') continue;
      bans.push({ teamId: team.teamId, pickTurn: ban.pickTurn, championId: ban.championId });
    }
  }
  return bans;
}

/**
 * Extract and upsert both fact tables for one archived match (#110). Shared by
 * `archiveMatch` (one match, on the way in) and `reextractBatch` (many, read
 * back out of the archive), so the extraction logic has exactly one home.
 *
 * Participants upsert with `onConflictDoUpdate`: `archiveMatch` already
 * refreshes `matches.data` on conflict, and `onConflictDoNothing` would
 * permanently strand a pre-C2 row's null fact columns even when the match is
 * re-archived. Bans upsert with `onConflictDoNothing` — a match's bans never
 * change once played, so there is nothing to reconcile a conflict against.
 */
async function applyMatchFacts(matchId: string, data: RiotMatch): Promise<void> {
  const participants = extractParticipants(data);
  if (participants.length > 0) {
    await db
      .insert(matchParticipants)
      .values(participants.map((p) => ({ matchId, ...p })))
      .onConflictDoUpdate({
        target: [matchParticipants.matchId, matchParticipants.puuid],
        set: {
          championId: raw`excluded.champion_id`,
          win: raw`excluded.win`,
          teamId: raw`excluded.team_id`,
          teamPosition: raw`excluded.team_position`,
          kills: raw`excluded.kills`,
          deaths: raw`excluded.deaths`,
          assists: raw`excluded.assists`,
          cs: raw`excluded.cs`,
          gold: raw`excluded.gold`,
          damage: raw`excluded.damage`,
          vision: raw`excluded.vision`,
          item0: raw`excluded.item0`,
          item1: raw`excluded.item1`,
          item2: raw`excluded.item2`,
          item3: raw`excluded.item3`,
          item4: raw`excluded.item4`,
          item5: raw`excluded.item5`,
          keystoneId: raw`excluded.keystone_id`,
          subStyleId: raw`excluded.sub_style_id`,
          spell1: raw`excluded.spell1`,
          spell2: raw`excluded.spell2`,
          placement: raw`excluded.placement`,
          subteamId: raw`excluded.subteam_id`,
        },
      });
  }

  const bans = extractBans(data);
  if (bans.length > 0) {
    await db
      .insert(matchBans)
      .values(bans.map((b) => ({ matchId, ...b })))
      .onConflictDoNothing();
  }
}

export interface ReextractBatchResult {
  /** Ids processed, in the order this batch walked them. */
  matchIds: string[];
  /** The last id processed — the next call's `after` — or null when nothing was left. */
  cursor: string | null;
}

/**
 * One page of the archive, re-extracted (#110) — what backfills the widened
 * fact columns and `match_bans` for rows written before C2 existed.
 *
 * Walked in `match_id` order rather than `fetched_at`: a plain string
 * comparison needs no index beyond the primary key, and a match archived
 * mid-run either sorts before the cursor (already passed this run) or after
 * it (still ahead) — never both, so nothing is silently skipped.
 */
export async function reextractBatch(
  after: string | null,
  limit: number,
): Promise<ReextractBatchResult> {
  const base = db.select({ matchId: matches.matchId, data: matches.data }).from(matches);
  const rows = after
    ? await base.where(gt(matches.matchId, after)).orderBy(matches.matchId).limit(limit)
    : await base.orderBy(matches.matchId).limit(limit);
  if (rows.length === 0) return { matchIds: [], cursor: null };

  for (const row of rows) {
    await applyMatchFacts(row.matchId, row.data as RiotMatch);
  }

  return { matchIds: rows.map((r) => r.matchId), cursor: rows[rows.length - 1]!.matchId };
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

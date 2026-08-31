import { and, desc, eq, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import type { Division, RankedQueue, Tier } from '../riot/ladder.js';
import { db } from './index.js';
import { ladderCrawls, leagueEntries, type LadderCrawl, type LeagueEntry } from './schema.js';

/**
 * The ladder's storage (§4 of docs/ladder-crawl-plan.md). Two shapes: a run
 * log (`ladder_crawls`) and the ladder itself (`league_entries`), both scoped
 * by KEY_SCOPE like `players` — a crawl enumerates PUUIDs, and a rotated key
 * must not inherit them (§7.4).
 */

/** A crawl is running, or it ended one of three ways. */
export const CRAWL_STATUSES = ['running', 'completed', 'failed', 'cancelled'] as const;
export type CrawlStatus = (typeof CRAWL_STATUSES)[number];
export type TerminalCrawlStatus = Exclude<CrawlStatus, 'running'>;

export interface CreateCrawlInput {
  platform: string;
  queue: RankedQueue;
  tierFloor: Tier;
}

/**
 * Start a crawl, or report the one already running.
 *
 * One live crawl per (key_scope, platform, queue) is enforced by a partial
 * unique index, not by a check up here: two workers reading "none running" in
 * the same instant would both pass a check, and the loser of that race would
 * spend a second full ladder's worth of quota. `created: false` is that loser
 * being told, correctly, whose crawl to watch instead.
 */
export async function createCrawl(
  input: CreateCrawlInput,
): Promise<{ crawl: LadderCrawl; created: boolean }> {
  const [row] = await db
    .insert(ladderCrawls)
    .values({
      keyScope: KEY_SCOPE,
      platform: input.platform,
      queue: input.queue,
      tierFloor: input.tierFloor,
    })
    .onConflictDoNothing()
    .returning();

  if (row) return { crawl: row, created: true };

  const running = await getRunningCrawl(input.platform, input.queue);
  // The index refused the insert, so a running crawl existed a moment ago. If
  // it is gone now it finished in between, and the caller should simply retry
  // rather than be handed a crawl that is no longer running.
  if (!running) throw new Error('ladder crawl conflicted with a run that has since finished');
  return { crawl: running, created: false };
}

export async function getCrawl(id: string): Promise<LadderCrawl | undefined> {
  const rows = await db
    .select()
    .from(ladderCrawls)
    .where(and(eq(ladderCrawls.keyScope, KEY_SCOPE), eq(ladderCrawls.id, id)))
    .limit(1);
  return rows[0];
}

export async function getRunningCrawl(
  platform: string,
  queue: string,
): Promise<LadderCrawl | undefined> {
  const rows = await db
    .select()
    .from(ladderCrawls)
    .where(
      and(
        eq(ladderCrawls.keyScope, KEY_SCOPE),
        eq(ladderCrawls.platform, platform),
        eq(ladderCrawls.queue, queue),
        eq(ladderCrawls.status, 'running'),
      ),
    )
    .limit(1);
  return rows[0];
}

/** The crawl whose stamp means "this is the ladder as it stands". */
export async function getLatestCompletedCrawl(
  platform: string,
  queue: string,
): Promise<LadderCrawl | undefined> {
  const rows = await db
    .select()
    .from(ladderCrawls)
    .where(
      and(
        eq(ladderCrawls.keyScope, KEY_SCOPE),
        eq(ladderCrawls.platform, platform),
        eq(ladderCrawls.queue, queue),
        eq(ladderCrawls.status, 'completed'),
      ),
    )
    .orderBy(desc(ladderCrawls.finishedAt))
    .limit(1);
  return rows[0];
}

export interface ListCrawlsFilter {
  platform?: string;
  queue?: string;
  limit?: number;
}

/** Recent runs, newest first — the admin listing and the dashboard block. */
export async function listCrawls(filter: ListCrawlsFilter = {}): Promise<LadderCrawl[]> {
  const where = [eq(ladderCrawls.keyScope, KEY_SCOPE)];
  if (filter.platform) where.push(eq(ladderCrawls.platform, filter.platform));
  if (filter.queue) where.push(eq(ladderCrawls.queue, filter.queue));

  return db
    .select()
    .from(ladderCrawls)
    .where(and(...where))
    .orderBy(desc(ladderCrawls.startedAt))
    .limit(filter.limit ?? 20);
}

/**
 * End a crawl. Guarded on `status = 'running'` so a cancel already recorded is
 * not overwritten by the last walk job finishing a moment later and calling
 * itself complete — the returned row is undefined in that case, which is how
 * the caller learns it lost the race.
 */
export async function finishCrawl(
  id: string,
  status: TerminalCrawlStatus,
): Promise<LadderCrawl | undefined> {
  const rows = await db
    .update(ladderCrawls)
    .set({ status, finishedAt: new Date() })
    .where(
      and(
        eq(ladderCrawls.keyScope, KEY_SCOPE),
        eq(ladderCrawls.id, id),
        eq(ladderCrawls.status, 'running'),
      ),
    )
    .returning();
  return rows[0];
}

export interface CrawlCounters {
  pagesFetched?: number;
  entriesSeen?: number;
  playersDiscovered?: number;
  backfillsEnqueued?: number;
}

/**
 * Add to a crawl's counters. Increments in SQL rather than read-modify-write:
 * every walk job for a crawl bumps the same row concurrently, and a read of
 * the old value is stale the moment it lands.
 */
export async function bumpCrawlCounters(id: string, by: CrawlCounters): Promise<void> {
  const set: Record<string, unknown> = {};
  if (by.pagesFetched) set['pagesFetched'] = raw`${ladderCrawls.pagesFetched} + ${by.pagesFetched}`;
  if (by.entriesSeen) set['entriesSeen'] = raw`${ladderCrawls.entriesSeen} + ${by.entriesSeen}`;
  if (by.playersDiscovered)
    set['playersDiscovered'] = raw`${ladderCrawls.playersDiscovered} + ${by.playersDiscovered}`;
  if (by.backfillsEnqueued)
    set['backfillsEnqueued'] = raw`${ladderCrawls.backfillsEnqueued} + ${by.backfillsEnqueued}`;

  if (Object.keys(set).length === 0) return;
  await db
    .update(ladderCrawls)
    .set(set)
    .where(and(eq(ladderCrawls.keyScope, KEY_SCOPE), eq(ladderCrawls.id, id)));
}

/** One player's standing on one ladder, as a crawl saw it. */
export interface LeagueEntryInput {
  puuid: string;
  tier: Tier;
  division: Division;
  leaguePoints: number;
  wins: number;
  losses: number;
  veteran?: boolean;
  inactive?: boolean;
  freshBlood?: boolean;
  hotStreak?: boolean;
}

/**
 * Riot serves ~205 entries per page and the archive helpers batch at 100, so
 * one page is two statements. Postgres' bind-parameter ceiling is the real
 * constraint: these rows are 16 columns wide, and a whole page in one insert
 * would be over 3 000 parameters.
 */
const UPSERT_BATCH = 100;

/**
 * Write what a crawl saw. `first_seen_crawl_id` is set once and never
 * overwritten — that is the column that answers "when did this player first
 * appear on this ladder" — while `last_seen_crawl_id` is restamped every time,
 * which is what makes the current ladder and the players who fell off it two
 * halves of the same index.
 *
 * Returns the number of rows written, which for a re-crawl is the same as the
 * number seen: an unchanged player still gets their stamp moved forward.
 */
export async function upsertLeagueEntries(
  crawlId: string,
  platform: string,
  queue: RankedQueue,
  entries: LeagueEntryInput[],
): Promise<number> {
  let written = 0;

  for (let i = 0; i < entries.length; i += UPSERT_BATCH) {
    const batch = entries.slice(i, i + UPSERT_BATCH);
    const rows = await db
      .insert(leagueEntries)
      .values(
        batch.map((entry) => ({
          keyScope: KEY_SCOPE,
          platform,
          queue,
          puuid: entry.puuid,
          tier: entry.tier,
          division: entry.division,
          leaguePoints: entry.leaguePoints,
          wins: entry.wins,
          losses: entry.losses,
          veteran: entry.veteran ?? false,
          inactive: entry.inactive ?? false,
          freshBlood: entry.freshBlood ?? false,
          hotStreak: entry.hotStreak ?? false,
          firstSeenCrawlId: crawlId,
          lastSeenCrawlId: crawlId,
          updatedAt: new Date(),
        })),
      )
      .onConflictDoUpdate({
        target: [
          leagueEntries.keyScope,
          leagueEntries.platform,
          leagueEntries.queue,
          leagueEntries.puuid,
        ],
        set: {
          tier: raw`excluded.tier`,
          division: raw`excluded.division`,
          leaguePoints: raw`excluded.league_points`,
          wins: raw`excluded.wins`,
          losses: raw`excluded.losses`,
          veteran: raw`excluded.veteran`,
          inactive: raw`excluded.inactive`,
          freshBlood: raw`excluded.fresh_blood`,
          hotStreak: raw`excluded.hot_streak`,
          lastSeenCrawlId: raw`excluded.last_seen_crawl_id`,
          updatedAt: raw`excluded.updated_at`,
        },
      })
      .returning({ puuid: leagueEntries.puuid });
    written += rows.length;
  }

  return written;
}

export interface LadderFilter {
  platform: string;
  queue: string;
  tier?: Tier;
  division?: Division;
  /** Only rows stamped by this crawl — the ladder as that run saw it. */
  crawlId?: string;
  limit?: number;
  offset?: number;
}

/** A slice of the ladder, best first. */
export async function listLeagueEntries(filter: LadderFilter): Promise<LeagueEntry[]> {
  return db
    .select()
    .from(leagueEntries)
    .where(and(...ladderWhere(filter)))
    .orderBy(desc(leagueEntries.leaguePoints))
    .limit(filter.limit ?? 100)
    .offset(filter.offset ?? 0);
}

export async function getLeagueEntry(
  platform: string,
  queue: string,
  puuid: string,
): Promise<LeagueEntry | undefined> {
  const rows = await db
    .select()
    .from(leagueEntries)
    .where(
      and(
        eq(leagueEntries.keyScope, KEY_SCOPE),
        eq(leagueEntries.platform, platform),
        eq(leagueEntries.queue, queue),
        eq(leagueEntries.puuid, puuid),
      ),
    )
    .limit(1);
  return rows[0];
}

export async function countLeagueEntries(filter: LadderFilter): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(leagueEntries)
    .where(and(...ladderWhere(filter)));
  return rows[0]?.n ?? 0;
}

function ladderWhere(filter: LadderFilter) {
  const where = [
    eq(leagueEntries.keyScope, KEY_SCOPE),
    eq(leagueEntries.platform, filter.platform),
    eq(leagueEntries.queue, filter.queue),
  ];
  if (filter.tier) where.push(eq(leagueEntries.tier, filter.tier));
  if (filter.division) where.push(eq(leagueEntries.division, filter.division));
  if (filter.crawlId) where.push(eq(leagueEntries.lastSeenCrawlId, filter.crawlId));
  return where;
}

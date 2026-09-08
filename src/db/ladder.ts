import { and, desc, eq, isNotNull, isNull, lt, or, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import type { Division, RankedQueue, Tier } from '../riot/ladder.js';
import { db } from './index.js';
import {
  ladderCrawls,
  leagueEntries,
  players,
  type LadderCrawl,
  type LeagueEntry,
} from './schema.js';

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

/**
 * The three stages a running crawl passes through, in order.
 *
 * Stages rather than one pipeline because a match belongs to ten players. A
 * crawl that walked each player's history as it discovered them would reach
 * one game from ten different walks at ten different moments, and every walk
 * that ran before the match landed would pay for it again. Collecting all the
 * ids first turns that into one set, and `filterUnarchived` over the set means
 * a match costs one `match.byId` no matter how many of its participants the
 * ladder holds.
 */
export const CRAWL_PHASES = ['enumerate', 'collect', 'archive'] as const;
export type CrawlPhase = (typeof CRAWL_PHASES)[number];

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

/**
 * Crawls in flight anywhere in this key scope — the snapshot's view, which is
 * not per (platform, queue) like `getRunningCrawl`. Usually empty, and bounded
 * by the live-crawl index at one row per ladder.
 */
export async function listRunningCrawls(): Promise<LadderCrawl[]> {
  return db
    .select()
    .from(ladderCrawls)
    .where(and(eq(ladderCrawls.keyScope, KEY_SCOPE), eq(ladderCrawls.status, 'running')))
    .orderBy(desc(ladderCrawls.startedAt));
}

/** The most recent run that ended, however it ended. */
export async function lastFinishedCrawl(): Promise<LadderCrawl | undefined> {
  const rows = await db
    .select()
    .from(ladderCrawls)
    .where(and(eq(ladderCrawls.keyScope, KEY_SCOPE), isNotNull(ladderCrawls.finishedAt)))
    .orderBy(desc(ladderCrawls.finishedAt))
    .limit(1);
  return rows[0];
}

/** How much ladder this key scope holds, for the snapshot's totals. */
export async function countAllLeagueEntries(): Promise<number> {
  const rows = await db
    .select({ n: raw<number>`count(*)::int` })
    .from(leagueEntries)
    .where(eq(leagueEntries.keyScope, KEY_SCOPE));
  return rows[0]?.n ?? 0;
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

/**
 * Move a running crawl on to its next stage, and report whether this caller is
 * the one that moved it.
 *
 * Guarded on the stage it is leaving for the same reason `finishCrawl` is
 * guarded on `running`: the stage change is triggered by whichever leg happens
 * to finish last, and two legs ending in the same instant can both read an
 * empty outstanding set. The database picks one. The loser gets `undefined`
 * and does nothing, rather than fanning the next stage out a second time.
 */
export async function advanceCrawlPhase(
  id: string,
  from: CrawlPhase,
  to: CrawlPhase,
): Promise<LadderCrawl | undefined> {
  const rows = await db
    .update(ladderCrawls)
    .set({ phase: to })
    .where(
      and(
        eq(ladderCrawls.keyScope, KEY_SCOPE),
        eq(ladderCrawls.id, id),
        eq(ladderCrawls.status, 'running'),
        eq(ladderCrawls.phase, from),
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
  matchIdsSeen?: number;
  matchesQueued?: number;
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
  if (by.matchIdsSeen) set['matchIdsSeen'] = raw`${ladderCrawls.matchIdsSeen} + ${by.matchIdsSeen}`;
  if (by.matchesQueued)
    set['matchesQueued'] = raw`${ladderCrawls.matchesQueued} + ${by.matchesQueued}`;

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

export interface CrawlCandidateFilter {
  /** Only entries this run stamped — the ladder as this crawl saw it. */
  crawlId: string;
  platform: string;
  queue: string;
  /**
   * Skip players whose walk started at or after this instant — the crawl's own
   * start. A repeat crawl should converge on what is new rather than re-walk a
   * ladder somebody else has just walked.
   */
  notWalkedSince: Date;
  limit: number;
  /** Where the previous page ended. Omitted for the first page. */
  after?: CrawlCandidate;
}

/** One candidate, and the cursor that continues after it. */
export interface CrawlCandidate {
  puuid: string;
  leaguePoints: number;
}

/**
 * Whose match history this crawl should collect, a page at a time.
 *
 * Read back out of `league_entries` rather than carried in Redis from the
 * enumeration that found them: the entries are already stamped with the crawl
 * id, which makes "everyone this run saw" an indexed predicate on a durable
 * table. A crawl that survives a Redis flush then still knows who is in it —
 * only the page cursors are cheap enough to lose.
 *
 * **Keyset, not OFFSET**, and that is not a micro-optimisation: the jobs this
 * paging feeds start running while it is still paging, and the first thing
 * each does is stamp `history_backfill_started_at`, which takes that player
 * out of the `notWalkedSince` predicate. Under OFFSET every stamped player
 * shifts the remaining rows left, so `offset += page` would skip exactly as
 * many players as the workers had got through — silently, and worst on the
 * fastest deployments. A cursor on the sort key cannot shift.
 *
 * Ordered by LP so that, if the crawl is cancelled halfway, what it did
 * collect is the top of the ladder. The order is only meaningful within the
 * apex tiers, where LP is one continuous pool; below Master every division
 * restarts at zero, so a Diamond I on 99 LP sorts above a Master on 50. That
 * costs nothing — the stage is all-or-nothing by design — and beats an
 * arbitrary order for the case where somebody stops it early.
 */
export async function listCrawlBackfillCandidates(
  filter: CrawlCandidateFilter,
): Promise<CrawlCandidate[]> {
  const where = [
    eq(leagueEntries.keyScope, KEY_SCOPE),
    eq(leagueEntries.platform, filter.platform),
    eq(leagueEntries.queue, filter.queue),
    eq(leagueEntries.lastSeenCrawlId, filter.crawlId),
    or(
      isNull(players.historyBackfillStartedAt),
      lt(players.historyBackfillStartedAt, filter.notWalkedSince),
    ),
  ];

  // Both columns descend, so "after this row" is one comparison per column
  // rather than the mixed-direction row constructor a `puuid ASC` tie-break
  // would need. The direction of `puuid` is arbitrary either way — it is only
  // there because LP is not unique across a ladder, and a sort key that ties
  // has no "after".
  const cursor = filter.after;
  if (cursor) {
    where.push(
      or(
        lt(leagueEntries.leaguePoints, cursor.leaguePoints),
        and(
          eq(leagueEntries.leaguePoints, cursor.leaguePoints),
          lt(leagueEntries.puuid, cursor.puuid),
        ),
      ),
    );
  }

  const rows = await db
    .select({ puuid: leagueEntries.puuid, leaguePoints: leagueEntries.leaguePoints })
    .from(leagueEntries)
    // Left join: a player discovered by this very crawl has a `players` row
    // with no backfill stamp at all, and an inner join would drop exactly the
    // players the crawl exists to find.
    .leftJoin(
      players,
      and(eq(players.keyScope, leagueEntries.keyScope), eq(players.puuid, leagueEntries.puuid)),
    )
    .where(and(...where))
    .orderBy(desc(leagueEntries.leaguePoints), desc(leagueEntries.puuid))
    .limit(filter.limit);

  return rows;
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

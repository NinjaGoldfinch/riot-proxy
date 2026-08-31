import type { Job, Queue } from 'bullmq';
import { KEY_SCOPE } from '../config.js';
import { config } from '../config.js';
import {
  bumpCrawlCounters,
  createCrawl,
  finishCrawl,
  getCrawl,
  upsertLeagueEntries,
  type LeagueEntryInput,
} from '../db/ladder.js';
import { archiveMatch, archiveTimeline, filterUnarchived, type RiotMatch } from '../db/matches.js';
import {
  getPlayer,
  listTrackedPlayers,
  markBackfillComplete,
  markBackfillStarted,
  setLastSeenMatch,
} from '../db/players.js';
import { PATCH_TOPIC, playerTopic, publish } from '../events/index.js';
import { fetcher } from '../fetcher.js';
import { ProxyError } from '../errors.js';
import { logger } from '../logger.js';
import { jobsTotal } from '../metrics.js';
import { redis } from '../redis.js';
import { build } from '../riot/endpoints.js';
import {
  DIVISIONS,
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
  isApexTier,
  tiersAtOrAbove,
  type ApexTier,
  type PagedTier,
} from '../riot/ladder.js';
import { assertPlatform, platformToRegion, regionFromMatchId } from '../riot/routing.js';
import { clearCrawlState, getCursor, releaseLeg, setCursor, trackLegs } from './ladder-state.js';
import { syncDdragon } from '../static/ddragon.js';
import {
  ARCHIVE_PRIORITY,
  JOB,
  LADDER_PRIORITY,
  archiveQueue,
  backfillPriority,
  enqueueBackfill,
  jobKey,
  ladderLegId,
  ladderQueue,
  pollDedupeId,
  pollQueue,
  type ArchiveMatchJob,
  type BackfillPlayerJob,
  type LadderApexJob,
  type LadderCrawlJob,
  type LadderWalkJob,
  type PollPlayerJob,
} from './queues.js';

/**
 * §10 — every job is idempotent and every upstream call it makes runs at `bulk`
 * priority so backfills cannot starve interactive traffic (§9.3).
 */
const BULK = { priority: 'bulk' as const };

/** Previous-state keys for transition detection (Phase 6.2). */
const stateKey = {
  live: (puuid: string) => `state:live:${KEY_SCOPE}:${puuid}`,
  rank: (puuid: string) => `state:rank:${KEY_SCOPE}:${puuid}`,
};

const STATE_TTL = 7 * 24 * 3600;

// ── fan-out ticks ────────────────────────────────────────────────────────────

/**
 * One repeatable tick per poll type fans out to one job per tracked player.
 * Cheaper than maintaining N repeatable jobs, and tracked-player changes take
 * effect on the next tick with no scheduler churn.
 *
 * The returned count is how many tracked players the tick fanned out for, not
 * how many jobs it created: `pollDedupeId` drops the ones already pending.
 *
 * `queue` is injectable for the same reason `enqueueBackfill`'s is — the real
 * poll queue is drained by whatever worker happens to be running, which would
 * make the de-duplication assertions depend on the machine.
 */
export async function fanOut(jobName: string, queue: Queue = pollQueue): Promise<number> {
  const players = await listTrackedPlayers();
  if (players.length === 0) return 0;

  await queue.addBulk(
    players.map((p) => ({
      name: jobName,
      data: { puuid: p.puuid, platform: p.platform } satisfies PollPlayerJob,
      opts: {
        deduplication: { id: pollDedupeId(jobName, p.puuid) },
        removeOnComplete: { age: 600, count: 200 },
      },
    })),
  );
  return players.length;
}

// ── poll:live ────────────────────────────────────────────────────────────────

interface ActiveGame {
  gameId?: number;
  gameQueueConfigId?: number;
  participants?: { puuid?: string; championId?: number }[];
}

export async function pollLive(job: Job<PollPlayerJob>): Promise<void> {
  const { puuid, platform } = job.data;
  const key = stateKey.live(puuid);
  const previous = await redis.get(key);

  let game: ActiveGame | undefined;
  try {
    const result = await fetcher.fetch<ActiveGame>(
      build.activeGame(assertPlatform(platform), puuid),
      BULK,
    );
    game = result.data;
  } catch (err) {
    // 404 is the normal "not in game" answer, negative-cached upstream (§8.3).
    if (!(err instanceof ProxyError && err.code === 'NOT_FOUND')) throw err;
  }

  const currentGameId = game?.gameId ? String(game.gameId) : undefined;

  if (currentGameId && currentGameId !== previous) {
    const self = game?.participants?.find((p) => p.puuid === puuid);
    await redis.set(key, currentGameId, 'EX', STATE_TTL);
    await publish('game.started', playerTopic(puuid), {
      puuid,
      platform,
      gameId: Number(currentGameId),
      ...(self?.championId !== undefined ? { championId: self.championId } : {}),
      ...(game?.gameQueueConfigId !== undefined ? { queueId: game.gameQueueConfigId } : {}),
    });
    return;
  }

  if (!currentGameId && previous) {
    await redis.del(key);
    await publish('game.ended', playerTopic(puuid), { puuid, gameId: Number(previous) });
    // The match becomes available shortly after the game ends; nudge the match
    // poller rather than waiting for its next tick. Shares the fan-out's
    // de-duplication id: a nudge and a tick are the same work, and whichever is
    // already pending covers both games if two end inside the delay.
    await pollQueue.add(JOB.pollMatches, { puuid, platform } satisfies PollPlayerJob, {
      delay: 60_000,
      deduplication: { id: pollDedupeId(JOB.pollMatches, puuid) },
    });
  }
}

// ── poll:rank ────────────────────────────────────────────────────────────────

interface LeagueEntry {
  queueType?: string;
  tier?: string;
  rank?: string;
  leaguePoints?: number;
}

type RankSnapshot = Record<string, { tier?: string; rank?: string; lp?: number }>;

export async function pollRank(job: Job<PollPlayerJob>): Promise<void> {
  const { puuid, platform } = job.data;
  const key = stateKey.rank(puuid);

  const { data: entries } = await fetcher.fetch<LeagueEntry[]>(
    build.leagueEntriesByPuuid(assertPlatform(platform), puuid),
    BULK,
  );

  const snapshot: RankSnapshot = {};
  for (const entry of entries ?? []) {
    if (!entry.queueType) continue;
    snapshot[entry.queueType] = {
      ...(entry.tier !== undefined ? { tier: entry.tier } : {}),
      ...(entry.rank !== undefined ? { rank: entry.rank } : {}),
      ...(entry.leaguePoints !== undefined ? { lp: entry.leaguePoints } : {}),
    };
  }

  const previousRaw = await redis.get(key);
  await redis.set(key, JSON.stringify(snapshot), 'EX', STATE_TTL);

  if (!previousRaw) return; // First observation is a baseline, not a change.

  let previous: RankSnapshot;
  try {
    previous = JSON.parse(previousRaw) as RankSnapshot;
  } catch {
    return;
  }

  for (const [queue, after] of Object.entries(snapshot)) {
    const before = previous[queue] ?? null;
    if (JSON.stringify(before) === JSON.stringify(after)) continue;
    await publish('rank.changed', playerTopic(puuid), { puuid, queue, before, after });
  }
}

// ── poll:matches ─────────────────────────────────────────────────────────────

/** The steady-state window: what one tick reads when nothing has gone wrong. */
const POLL_PAGE = 5;
/** Catch-up pages are only read after falling behind, so they are read wide. */
const CATCHUP_PAGE = 100;

/**
 * Page back to where the last tick got to, rather than reading a fixed window
 * off the top (#46).
 *
 * `last_seen_match_id` has always been written here and never read. A fixed
 * window is fine while the ticks keep coming — nobody finishes five games in
 * five minutes — but when they stop, for a redeploy or a stalled queue, every
 * match that fell past the window was lost permanently. Nothing else walks a
 * tracked player, so nothing repaired it.
 *
 * The cursor costs nothing in steady state: it is on the first page, so this is
 * the same single call it always was. Depth is only spent in proportion to how
 * long the ticks were away.
 */
export async function pollMatches(job: Job<PollPlayerJob>): Promise<{ queued: number }> {
  const { puuid, platform } = job.data;
  const region = platformToRegion(assertPlatform(platform));
  const cap = config.TRACK_CATCHUP_LIMIT;

  const player = await getPlayer(puuid);
  const cursor = player?.lastSeenMatchId ?? null;

  const collected: string[] = [];
  let start = 0;
  let caughtUp = false;

  for (;;) {
    const count = start === 0 ? POLL_PAGE : CATCHUP_PAGE;
    const { data: ids } = await fetcher.fetch<string[]>(
      build.matchIdsByPuuid(region, puuid, { start, count }),
      BULK,
    );
    if (!ids || ids.length === 0) {
      caughtUp = true;
      break;
    }

    // Everything above the cursor is new; the cursor itself we already have.
    const cut = cursor === null ? -1 : ids.indexOf(cursor);
    if (cut >= 0) {
      collected.push(...ids.slice(0, cut));
      caughtUp = true;
      break;
    }
    collected.push(...ids);

    // Nothing to catch up to (this player has never been polled), catching up
    // turned off, or the end of their history. All three end the walk here.
    if (cursor === null || cap <= 0 || ids.length < count) {
      caughtUp = true;
      break;
    }

    start += ids.length;
    if (start >= cap) break;
  }

  if (!caughtUp) {
    // Further behind than a tick should chase inline. The backfill exists for
    // depth, deduplicates per match, and runs at a priority that will not
    // crowd out live games, so hand it over rather than leaving the tail.
    logger.warn({ puuid, cap }, 'match poll fell behind its catch-up limit; queuing a walk');
    await enqueueBackfill({
      puuid,
      platform,
      limit: config.LOOKUP_BACKFILL_LIMIT,
      reason: 'catchup',
    });
  }

  if (collected.length === 0) return { queued: 0 };

  const unarchived = await filterUnarchived(collected);
  if (unarchived.length === 0) return { queued: 0 };

  const depthOf = new Map(collected.map((id, index) => [id, index]));

  await archiveQueue.addBulk(
    unarchived.map((matchId) => {
      const depth = depthOf.get(matchId) ?? 0;
      return {
        name: JOB.archiveMatch,
        data: { matchId, puuid, fetchTimeline: config.ARCHIVE_TIMELINES } satisfies ArchiveMatchJob,
        // Idempotency: the same match never queues twice. A game that has just
        // finished is the most valuable thing in the queue, so it takes the top
        // priority rather than the implicit one (see ARCHIVE_PRIORITY). A
        // catch-up tail is not fresh in that sense, though, and a long one
        // would swamp the live band — so past the first page it is ranked by
        // depth like any other walk (§9.3, #31).
        opts: {
          jobId: jobKey('archive', matchId),
          priority: depth < POLL_PAGE ? ARCHIVE_PRIORITY.live : backfillPriority(depth),
        },
      };
    }),
  );

  const newest = collected[0];
  if (newest) await setLastSeenMatch(puuid, newest);

  return { queued: unarchived.length };
}

// ── archive:match ────────────────────────────────────────────────────────────

export async function archiveMatchJob(job: Job<ArchiveMatchJob>): Promise<void> {
  const { matchId, puuid, fetchTimeline } = job.data;
  const region = regionFromMatchId(matchId);
  if (!region) throw new Error(`Cannot derive region from match id '${matchId}'`);

  const { data } = await fetcher.fetch<RiotMatch>(build.matchById(region, matchId), BULK);
  await archiveMatch(matchId, region, data);

  if (fetchTimeline) {
    try {
      const timeline = await fetcher.fetch<unknown>(build.matchTimeline(region, matchId), BULK);
      await archiveTimeline(matchId, region, timeline.data);
    } catch (err) {
      // Timelines are large and optional — never fail the archive over one.
      logger.warn({ err, matchId }, 'timeline archive failed');
    }
  }

  await publish('match.archived', playerTopic(puuid ?? ''), {
    ...(puuid ? { puuid } : {}),
    matchId,
  });
}

// ── backfill:player ──────────────────────────────────────────────────────────

const BACKFILL_PAGE = 100;

export async function backfillPlayer(
  job: Job<BackfillPlayerJob>,
): Promise<{ queued: number; depth: number }> {
  const { puuid, platform, limit = 500, fetchTimeline, reason } = job.data;
  const region = platformToRegion(assertPlatform(platform));

  // #44 — claim the walk before doing any of it. If this job dies the stamp is
  // left without a completion, which reads as "tried, did not finish" and lets
  // the next lookup queue it again.
  await markBackfillStarted(puuid, platform);

  let start = 0;
  let queued = 0;
  let depth = 0;

  while (start < limit) {
    const count = Math.min(BACKFILL_PAGE, limit - start);
    const { data: ids } = await fetcher.fetch<string[]>(
      build.matchIdsByPuuid(region, puuid, { start, count }),
      BULK,
    );
    if (!ids || ids.length === 0) break;

    // Rank by position in the *history*, not position in this filtered list:
    // skipping ten already-archived matches must not promote the eleventh.
    const depthOf = new Map(ids.map((id, index) => [id, start + index]));
    const unarchived = await filterUnarchived(ids);

    if (unarchived.length > 0) {
      await archiveQueue.addBulk(
        unarchived.map((matchId) => ({
          name: JOB.archiveMatch,
          data: {
            matchId,
            puuid,
            fetchTimeline: fetchTimeline ?? config.ARCHIVE_TIMELINES,
          } satisfies ArchiveMatchJob,
          opts: {
            jobId: jobKey('archive', matchId),
            priority: backfillPriority(depthOf.get(matchId) ?? start),
          },
        })),
      );
      queued += unarchived.length;
    }

    depth = start + ids.length;
    await job.updateProgress(Math.min(100, Math.round((depth / limit) * 100)));
    if (ids.length < count) break; // Reached the end of this player's history.
    start += ids.length;
  }

  // Only now, on the way out: this is the record that stops the next lookup
  // queueing the same walk again, so a partial walk must never write it.
  await markBackfillComplete(puuid, depth);

  logger.info({ puuid, queued, depth, reason: reason ?? 'admin' }, 'backfill complete');
  return { queued, depth };
}

// ── ladder:crawl / :apex / :walk ─────────────────────────────────────────────

/**
 * A ladder page can afford to wait a long time for a token. The default bulk
 * budget is two minutes, which is tuned for a backfill that has somewhere else
 * to be; a crawl has nowhere else to be, and failing the job means re-entering
 * the queue behind everything else to fetch the very same page. Five minutes
 * of waiting is cheaper than that, and a freeze longer than five minutes
 * should surface as a retry rather than as a job that appears to hang.
 */
const LADDER_FETCH = { priority: 'bulk' as const, waitBudgetMs: 300_000 };

/** How many pages a walk covers before re-reading the crawl's status. */
const CANCEL_CHECK_PAGES = 10;

/** league-v4's two response shapes, narrowed to what the ladder stores. */
interface RiotLeagueEntry {
  puuid?: string;
  tier?: string;
  rank?: string;
  leaguePoints?: number;
  wins?: number;
  losses?: number;
  veteran?: boolean;
  inactive?: boolean;
  freshBlood?: boolean;
  hotStreak?: boolean;
}

interface RiotLeagueList {
  tier?: string;
  queue?: string;
  entries?: RiotLeagueEntry[];
}

/**
 * Riot's entry, as a row. The tier is passed in rather than read off the
 * entry: the apex endpoints put it on the wrapper and leave it off every
 * entry, and trusting the entry there would write 5 000 rows with no tier.
 */
function toEntry(raw: RiotLeagueEntry, tier: string): LeagueEntryInput | undefined {
  if (!raw.puuid) return undefined;
  return {
    puuid: raw.puuid,
    tier: assertTier(tier),
    // Apex entries report `I` for everyone; paged entries carry the real one.
    division: assertDivision(raw.rank ?? 'I'),
    leaguePoints: raw.leaguePoints ?? 0,
    wins: raw.wins ?? 0,
    losses: raw.losses ?? 0,
    veteran: raw.veteran ?? false,
    inactive: raw.inactive ?? false,
    freshBlood: raw.freshBlood ?? false,
    hotStreak: raw.hotStreak ?? false,
  };
}

export interface StartCrawlInput {
  platform: string;
  queue: string;
  tierFloor?: string;
}

export interface StartCrawlResult {
  crawlId: string;
  created: boolean;
  legs: number;
}

/**
 * Create the crawl and fan out one job per leg — one per apex league, one per
 * (tier, division) below them.
 *
 * One job per (tier, division) rather than one per page is the whole reason
 * this scales: a full ladder is 28 walk jobs, not 20 000 page jobs, and the
 * Redis cursor is what lets a walk that died on page 400 resume there.
 *
 * Shared by the admin route and the repeatable, because both want the same
 * answer and the fan-out is cheap — it makes no upstream calls, so doing it
 * inside the request is what lets the route hand back a crawl id rather than a
 * job id nobody can look anything up with.
 */
export async function startCrawl(input: StartCrawlInput): Promise<StartCrawlResult> {
  const platform = assertPlatform(input.platform);
  const queue = assertRankedQueue(input.queue);
  const tierFloor = assertTier(input.tierFloor ?? config.ladderTierFloor);

  const { crawl, created } = await createCrawl({ platform, queue, tierFloor });
  if (!created) {
    logger.info(
      { crawlId: crawl.id, platform, queue },
      'ladder crawl already running; returning the live one',
    );
    return { crawlId: crawl.id, created: false, legs: 0 };
  }

  const tiers = tiersAtOrAbove(tierFloor);
  const apexTiers = tiers.filter((t): t is ApexTier => isApexTier(t));
  const pagedTiers = tiers.filter((t): t is PagedTier => !isApexTier(t));

  const legs = [
    ...apexTiers.map((tier) => ({
      name: JOB.ladderApex,
      legId: ladderLegId(JOB.ladderApex, crawl.id, tier),
      data: { crawlId: crawl.id, platform, queue, tier } satisfies LadderApexJob,
      priority: LADDER_PRIORITY.apex,
    })),
    ...pagedTiers.flatMap((tier) =>
      DIVISIONS.map((division) => ({
        name: JOB.ladderWalk,
        legId: ladderLegId(JOB.ladderWalk, crawl.id, tier, division),
        data: { crawlId: crawl.id, platform, queue, tier, division } satisfies LadderWalkJob,
        priority: LADDER_PRIORITY.walk,
      })),
    ),
  ];

  // Before the jobs, not after: a leg that runs and releases itself before the
  // set knows about it would leave the crawl running with nothing outstanding.
  await trackLegs(
    crawl.id,
    legs.map((l) => l.legId),
  );

  await ladderQueue.addBulk(
    legs.map((leg) => ({
      name: leg.name,
      data: leg.data,
      opts: {
        priority: leg.priority,
        deduplication: { id: leg.legId },
        removeOnComplete: { age: 3600, count: 200 },
      },
    })),
  );

  logger.info(
    { crawlId: crawl.id, platform, queue, tierFloor, legs: legs.length },
    'ladder crawl started',
  );
  return { crawlId: crawl.id, created: true, legs: legs.length };
}

export async function ladderCrawl(job: Job<LadderCrawlJob>): Promise<StartCrawlResult> {
  return startCrawl(job.data);
}

/**
 * Whether BullMQ will hand this job back. `attemptsMade` counts the attempts
 * *before* this one, and a job is retried while `attemptsMade + 1 < attempts`
 * — so this is the last chance to release the leg, and not releasing it would
 * leave the crawl running forever with nothing left to finish it. A crawl
 * stuck `running` is worse than a failed one: the live-crawl index means it
 * also blocks every future crawl of that ladder.
 */
function isFinalAttempt(job: Job): boolean {
  return job.attemptsMade + 1 >= (job.opts.attempts ?? 1);
}

/**
 * End of a leg. Only the caller that removed the last one finishes the crawl,
 * and it is the same call that cleans the cursors up.
 */
async function endLeg(crawlId: string, legId: string, outcome: 'done' | 'failed'): Promise<void> {
  const { last, failed } = await releaseLeg(crawlId, legId, outcome);
  if (!last) return;

  const finished = await finishCrawl(crawlId, failed ? 'failed' : 'completed');
  await clearCrawlState(crawlId);
  if (finished) {
    logger.info(
      {
        crawlId,
        status: finished.status,
        entries: finished.entriesSeen,
        pages: finished.pagesFetched,
      },
      'ladder crawl finished',
    );
  }
}

/** An apex league arrives whole: one request, one upsert, one leg done. */
export async function ladderApex(job: Job<LadderApexJob>): Promise<{ entries: number }> {
  const { crawlId } = job.data;
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);
  const tier = assertApexTier(job.data.tier);
  const legId = ladderLegId(JOB.ladderApex, crawlId, tier);

  try {
    if (!(await stillRunning(crawlId))) {
      await endLeg(crawlId, legId, 'done');
      return { entries: 0 };
    }

    const { data } = await fetcher.fetch<RiotLeagueList>(
      build.apexLeague(platform, tier, queue),
      LADDER_FETCH,
    );

    const entries = (data?.entries ?? [])
      .map((raw) => toEntry(raw, tier))
      .filter((e): e is LeagueEntryInput => e !== undefined);

    await upsertLeagueEntries(crawlId, platform, queue, entries);
    await bumpCrawlCounters(crawlId, { pagesFetched: 1, entriesSeen: entries.length });
    await endLeg(crawlId, legId, 'done');

    return { entries: entries.length };
  } catch (err) {
    if (isFinalAttempt(job)) await endLeg(crawlId, legId, 'failed');
    throw err;
  }
}

/**
 * One (tier, division), page by page, until an empty one.
 *
 * Empty is the only reliable terminator. A short page looks like the end and
 * is not: the ladder churns under the walk, so a page can come back with 180
 * entries in the middle of a division that has thousands left.
 *
 * The cursor advances only after the page is stored, so a crash between the
 * two re-walks a page rather than skipping it. `RateLimitBudgetExceeded`
 * propagates untouched — BullMQ retries with backoff, and the walk resumes on
 * the page it was refused rather than starting over.
 */
export async function ladderWalk(
  job: Job<LadderWalkJob>,
): Promise<{ pages: number; entries: number; done: boolean }> {
  const { crawlId } = job.data;
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);
  const tier = assertPagedTier(job.data.tier);
  const division = assertDivision(job.data.division);
  const legId = ladderLegId(JOB.ladderWalk, crawlId, tier, division);

  let pages = 0;
  let entries = 0;

  try {
    let page = await getCursor(crawlId, tier, division);
    let done = false;

    for (;;) {
      // A cancel cannot reach a job that is already running, so a long walk
      // asks. Once every ten pages: cheap next to the request it guards, and
      // the crawl row is indexed by the id being read.
      if (pages % CANCEL_CHECK_PAGES === 0 && !(await stillRunning(crawlId))) {
        logger.info(
          { crawlId, tier, division, page },
          'ladder walk stopping; crawl is not running',
        );
        break;
      }

      const { data } = await fetcher.fetch<RiotLeagueEntry[]>(
        build.leagueEntriesByTier(platform, queue, tier, division, page),
        LADDER_FETCH,
      );

      if (!data || data.length === 0) {
        done = true;
        break;
      }

      const rows = data
        .map((raw) => toEntry(raw, tier))
        .filter((e): e is LeagueEntryInput => e !== undefined);

      await upsertLeagueEntries(crawlId, platform, queue, rows);
      await bumpCrawlCounters(crawlId, { pagesFetched: 1, entriesSeen: rows.length });
      await setCursor(crawlId, tier, division, page + 1);

      page += 1;
      pages += 1;
      entries += rows.length;
      await job.updateProgress({ tier, division, page, entries });
    }

    await endLeg(crawlId, legId, 'done');
    return { pages, entries, done };
  } catch (err) {
    if (isFinalAttempt(job)) await endLeg(crawlId, legId, 'failed');
    throw err;
  }
}

/** A cancelled or already-finished crawl should not keep spending quota. */
async function stillRunning(crawlId: string): Promise<boolean> {
  const crawl = await getCrawl(crawlId);
  return crawl?.status === 'running';
}

// ── ddragon:sync ─────────────────────────────────────────────────────────────

export async function ddragonSync(job: Job<{ force?: boolean }>): Promise<void> {
  const result = await syncDdragon({ force: job.data?.force ?? false });
  if (!result.changed) return;

  // FR-11: bust the static cache and tell subscribers a patch landed.
  await publish('patch.new', PATCH_TOPIC, { version: result.version });
}

// ── maintenance ──────────────────────────────────────────────────────────────

/**
 * §10 — daily housekeeping. Negative keys and quota counters carry their own
 * TTLs, so this is mostly a safety net against orphaned single-flight locks
 * left behind by a hard crash.
 */
export async function maintenance(): Promise<{ locksCleared: number }> {
  let cursor = '0';
  let cleared = 0;
  do {
    const [next, keys] = await redis.scan(cursor, 'MATCH', 'sf:*', 'COUNT', 500);
    cursor = next;
    if (keys.length === 0) continue;

    // One pipeline per scanned page rather than a PTTL per key, serially: the
    // scan already batches, and the round trips were the whole cost of the job.
    const pipeline = redis.pipeline();
    for (const key of keys) pipeline.pttl(key);
    const ttls = await pipeline.exec();

    // -1 means "no expiry": a lock that outlived its PX, i.e. a leak.
    const orphaned = keys.filter((_, i) => Number(ttls?.[i]?.[1] ?? 0) === -1);
    if (orphaned.length > 0) cleared += await redis.del(...orphaned);
  } while (cursor !== '0');

  logger.info({ locksCleared: cleared }, 'maintenance complete');
  return { locksCleared: cleared };
}

// ── dispatch ─────────────────────────────────────────────────────────────────

export async function dispatch(job: Job): Promise<unknown> {
  try {
    let result: unknown;
    switch (job.name) {
      case JOB.pollLiveTick:
        result = { fannedOut: await fanOut(JOB.pollLive) };
        break;
      case JOB.pollRankTick:
        result = { fannedOut: await fanOut(JOB.pollRank) };
        break;
      case JOB.pollMatchesTick:
        result = { fannedOut: await fanOut(JOB.pollMatches) };
        break;
      case JOB.pollLive:
        result = await pollLive(job as Job<PollPlayerJob>);
        break;
      case JOB.pollRank:
        result = await pollRank(job as Job<PollPlayerJob>);
        break;
      case JOB.pollMatches:
        result = await pollMatches(job as Job<PollPlayerJob>);
        break;
      case JOB.archiveMatch:
        result = await archiveMatchJob(job as Job<ArchiveMatchJob>);
        break;
      case JOB.backfillPlayer:
        result = await backfillPlayer(job as Job<BackfillPlayerJob>);
        break;
      case JOB.ladderCrawl:
        result = await ladderCrawl(job as Job<LadderCrawlJob>);
        break;
      case JOB.ladderApex:
        result = await ladderApex(job as Job<LadderApexJob>);
        break;
      case JOB.ladderWalk:
        result = await ladderWalk(job as Job<LadderWalkJob>);
        break;
      case JOB.ddragonSync:
        result = await ddragonSync(job as Job<{ force?: boolean }>);
        break;
      case JOB.maintenance:
        result = await maintenance();
        break;
      default:
        throw new Error(`Unknown job '${job.name}'`);
    }
    jobsTotal.inc({ job: job.name, status: 'completed' });
    return result;
  } catch (err) {
    jobsTotal.inc({ job: job.name, status: 'failed' });
    throw err;
  }
}

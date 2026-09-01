import { setTimeout as sleep } from 'node:timers/promises';
import type { Job, Queue } from 'bullmq';
import { KEY_SCOPE } from '../config.js';
import { config } from '../config.js';
import {
  advanceCrawlPhase,
  bumpCrawlCounters,
  createCrawl,
  finishCrawl,
  getCrawl,
  listCrawlBackfillCandidates,
  upsertLeagueEntries,
  type CrawlCandidate,
  type LeagueEntryInput,
} from '../db/ladder.js';
import { recomputeChampionStats } from '../db/analytics.js';
import {
  archiveMatch,
  archiveTimeline,
  countArchivedMatches,
  filterUnarchived,
  reextractBatch,
  type RiotMatch,
} from '../db/matches.js';
import type { LadderCrawl } from '../db/schema.js';
import {
  getPlayer,
  listTrackedPlayers,
  markBackfillComplete,
  markBackfillStarted,
  setLastSeenMatch,
  upsertDiscoveredPlayers,
} from '../db/players.js';
import { LADDER_TOPIC, PATCH_TOPIC, playerTopic, publish } from '../events/index.js';
import { fetcher } from '../fetcher.js';
import { ProxyError } from '../errors.js';
import { logger } from '../logger.js';
import {
  jobsTotal,
  ladderCrawlDuration,
  ladderEntriesTotal,
  ladderMatchIdsTotal,
  ladderMatchesQueuedTotal,
  ladderPagesTotal,
} from '../metrics.js';
import { redis } from '../redis.js';
import { build } from '../riot/endpoints.js';
import {
  DIVISIONS,
  QUEUE_IDS,
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
  isApexTier,
  tierAtOrAbove,
  tiersAtOrAbove,
  type ApexTier,
  type PagedTier,
  type RankedQueue,
} from '../riot/ladder.js';
import {
  assertPlatform,
  platformToRegion,
  regionFromMatchId,
  type Platform,
  type Region,
} from '../riot/routing.js';
import {
  addMatchIds,
  clearCrawlState,
  countMatchIds,
  dropMatchIds,
  getCursor,
  peekMatchIds,
  releaseLeg,
  setCursor,
  trackLegs,
} from './ladder-state.js';
import { clearReextractCursor, getReextractCursor, setReextractCursor } from './facts-state.js';
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
  maintenanceQueue,
  pollDedupeId,
  pollQueue,
  type ArchiveMatchJob,
  type BackfillPlayerJob,
  type LadderApexJob,
  type LadderArchiveJob,
  type LadderCollectJob,
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

/**
 * One player's match ids, newest first, a page at a time.
 *
 * The paging is the same wherever it is done — the lookup backfill queues each
 * page's unarchived matches as it goes, and a crawl's collect stage puts them
 * in a set instead — so the loop lives here and the caller says what a page is
 * for. `start` comes with the page because both callers rank by position in
 * the *history*: skipping ten already-archived matches must not promote the
 * eleventh.
 *
 * `ranOut` distinguishes a history that ended from a walk that hit its limit,
 * which is what decides whether the player can be stamped as backfilled.
 */
async function walkMatchIds(
  region: Region,
  puuid: string,
  options: { limit: number; queueId?: number; fetch?: typeof BULK },
  onPage: (ids: string[], start: number) => Promise<void>,
): Promise<{ depth: number; ranOut: boolean }> {
  const { limit, queueId } = options;
  let start = 0;
  let depth = 0;

  while (start < limit) {
    const count = Math.min(BACKFILL_PAGE, limit - start);
    const { data: ids } = await fetcher.fetch<string[]>(
      build.matchIdsByPuuid(region, puuid, {
        start,
        count,
        ...(queueId !== undefined ? { queue: queueId } : {}),
      }),
      options.fetch ?? BULK,
    );
    if (!ids || ids.length === 0) break;

    await onPage(ids, start);
    depth = start + ids.length;

    // Reached the end of this player's history.
    if (ids.length < count) return { depth, ranOut: true };
    start += ids.length;
  }

  return { depth, ranOut: false };
}

/**
 * Whether a walk that got this far has earned the "someone has this player's
 * history" stamp.
 *
 * A *shallow* walk has not. `historyBackfilledAt` means the whole history is
 * accounted for, and a walk that stopped at its own limit has only the top of
 * it — so a 100-match ladder walk must not lock a player out of the
 * 10 000-match walk their first lookup would have done. Two ways to have
 * earned it: the history ran out before the limit did, or the limit was at
 * least as deep as the lookup path asks for.
 *
 * `ranOut` only counts for an unfiltered walk: running out of *ranked* ids
 * says nothing about the rest of a player's history.
 */
function walkIsComplete(ranOut: boolean, limit: number, queueId?: number): boolean {
  return (ranOut && queueId === undefined) || limit >= config.LOOKUP_BACKFILL_LIMIT;
}

export async function backfillPlayer(
  job: Job<BackfillPlayerJob>,
): Promise<{ queued: number; depth: number; complete: boolean }> {
  const { puuid, platform, limit = 500, fetchTimeline, queueId, reason } = job.data;
  const region = platformToRegion(assertPlatform(platform));

  // #44 — claim the walk before doing any of it. If this job dies the stamp is
  // left without a completion, which reads as "tried, did not finish" and lets
  // the next lookup queue it again.
  await markBackfillStarted(puuid, platform);

  let queued = 0;

  const { depth, ranOut } = await walkMatchIds(
    region,
    puuid,
    { limit, ...(queueId !== undefined ? { queueId } : {}) },
    async (ids, start) => {
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

      await job.updateProgress(Math.min(100, Math.round(((start + ids.length) / limit) * 100)));
    },
  );

  // Only now, on the way out: this is the record that stops the next lookup
  // queueing the same walk again, so a partial walk must never write it.
  const complete = walkIsComplete(ranOut, limit, queueId);
  if (complete) await markBackfillComplete(puuid, depth);

  logger.info({ puuid, queued, depth, complete, reason: reason ?? 'admin' }, 'backfill complete');
  return { queued, depth, complete };
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

/**
 * A page landed. Prometheus counts what is moving right now; the crawl row
 * carries the same numbers as the durable per-run summary. Both, because they
 * answer different questions — "is this crawl advancing" is a rate, and
 * "what did last night's run see" is a row.
 */
function countPage(platform: string, queue: string, entries: number): void {
  ladderPagesTotal.inc({ platform, queue });
  if (entries > 0) ladderEntriesTotal.inc({ platform, queue }, entries);
}

export interface StartCrawlInput {
  platform: string;
  queue: string;
  tierFloor?: string;
}

export interface StartCrawlResult {
  crawlId: string;
  created: boolean;
  /** The ladder the id names, normalised — what the caller asked for. */
  platform: Platform;
  queue: RankedQueue;
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
    return { crawlId: crawl.id, created: false, platform, queue, legs: 0 };
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
  return { crawlId: crawl.id, created: true, platform, queue, legs: legs.length };
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
 * End of a leg, and the crawl's state machine with it. Only the caller that
 * removed the last leg of a stage moves the crawl on — to the next stage, or,
 * out of the last one, to finished.
 *
 * The stages are what keep a match from being fetched twice, so the boundary
 * between them has to be exactly this: not one id is collected until every
 * page of the ladder is in, and not one match is fetched until every id is.
 * A crawl that let its stages overlap would be back to ten walks racing for
 * the same game.
 *
 * A failed leg ends the run where it stands rather than advancing. A crawl
 * that has seen part of a ladder should not go on to spend a match budget on
 * the part it did see as though that were the whole thing — the same
 * reasoning that stops it aggregating.
 */
async function endLeg(crawlId: string, legId: string, outcome: 'done' | 'failed'): Promise<void> {
  const { last, failed } = await releaseLeg(crawlId, legId, outcome);
  if (!last) return;

  if (failed) {
    await completeCrawl(crawlId, 'failed');
    return;
  }

  // Re-read rather than trust a row from before the leg ran: a cancel while
  // the leg was in flight has to stop the next stage from being fanned out.
  const crawl = await runningCrawl(crawlId);
  if (!crawl) return;

  switch (crawl.phase) {
    case 'enumerate':
      await startCollectPhase(crawl);
      return;
    case 'collect':
      await startArchivePhase(crawl);
      return;
    default:
      await completeCrawl(crawlId, 'completed');
  }
}

/**
 * Queue a recompute for one ladder.
 *
 * Lifecycle-scoped de-duplication rather than a stable `jobId`: BullMQ matches
 * a job id against finished jobs it has retained, so a stable one would make
 * the *second* crawl of the day a silent no-op (#18). What this drops instead
 * is only a recompute that has not run yet — and one that has not run yet will
 * read the same tables the dropped one would have.
 */
export async function enqueueChampionAggregate(platform: string, queue: string): Promise<void> {
  await maintenanceQueue.add(
    JOB.aggregateChampions,
    { platform, queue } satisfies AggregateChampionsJob,
    { deduplication: { id: jobKey(JOB.aggregateChampions, platform, queue) } },
  );
}

/**
 * Ladder entry → a row in `players`, and nothing more.
 *
 * The hand-off to the match pipeline used to happen here, one player at a time
 * as the pages came in. It no longer does, and that is the point: a match has
 * ten participants, so walking a player's history the moment they are
 * discovered reaches the same game from ten walks spread across the whole
 * crawl. Each of those walks only skips what `filterUnarchived` can already
 * see, so the nine that ran before the match landed all paid for it.
 *
 * So enumeration only records who is there. The collect stage (§6 of
 * docs/ladder-crawl-plan.md, staged) reads them back out of `league_entries`
 * once every leg has finished, and every id it gathers goes into one set.
 *
 * Two pieces of restraint survive from the old hand-off:
 *
 * 1. **The tier floor.** `LADDER_BACKFILL_TIER_FLOOR` is separate from the one
 *    that bounds enumeration, because enumerating a ladder is thousands of
 *    requests and walking the players behind it is millions. Entries below it
 *    land in `league_entries` and go no further — not even into `players`.
 * 2. **Never tracked.** `upsertDiscoveredPlayers` cannot set `tracked`, so a
 *    crawl can never sign thousands of players up for a 60-second poll.
 */
async function recordPlayers(
  crawlId: string,
  platform: Platform,
  entries: LeagueEntryInput[],
): Promise<number> {
  const floor = config.ladderBackfillTierFloor;
  const eligible = entries.filter((e) => tierAtOrAbove(e.tier, floor));
  if (eligible.length === 0) return 0;

  const rows = await upsertDiscoveredPlayers(eligible.map((e) => ({ puuid: e.puuid, platform })));
  await bumpCrawlCounters(crawlId, { playersDiscovered: rows.length });
  return rows.length;
}

/** Players one `ladder:collect` job walks. */
const COLLECT_BATCH = 25;

/** Collect jobs created per round trip to the candidate query. */
const COLLECT_FANOUT_BATCH = 40;

/**
 * Match ids handed to the archive queue at a time. The same 100 the backfill
 * pages at, and the same reason: `filterUnarchived` binds one parameter per id.
 */
const ARCHIVE_BATCH = 100;

/**
 * The fan-out holds a leg of its own while it runs.
 *
 * Without it the first batch of collect jobs could finish — and empty the
 * outstanding set — before the second batch had been added to it, and whoever
 * removed the last leg of that first batch would declare the stage over with
 * most of the ladder still unqueued. The sentinel is released at the end, so
 * the earliest the stage can finish is after every leg exists.
 */
const FANOUT_LEG = 'fanout';

/**
 * Enumeration is done. Everyone this crawl saw, in batches, on the collect
 * queue.
 *
 * Paged rather than read whole: an Emerald-floor crawl discovers hundreds of
 * thousands of players, and the list of them is not a thing to hold in one
 * array on a worker.
 */
async function startCollectPhase(crawl: LadderCrawl): Promise<void> {
  const advanced = await advanceCrawlPhase(crawl.id, 'enumerate', 'collect');
  // Someone else got there first, or the crawl was cancelled in between.
  if (!advanced) return;

  const platform = assertPlatform(crawl.platform);
  const queue = assertRankedQueue(crawl.queue);
  const limit = config.LADDER_BACKFILL_LIMIT;

  // `LADDER_BACKFILL_LIMIT=0` is the cheapest useful mode: enumerate the
  // ladder, walk nobody. There is then nothing to collect and nothing to
  // archive, so the crawl is done.
  if (limit === 0) {
    logger.info({ crawlId: crawl.id }, 'ladder backfill disabled; crawl ends at enumeration');
    await completeCrawl(crawl.id, 'completed');
    return;
  }

  await trackLegs(crawl.id, [FANOUT_LEG]);

  const tiers = tiersAtOrAbove(config.ladderBackfillTierFloor);
  // The cursor is the position in the ladder; `seen` is what names the legs.
  // A leg id has to be unique within the crawl and reproducible by the job
  // that runs it, and the position in the candidate list is both.
  let after: CrawlCandidate | undefined;
  let seen = 0;
  let jobs = 0;

  for (;;) {
    const candidates = await listCrawlBackfillCandidates({
      crawlId: crawl.id,
      platform,
      queue,
      tiers,
      notWalkedSince: crawl.startedAt,
      limit: COLLECT_BATCH * COLLECT_FANOUT_BATCH,
      ...(after ? { after } : {}),
    });
    if (candidates.length === 0) break;
    after = candidates.at(-1);

    const batches: string[][] = [];
    for (let i = 0; i < candidates.length; i += COLLECT_BATCH) {
      batches.push(candidates.slice(i, i + COLLECT_BATCH).map((c) => c.puuid));
    }

    const legs = batches.map((puuids, index) => {
      const batchOffset = seen + index * COLLECT_BATCH;
      return {
        legId: ladderLegId(JOB.ladderCollect, crawl.id, String(batchOffset)),
        puuids,
        batchOffset,
      };
    });

    await trackLegs(
      crawl.id,
      legs.map((leg) => leg.legId),
    );
    await ladderQueue.addBulk(
      legs.map((leg) => ({
        name: JOB.ladderCollect,
        data: {
          crawlId: crawl.id,
          platform,
          queue,
          puuids: leg.puuids,
          offset: leg.batchOffset,
        } satisfies LadderCollectJob,
        opts: {
          priority: LADDER_PRIORITY.collect,
          deduplication: { id: leg.legId },
          removeOnComplete: { age: 3600, count: 500 },
        },
      })),
    );

    seen += candidates.length;
    jobs += legs.length;
  }

  await bumpCrawlCounters(crawl.id, { backfillsEnqueued: seen });
  logger.info({ crawlId: crawl.id, players: seen, jobs }, 'ladder crawl collecting match ids');

  // Releases the stage if there was nobody to collect — a crawl of a ladder
  // whose players have all been walked since it started is a real outcome.
  await endLeg(crawl.id, FANOUT_LEG, 'done');
}

/**
 * Every id is in. One job drains the set, because de-duplication only holds if
 * a single reader owns it — two of them would each find the same id
 * unarchived and each queue it.
 */
async function startArchivePhase(crawl: LadderCrawl): Promise<void> {
  const advanced = await advanceCrawlPhase(crawl.id, 'collect', 'archive');
  if (!advanced) return;

  const legId = ladderLegId(JOB.ladderArchive, crawl.id);
  await trackLegs(crawl.id, [legId]);
  await ladderQueue.add(
    JOB.ladderArchive,
    {
      crawlId: crawl.id,
      platform: crawl.platform,
      queue: crawl.queue,
    } satisfies LadderArchiveJob,
    {
      priority: LADDER_PRIORITY.archive,
      deduplication: { id: legId },
      removeOnComplete: { age: 3600, count: 200 },
    },
  );
  logger.info(
    { crawlId: crawl.id, matchIds: await countMatchIds(crawl.id) },
    'ladder crawl archiving matches',
  );
}

/**
 * The crawl is over. Marks the row, cleans the Redis state up, and — for a
 * clean run only — announces it and queues the aggregate.
 */
async function completeCrawl(crawlId: string, status: 'completed' | 'failed'): Promise<void> {
  const finished = await finishCrawl(crawlId, status);
  await clearCrawlState(crawlId);
  if (!finished) return;

  logger.info(
    {
      crawlId,
      status: finished.status,
      entries: finished.entriesSeen,
      pages: finished.pagesFetched,
      matchIds: finished.matchIdsSeen,
      matchesQueued: finished.matchesQueued,
    },
    'ladder crawl finished',
  );

  const durationS = Math.round(
    ((finished.finishedAt ?? new Date()).getTime() - finished.startedAt.getTime()) / 1000,
  );
  ladderCrawlDuration.observe(
    { platform: finished.platform, queue: finished.queue, status: finished.status },
    durationS,
  );

  // Only a clean run. A crawl that gave up has seen part of a ladder, and
  // aggregating a part of one as though it were the whole thing is worse than
  // leaving the previous numbers in place.
  if (finished.status !== 'completed') return;

  await publish('ladder.crawl.completed', LADDER_TOPIC, {
    crawlId,
    platform: finished.platform,
    queue: finished.queue,
    entries: finished.entriesSeen,
    players: finished.playersDiscovered,
    durationS,
  });

  await enqueueChampionAggregate(finished.platform, finished.queue);
}

/**
 * One batch of discovered players, walked for match ids only.
 *
 * Nothing here fetches a match. The ids go into the crawl's set and the
 * matches behind them are fetched once, by the archive stage, after every
 * collect job has finished — which is what makes one match one `match.byId`
 * however many of its ten participants are on this ladder.
 */
export async function ladderCollect(
  job: Job<LadderCollectJob>,
): Promise<{ players: number; ids: number; newIds: number }> {
  const { crawlId } = job.data;
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);
  const puuids = job.data.puuids ?? [];
  const legId = ladderLegId(JOB.ladderCollect, crawlId, String(job.data.offset));

  const region = platformToRegion(platform);
  let ids = 0;
  let newIds = 0;
  let players = 0;

  try {
    for (const puuid of puuids) {
      // Same cancel check as the walk, per player rather than per ten pages:
      // a player is one or two requests, so this is the same granularity.
      if (!(await runningCrawl(crawlId))) {
        logger.info({ crawlId, players }, 'ladder collect stopping; crawl is not running');
        break;
      }

      await markBackfillStarted(puuid, platform);
      const walked = await collectOne(crawlId, region, platform, queue, puuid);
      if (walked === undefined) continue;
      ids += walked.ids;
      newIds += walked.newIds;
      players += 1;
      await job.updateProgress(Math.round((players / Math.max(1, puuids.length)) * 100));
    }

    if (newIds > 0) {
      ladderMatchIdsTotal.inc({ platform, queue }, newIds);
      await bumpCrawlCounters(crawlId, { matchIdsSeen: newIds });
    }
    await endLeg(crawlId, legId, 'done');
    return { players, ids, newIds };
  } catch (err) {
    if (isFinalAttempt(job)) await endLeg(crawlId, legId, 'failed');
    throw err;
  }
}

/**
 * One player's ids into the crawl's set, or `undefined` if there is no such
 * player any more.
 *
 * A 404 is the one upstream error this swallows. A ladder of thousands
 * contains accounts that have been transferred or deleted since the page that
 * named them, and a leg that fails takes the whole crawl down with it (§6 of
 * docs/ladder-crawl-plan.md) — so one dead PUUID must not cost a run. Anything
 * else propagates: a budget refusal or a 502 is about the service, not the
 * player, and the right answer to it is the retry BullMQ gives the job.
 */
async function collectOne(
  crawlId: string,
  region: Region,
  platform: Platform,
  queue: RankedQueue,
  puuid: string,
): Promise<{ ids: number; newIds: number } | undefined> {
  const limit = config.LADDER_BACKFILL_LIMIT;
  let ids = 0;
  let newIds = 0;

  try {
    const { depth, ranOut } = await walkMatchIds(
      region,
      puuid,
      {
        limit,
        // The crawl is about one ranked ladder, so it pays for that ladder's
        // games rather than the player's whole back-catalogue.
        queueId: QUEUE_IDS[queue],
        // The same generous budget the ladder pages get, and for the same
        // reason: a collect job has nowhere else to be, and a leg that gives
        // up ends the whole crawl rather than merely losing its own place.
        fetch: LADDER_FETCH,
      },
      async (page) => {
        ids += page.length;
        newIds += await addMatchIds(crawlId, page);
      },
    );
    if (walkIsComplete(ranOut, limit, QUEUE_IDS[queue])) {
      await markBackfillComplete(puuid, depth);
    }
    return { ids, newIds };
  } catch (err) {
    if (err instanceof ProxyError && err.code === 'NOT_FOUND') {
      logger.warn({ puuid, platform }, 'ladder collect skipping a player match-v5 does not know');
      return undefined;
    }
    throw err;
  }
}

/**
 * The crawl's de-duplicated match ids, minus what the archive already holds,
 * onto the archive queue.
 *
 * Drained in batches with the ids removed only *after* their jobs exist, so a
 * crash re-queues a batch rather than dropping it — and re-queueing is free,
 * because the archive job id is the match id.
 */
export async function ladderArchive(
  job: Job<LadderArchiveJob>,
): Promise<{ seen: number; queued: number }> {
  const { crawlId } = job.data;
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);
  const legId = ladderLegId(JOB.ladderArchive, crawlId);

  let seen = 0;
  let queued = 0;

  try {
    for (;;) {
      if (!(await runningCrawl(crawlId))) {
        logger.info({ crawlId, seen }, 'ladder archive stopping; crawl is not running');
        break;
      }

      const batch = await peekMatchIds(crawlId, ARCHIVE_BATCH);
      if (batch.length === 0) break;

      const unarchived = await filterUnarchived(batch);
      if (unarchived.length > 0) {
        await archiveQueue.addBulk(
          unarchived.map((matchId) => ({
            name: JOB.archiveMatch,
            data: {
              matchId,
              fetchTimeline: config.ARCHIVE_TIMELINES,
            } satisfies ArchiveMatchJob,
            opts: {
              jobId: jobKey('archive', matchId),
              priority: ARCHIVE_PRIORITY.ladder,
            },
          })),
        );
      }
      await dropMatchIds(crawlId, batch);

      seen += batch.length;
      queued += unarchived.length;
      await job.updateProgress({ seen, queued });
    }

    if (queued > 0) ladderMatchesQueuedTotal.inc({ platform, queue }, queued);
    await bumpCrawlCounters(crawlId, { matchesQueued: queued });
    logger.info({ crawlId, seen, queued }, 'ladder matches handed to the archive queue');
    await endLeg(crawlId, legId, 'done');
    return { seen, queued };
  } catch (err) {
    if (isFinalAttempt(job)) await endLeg(crawlId, legId, 'failed');
    throw err;
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
    const crawl = await runningCrawl(crawlId);
    if (!crawl) {
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
    await countPage(platform, queue, entries.length);
    await bumpCrawlCounters(crawlId, { pagesFetched: 1, entriesSeen: entries.length });
    await recordPlayers(crawlId, platform, entries);
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
    let crawl = await runningCrawl(crawlId);

    for (;;) {
      // A cancel cannot reach a job that is already running, so a long walk
      // asks. Once every ten pages: cheap next to the request it guards, and
      // the crawl row is indexed by the id being read.
      if (pages > 0 && pages % CANCEL_CHECK_PAGES === 0) crawl = await runningCrawl(crawlId);
      if (!crawl) {
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
      await countPage(platform, queue, rows.length);
      await bumpCrawlCounters(crawlId, { pagesFetched: 1, entriesSeen: rows.length });
      await recordPlayers(crawlId, platform, rows);
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

/**
 * The crawl row, or undefined if it is no longer running — a cancelled or
 * already-finished crawl should not keep spending quota. The row rather than a
 * boolean because `started_at` is what tells discovery whom this crawl has
 * already walked.
 */
async function runningCrawl(crawlId: string): Promise<LadderCrawl | undefined> {
  const crawl = await getCrawl(crawlId);
  return crawl?.status === 'running' ? crawl : undefined;
}

// ── aggregate:champions ──────────────────────────────────────────────────────

export interface AggregateChampionsJob {
  platform: string;
  queue: string;
}

/**
 * Read the archive back into `champion_stats` (§7 of the plan).
 *
 * On the `maintenance` queue rather than `ladder`, because it is not part of
 * the crawl: it touches Riot not at all, and a crawl should be free to finish
 * — and free the ladder for the next one — without waiting on a table scan.
 * Nothing here is prioritized, matching the daily job beside it; giving one of
 * two jobs on a queue a priority would put the other permanently ahead of it.
 */
export async function aggregateChampions(
  job: Job<AggregateChampionsJob>,
): Promise<{ rows: number; games: number }> {
  const platform = assertPlatform(job.data.platform);
  const queue = assertRankedQueue(job.data.queue);

  const started = Date.now();
  const result = await recomputeChampionStats(platform, queue);
  logger.info(
    { platform, queue, rows: result.rows, games: result.games, ms: Date.now() - started },
    'champion aggregates recomputed',
  );
  return { rows: result.rows, games: result.games };
}

// ── facts:reextract ─────────────────────────────────────────────────────────

/** Queue a re-extraction of the whole archive's facts (§5.3 of the plan). */
export async function enqueueFactsReextract(): Promise<void> {
  await maintenanceQueue.add(
    JOB.factsReextract,
    {},
    // Lifecycle-scoped, like `enqueueChampionAggregate`: the walk has no id of
    // its own, and a second trigger while one is already in flight must join
    // it rather than start a competing walk over the same cursor.
    { deduplication: { id: jobKey(JOB.factsReextract) } },
  );
}

/** Milliseconds between batches — pure Postgres work at maintenance
 * concurrency 1, paced so it never competes with interactive traffic for the
 * connection pool (§5.3 of the plan). */
const REEXTRACT_PACE_MS = 50;

/**
 * Backfill the widened `match_participants` columns and `match_bans` for
 * every row the archive holds (#110) — everything `archiveMatch` wrote before
 * C2 existed has null fact columns and no bans.
 *
 * One job walks the whole archive rather than one batch per job: the cursor
 * lives in Redis (`facts-state.ts`) precisely so a crash or redeploy resumes
 * the walk instead of restarting it, and BullMQ's own lock renewal is what
 * lets a job run for as long as an archive this size takes.
 */
export async function factsReextract(
  job: Job,
): Promise<{ matches: number; batches: number }> {
  let cursor = await getReextractCursor();
  let matches = 0;
  let batches = 0;

  // Counted once, not per batch: a full-table count is itself an O(archive)
  // scan, and re-paying it every batch would compete with the very pacing
  // this job exists to respect. A denominator fixed at the walk's start is
  // still a true progress read — matches archived mid-walk simply count
  // toward the *next* run, the same way a crawl completing mid-walk does.
  const archived = await countArchivedMatches();

  for (;;) {
    const batch = await reextractBatch(cursor, config.FACTS_REEXTRACT_BATCH);
    if (batch.matchIds.length === 0) break;

    // Non-null: `reextractBatch` only returns a null cursor alongside an
    // empty `matchIds`, which the check above already ruled out.
    cursor = batch.cursor!;
    await setReextractCursor(cursor);
    matches += batch.matchIds.length;
    batches += 1;

    await job.updateProgress({ matches, cursor, archived });
    logger.info({ matches, batches, cursor, archived }, 'facts reextract progress');

    await sleep(REEXTRACT_PACE_MS);
  }

  // The whole archive is caught up, so there is nothing left to resume — the
  // next trigger (a later schema widening) should start from the beginning.
  await clearReextractCursor();
  logger.info({ matches, batches }, 'facts reextract complete');
  return { matches, batches };
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
      case JOB.ladderCollect:
        result = await ladderCollect(job as Job<LadderCollectJob>);
        break;
      case JOB.ladderArchive:
        result = await ladderArchive(job as Job<LadderArchiveJob>);
        break;
      case JOB.aggregateChampions:
        result = await aggregateChampions(job as Job<AggregateChampionsJob>);
        break;
      case JOB.factsReextract:
        result = await factsReextract(job);
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

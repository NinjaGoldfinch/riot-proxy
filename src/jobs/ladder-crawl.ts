import { Job } from 'bullmq';
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
import { filterUnarchived } from '../db/matches.js';
import type { LadderCrawl } from '../db/schema.js';
import {
  markBackfillComplete,
  markBackfillStarted,
  upsertDiscoveredPlayers,
} from '../db/players.js';
import { LADDER_TOPIC, publish } from '../events/index.js';
import { fetcher } from '../fetcher.js';
import { ProxyError } from '../errors.js';
import { logger } from '../logger.js';
import {
  ladderCrawlDuration,
  ladderEntriesTotal,
  ladderMatchesQueuedTotal,
  ladderMatchIdsTotal,
  ladderPagesTotal,
} from '../metrics.js';
import { build } from '../riot/endpoints.js';
import {
  assertApexTier,
  assertDivision,
  assertPagedTier,
  assertRankedQueue,
  assertTier,
  DIVISIONS,
  isApexTier,
  QUEUE_IDS,
  tiersAtOrAbove,
  type ApexTier,
  type PagedTier,
  type RankedQueue,
} from '../riot/ladder.js';
import { assertPlatform, platformToRegion, type Platform, type Region } from '../riot/routing.js';
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
import {
  ARCHIVE_PRIORITY,
  archiveQueue,
  JOB,
  jobKey,
  LADDER_PRIORITY,
  ladderLegId,
  ladderQueue,
  type ArchiveMatchJob,
  type LadderApexJob,
  type LadderArchiveJob,
  type LadderCollectJob,
  type LadderCrawlJob,
  type LadderWalkJob,
} from './queues.js';
import { enqueueAnalyticsRecompute } from './analytics.js';
import { enqueueNameBackfill } from './player-names.js';
import { walkIsComplete, walkMatchIds } from './match-walk.js';

/**
 * The ladder crawl state machine (#88, #120) — `ladder:crawl` and the four job
 * types it fans out to, plus the phase transitions between them.
 *
 * Its own file since #122: this is a state machine with its own vocabulary
 * (legs, phases, cursors, cancellation checks) and roughly six hundred lines of
 * it, and it shares nothing with the per-player polling beside which it used to
 * sit but the queue abstraction.
 */

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
 * One piece of restraint survives from the old hand-off: **never tracked.**
 * `upsertDiscoveredPlayers` cannot set `tracked`, so a crawl can never sign
 * thousands of players up for a 60-second poll.
 *
 * There is no separate backfill floor any more: everyone the crawl enumerates
 * becomes a player and is walked by the collect stage. How much a crawl costs
 * is governed by the one floor it already has — how far down it enumerates —
 * and by `LADDER_BACKFILL_LIMIT`, the matches walked per player.
 */
async function recordPlayers(
  crawlId: string,
  platform: Platform,
  entries: LeagueEntryInput[],
): Promise<number> {
  if (entries.length === 0) return 0;

  const rows = await upsertDiscoveredPlayers(entries.map((e) => ({ puuid: e.puuid, platform })));
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

  // Above the clean-run guard, unlike the aggregate below it, because the two
  // fail differently on a partial run. An aggregate over half a ladder is a
  // wrong number wearing a right one's clothes; a name read out of a match
  // that did land is simply correct, and the crawl that gave up archived those
  // matches all the same.
  await enqueueNameBackfill();

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

  await enqueueAnalyticsRecompute(finished.platform, finished.queue);
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
      logger.debug(
        { crawlId, puuid, ids: walked.ids, newIds: walked.newIds },
        'ladder collect walked a player',
      );
    }

    if (newIds > 0) {
      ladderMatchIdsTotal.inc({ platform, queue }, newIds);
      await bumpCrawlCounters(crawlId, { matchIdsSeen: newIds });
    }
    logger.debug({ crawlId, players, ids, newIds }, 'ladder collect batch done');
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

      // Bumped per batch, not once at the end: this is the one job in the
      // whole crawl that runs for as long as the archive phase does, so it is
      // the one place a counter written only on return would sit stale on the
      // crawl row — and on the dashboard reading it — for the entire drain.
      ladderMatchesQueuedTotal.inc({ platform, queue }, unarchived.length);
      await bumpCrawlCounters(crawlId, { matchesQueued: unarchived.length });
      logger.debug(
        { crawlId, batchSize: batch.length, newlyQueued: unarchived.length, seen, queued },
        'ladder archive batch queued',
      );
    }

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

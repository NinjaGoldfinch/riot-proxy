import { Queue, type JobsOptions } from 'bullmq';
import { config } from '../config.js';
import { logger } from '../logger.js';
import { backfillsQueuedTotal } from '../metrics.js';
import { redis } from '../redis.js';

/** §10 — one queue per concern so priorities and retention can differ. */
export const QUEUE_NAMES = {
  poll: 'poll',
  archive: 'archive',
  backfill: 'backfill',
  ddragon: 'ddragon',
  ladder: 'ladder',
  maintenance: 'maintenance',
} as const;

export type QueueName = (typeof QUEUE_NAMES)[keyof typeof QUEUE_NAMES];

/** Every job is idempotent (§10), so retries are always safe. */
export const DEFAULT_JOB_OPTIONS: JobsOptions = {
  attempts: 3,
  backoff: { type: 'exponential', delay: 2000 },
  removeOnComplete: { age: 3600, count: 1000 },
  removeOnFail: { age: 86_400, count: 1000 },
};

function makeQueue(name: QueueName): Queue {
  return new Queue(name, { connection: redis, defaultJobOptions: DEFAULT_JOB_OPTIONS });
}

export const pollQueue = makeQueue(QUEUE_NAMES.poll);
export const archiveQueue = makeQueue(QUEUE_NAMES.archive);
export const backfillQueue = makeQueue(QUEUE_NAMES.backfill);
export const ddragonQueue = makeQueue(QUEUE_NAMES.ddragon);
export const ladderQueue = makeQueue(QUEUE_NAMES.ladder);
export const maintenanceQueue = makeQueue(QUEUE_NAMES.maintenance);

export const allQueues = [
  pollQueue,
  archiveQueue,
  backfillQueue,
  ddragonQueue,
  ladderQueue,
  maintenanceQueue,
];

/** Job name constants — shared by producers and the worker's processors. */
export const JOB = {
  pollLiveTick: 'poll:live:tick',
  pollLive: 'poll:live',
  pollRankTick: 'poll:rank:tick',
  pollRank: 'poll:rank',
  pollMatchesTick: 'poll:matches:tick',
  pollMatches: 'poll:matches',
  archiveMatch: 'archive:match',
  backfillPlayer: 'backfill:player',
  ddragonSync: 'ddragon:sync',
  ladderCrawl: 'ladder:crawl',
  ladderApex: 'ladder:apex',
  ladderWalk: 'ladder:walk',
  ladderCollect: 'ladder:collect',
  ladderArchive: 'ladder:archive',
  aggregateChampions: 'aggregate:champions',
  namesBackfill: 'names:backfill',
  factsReextract: 'facts:reextract',
  maintenance: 'maintenance',
} as const;

/**
 * BullMQ rejects a custom job id containing ':' unless it splits into exactly
 * three parts — the shape it reserves for its own repeatable-job keys. Our job
 * names are already colon-namespaced ('poll:live'), so composing ids by hand
 * lands on the wrong side of that rule. Join with '-' and strip any colons the
 * parts bring with them.
 */
export function jobKey(...parts: (string | number)[]): string {
  return parts.map((part) => String(part).replaceAll(':', '-')).join('-');
}

/**
 * The de-duplication id for one poll of one player.
 *
 * Deliberately *not* a `jobId`. A custom job id is matched against finished
 * jobs BullMQ has retained as well as pending ones — the trap behind #18 — so a
 * stable job id here would silence a player's polling for the whole
 * `removeOnComplete` window after every successful tick. That is why the id
 * used to carry a timestamp, and why it then de-duplicated nothing: ticks are
 * further apart than a second, so every tick minted a fresh id.
 *
 * A deduplication id is keyed on the job's *lifecycle* instead. BullMQ writes
 * `de:{id}` with no expiry when the job is created and deletes it when the job
 * reaches completed or failed, so the window is exactly "still pending or
 * running" — what the fan-out wanted all along, and immune to retention.
 *
 * The job name is part of the id because all three poll types share one queue,
 * and the de-duplication namespace is per queue.
 */
export function pollDedupeId(jobName: string, puuid: string): string {
  return jobKey(jobName, puuid);
}

/**
 * Ordering inside the archive queue, and the one BullMQ detail that is easy to
 * get backwards: the worker pops the plain `wait` list first and only falls
 * back to the prioritized set once it is empty, so a job with *no* priority
 * outranks every prioritized job. Every archive job therefore carries an
 * explicit priority — omitting one would silently promote it above the
 * freshest game in the queue.
 *
 * 1 is the highest priority; larger numbers yield to smaller ones.
 */
export const ARCHIVE_PRIORITY = {
  /** A game that has just finished for a tracked player. */
  live: 1,
  /**
   * A match a ladder crawl found. Below every depth `backfillPriority()` can
   * reach — a lookup walk is capped at `LOOKUP_BACKFILL_LIMIT`, whose ceiling
   * of 10 000 matches ranks at 1 010 — so a crawl's forty thousand matches
   * never sit in front of the history somebody is watching fill in.
   */
  ladder: 10_000,
} as const;

/** BullMQ rejects a priority above 2^21 - 1. */
export const MAX_PRIORITY = 2_097_151;

export const BACKFILL_PRIORITY_BASE = 10;
/** Matches are ranked in blocks of ten, so a page's worth shares a rank. */
export const BACKFILL_PRIORITY_BLOCK = 10;

/**
 * Recency wins: a player's most recent ten games are archived before anyone's
 * hundredth, so someone who has just been looked up sees their history fill in
 * from the top rather than waiting on a stranger's 2022 season.
 *
 * `index` is the match's position in the player's history, newest first.
 */
export function backfillPriority(index: number): number {
  const depth = Math.floor(Math.max(0, index) / BACKFILL_PRIORITY_BLOCK);
  return Math.min(MAX_PRIORITY, BACKFILL_PRIORITY_BASE + depth);
}

/**
 * Ordering inside the ladder queue. Same trap as `ARCHIVE_PRIORITY`: a job
 * with no priority is popped before every prioritized one, so all three of
 * these are explicit.
 *
 * The apex leagues come before the paged walks because they are three requests
 * for the most valuable slice of the ladder — a crawl that gets no further has
 * still produced something. The fan-out itself outranks both so a triggered
 * crawl starts fanning out rather than queueing behind the previous one's tail.
 */
export const LADDER_PRIORITY = {
  crawl: 1,
  apex: 2,
  walk: 3,
  /**
   * The later stages rank below the enumeration, which costs nothing while a
   * crawl runs one stage at a time — but a *second* ladder's crawl can overlap
   * this one's, and when it does, enumerating that ladder (thousands of pages,
   * and the only stage with a page cursor to keep warm) should not queue
   * behind this one's match-id walk.
   */
  collect: 4,
  archive: 5,
} as const;

export interface LadderCrawlJob {
  platform: string;
  queue: string;
  tierFloor?: string;
}

export interface LadderApexJob {
  crawlId: string;
  platform: string;
  queue: string;
  tier: string;
}

export interface LadderWalkJob {
  crawlId: string;
  platform: string;
  queue: string;
  tier: string;
  division: string;
}

/**
 * One batch of discovered players whose match ids are wanted. A batch rather
 * than a job per player because a ladder is thousands of them and the work per
 * player is one or two requests — the fan-out would otherwise cost more than
 * the walk.
 */
export interface LadderCollectJob {
  crawlId: string;
  platform: string;
  queue: string;
  puuids: string[];
  /**
   * Where this batch started in the crawl's candidate list. It names the leg —
   * the batches are disjoint slices of one ordered query, so the offset is the
   * only part of the job that distinguishes them.
   */
  offset: number;
}

/** The crawl's de-duplicated match ids, handed to the archive queue. */
export interface LadderArchiveJob {
  crawlId: string;
  platform: string;
  queue: string;
}

/**
 * One leg of a crawl — an apex league, or one (tier, division) walk. The id is
 * both the BullMQ de-duplication id and the member of the outstanding-legs set
 * that decides when the crawl is finished, so the two cannot drift apart.
 *
 * Lifecycle-scoped like `pollDedupeId`, and for the same reason: a stable
 * `jobId` would be matched against retained finished jobs, so re-crawling a
 * ladder within the retention window would silently drop its legs.
 */
export function ladderLegId(job: string, ...parts: string[]): string {
  return jobKey(job, ...parts);
}

export interface ArchiveMatchJob {
  matchId: string;
  puuid?: string;
  fetchTimeline?: boolean;
}

/** Why a backfill was queued — the log line, the metric label, and the rank. */
export const BACKFILL_REASONS = ['admin', 'lookup', 'track', 'catchup'] as const;
export type BackfillReason = (typeof BACKFILL_REASONS)[number];

/**
 * Ordering inside the *backfill* queue. Not to be confused with
 * `backfillPriority()` below, which ranks the archive jobs a walk produces.
 *
 * Every reason ranks the same, and all four are listed anyway, because of the
 * trap `ARCHIVE_PRIORITY` documents: the worker drains the unprioritized
 * `wait` list before it touches the prioritized set, so one unranked reason
 * would be popped ahead of every ranked one.
 *
 * A ladder crawl is not among them. Its work reaches the archive queue as
 * `ARCHIVE_PRIORITY.ladder` — the crawl walks match ids on its own queue and
 * hands over matches, never backfill jobs — and that is where "a crawl yields
 * to somebody looking up a player" is enforced.
 */
export const BACKFILL_PRIORITY: Record<BackfillReason, number> = {
  lookup: 1,
  track: 1,
  admin: 1,
  catchup: 1,
};

export interface BackfillPlayerJob {
  puuid: string;
  platform: string;
  /** Total matches to walk back through; paged 100 at a time (§10). */
  limit?: number;
  fetchTimeline?: boolean;
  /**
   * match-v5 queue id to restrict the id list to. A ladder walk sets it so the
   * crawl pays for ranked games rather than every mode the player has touched;
   * everything else walks the history whole.
   */
  queueId?: number;
  /** Why it was queued, for the log line that says how it got here. */
  reason?: BackfillReason;
}

export interface PollPlayerJob {
  puuid: string;
  platform: string;
}

/**
 * §10 — a stable job id de-duplicates concurrent backfills for one player, but
 * BullMQ honours that id against *finished* jobs it has retained too: an hour
 * for completed, a day for failed. That made a re-request a silent no-op with a
 * response indistinguishable from a real enqueue, and recovering from a failed
 * backfill meant deleting the key in Redis by hand.
 *
 * So de-duplicate only while a backfill is actually pending. A repeat costs
 * almost nothing — `filterUnarchived` skips whatever is already stored — so a
 * retained finished job is dropped rather than honoured.
 */
const PENDING_JOB_STATES = new Set([
  'waiting',
  'waiting-children',
  'active',
  'delayed',
  'prioritized',
]);

export interface BackfillEnqueueResult {
  jobId: string;
  /** Which of the two actually happened, so a caller can tell them apart. */
  status: 'queued' | 'already-queued';
}

/**
 * `queue` is injectable so this can be exercised against a throwaway queue: the
 * real one is drained by whatever worker happens to be running, which would
 * make the outcome depend on the machine.
 */
export async function enqueueBackfill(
  data: BackfillPlayerJob,
  queue: Queue = backfillQueue,
): Promise<BackfillEnqueueResult> {
  const jobId = jobKey('backfill', data.puuid);
  const existing = await queue.getJob(jobId);

  if (existing) {
    if (PENDING_JOB_STATES.has(await existing.getState())) {
      return record(data, { jobId, status: 'already-queued' });
    }
    try {
      await existing.remove();
    } catch {
      // Claimed between the state read and the remove, so it is pending after all.
      return record(data, { jobId, status: 'already-queued' });
    }
  }

  const job = await queue.add(JOB.backfillPlayer, data, {
    jobId,
    priority: BACKFILL_PRIORITY[data.reason ?? 'admin'],
  });
  return record(data, { jobId: job.id ?? jobId, status: 'queued' });
}

/**
 * Counted here rather than at each call site: every path into a backfill goes
 * through this function, and the two outcomes it distinguishes are exactly the
 * ones worth telling apart (#81). `reason` is optional on the job — the admin
 * route accepts a body without one — and an unlabelled series would silently
 * merge with a named one, so it falls back to what that route means.
 */
function record(data: BackfillPlayerJob, result: BackfillEnqueueResult): BackfillEnqueueResult {
  backfillsQueuedTotal.inc({ reason: data.reason ?? 'admin', status: result.status });
  return result;
}

/**
 * The ladder crawl is the one repeatable that is off by default
 * (`LADDER_CRAWL_S=0`), because it is the one that can spend a month of a dev
 * key's budget on what it discovers. One schedule per (platform, queue), since
 * a crawl is per ladder.
 *
 * Turning it back off has to remove the schedules, not merely stop adding
 * them: a scheduler upserted by a previous boot lives in Redis and would keep
 * firing against a config that says it should not.
 */
export async function scheduleLadderCrawls(): Promise<void> {
  const wanted = config.ladderPlatforms.flatMap((platform) =>
    config.ladderQueues.map((queue) => ({
      schedulerId: jobKey(JOB.ladderCrawl, platform, queue),
      data: { platform, queue },
    })),
  );

  if (config.LADDER_CRAWL_S === 0) {
    for (const { schedulerId } of wanted) {
      const removed = await ladderQueue.removeJobScheduler(schedulerId);
      if (removed) logger.info({ schedulerId }, 'ladder crawl schedule removed (LADDER_CRAWL_S=0)');
    }
    return;
  }

  for (const { schedulerId, data } of wanted) {
    await ladderQueue.upsertJobScheduler(
      schedulerId,
      { every: config.LADDER_CRAWL_S * 1000 },
      {
        name: JOB.ladderCrawl,
        data,
        opts: {
          priority: LADDER_PRIORITY.crawl,
          removeOnComplete: { age: 3600, count: 100 },
        },
      },
    );
    logger.info({ ...data, everySeconds: config.LADDER_CRAWL_S }, 'ladder crawl scheduled');
  }
}

export async function closeQueues(): Promise<void> {
  await Promise.allSettled(allQueues.map((q) => q.close()));
}

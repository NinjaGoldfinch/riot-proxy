import { Queue, type JobsOptions } from 'bullmq';
import { redis } from '../redis.js';

/** §10 — one queue per concern so priorities and retention can differ. */
export const QUEUE_NAMES = {
  poll: 'poll',
  archive: 'archive',
  backfill: 'backfill',
  ddragon: 'ddragon',
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
export const maintenanceQueue = makeQueue(QUEUE_NAMES.maintenance);

export const allQueues = [pollQueue, archiveQueue, backfillQueue, ddragonQueue, maintenanceQueue];

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

export interface ArchiveMatchJob {
  matchId: string;
  puuid?: string;
  fetchTimeline?: boolean;
}

export interface BackfillPlayerJob {
  puuid: string;
  platform: string;
  /** Total matches to walk back through; paged 100 at a time (§10). */
  limit?: number;
  fetchTimeline?: boolean;
  /** Why it was queued, for the log line that says how it got here. */
  reason?: 'admin' | 'lookup';
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
      return { jobId, status: 'already-queued' };
    }
    try {
      await existing.remove();
    } catch {
      // Claimed between the state read and the remove, so it is pending after all.
      return { jobId, status: 'already-queued' };
    }
  }

  const job = await queue.add(JOB.backfillPlayer, data, { jobId });
  return { jobId: job.id ?? jobId, status: 'queued' };
}

export async function closeQueues(): Promise<void> {
  await Promise.allSettled(allQueues.map((q) => q.close()));
}

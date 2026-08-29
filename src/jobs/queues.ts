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
}

export interface PollPlayerJob {
  puuid: string;
  platform: string;
}

export async function closeQueues(): Promise<void> {
  await Promise.allSettled(allQueues.map((q) => q.close()));
}

import { Worker, type Job } from 'bullmq';
import { config } from './config.js';
import { closeDb } from './db/index.js';
import { startHeartbeat, stopHeartbeat } from './jobs/heartbeat.js';
import { dispatch } from './jobs/processors.js';
import {
  JOB,
  QUEUE_NAMES,
  closeQueues,
  ddragonQueue,
  jobKey,
  maintenanceQueue,
  pollQueue,
  scheduleLadderCrawls,
} from './jobs/queues.js';
import { logger } from './logger.js';
import { closeRedis, redis } from './redis.js';
import { riotClient } from './riot/client.js';

/**
 * `worker` process (§3.3): BullMQ consumers for polling, backfill, ddragon sync
 * and archive writes. Single instance in v1 (§10).
 */

/**
 * Concurrency is deliberately modest: the limiter is the real throttle, and a
 * high concurrency here just means more jobs queueing inside `acquire()`.
 */
const CONCURRENCY: Record<string, number> = {
  [QUEUE_NAMES.poll]: 8,
  [QUEUE_NAMES.archive]: 4,
  [QUEUE_NAMES.backfill]: 1,
  [QUEUE_NAMES.ddragon]: 1,
  // A walk job is mostly spent inside the limiter, so a few in parallel cost
  // nothing extra — the buckets are the throttle either way. Three keeps the
  // apex leagues and a couple of divisions moving together without letting the
  // crawl monopolise the worker.
  [QUEUE_NAMES.ladder]: 3,
  [QUEUE_NAMES.maintenance]: 1,
};

function startWorker(queueName: string): Worker {
  const worker = new Worker(queueName, async (job: Job) => dispatch(job), {
    connection: redis,
    concurrency: CONCURRENCY[queueName] ?? 1,
  });

  worker.on('failed', (job, err) => {
    logger.error({ err, job: job?.name, id: job?.id, attempts: job?.attemptsMade }, 'job failed');
  });
  worker.on('completed', (job) => {
    logger.debug({ job: job.name, id: job.id }, 'job completed');
  });

  return worker;
}

/**
 * §10 — repeatable schedulers. Each tick fans out to one job per tracked
 * player, so adding or removing a tracked player needs no scheduler changes.
 *
 * BullMQ 6 dropped the `repeat` job option; a schedule is now its own entity,
 * upserted by id. The id doubles as the de-duplication key, so a restart
 * re-points the existing schedule rather than stacking a second one.
 */
async function scheduleRepeatables(): Promise<void> {
  const repeatables: {
    queue: typeof pollQueue;
    name: string;
    everySeconds: number;
    data?: unknown;
  }[] = [
    { queue: pollQueue, name: JOB.pollLiveTick, everySeconds: config.TRACK_POLL_LIVE_S },
    { queue: pollQueue, name: JOB.pollRankTick, everySeconds: config.TRACK_POLL_RANK_S },
    { queue: pollQueue, name: JOB.pollMatchesTick, everySeconds: config.TRACK_POLL_MATCH_S },
    { queue: ddragonQueue, name: JOB.ddragonSync, everySeconds: config.DDRAGON_SYNC_S },
    { queue: maintenanceQueue, name: JOB.maintenance, everySeconds: 86_400 },
  ];

  for (const { queue, name, everySeconds, data } of repeatables) {
    // Each job the scheduler emits is named `repeat:<schedulerId>:<millis>`, so
    // a colon in the id would push that past the three parts BullMQ allows.
    // `jobKey` strips the colons our job names carry.
    await queue.upsertJobScheduler(
      jobKey(name),
      { every: everySeconds * 1000 },
      { name, data: data ?? {}, opts: { removeOnComplete: { age: 3600, count: 100 } } },
    );
    logger.info({ job: name, everySeconds }, 'repeatable scheduled');
  }

  await scheduleLadderCrawls();
}

async function main(): Promise<void> {
  await scheduleRepeatables();

  const workers = Object.values(QUEUE_NAMES).map(startWorker);
  // Only now: the point of the heartbeat is that something is consuming the
  // queues, so it must not start beating before anything is.
  const heartbeat = startHeartbeat();
  logger.info({ queues: Object.values(QUEUE_NAMES) }, 'riot-proxy worker started');

  const shutdown = async (signal: string) => {
    logger.info({ signal }, 'worker shutting down');
    try {
      // Before draining, not after: from here on nothing new is picked up, and
      // `close()` waits for jobs in flight — which can take a limiter budget.
      await stopHeartbeat(heartbeat);
      // `close()` waits for in-flight jobs so a poll is never half-applied.
      await Promise.allSettled(workers.map((w) => w.close()));
      await closeQueues();
      await riotClient.close();
      await Promise.allSettled([closeRedis(), closeDb()]);
    } catch (err) {
      logger.error({ err }, 'error during worker shutdown');
    } finally {
      process.exit(0);
    }
  };

  process.on('SIGTERM', () => void shutdown('SIGTERM'));
  process.on('SIGINT', () => void shutdown('SIGINT'));
}

main().catch((err: unknown) => {
  logger.error({ err }, 'worker failed to start');
  process.exit(1);
});

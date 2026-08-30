import './helpers/env.js';
import { Queue, Worker } from 'bullmq';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Player } from '../src/db/schema.js';

/**
 * §10 — a repeatable tick fans out to one job per tracked player, and must not
 * stack a second job on a player whose previous one has not run yet.
 *
 * The de-duplication it claimed never worked: the job id carried
 * `Date.now() / 1000`, and ticks are 60–600 s apart, so every tick minted a
 * fresh id. Under a backlog — the normal state, since polls run at bulk
 * priority behind BULK_USAGE_CEILING — the queue grew by one job per player
 * per tick with nothing to bound it.
 *
 * `listTrackedPlayers` is mocked rather than seeded: this is about what reaches
 * the queue, and a real row would make the count depend on what else is in the
 * developer's database.
 */
const tracked: Player[] = [];

/** The default `TRACK_POLL_LIVE_S`, in milliseconds — the real gap between ticks. */
const TRACK_POLL_LIVE_MS = 60_000;

vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return { ...actual, listTrackedPlayers: async () => tracked };
});

const { fanOut } = await import('../src/jobs/processors.js');
const { JOB, pollDedupeId } = await import('../src/jobs/queues.js');
const { closeRedis, redis } = await import('../src/redis.js');

let queue: Queue;
let available = false;

const player = (n: string): Player => ({
  puuid: `${'p'.repeat(60)}${n}`,
  keyScope: 'test',
  platform: 'euw1',
  gameName: null,
  tagLine: null,
  tracked: true,
  lastSeenMatchId: null,
  historyBackfillStartedAt: null,
  historyBackfilledAt: null,
  historyBackfillDepth: null,
  updatedAt: new Date(),
});

/** Everything the queue is holding for a player, whatever state it sits in. */
async function pending(): Promise<number> {
  const counts = await queue.getJobCounts('waiting', 'delayed', 'prioritized', 'active');
  return Object.values(counts).reduce((sum, n) => sum + n, 0);
}

/**
 * One scheduler tick, with the clock where it would actually be by then.
 *
 * Only `Date` is faked. BullMQ and ioredis run their own timers and read Redis
 * TIME, and freezing those would deadlock the worker in `drainOne`.
 */
let clock = Date.now();
async function tick(jobName: string): Promise<number> {
  clock += TRACK_POLL_LIVE_MS;
  vi.setSystemTime(clock);
  return fanOut(jobName, queue);
}

/** Let a real worker take exactly one job through to `completed`, then stop. */
async function drainOne(): Promise<void> {
  const worker = new Worker(queue.name, async () => 'done', {
    connection: redis,
    concurrency: 1,
  });
  try {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('no job completed in time')), 10_000);
      worker.once('completed', () => {
        clearTimeout(timer);
        resolve();
      });
      worker.once('failed', (_job, err) => {
        clearTimeout(timer);
        reject(err);
      });
    });
  } finally {
    await worker.close();
  }
}

beforeAll(async () => {
  try {
    await redis.ping();
    available = true;
  } catch {
    available = false;
  }
  // A throwaway queue: the real `poll` queue is drained by whatever worker is
  // running on this machine, which would consume the jobs mid-assertion.
  if (available) queue = new Queue(`poll-spec-${process.pid}`, { connection: redis });
});

afterAll(async () => {
  if (queue) {
    await queue.obliterate({ force: true }).catch(() => {});
    await queue.close();
  }
  await closeRedis();
});

beforeEach(async () => {
  tracked.length = 0;
  vi.useFakeTimers({ toFake: ['Date'], now: clock });
  if (available) await queue.obliterate({ force: true });
});

afterEach(() => {
  clock = Date.now();
  vi.useRealTimers();
});

describe('poll fan-out de-duplication (§10)', () => {
  it('queues one job per tracked player', async ({ skip }) => {
    if (!available) return skip();
    tracked.push(player('a'), player('b'));

    expect(await fanOut(JOB.pollLive, queue)).toBe(2);
    expect(await pending()).toBe(2);
  });

  it('does not stack a second job on a player whose first has not run', async ({ skip }) => {
    if (!available) return skip();
    tracked.push(player('a'), player('b'));

    // Ticks a poll interval apart, not three calls in the same millisecond.
    // The old id de-duplicated only within one second, so a burst would have
    // passed this while the real 60 s cadence it exists for never did.
    await tick(JOB.pollLive);
    await tick(JOB.pollLive);
    await tick(JOB.pollLive);

    expect(await pending()).toBe(2);
  });

  it('still reports every tracked player, even when it queued nothing new', async ({ skip }) => {
    if (!available) return skip();
    tracked.push(player('a'), player('b'));

    await fanOut(JOB.pollLive, queue);
    // The count is what the tick fanned out *for*, so a suppressed job does not
    // read as a shrinking tracked-player list.
    expect(await fanOut(JOB.pollLive, queue)).toBe(2);
  });

  it('keeps the three poll types independent, since they share one queue', async ({ skip }) => {
    if (!available) return skip();
    tracked.push(player('a'));

    // The de-duplication namespace is per queue, so the job name has to be part
    // of the id or a live poll would suppress that player's rank poll.
    expect(pollDedupeId(JOB.pollLive, 'x')).not.toBe(pollDedupeId(JOB.pollRank, 'x'));

    await fanOut(JOB.pollLive, queue);
    await fanOut(JOB.pollRank, queue);
    await fanOut(JOB.pollMatches, queue);

    expect(await pending()).toBe(3);
  });

  it('queues again once the previous job has finished', async ({ skip }) => {
    if (!available) return skip();
    tracked.push(player('a'));

    await fanOut(JOB.pollLive, queue);
    const [job] = await queue.getJobs(['waiting']);
    expect(job).toBeDefined();

    // Drain it with a real worker rather than moving the id between Redis keys
    // by hand: BullMQ drops the de-duplication key inside `moveToFinished`, so
    // a hand-written state change would leave the key behind and fail this for
    // the wrong reason.
    await drainOne();
    expect(await job!.getState()).toBe('completed');

    // The finished job is still retained — that is the trap #18 was about, and
    // a job id would still be blocked by it here. A de-duplication id is not.
    await fanOut(JOB.pollLive, queue);
    expect(await pending()).toBe(1);
  });
});

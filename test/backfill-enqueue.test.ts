import './helpers/env.js';
import { Queue } from 'bullmq';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { enqueueBackfill } from '../src/jobs/queues.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * §10 / #18 — de-duplication has to tell "one is already running" apart from
 * "one ran an hour ago"; both used to look identical to a caller.
 *
 * Run against a throwaway queue rather than the real one. The real `backfill`
 * queue is drained by whatever worker happens to be running, so enqueuing there
 * would both disturb it and make the result depend on the machine.
 */
let queue: Queue;
let available = false;

beforeAll(async () => {
  available = await probeServices('backfill-enqueue.test.ts', async () => {
    await redis.ping();
    return true;
  });
  if (available) queue = new Queue(`backfill-spec-${Date.now()}`, { connection: redis });
});

afterAll(async () => {
  if (queue) {
    await queue.obliterate({ force: true }).catch(() => {});
    await queue.close();
  }
  await closeRedis();
});

const player = (n: string) => ({ puuid: `${'p'.repeat(60)}${n}`, platform: 'euw1' });

describe('backfill de-duplication (§10)', () => {
  it('reports a real enqueue as queued', async ({ skip }) => {
    if (!available) return skip();
    expect(await enqueueBackfill(player('a'), queue)).toMatchObject({ status: 'queued' });
  });

  it('recognises a still-pending backfill instead of stacking another', async ({ skip }) => {
    if (!available) return skip();
    const first = await enqueueBackfill(player('b'), queue);
    const second = await enqueueBackfill(player('b'), queue);
    expect(second.status).toBe('already-queued');
    expect(second.jobId).toBe(first.jobId);
  });

  it('re-queues once a retained job has finished, rather than silently dropping it', async ({
    skip,
  }) => {
    if (!available) return skip();
    const first = await enqueueBackfill(player('c'), queue);

    // Stand the job down into `completed`, which is exactly the state BullMQ
    // retains for an hour and used to honour the id against.
    await redis.zadd(`bull:${queue.name}:completed`, Date.now(), first.jobId);
    await redis.lrem(`bull:${queue.name}:wait`, 0, first.jobId);
    expect(await (await queue.getJob(first.jobId))?.getState()).toBe('completed');

    const again = await enqueueBackfill(player('c'), queue);
    expect(again.status).toBe('queued');
    expect(await (await queue.getJob(again.jobId))?.getState()).not.toBe('completed');
  });
});

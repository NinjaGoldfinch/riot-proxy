import './helpers/env.js';
import { Value } from '@sinclair/typebox/value';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { closeQueues, ddragonQueue } from '../src/jobs/queues.js';
import { closeRedis, redis } from '../src/redis.js';
import {
  METRICS_HISTORY_KEY,
  METRICS_HISTORY_LOCK_KEY,
  METRICS_HISTORY_MAX_POINTS,
  MetricsHistoryRecorder,
  buildHistoryPoint,
  readMetricsHistory,
} from '../src/stats/history.js';
import { MetricsHistoryPoint } from '../src/stats/schema.js';

/**
 * The history recorder against real backing services — the point built from
 * live reads, the one-recorder-per-interval election, and the retention cap.
 */
let available = false;

beforeAll(async () => {
  available = await probeServices('metrics-history.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
});

afterAll(async () => {
  if (available) await redis.del(METRICS_HISTORY_KEY, METRICS_HISTORY_LOCK_KEY);
  await Promise.allSettled([closeQueues(), closeRedis(), closeDb()]);
});

describe('metrics history (dashboard 24 h charts)', () => {
  it('builds a point matching the published schema', async ({ skip }) => {
    if (!available) return skip();
    const point = await buildHistoryPoint();
    expect([...Value.Errors(MetricsHistoryPoint, point)]).toEqual([]);
  });

  /**
   * The 24 h backlog chart used to draw a flat line at the scheduler count
   * rather than at zero, because every repeatable parks a job in `delayed`
   * between firings. Twenty at once, not one: the suite shares Redis, so a real
   * enqueue landing between the two reads can move the number by a job or two —
   * it cannot move it by twenty, which is what a regression here would do.
   *
   * On `ddragon` rather than `maintenance` because the snapshot suite asserts
   * exact scheduler deltas on that queue, and vitest runs the files in parallel
   * against one Redis.
   */
  it('charts work waiting, not the schedulers parked in delayed', async ({ skip }) => {
    if (!available) return skip();
    const ids = Array.from({ length: 20 }, (_, i) => `test-history-scheduler-${i}`);

    const before = (await buildHistoryPoint()).queues.pending;
    // `startDate` a year out, so each parks in `delayed` rather than running its
    // first iteration immediately — a `waiting` job is real work and should
    // count, which is the distinction under test.
    const aYear = 31_536_000_000;
    for (const id of ids) {
      await ddragonQueue.upsertJobScheduler(
        id,
        { every: aYear, startDate: Date.now() + aYear },
        { name: 'ddragon:sync', data: {} },
      );
    }
    try {
      const after = (await buildHistoryPoint()).queues.pending;
      expect(after - before).toBeLessThan(ids.length / 2);
    } finally {
      for (const id of ids) await ddragonQueue.removeJobScheduler(id);
    }
  });

  it('records one point per interval, however many instances tick', async ({ skip }) => {
    if (!available) return skip();
    await redis.del(METRICS_HISTORY_KEY, METRICS_HISTORY_LOCK_KEY);

    const recorder = new MetricsHistoryRecorder();
    await recorder.tick();
    // A second tick inside the same interval loses the lock election and must
    // not write — this is what keeps N api instances producing one timeline.
    await recorder.tick();

    const points = await readMetricsHistory();
    expect(points).toHaveLength(1);
    expect([...Value.Errors(MetricsHistoryPoint, points[0])]).toEqual([]);

    // Retention is self-cleaning: the list expires a retention-span after the
    // last write, so an abandoned deployment leaves nothing immortal behind.
    expect(await redis.pttl(METRICS_HISTORY_KEY)).toBeGreaterThan(0);
  });

  it('drops the oldest point once the cap is reached', async ({ skip }) => {
    if (!available) return skip();
    await redis.del(METRICS_HISTORY_KEY, METRICS_HISTORY_LOCK_KEY);

    const filler = Array.from({ length: METRICS_HISTORY_MAX_POINTS }, (_, i) =>
      JSON.stringify({ t: i }),
    );
    await redis.rpush(METRICS_HISTORY_KEY, ...filler);

    await new MetricsHistoryRecorder().tick();

    expect(await redis.llen(METRICS_HISTORY_KEY)).toBe(METRICS_HISTORY_MAX_POINTS);
    const points = await readMetricsHistory();
    // Oldest first, oldest dropped: the seeded t=0 is gone, the fresh point last.
    expect(points[0]?.t).toBe(1);
    expect(points[points.length - 1]?.t).toBeGreaterThan(METRICS_HISTORY_MAX_POINTS);
  });
});

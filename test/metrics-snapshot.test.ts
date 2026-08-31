import './helpers/env.js';
import { Value } from '@sinclair/typebox/value';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { closeDb, pingDb } from '../src/db/index.js';
import { beat, stopHeartbeat } from '../src/jobs/heartbeat.js';
import { QUEUE_NAMES, closeQueues } from '../src/jobs/queues.js';
import { closeRedis, redis } from '../src/redis.js';
import { MetricsSnapshot } from '../src/stats/schema.js';
import { buildMetricsSnapshot } from '../src/stats/snapshot.js';

/**
 * The snapshot builder against real backing services. The shape assertion is
 * the load-bearing one: the same schema validates `GET /v1/admin/metrics`
 * responses and documents the `metrics.snapshot` event, so a builder that
 * drifts from it ships a route that cannot serialise.
 */
let available = false;

beforeAll(async () => {
  available = await probeServices('metrics-snapshot.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
});

afterAll(async () => {
  await Promise.allSettled([closeQueues(), closeRedis(), closeDb()]);
});

describe('buildMetricsSnapshot', () => {
  it('produces exactly the shape both transports promise', async ({ skip }) => {
    if (!available) return skip();
    const snap = await buildMetricsSnapshot({
      wsConnections: 2,
      wsSubscriptions: 5,
      wsEventCounts: { 'match.archived': 3 },
    });

    expect([...Value.Errors(MetricsSnapshot, snap)]).toEqual([]);
    expect(snap.ws).toEqual({ connections: 2, subscriptions: 5 });
    expect(snap.events).toEqual({ 'match.archived': 3 });
  });

  /**
   * #80 — an operator reading queue counts alone cannot tell a crashed worker
   * from an idle one; both are all-zero `active`. The snapshot is where that
   * question gets answered, so it has to carry the heartbeat.
   */
  it('says whether a worker is consuming the queues', async ({ skip }) => {
    if (!available) return skip();
    const inputs = { wsConnections: 0, wsSubscriptions: 0, wsEventCounts: {} };

    await stopHeartbeat(undefined);
    expect((await buildMetricsSnapshot(inputs)).worker).toEqual({
      alive: false,
      lastSeenMs: null,
    });

    await beat();
    const live = (await buildMetricsSnapshot(inputs)).worker;
    expect(live.alive).toBe(true);
    expect(live.lastSeenMs).toBeLessThan(1000);
    await stopHeartbeat(undefined);
  });

  /** #81 — demand, as against the work it turns into. */
  it('carries the flow counters both transports now publish', async ({ skip }) => {
    if (!available) return skip();
    const snap = await buildMetricsSnapshot({
      wsConnections: 0,
      wsSubscriptions: 0,
      wsEventCounts: {},
    });

    // Sparse by design: a deployment nobody has asked to refresh publishes an
    // empty object rather than a grid of invented zeroes.
    expect(snap.flows.backfillsQueued).toBeTypeOf('object');
    expect(snap.flows.refreshClaims).toBeTypeOf('object');
    for (const key of Object.keys(snap.flows.backfillsQueued)) {
      expect(key).toMatch(/^(admin|lookup|track|catchup):(queued|already-queued)$/);
    }
    for (const key of Object.keys(snap.flows.refreshClaims)) {
      expect(key).toMatch(/^[a-z]+:(claimed|coalesced)$/);
    }
  });

  it('reports every queue, present or empty', async ({ skip }) => {
    if (!available) return skip();
    const snap = await buildMetricsSnapshot({
      wsConnections: 0,
      wsSubscriptions: 0,
      wsEventCounts: {},
    });
    expect(Object.keys(snap.queues).sort()).toEqual(Object.values(QUEUE_NAMES).sort());
  });
});

import './helpers/env.js';
import { Queue } from 'bullmq';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { probeServices } from './helpers/services.js';
import { enqueueBackfill } from '../src/jobs/queues.js';
import { backfillsQueuedTotal, refreshClaimsTotal } from '../src/metrics.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * #81 — two §13-worthy behaviours whose only trace was a log line.
 *
 * A lookup-triggered backfill (#44) and an operator asking for one by hand
 * produce the same execution, so `proxy_jobs_total{job="backfill:player"}`
 * cannot separate them; and a `?refresh=true` that loses its cooldown window
 * (#33) counted nowhere at all, which is precisely the outcome worth watching —
 * it is quota the caller asked to spend and did not.
 */
let queue: Queue;
let available = false;

beforeAll(async () => {
  available = await probeServices('flow-metrics.test.ts', async () => {
    await redis.ping();
    return true;
  });
  if (available) queue = new Queue(`flow-metrics-${process.pid}`, { connection: redis });
});

afterAll(async () => {
  if (queue) {
    await queue.obliterate({ force: true }).catch(() => {});
    await queue.close();
  }
  await closeRedis();
});

/** One labelled series' value, or 0 — a counter publishes nothing until it fires. */
async function count(
  counter: typeof backfillsQueuedTotal | typeof refreshClaimsTotal,
  labels: Record<string, string>,
): Promise<number> {
  const metric = await counter.get();
  const match = metric.values.find((v) => {
    // The two counters carry different label names, so the union's key type is
    // narrower than the lookup — widened here rather than duplicating this.
    const seen = v.labels as Record<string, string | number | undefined>;
    return Object.entries(labels).every(([k, want]) => seen[k] === want);
  });
  return match?.value ?? 0;
}

const player = (n: string) => ({
  puuid: `${'f'.repeat(60)}${n}`,
  platform: 'euw1',
  reason: 'lookup' as const,
});

describe('backfill demand (#81)', () => {
  it('counts why a walk was asked for, not just that one ran', async ({ skip }) => {
    if (!available) return skip();
    const before = await count(backfillsQueuedTotal, { reason: 'lookup', status: 'queued' });

    await enqueueBackfill(player('a'), queue);

    expect(await count(backfillsQueuedTotal, { reason: 'lookup', status: 'queued' })).toBe(
      before + 1,
    );
  });

  it('counts a request that coalesced onto a pending walk as demand', async ({ skip }) => {
    if (!available) return skip();
    const before = await count(backfillsQueuedTotal, {
      reason: 'lookup',
      status: 'already-queued',
    });

    await enqueueBackfill(player('b'), queue);
    // The second asks for work that is already coming: no job, but a caller
    // who wanted one. Counting only the enqueue would read as no demand.
    expect(await enqueueBackfill(player('b'), queue)).toMatchObject({ status: 'already-queued' });

    expect(await count(backfillsQueuedTotal, { reason: 'lookup', status: 'already-queued' })).toBe(
      before + 1,
    );
  });

  it('labels a job carrying no reason as the admin route that allows it', async ({ skip }) => {
    if (!available) return skip();
    const before = await count(backfillsQueuedTotal, { reason: 'admin', status: 'queued' });

    const { reason: _reason, ...noReason } = player('c');
    await enqueueBackfill(noReason, queue);

    // An unlabelled series would merge with a named one on the wire.
    expect(await count(backfillsQueuedTotal, { reason: 'admin', status: 'queued' })).toBe(
      before + 1,
    );
  });
});

/**
 * The route module is imported lazily: it pulls in the composite fetchers, and
 * this file only wants the metric they are wired to.
 */
describe('refresh claims (#81)', () => {
  it('counts the window won and the window lost, and nothing else', async ({ skip }) => {
    if (!available) return skip();
    const { __test } = await import('../src/routes/players.js');
    const identity = `refresh-metric-${process.pid}`;
    const key = `refresh:${(await import('../src/config.js')).KEY_SCOPE}:profile:${identity}`;
    await redis.del(key);

    const claimed = await count(refreshClaimsTotal, { part: 'profile', outcome: 'claimed' });
    const coalesced = await count(refreshClaimsTotal, { part: 'profile', outcome: 'coalesced' });

    // A read that never asked for a refresh. Every request passes through here
    // to learn `availableIn`, so counting these would drown the signal.
    await __test.refreshWindow('profile', identity, false);
    expect(await count(refreshClaimsTotal, { part: 'profile', outcome: 'claimed' })).toBe(claimed);
    expect(await count(refreshClaimsTotal, { part: 'profile', outcome: 'coalesced' })).toBe(
      coalesced,
    );

    expect(await __test.refreshWindow('profile', identity, true)).toMatchObject({
      refreshed: true,
    });
    // Losing is not an error — the winner fetched seconds ago — but it is
    // quota a caller asked to spend and did not.
    expect(await __test.refreshWindow('profile', identity, true)).toMatchObject({
      refreshed: false,
    });

    expect(await count(refreshClaimsTotal, { part: 'profile', outcome: 'claimed' })).toBe(
      claimed + 1,
    );
    expect(await count(refreshClaimsTotal, { part: 'profile', outcome: 'coalesced' })).toBe(
      coalesced + 1,
    );
    await redis.del(key);
  });
});

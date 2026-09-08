import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { probeServices } from './helpers/services.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * `aggregate:analytics` as a job (#114) — what it reports, what it records for
 * the dashboard, and what it does when a step fails.
 *
 * The recomputes themselves are covered against real SQL in
 * `ladder-analytics.test.ts`; they are stubbed here so the orchestration is
 * what is under test. Only Redis is real, because the run record is the
 * deliverable.
 */

const recomputeChampionStats = vi.fn();
const recomputeChampionMatchups = vi.fn();
const recomputeChampionBuilds = vi.fn();

vi.mock('../src/db/analytics.js', () => ({
  recomputeChampionStats: (...args: unknown[]) => recomputeChampionStats(...args),
  recomputeChampionMatchups: (...args: unknown[]) => recomputeChampionMatchups(...args),
  recomputeChampionBuilds: (...args: unknown[]) => recomputeChampionBuilds(...args),
}));
vi.mock('../src/db/matches.js', () => ({
  countArchivedMatches: async () => 0,
  reextractBatch: async () => ({ matchIds: [], cursor: null }),
}));

const { aggregateAnalytics } = await import('../src/jobs/analytics.js');
const { listAnalyticsRuns } = await import('../src/jobs/analytics-state.js');
const { registry } = await import('../src/metrics.js');

const PLATFORM = 'vn2';
const QUEUE = 'RANKED_SOLO_5x5';
const job = () => ({ data: { platform: PLATFORM, queue: QUEUE } }) as never;

let available = false;

async function wipe(): Promise<void> {
  const keys = await redis.keys('analytics:run*');
  if (keys.length > 0) await redis.del(...keys);
}

beforeAll(async () => {
  available = await probeServices('analytics-job.test.ts', async () => {
    await redis.ping();
    return true;
  });
});

beforeEach(async () => {
  recomputeChampionStats.mockReset().mockResolvedValue({ rows: 12, games: 340 });
  recomputeChampionMatchups.mockReset().mockResolvedValue({ rows: 7 });
  recomputeChampionBuilds.mockReset().mockResolvedValue({ items: 5, runes: 3, spells: 2 });
  if (available) await wipe();
});

afterAll(async () => {
  if (available) await wipe();
  await closeRedis();
});

describe('aggregate:analytics', () => {
  it('runs every step and reports a row count per table', async ({ skip }) => {
    if (!available) return skip();
    const result = await aggregateAnalytics(job());

    expect(recomputeChampionStats).toHaveBeenCalledWith(PLATFORM, QUEUE);
    expect(recomputeChampionMatchups).toHaveBeenCalledWith(PLATFORM, QUEUE);
    expect(recomputeChampionBuilds).toHaveBeenCalledWith(PLATFORM, QUEUE);

    // Every table, not just `champion_stats`. The admin route answers 202, so
    // this is the only structured signal an operator gets that a build step
    // inserted nothing.
    expect(result.rows).toEqual({
      champion_stats: 12,
      champion_matchups: 7,
      champion_items: 5,
      champion_runes: 3,
      champion_spells: 2,
    });
    expect(result.games).toBe(340);
  });

  it('records the run where the dashboard reads it, with a time per step', async ({ skip }) => {
    if (!available) return skip();
    await aggregateAnalytics(job());

    const [run] = await listAnalyticsRuns();
    expect(run).toMatchObject({
      platform: PLATFORM,
      queue: QUEUE,
      status: 'completed',
      games: 340,
    });
    // The job runs in the worker and the snapshot is built in the api, so a
    // per-process metric could never carry this across — which is why it is in
    // Redis at all.
    expect(Object.keys(run?.steps ?? {})).toEqual(['champions', 'matchups', 'builds']);
    expect(run?.rows.champion_items).toBe(5);
  });

  it('records a failed run with the steps that did finish', async ({ skip }) => {
    if (!available) return skip();
    recomputeChampionBuilds.mockRejectedValue(new Error('builds exploded'));

    await expect(aggregateAnalytics(job())).rejects.toThrow('builds exploded');

    const [run] = await listAnalyticsRuns();
    expect(run?.status).toBe('failed');
    // Half a run is exactly the state the per-step transactions permit, so how
    // far it got is the thing worth recording: the two steps that committed
    // are named, the one that died is not.
    expect(Object.keys(run?.steps ?? {})).toEqual(['champions', 'matchups']);
    expect(run?.rows).toMatchObject({ champion_stats: 12, champion_matchups: 7 });
    expect(run?.rows).not.toHaveProperty('champion_items');
  });

  it('counts runs by outcome, so a half-run is visible to Prometheus', async ({ skip }) => {
    if (!available) return skip();
    // Read as a delta: the registry is process-global, so every test above has
    // already moved these counters and an absolute assertion would encode the
    // order this file happens to run in.
    const value = async (status: string): Promise<number> => {
      const metric = await registry.getSingleMetricAsString('proxy_aggregate_runs_total');
      const line = metric
        .split('\n')
        .find((l) => l.startsWith('proxy_aggregate_runs_total{') && l.includes(`"${status}"`));
      return line ? Number(line.slice(line.lastIndexOf(' ') + 1)) : 0;
    };

    const before = { completed: await value('completed'), failed: await value('failed') };

    await aggregateAnalytics(job());
    recomputeChampionMatchups.mockRejectedValue(new Error('nope'));
    await expect(aggregateAnalytics(job())).rejects.toThrow('nope');

    expect(await value('completed')).toBe(before.completed + 1);
    expect(await value('failed')).toBe(before.failed + 1);

    const metrics = await registry.metrics();
    expect(metrics).toContain('proxy_aggregate_duration_seconds');
    expect(metrics).toContain('proxy_aggregate_rows');
  });
});

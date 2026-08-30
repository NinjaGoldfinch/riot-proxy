import './helpers/env.js';
import { readFile } from 'node:fs/promises';
import { describe, expect, it } from 'vitest';
import {
  CACHE_HIT_STATES,
  cacheReadsTotal,
  recordCacheOutcome,
  registry,
  type CacheOutcome,
} from '../src/metrics.js';

/**
 * §13 — the cache hit ratio.
 *
 * It used to be a gauge holding `hits / total` over two counters that only ever
 * grew, so it reported the average since the process started. The longer the
 * proxy stayed up the less it could move, and `CacheHitRatioLow` lost the
 * ability to fire at all. Nothing caught that, because from inside the process
 * the number looked entirely reasonable.
 *
 * Recency is now a property of the query, not of the process, so what is worth
 * asserting here is that the process publishes the raw counts the query needs —
 * and does not publish a pre-divided ratio that would invite the same mistake.
 */
const ALERTS = new URL('../ops/prometheus-alerts.yml', import.meta.url);
const DASHBOARD = new URL('../ops/grafana/riot-proxy-dashboard.json', import.meta.url);

async function count(state: string): Promise<number> {
  const metric = await cacheReadsTotal.get();
  return metric.values.find((v) => v.labels['state'] === state)?.value ?? 0;
}

describe('cache outcome accounting (§13)', () => {
  it('counts each outcome under its own state label', async () => {
    const before = await Promise.all(
      (['hit', 'miss', 'neg', 'stale'] as CacheOutcome[]).map(count),
    );

    recordCacheOutcome('hit');
    recordCacheOutcome('hit');
    recordCacheOutcome('miss');
    recordCacheOutcome('neg');
    recordCacheOutcome('stale');

    const after = await Promise.all((['hit', 'miss', 'neg', 'stale'] as CacheOutcome[]).map(count));
    expect(after.map((n, i) => n - before[i]!)).toEqual([2, 1, 1, 1]);
  });

  it('exposes counters rather than a pre-divided ratio', async () => {
    const exposed = await registry.metrics();
    expect(exposed).toContain('proxy_cache_reads_total');
    // A gauge cannot carry a windowed ratio, and one that tried is what this
    // whole change is about. If it comes back, so does the silent alert.
    expect(exposed).not.toContain('proxy_cache_hit_ratio');
  });

  it('agrees with the alert about which outcomes count as a hit', async () => {
    const alerts = await readFile(ALERTS, 'utf8');
    const selector = /proxy_cache_reads_total\{state=~"([^"]+)"\}/.exec(alerts);
    expect(selector).not.toBeNull();
    // Drift here is invisible until the ratio is quietly wrong: add a fourth
    // hit-ish outcome in code, forget the alert, and it starts reading as a
    // miss.
    expect(selector![1]!.split('|').sort()).toEqual([...CACHE_HIT_STATES].sort());
  });

  it('is charted the same way the alert measures it', async () => {
    const dashboard = await readFile(DASHBOARD, 'utf8');
    // The stat panel and the alert must not disagree about what "hit ratio"
    // means, or the graph will look fine while the alert fires.
    expect(dashboard).toContain(
      'sum(rate(proxy_cache_reads_total{state=~\\"hit|neg|stale\\"}[5m])) / sum(rate(proxy_cache_reads_total[5m]))',
    );
    expect(dashboard).not.toContain('proxy_cache_hit_ratio');
  });
});

import { Counter, Gauge, Histogram, Registry, collectDefaultMetrics } from 'prom-client';

/** §13 — minimum metric set. One registry shared by the api and worker processes. */
export const registry = new Registry();
collectDefaultMetrics({ register: registry, prefix: 'proxy_node_' });

export const requestsTotal = new Counter({
  name: 'proxy_requests_total',
  help: 'Downstream requests served by the proxy',
  labelNames: ['route', 'status', 'cache'] as const,
  registers: [registry],
});

export const upstreamRequestsTotal = new Counter({
  name: 'proxy_upstream_requests_total',
  help: 'Requests dispatched to the Riot API',
  labelNames: ['region', 'method', 'status'] as const,
  registers: [registry],
});

export const upstreamLatency = new Histogram({
  name: 'proxy_upstream_latency_seconds',
  help: 'Riot API round-trip latency',
  labelNames: ['region', 'method'] as const,
  buckets: [0.025, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10],
  registers: [registry],
});

export const rlWaitSeconds = new Histogram({
  name: 'proxy_rl_wait_seconds',
  help: 'Time spent waiting on the rate limiter before dispatch',
  labelNames: ['region', 'priority'] as const,
  buckets: [0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10],
  registers: [registry],
});

export const rl429Total = new Counter({
  name: 'proxy_rl_429_total',
  help: 'Upstream 429s received, by rate limit type',
  labelNames: ['region', 'type'] as const,
  registers: [registry],
});

/**
 * §13 wants a cache hit ratio. It is exposed as a counter pair rather than a
 * ready-made ratio so the *window* belongs to whoever asks the question.
 *
 * A gauge cannot carry one. The previous one divided two process-lifetime
 * counters, so it reported the average since boot and grew steadily less able
 * to move: after a week of healthy traffic a total cache outage barely dented
 * it, and `CacheHitRatioLow` — which alerts below 70 % — quietly stopped being
 * able to fire. It was also per-process, so api and worker each published their
 * own and the alert's bare comparison matched them one at a time.
 *
 * `sum(rate(...))` over a label selector answers all of that: recent by
 * construction, and aggregated across processes before the division.
 */
export const cacheReadsTotal = new Counter({
  name: 'proxy_cache_reads_total',
  help: 'Cacheable reads served, by cache outcome',
  labelNames: ['state'] as const,
  registers: [registry],
});

export const wsConnections = new Gauge({
  name: 'proxy_ws_connections',
  help: 'Currently open WebSocket connections on this instance',
  registers: [registry],
});

export const jobsTotal = new Counter({
  name: 'proxy_jobs_total',
  help: 'Background jobs processed',
  labelNames: ['job', 'status'] as const,
  registers: [registry],
});

export const archivedMatchesTotal = new Counter({
  name: 'proxy_archived_matches_total',
  help: 'Matches upserted into the Postgres archive',
  registers: [registry],
});

/**
 * Outcomes that spared an upstream call. `stale` counts as a hit because the
 * caller was answered from cache; the refresh it triggers is counted separately
 * when it runs.
 */
export const CACHE_HIT_STATES = ['hit', 'neg', 'stale'] as const;
export type CacheOutcome = 'hit' | 'miss' | 'neg' | 'stale';

export function recordCacheOutcome(state: CacheOutcome): void {
  cacheReadsTotal.inc({ state });
}

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

export const cacheHitRatio = new Gauge({
  name: 'proxy_cache_hit_ratio',
  help: 'Rolling cache hit ratio across all cacheable reads',
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
 * The hit ratio gauge needs its own accounting: a Counter pair would force
 * every scrape to do the division in PromQL, and §13 alerts on the gauge.
 */
let hits = 0;
let total = 0;

export function recordCacheOutcome(state: 'hit' | 'miss' | 'neg' | 'stale'): void {
  total += 1;
  if (state === 'hit' || state === 'neg' || state === 'stale') hits += 1;
  cacheHitRatio.set(total === 0 ? 0 : hits / total);
}

export function resetCacheRatio(): void {
  hits = 0;
  total = 0;
  cacheHitRatio.set(0);
}

import { KEY_SCOPE } from '../config.js';
import { countActiveConsumers } from '../db/consumers.js';
import { countArchivedMatches } from '../db/matches.js';
import { countPlayers, countTrackedPlayers } from '../db/players.js';
import { allQueues } from '../jobs/queues.js';
import { cacheReadsTotal, type CacheOutcome } from '../metrics.js';
import { limiter } from '../riot/limiter.js';
import type { MetricsSnapshotData } from './schema.js';

/**
 * What the caller already holds and this module cannot reach: the hub's gauges
 * are per-process state on `wsHub`, and importing the hub here would tie the
 * builder to the api process it happens to run in.
 */
export interface SnapshotInputs {
  wsConnections: number;
  wsSubscriptions: number;
  wsEventCounts: Record<string, number>;
}

const QUEUE_STATES = [
  'active',
  'waiting',
  'prioritized',
  'delayed',
  'failed',
  'completed',
] as const;

async function queueCounts(): Promise<MetricsSnapshotData['queues']> {
  const counts = await Promise.all(allQueues.map((q) => q.getJobCounts(...QUEUE_STATES)));
  return Object.fromEntries(
    allQueues.map((q, i) => {
      const c = counts[i] ?? {};
      return [
        q.name,
        {
          active: c.active ?? 0,
          waiting: c.waiting ?? 0,
          prioritized: c.prioritized ?? 0,
          delayed: c.delayed ?? 0,
          failed: c.failed ?? 0,
          completed: c.completed ?? 0,
        },
      ];
    }),
  );
}

async function limiterState(): Promise<MetricsSnapshotData['limiter']> {
  const scopes = await limiter.knownScopes();
  return Promise.all(
    scopes.map(async (scope) => {
      const [windows, frozenMs] = await Promise.all([
        limiter.usage(scope),
        limiter.isFrozen(scope),
      ]);
      return { scope, frozenMs, windows };
    }),
  );
}

async function cacheCounts(): Promise<MetricsSnapshotData['cache']> {
  const out: MetricsSnapshotData['cache'] = { hit: 0, miss: 0, neg: 0, stale: 0 };
  const metric = await cacheReadsTotal.get();
  for (const { labels, value } of metric.values) {
    const state = labels.state as CacheOutcome | undefined;
    if (state && state in out) out[state] = value;
  }
  return out;
}

/**
 * The one builder behind both transports: `GET /v1/admin/metrics` returns this
 * and the `metrics` topic ticks it, so the two cannot disagree.
 *
 * `count(*)` on matches is exact and cheap at self-hosted scale; the day an
 * archive outgrows it, the escape hatch is `pg_class.reltuples` — an estimate,
 * which a dashboard would not notice.
 */
export async function buildMetricsSnapshot(inputs: SnapshotInputs): Promise<MetricsSnapshotData> {
  const [
    archivedMatches,
    trackedPlayers,
    knownPlayers,
    activeConsumers,
    queues,
    limiterScopes,
    cache,
  ] = await Promise.all([
    countArchivedMatches(),
    countTrackedPlayers(),
    countPlayers(),
    countActiveConsumers(),
    queueCounts(),
    limiterState(),
    cacheCounts(),
  ]);

  const memory = process.memoryUsage();
  return {
    v: 1,
    keyScope: KEY_SCOPE,
    totals: { archivedMatches, trackedPlayers, knownPlayers, activeConsumers },
    queues,
    ws: { connections: inputs.wsConnections, subscriptions: inputs.wsSubscriptions },
    events: inputs.wsEventCounts,
    cache,
    limiter: limiterScopes,
    process: { uptimeSeconds: process.uptime(), rssBytes: memory.rss },
  };
}

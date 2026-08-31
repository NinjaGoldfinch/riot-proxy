import { KEY_SCOPE } from '../config.js';
import { countActiveConsumers } from '../db/consumers.js';
import { countAllLeagueEntries, lastFinishedCrawl, listRunningCrawls } from '../db/ladder.js';
import { countArchivedMatches } from '../db/matches.js';
import { countPlayers, countTrackedPlayers } from '../db/players.js';
import { workerLiveness } from '../jobs/heartbeat.js';
import { pendingLegs } from '../jobs/ladder-state.js';
import { allQueues } from '../jobs/queues.js';
import {
  backfillsQueuedTotal,
  cacheReadsTotal,
  refreshClaimsTotal,
  type CacheOutcome,
} from '../metrics.js';
import { limiter } from '../riot/limiter.js';
import { PLATFORM_LABELS, REGION_LABELS, isPlatform, isRegion } from '../riot/routing.js';
import type { LadderCrawl } from '../db/schema.js';
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

/** Exported for the history recorder, which cuts a compact point from the same reads. */
export async function queueCounts(): Promise<MetricsSnapshotData['queues']> {
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
  const scopes = await limiter.knownScopeMethods();
  return Promise.all(
    [...scopes].map(async ([scope, methodNames]) => {
      const [windows, frozenMs, methods] = await Promise.all([
        limiter.usage(scope),
        limiter.isFrozen(scope),
        limiter.methodUsage(scope, methodNames),
      ]);
      const kind = isPlatform(scope) ? 'platform' : isRegion(scope) ? 'region' : 'other';
      const label =
        kind === 'platform'
          ? PLATFORM_LABELS[scope as keyof typeof PLATFORM_LABELS]
          : kind === 'region'
            ? REGION_LABELS[scope as keyof typeof REGION_LABELS]
            : scope;
      return { scope, kind, label, frozenMs, windows, methods };
    }),
  );
}

/** Exported for the history recorder, which cuts a compact point from the same reads. */
export async function cacheCounts(): Promise<MetricsSnapshotData['cache']> {
  const out: MetricsSnapshotData['cache'] = { hit: 0, miss: 0, neg: 0, stale: 0 };
  const metric = await cacheReadsTotal.get();
  for (const { labels, value } of metric.values) {
    const state = labels.state as CacheOutcome | undefined;
    if (state && state in out) out[state] = value;
  }
  return out;
}

/**
 * Flatten a labelled counter into `<a>:<b>` keys.
 *
 * The wire shape is a flat record rather than a nested one because these are
 * sparse: a deployment nobody has called `?refresh=true` against publishes an
 * empty object, and a reason that has never fired never appears. A fixed
 * nesting would have to invent zeroes for every combination instead.
 */
async function labelledCounts(
  counter: { get: () => Promise<{ values: { labels: Record<string, unknown>; value: number }[] }> },
  first: string,
  second: string,
): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  const metric = await counter.get();
  for (const { labels, value } of metric.values) {
    const a = labels[first];
    const b = labels[second];
    if (typeof a === 'string' && typeof b === 'string') out[`${a}:${b}`] = value;
  }
  return out;
}

/**
 * The crawl block. `pendingLegs` comes from Redis rather than the row because
 * that is where the fan-out records them — and it is the only number that
 * moves while a multi-hour walk is between legs.
 */
async function ladderState(): Promise<MetricsSnapshotData['ladder']> {
  const [running, last, entries] = await Promise.all([
    listRunningCrawls(),
    lastFinishedCrawl(),
    countAllLeagueEntries(),
  ]);

  return {
    running: await Promise.all(running.map((crawl) => progress(crawl))),
    lastCompleted: last ? await progress(last) : null,
    entries,
  };
}

async function progress(
  crawl: LadderCrawl,
): Promise<MetricsSnapshotData['ladder']['running'][number]> {
  return {
    id: crawl.id,
    platform: crawl.platform,
    queue: crawl.queue,
    tierFloor: crawl.tierFloor,
    status: crawl.status,
    startedAt: crawl.startedAt.toISOString(),
    finishedAt: crawl.finishedAt ? crawl.finishedAt.toISOString() : null,
    pagesFetched: crawl.pagesFetched,
    entriesSeen: crawl.entriesSeen,
    playersDiscovered: crawl.playersDiscovered,
    backfillsEnqueued: crawl.backfillsEnqueued,
    // A finished crawl's legs have been cleared, so the read is skipped rather
    // than reported as a spuriously exact zero from a key that is gone.
    pendingLegs: crawl.status === 'running' ? await pendingLegs(crawl.id) : 0,
  };
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
    worker,
    backfillsQueued,
    refreshClaims,
    ladder,
  ] = await Promise.all([
    countArchivedMatches(),
    countTrackedPlayers(),
    countPlayers(),
    countActiveConsumers(),
    queueCounts(),
    limiterState(),
    cacheCounts(),
    workerLiveness(),
    labelledCounts(backfillsQueuedTotal, 'reason', 'status'),
    labelledCounts(refreshClaimsTotal, 'part', 'outcome'),
    ladderState(),
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
    worker,
    flows: { backfillsQueued, refreshClaims },
    ladder,
    process: { uptimeSeconds: process.uptime(), rssBytes: memory.rss },
  };
}

import { KEY_SCOPE } from '../config.js';
import { redis } from '../redis.js';

/**
 * What the last analytics recompute did, per ladder (#114).
 *
 * In Redis rather than in `prom-client`, and the reason is the process split:
 * `aggregate:analytics` runs in the **worker**, and the snapshot the dashboard
 * reads is built in the **api**. Prometheus metrics are per-process, so the
 * api's registry has never seen a single one of the worker's observations — the
 * same reason `MetricsSnapshot.ladder` is documented as "read from Postgres,
 * not from this process". The metrics in `metrics.ts` are for Prometheus, which
 * scrapes both; this is for the one dashboard that has to render them together.
 *
 * Not in Postgres, unlike the crawl: a crawl is a record of work done that an
 * operator may want to look back over weeks later, which is why `ladder_crawls`
 * is a table. This is "how did the last run go", it is superseded every time
 * the job runs, and a table for it would be a table of rows nobody ever reads
 * twice.
 */
const runKey = (platform: string, queue: string): string =>
  `analytics:run:${KEY_SCOPE}:${platform}:${queue}`;

const INDEX_KEY = `analytics:runs:${KEY_SCOPE}`;

/**
 * Kept well past any plausible recompute cadence — a deployment that
 * aggregates once a crawl and crawls weekly should still show its last run —
 * but not forever, so a ladder that is no longer aggregated ages out of the
 * dashboard instead of sitting there looking current.
 */
const RUN_TTL_S = 30 * 24 * 3600;

export interface AnalyticsRun {
  platform: string;
  queue: string;
  /** Epoch ms the run finished, however it ended. */
  at: number;
  status: 'completed' | 'failed';
  /** Wall-clock milliseconds for the whole job. */
  ms: number;
  /** Seconds per step, in the order the steps ran. */
  steps: Record<string, number>;
  /** Rows written, per table. */
  rows: Record<string, number>;
  games: number;
}

export async function recordAnalyticsRun(run: AnalyticsRun): Promise<void> {
  const key = runKey(run.platform, run.queue);
  await redis
    .multi()
    .set(key, JSON.stringify(run), 'EX', RUN_TTL_S)
    // An index, because the snapshot wants every ladder's last run and
    // `KEYS`/`SCAN` for a handful of known keys is the wrong tool. Set members
    // outlive their runs harmlessly: a read that misses simply skips it.
    .sadd(INDEX_KEY, `${run.platform}:${run.queue}`)
    .expire(INDEX_KEY, RUN_TTL_S)
    .exec();
}

/** Every ladder's last run, newest first. Absent runs are skipped, not faked. */
export async function listAnalyticsRuns(): Promise<AnalyticsRun[]> {
  const members = await redis.smembers(INDEX_KEY);
  if (members.length === 0) return [];

  const raw = await redis.mget(
    ...members.map((m) => {
      const [platform, ...rest] = m.split(':');
      return runKey(platform ?? '', rest.join(':'));
    }),
  );

  const runs: AnalyticsRun[] = [];
  for (const entry of raw) {
    if (entry === null) continue;
    try {
      runs.push(JSON.parse(entry) as AnalyticsRun);
    } catch {
      // A malformed entry is worse than an absent one; skip it rather than
      // failing the whole snapshot the dashboard is waiting on.
    }
  }
  return runs.sort((a, b) => b.at - a.at);
}

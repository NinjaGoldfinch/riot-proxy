import { KEY_SCOPE, config } from '../config.js';
import { countArchivedMatches } from '../db/matches.js';
import { countPlayers, countTrackedPlayers } from '../db/players.js';
import { logger } from '../logger.js';
import { redis } from '../redis.js';
import type { MetricsHistoryPointData } from './schema.js';
import { cacheCounts, queueCounts } from './snapshot.js';

/**
 * The metrics history — one compact point per `METRICS_HISTORY_INTERVAL_S`
 * into a capped Redis list, so a dashboard opened cold can draw the last day
 * instead of starting from the moment its socket connected.
 *
 * Deliberately the opposite of the broadcaster's no-subscriber gate: history
 * is only worth anything if it was being recorded while nobody watched. The
 * cost is one count(*) pair and one queue read per minute, which is nothing —
 * the snapshot's expensive parts (limiter scans, per-scope pipelines) are not
 * in the point.
 */

// Key-scoped like every other Redis key (§7.4), and for the broadcaster's
// reason: a dev server and a test run sharing one Redis are different
// deployments with different histories.
export const METRICS_HISTORY_KEY = `metrics:history:${KEY_SCOPE}`;
export const METRICS_HISTORY_LOCK_KEY = `metrics:history:lock:${KEY_SCOPE}`;

/**
 * 24 hours at the default 60 s cadence. A constant rather than a knob: the
 * retention question an operator actually asks is "how far back can I look",
 * and the answer scales with the one knob that exists (the interval).
 */
export const METRICS_HISTORY_MAX_POINTS = 1440;

/** Under the interval so a slightly-slow tick never yields a silent gap. */
const LOCK_MARGIN_MS = 250;

export async function buildHistoryPoint(): Promise<MetricsHistoryPointData> {
  const [archivedMatches, trackedPlayers, knownPlayers, queues, cache] = await Promise.all([
    countArchivedMatches(),
    countTrackedPlayers(),
    countPlayers(),
    queueCounts(),
    cacheCounts(),
  ]);

  const summed = { active: 0, pending: 0, failed: 0 };
  for (const q of Object.values(queues)) {
    summed.active += q.active;
    summed.pending += q.waiting + q.prioritized + q.delayed;
    summed.failed += q.failed;
  }

  return {
    t: Date.now(),
    totals: { archivedMatches, trackedPlayers, knownPlayers },
    queues: summed,
    cache,
  };
}

export class MetricsHistoryRecorder {
  private timer?: NodeJS.Timeout;

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.tick(), config.METRICS_HISTORY_INTERVAL_S * 1000);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
  }

  /** Public so tests drive ticks directly instead of faking timers. */
  async tick(): Promise<void> {
    // Fire-and-forget like the broadcaster: a failed point is a log line and a
    // gap in a chart, never a crashed api.
    try {
      // Same election as the broadcaster's tick: concurrent api instances pick
      // one recorder per interval, and the list stays one timeline.
      const ttl = Math.max(1, config.METRICS_HISTORY_INTERVAL_S * 1000 - LOCK_MARGIN_MS);
      const won = await redis.set(METRICS_HISTORY_LOCK_KEY, '1', 'PX', ttl, 'NX');
      if (won === null) return;

      const point = await buildHistoryPoint();
      // The expiry is the retention span, refreshed on every write — a
      // deployment that stops recording leaves no immortal list behind.
      const retainMs = config.METRICS_HISTORY_INTERVAL_S * 1000 * METRICS_HISTORY_MAX_POINTS;
      await redis
        .multi()
        .rpush(METRICS_HISTORY_KEY, JSON.stringify(point))
        .ltrim(METRICS_HISTORY_KEY, -METRICS_HISTORY_MAX_POINTS, -1)
        .pexpire(METRICS_HISTORY_KEY, retainMs)
        .exec();
    } catch (err) {
      logger.warn({ err }, 'metrics history point failed');
    }
  }
}

export const metricsHistory = new MetricsHistoryRecorder();

/** Oldest first, as stored. A point that does not parse is dropped, not fatal. */
export async function readMetricsHistory(): Promise<MetricsHistoryPointData[]> {
  const raw = await redis.lrange(METRICS_HISTORY_KEY, 0, -1);
  const points: MetricsHistoryPointData[] = [];
  for (const item of raw) {
    try {
      points.push(JSON.parse(item) as MetricsHistoryPointData);
    } catch {
      // skip — a corrupt entry must not take the whole history down
    }
  }
  return points;
}

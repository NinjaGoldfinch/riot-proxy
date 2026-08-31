import { config, KEY_SCOPE } from '../config.js';
import { METRICS_TOPIC, publish } from '../events/index.js';
import { logger } from '../logger.js';
import { redis } from '../redis.js';
import { wsHub } from '../ws/index.js';
import { buildMetricsSnapshot } from './snapshot.js';

/**
 * The `metrics` topic's clock, running in the api process — the worker cannot
 * see the hub's per-process gauges, and a BullMQ repeatable at seconds cadence
 * would be thousands of jobs a day of queue churn for a timer.
 *
 * Two gates keep it honest:
 *
 *  - No local subscriber, no work. The snapshot's row counts and queue reads
 *    are only spent while a dashboard is actually open.
 *  - A short `SET NX` lock in Redis, so concurrent api instances elect one
 *    publisher per tick. The frame still reaches every instance's subscribers —
 *    it travels the normal pub/sub relay — and the deployment this repo ships
 *    runs one api anyway; the lock is what keeps that an implementation detail.
 */
// Key-scoped like every other Redis key (§7.4): a dev server and a test run
// sharing one Redis are different deployments, and must not starve each
// other's ticks by contending for one lock.
export const METRICS_LOCK_KEY = `metrics:tick:lock:${KEY_SCOPE}`;

/** Under the interval so a slightly-slow tick never yields a silent gap. */
const LOCK_MARGIN_MS = 250;

export class MetricsBroadcaster {
  private timer?: NodeJS.Timeout;

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.tick(), config.METRICS_INTERVAL_S * 1000);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
  }

  /** Public so tests drive ticks directly instead of faking timers. */
  async tick(): Promise<void> {
    // Fire-and-forget like `publish` itself: a failed snapshot is a log line,
    // never a crashed api.
    try {
      if (wsHub.countSubscribers(METRICS_TOPIC) === 0) return;

      const ttl = Math.max(1, config.METRICS_INTERVAL_S * 1000 - LOCK_MARGIN_MS);
      const won = await redis.set(METRICS_LOCK_KEY, '1', 'PX', ttl, 'NX');
      if (won === null) return;

      const snapshot = await buildMetricsSnapshot({
        wsConnections: wsHub.size,
        wsSubscriptions: wsHub.subscriptionCount,
        wsEventCounts: wsHub.eventCounts,
      });
      await publish('metrics.snapshot', METRICS_TOPIC, snapshot);
    } catch (err) {
      logger.warn({ err }, 'metrics snapshot failed');
    }
  }
}

export const metricsBroadcaster = new MetricsBroadcaster();

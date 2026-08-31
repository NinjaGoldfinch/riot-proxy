import { KEY_SCOPE } from '../config.js';
import { redis } from '../redis.js';

/**
 * Whether anything is consuming the queues (#80).
 *
 * Queue counts are read straight from Redis and say nothing about the worker:
 * a crashed one and an idle one both report all-zero `active`, and the
 * difference only becomes visible later, when `waiting`/`delayed` have piled
 * up. The repeatable schedulers hide it further — `upsertJobScheduler` state
 * lives in Redis, so the tick jobs keep appearing whether or not anything runs
 * them.
 *
 * So the worker says so itself: one key it re-`SET`s with a TTL longer than the
 * interval, whose `PTTL` the snapshot reads. Nothing is stored in it — its
 * existence is the whole signal — which is why this needs no clock agreement
 * between the two processes, only Redis'.
 */
const HEARTBEAT_KEY = `wrk:hb:${KEY_SCOPE}`;

export const HEARTBEAT_INTERVAL_MS = 15_000;

/**
 * Three intervals, not two. A worker is single-threaded and spends its time in
 * job handlers, so one beat can be late for reasons that are not death — a GC
 * pause, a burst of archive writes. Two would flap; three costs 45 s of notice
 * for a signal nobody watches by the second.
 */
export const HEARTBEAT_TTL_MS = HEARTBEAT_INTERVAL_MS * 3;

export async function beat(): Promise<void> {
  await redis.set(HEARTBEAT_KEY, '1', 'PX', HEARTBEAT_TTL_MS);
}

export interface WorkerLiveness {
  alive: boolean;
  /** Milliseconds since the last beat, or `null` when there is none to date. */
  lastSeenMs: number | null;
}

/**
 * Derived from the key's remaining TTL rather than a stored timestamp: the age
 * of a beat is `TTL − PTTL` however far the two processes' clocks have drifted.
 */
export async function workerLiveness(): Promise<WorkerLiveness> {
  const pttl = await redis.pttl(HEARTBEAT_KEY);
  if (pttl <= 0) return { alive: false, lastSeenMs: null };
  return { alive: true, lastSeenMs: Math.max(0, HEARTBEAT_TTL_MS - pttl) };
}

/**
 * Beat now, then on the interval. Unref'd: the queues' own consumers are what
 * keep the worker process alive, and a heartbeat that could hold it open past
 * its shutdown would be lying about the very thing it reports.
 */
export function startHeartbeat(): NodeJS.Timeout {
  void beat();
  const timer = setInterval(() => void beat(), HEARTBEAT_INTERVAL_MS);
  timer.unref();
  return timer;
}

/**
 * Stop beating and drop the key, so a worker that was shut down deliberately
 * reads as gone at once rather than after the TTL — the operator watching the
 * dot is usually the person who just stopped it.
 */
export async function stopHeartbeat(timer: NodeJS.Timeout | undefined): Promise<void> {
  if (timer) clearInterval(timer);
  await redis.del(HEARTBEAT_KEY).catch(() => undefined);
}

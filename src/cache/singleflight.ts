import type { Redis } from 'ioredis';
import { config } from '../config.js';
import { redis as defaultRedis } from '../redis.js';
import { singleFlightKey } from './keys.js';

/** §8.4 — losers poll the cache key rather than duplicating the upstream call. */
const POLL_INTERVAL_MS = 25;
const POLL_BUDGET_MS = 2000;

export interface SingleFlightOptions<T> {
  /** Called by the winner; its result is shared with in-process waiters. */
  work: () => Promise<T>;
  /**
   * Called by cross-instance losers while they poll. Returning a value ends
   * the wait; returning undefined keeps polling until the budget expires.
   */
  peek: () => Promise<T | undefined>;
  lockMs?: number;
  pollBudgetMs?: number;
}

export interface SingleFlightResult<T> {
  value: T;
  /** True when this caller did the upstream work rather than sharing someone else's. */
  didWork: boolean;
}

/**
 * Two layers of coalescing (§8.4):
 *
 *  1. In-process — a `Map<cacheKey, Promise>` so concurrent identical misses in
 *     the same Node process await one promise. This is the layer that turns
 *     "100 concurrent requests" into exactly one upstream call.
 *  2. Cross-instance — `SET sf:{key} 1 NX PX`, with losers polling the cache
 *     key before falling through to their own upstream call. Best-effort:
 *     correctness never depends on winning the lock.
 */
export class SingleFlight {
  private readonly inflight = new Map<string, Promise<unknown>>();
  private readonly redis: Redis;

  constructor(redis: Redis = defaultRedis) {
    this.redis = redis;
  }

  get inflightCount(): number {
    return this.inflight.size;
  }

  async run<T>(key: string, opts: SingleFlightOptions<T>): Promise<SingleFlightResult<T>> {
    const existing = this.inflight.get(key);
    if (existing) {
      return { value: (await existing) as T, didWork: false };
    }

    const promise = this.execute(key, opts);
    this.inflight.set(key, promise);
    try {
      const value = await promise;
      return { value, didWork: true };
    } finally {
      this.inflight.delete(key);
    }
  }

  private async execute<T>(key: string, opts: SingleFlightOptions<T>): Promise<T> {
    const lockKey = singleFlightKey(key);
    const lockMs = opts.lockMs ?? config.SF_LOCK_MS;

    const won = await this.redis.set(lockKey, '1', 'PX', lockMs, 'NX');
    if (won === 'OK') {
      try {
        return await opts.work();
      } finally {
        // Release early so the next miss is not stuck behind a stale lock.
        await this.redis.del(lockKey).catch(() => undefined);
      }
    }

    // Lost the cross-instance race: poll for the winner's cache write.
    const budget = opts.pollBudgetMs ?? POLL_BUDGET_MS;
    const deadline = Date.now() + budget;
    while (Date.now() < deadline) {
      await sleep(POLL_INTERVAL_MS);
      const peeked = await opts.peek();
      if (peeked !== undefined) return peeked;
    }

    // The winner died or is slower than our budget — do the work ourselves
    // rather than failing the request.
    return opts.work();
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const singleFlight = new SingleFlight();

import type { Redis } from 'ioredis';
import { config } from '../config.js';
import { logger } from '../logger.js';
import { redis as defaultRedis } from '../redis.js';
import { scopedPurgePattern } from './keys.js';

/**
 * §8.5 — every payload carries a soft TTL (freshness) and a hard TTL
 * (`soft × 4`). Between the two the value is served with `X-Cache: STALE`
 * while a background refresh runs. Redis expiry is set to the hard TTL.
 */
export const STALE_MULTIPLIER = 4;

/** Immutable payloads (matches, timelines) never expire out of Redis. */
export const NO_EXPIRY = Infinity;

/** Redis caps EXPIRE at ~68 years; anything beyond a month is effectively forever. */
const MAX_TTL_SECONDS = 30 * 24 * 3600;

interface Envelope<T> {
  /** value */
  v: T;
  /** created-at, epoch ms */
  a: number;
  /** soft expiry, epoch ms; Infinity is serialised as null */
  s: number | null;
}

export type CacheState = 'HIT' | 'MISS' | 'HIT-NEG' | 'STALE';

export interface CacheEntry<T> {
  value: T;
  /** Seconds since the entry was written. */
  ageSeconds: number;
  /** True when past the soft TTL but inside the hard TTL. */
  stale: boolean;
}

export class CacheStore {
  private readonly redis: Redis;

  constructor(redis: Redis = defaultRedis) {
    this.redis = redis;
  }

  async get<T>(key: string): Promise<CacheEntry<T> | undefined> {
    const raw = await this.redis.get(key);
    if (raw === null) return undefined;

    let env: Envelope<T>;
    try {
      env = JSON.parse(raw) as Envelope<T>;
    } catch {
      // A malformed entry is worse than a miss; drop it.
      await this.redis.del(key).catch(() => undefined);
      return undefined;
    }

    const now = Date.now();
    return {
      value: env.v,
      ageSeconds: Math.max(0, Math.round((now - env.a) / 1000)),
      stale: env.s !== null && now > env.s,
    };
  }

  /**
   * @param ttlSeconds soft TTL. Hard TTL (Redis expiry) is `ttl × STALE_MULTIPLIER`
   *        when stale-while-revalidate is on, otherwise the soft TTL itself.
   */
  async set<T>(key: string, value: T, ttlSeconds: number): Promise<void> {
    const now = Date.now();
    const immutable = !Number.isFinite(ttlSeconds);

    const env: Envelope<T> = {
      v: value,
      a: now,
      s: immutable ? null : now + ttlSeconds * 1000,
    };
    const payload = JSON.stringify(env);

    if (immutable) {
      await this.redis.set(key, payload, 'EX', MAX_TTL_SECONDS);
      return;
    }

    const hardTtl = config.STALE_WHILE_REVALIDATE
      ? Math.ceil(ttlSeconds * STALE_MULTIPLIER)
      : Math.ceil(ttlSeconds);
    await this.redis.set(key, payload, 'EX', Math.max(1, Math.min(hardTtl, MAX_TTL_SECONDS)));
  }

  /** §8.3 — mark an upstream 404 so the next identical lookup costs no quota. */
  async setNegative(key: string, ttlSeconds: number): Promise<void> {
    if (ttlSeconds <= 0) return;
    await this.redis.set(key, '404', 'EX', Math.ceil(ttlSeconds));
  }

  async getNegative(key: string): Promise<boolean> {
    return (await this.redis.exists(key)) === 1;
  }

  async del(...keys: string[]): Promise<number> {
    if (keys.length === 0) return 0;
    return this.redis.del(...keys);
  }

  /**
   * Admin purge (§6.2). SCAN rather than KEYS so a large namespace does not
   * block Redis for other callers.
   */
  async purge(pattern: string): Promise<number> {
    const match = scopedPurgePattern(pattern);
    let cursor = '0';
    let deleted = 0;
    do {
      const [next, keys] = await this.redis.scan(cursor, 'MATCH', match, 'COUNT', 500);
      cursor = next;
      if (keys.length > 0) deleted += await this.redis.del(...keys);
    } while (cursor !== '0');
    logger.info({ pattern: match, deleted }, 'cache purged');
    return deleted;
  }
}

export const cacheStore = new CacheStore();

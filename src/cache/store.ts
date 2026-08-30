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
  /**
   * When this *content* was first seen, epoch ms — not when it was last
   * written. See `set`. Staleness is decided by `s`, so the two never fight.
   */
  a: number;
  /** soft expiry, epoch ms; Infinity is serialised as null */
  s: number | null;
}

export type CacheState = 'HIT' | 'MISS' | 'HIT-NEG' | 'STALE';

export interface CacheEntry<T> {
  value: T;
  /** Seconds since this payload last differed from what came before it. */
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
   *
   * A re-fetch that comes back byte-identical keeps the timestamp the content
   * was first seen with. `X-Cache-Age` then answers "how old is this data",
   * which is what it claims to, rather than "how long since we last asked" —
   * so re-reading a player who has not played since cannot be mistaken for
   * news. Expiry is driven by `s` and the Redis TTL, both set from `now`, so
   * holding `a` back never keeps an entry alive or makes it stale early.
   */
  async set<T>(key: string, value: T, ttlSeconds: number): Promise<void> {
    const now = Date.now();
    const immutable = !Number.isFinite(ttlSeconds);
    const firstSeen = (await this.unchangedSince(key, value)) ?? now;

    const env: Envelope<T> = {
      v: value,
      a: firstSeen,
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

  /**
   * The timestamp to carry forward, or undefined when this really is new data.
   * A miss, a malformed entry or a serialisation difference all read as new,
   * which is the safe direction: the worst case is a label that moves when it
   * did not have to.
   */
  private async unchangedSince<T>(key: string, value: T): Promise<number | undefined> {
    try {
      const raw = await this.redis.get(key);
      if (raw === null) return undefined;
      const env = JSON.parse(raw) as Envelope<T>;
      return JSON.stringify(env.v) === JSON.stringify(value) ? env.a : undefined;
    } catch {
      return undefined;
    }
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

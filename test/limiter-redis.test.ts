import './helpers/env.js';
import { Redis } from 'ioredis';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { KEY_SCOPE } from '../src/config.js';
import { registry } from '../src/metrics.js';
import { RateLimitBudgetExceeded, RateLimiter } from '../src/riot/limiter.js';

/**
 * The Lua acquisition path (§9.2) can only be tested against a real Redis —
 * that atomicity is the whole point. Skipped when no Redis is reachable so the
 * unit suite still runs anywhere.
 */
const url = process.env['REDIS_URL'] ?? 'redis://localhost:6379';
let available = false;
let redis: Redis;

beforeAll(async () => {
  redis = new Redis(url, { lazyConnect: true, maxRetriesPerRequest: 1, retryStrategy: () => null });
  try {
    await redis.connect();
    await redis.ping();
    available = true;
  } catch {
    available = false;
  }
});

afterAll(async () => {
  if (available) await redis.quit();
});

const SCOPE = 'test-region';

async function clearScope() {
  const keys = await redis.keys(`rl:*${KEY_SCOPE}:${SCOPE}*`);
  if (keys.length > 0) await redis.del(...keys);
}

beforeEach(async () => {
  if (available) await clearScope();
});

describe.runIf(process.env['SKIP_REDIS_TESTS'] !== '1')('rate limiter against Redis (§9)', () => {
  it('admits exactly `limit` requests inside one window', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '5:10');
    limiter.clearLocalConfig();

    let admitted = 0;
    let refused = 0;
    for (let i = 0; i < 8; i += 1) {
      try {
        await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
        admitted += 1;
      } catch (err) {
        expect(err).toBeInstanceOf(RateLimitBudgetExceeded);
        refused += 1;
      }
    }

    expect(admitted).toBe(5);
    expect(refused).toBe(3);
  });

  it('never over-commits a bucket under concurrency', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '10:10');
    limiter.clearLocalConfig();

    const results = await Promise.allSettled(
      Array.from({ length: 50 }, () => limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 })),
    );
    const admitted = results.filter((r) => r.status === 'fulfilled').length;

    // The rollback path in the Lua script means a partial acquisition can never
    // leak a token, so the count is exact rather than approximate.
    expect(admitted).toBe(10);
    expect(await redis.get(`rl:app:${KEY_SCOPE}:${SCOPE}:10:10`)).toBe('10');
  });

  it('requires a token from every window before dispatch (§9.2)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    // A tight short window inside a generous long one: the short one binds.
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '2:10,100:120');
    limiter.clearLocalConfig();

    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    await expect(limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 })).rejects.toBeInstanceOf(
      RateLimitBudgetExceeded,
    );

    // The refused attempt must not have consumed from the wider window.
    expect(await redis.get(`rl:app:${KEY_SCOPE}:${SCOPE}:100:120`)).toBe('2');
  });

  it('also enforces method buckets on top of app buckets', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:10');
    await redis.set(`rl:cfg:m:${KEY_SCOPE}:${SCOPE}:narrow`, '3:10');
    limiter.clearLocalConfig();

    let admitted = 0;
    for (let i = 0; i < 6; i += 1) {
      try {
        await limiter.acquire(SCOPE, 'narrow', { waitBudgetMs: 0 });
        admitted += 1;
      } catch {
        /* expected once the method bucket is exhausted */
      }
    }
    expect(admitted).toBe(3);
    // A different method on the same scope is unaffected.
    await expect(limiter.acquire(SCOPE, 'other', { waitBudgetMs: 0 })).resolves.toBeDefined();
  });

  it('syncs Riot counts without moving the window boundary (§9.1)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:120');
    limiter.clearLocalConfig();

    // One acquisition opens the window and arms its expiry.
    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    const key = `rl:app:${KEY_SCOPE}:${SCOPE}:100:120`;
    await redis.pexpire(key, 30_000); // pretend the window is most of the way through

    await limiter.observeHeaders(SCOPE, 'm', {
      'x-app-rate-limit': '100:120',
      'x-app-rate-limit-count': '47:120',
    });

    expect(Number(await redis.get(key))).toBe(47);
    // A plain SET would have cleared the expiry and re-armed the full 120 s,
    // decoupling our window from Riot's.
    const pttl = await redis.pttl(key);
    expect(pttl).toBeGreaterThan(0);
    expect(pttl).toBeLessThanOrEqual(30_000);
  });

  it('freezes the whole scope after a 429 with Retry-After (§9.4)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:10');
    limiter.clearLocalConfig();

    const metric = registry.getSingleMetric('proxy_rl_429_total');
    const count429 = async () =>
      (await metric?.get())?.values.reduce((sum, v) => sum + v.value, 0) ?? 0;
    const before = await count429();

    await limiter.freeze(SCOPE, 2, 'application');
    expect(await limiter.isFrozen(SCOPE)).toBeGreaterThan(0);
    // Freezing is a policy action, not an observation — the client owns the
    // counter, and counting here too would double every accountable 429.
    expect(await count429()).toBe(before);
    await expect(limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 })).rejects.toBeInstanceOf(
      RateLimitBudgetExceeded,
    );

    await redis.del(`rl:frozen:${KEY_SCOPE}:${SCOPE}`);
    await expect(limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 })).resolves.toBeDefined();
  });

  it('holds bulk work back at the usage ceiling while interactive still passes (§9.3)', async ({
    skip,
  }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '10:10');
    limiter.clearLocalConfig();

    // Consume 80 % of the bucket, past the 0.75 default ceiling.
    for (let i = 0; i < 8; i += 1) {
      await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    }

    await expect(
      limiter.acquire(SCOPE, 'm', { priority: 'bulk', waitBudgetMs: 0 }),
    ).rejects.toBeInstanceOf(RateLimitBudgetExceeded);

    // Interactive traffic still gets the remaining tokens.
    await expect(limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 })).resolves.toBeDefined();
  });

  it('learns limits from response headers and absorbs external usage (§9.1)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);

    await limiter.observeHeaders(SCOPE, 'm', {
      'x-app-rate-limit': '20:1,100:120',
      'x-app-rate-limit-count': '18:1,40:120',
      'x-method-rate-limit': '50:10',
      'x-method-rate-limit-count': '5:10',
    });

    expect(await redis.get(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`)).toBe('20:1,100:120');
    expect(await redis.get(`rl:cfg:m:${KEY_SCOPE}:${SCOPE}:m`)).toBe('50:10');
    // Riot says 18 of 20 used in the 1 s window — our counter must agree.
    expect(await redis.get(`rl:app:${KEY_SCOPE}:${SCOPE}:20:1`)).toBe('18');
    expect(await redis.get(`rl:app:${KEY_SCOPE}:${SCOPE}:100:120`)).toBe('40');

    limiter.clearLocalConfig();
    const usage = await limiter.usage(SCOPE);
    expect(usage).toContainEqual({ window: '20:1', used: 18, limit: 20 });
  });

  it('waits and succeeds once a window rolls over', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '1:1');
    limiter.clearLocalConfig();

    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    const result = await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 3000 });
    // It queued rather than failing — that is the §9.2 "else queue" behaviour.
    expect(result.waitedMs).toBeGreaterThan(0);
  });
});

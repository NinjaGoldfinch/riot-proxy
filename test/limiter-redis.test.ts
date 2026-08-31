import './helpers/env.js';
import { Redis } from 'ioredis';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { requireServices } from './helpers/services.js';
import { KEY_SCOPE } from '../src/config.js';
import { registry } from '../src/metrics.js';
import { RateLimitBudgetExceeded, RateLimiter, delay } from '../src/riot/limiter.js';

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
  requireServices(available, 'limiter-redis.test.ts');
});

afterAll(async () => {
  // The config writes here carry no TTL, and `beforeEach` only cleans on the
  // way *in* — without this, the last test's `test-region` config outlives the
  // suite forever and shows up in whatever else shares this Redis.
  if (available) await clearScope();
  if (available) await redis.quit();
});

/** The most admissions recorded inside any `ms`-long span. */
function worstBurst(stamps: number[], ms: number): number {
  let worst = 0;
  for (let i = 0; i < stamps.length; i += 1) {
    let n = 0;
    for (let j = i; j < stamps.length && stamps[j]! < stamps[i]! + ms; j += 1) n += 1;
    worst = Math.max(worst, n);
  }
  return worst;
}

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
    expect(await redis.zcard(`rl:app:v2:${KEY_SCOPE}:${SCOPE}:10:10`)).toBe(10);
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
    expect(await redis.zcard(`rl:app:v2:${KEY_SCOPE}:${SCOPE}:100:120`)).toBe(2);
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

  it('absorbs Riot counts without disturbing what we already admitted (§9.1)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:120');
    limiter.clearLocalConfig();

    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
    const key = `rl:app:v2:${KEY_SCOPE}:${SCOPE}:100:120`;
    const [, ourScore] = await redis.zrange(key, 0, '0', 'WITHSCORES');

    await limiter.observeHeaders(SCOPE, 'm', {
      'x-app-rate-limit': '100:120',
      'x-app-rate-limit-count': '47:120',
    });

    // Riot's higher count wins, padded out to match.
    expect(await redis.zcard(key)).toBe(47);
    // Each admission ages out on its own timestamp, so absorbing an external
    // count must not reschedule the one we made — under the counter this
    // replaced, that was a re-armed expiry sliding our window away from Riot's.
    const [, afterScore] = await redis.zrange(key, 0, '0', 'WITHSCORES');
    expect(afterScore).toBe(ourScore);
  });

  /**
   * §9.2 / #17 — the property Riot actually enforces: never more than `limit`
   * inside any rolling window, not merely inside one we chose ourselves.
   *
   * The counter this replaced pinned its expiry to our first request. Spend the
   * allowance just before that lapses and a fresh one arrives immediately after,
   * putting up to 2x the limit inside one of Riot's seconds — which is where the
   * accountable 429s in the Phase 2 gate came from.
   */
  it('never admits more than `limit` in any rolling window, boundary included (§9.2)', async ({
    skip,
  }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '5:1');
    limiter.clearLocalConfig();

    const admitted: number[] = [];
    const take = async () => {
      await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
      admitted.push(Date.now());
    };

    await take();
    const opened = Date.now();

    // Spend what is left of the allowance just before a fixed window would lapse.
    await delay(Math.max(0, opened + 820 - Date.now()));
    for (let i = 0; i < 4; i += 1) await take();

    // Just past the old boundary, where a counter would hand out a full allowance.
    await delay(Math.max(0, opened + 1060 - Date.now()));
    for (let i = 0; i < 5; i += 1) {
      try {
        await take();
      } catch {
        /* refusing here is the point */
      }
    }

    expect(worstBurst(admitted, 1000)).toBeLessThanOrEqual(5);
  }, 15_000);

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

  /**
   * The interactive waiter count is what makes bulk work stand aside (§9.3),
   * and it used to be a counter with a sliding TTL: `INCR`, then `EXPIRE 60`
   * re-armed by every later interactive request. A process killed between the
   * increment and its `finally` left the count one too high, and on a service
   * with interactive traffic more often than once a minute — the state in which
   * the leak happens — the key never expired, so every bulk acquisition on that
   * scope returned `interactive-queue-busy` for good. Nothing errored: polling,
   * backfill and archive writes for that platform simply stopped.
   */
  describe('announced interactive waiters (§9.3)', () => {
    const waiters = `rl:waiters:${KEY_SCOPE}:${SCOPE}`;

    it('holds bulk back while an interactive request is actually queueing', async ({ skip }) => {
      if (!available) return skip();
      const limiter = new RateLimiter(redis);
      await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '1:10');
      limiter.clearLocalConfig();

      // One interactive caller in flight, still waiting for the window.
      await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
      const queueing = limiter.acquire(SCOPE, 'm', { waitBudgetMs: 1500 });
      await delay(150);
      expect(await redis.zcard(waiters)).toBe(1);

      await expect(
        limiter.acquire(SCOPE, 'm', { priority: 'bulk', waitBudgetMs: 0 }),
      ).rejects.toBeInstanceOf(RateLimitBudgetExceeded);

      await queueing.catch(() => undefined);
      // And withdraws itself on the way out, however it left.
      expect(await redis.zcard(waiters)).toBe(0);
    }, 10_000);

    it('recovers from a waiter leaked by a killed process, under live traffic', async ({
      skip,
    }) => {
      if (!available) return skip();
      const limiter = new RateLimiter(redis);
      await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:10');
      limiter.clearLocalConfig();

      // A waiter whose process is gone: announced, never withdrawn. Its expiry
      // is a second from now, which is what a SIGKILL leaves behind.
      const now = Date.now();
      await redis.zadd(waiters, now + 1000, 'orphan-from-a-dead-pod');
      await expect(
        limiter.acquire(SCOPE, 'm', { priority: 'bulk', waitBudgetMs: 0 }),
      ).rejects.toBeInstanceOf(RateLimitBudgetExceeded);

      // Interactive traffic keeps arriving throughout, which is exactly what
      // used to keep the leaked count alive: each request re-armed the key's
      // TTL. Nothing here refreshes the orphan's own score.
      for (let i = 0; i < 4; i += 1) {
        await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });
        await delay(300);
      }

      await expect(
        limiter.acquire(SCOPE, 'm', { priority: 'bulk', waitBudgetMs: 0 }),
      ).resolves.toBeDefined();
      expect(await redis.zcard(waiters)).toBe(0);
    }, 10_000);

    it('does not count bulk callers as waiters, so they cannot block each other', async ({
      skip,
    }) => {
      if (!available) return skip();
      const limiter = new RateLimiter(redis);
      await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:10');
      limiter.clearLocalConfig();

      await limiter.acquire(SCOPE, 'm', { priority: 'bulk', waitBudgetMs: 0 });
      expect(await redis.zcard(waiters)).toBe(0);
    });
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
    // Riot says 18 of 20 used in the 1 s window — our bucket must agree.
    expect(await redis.zcard(`rl:app:v2:${KEY_SCOPE}:${SCOPE}:20:1`)).toBe(18);
    expect(await redis.zcard(`rl:app:v2:${KEY_SCOPE}:${SCOPE}:100:120`)).toBe(40);

    limiter.clearLocalConfig();
    const usage = await limiter.usage(SCOPE);
    expect(usage).toContainEqual({ window: '20:1', used: 18, limit: 20 });
  });

  it('pads with placeholders unique to each sync (§9.1)', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    const key = `rl:app:v2:${KEY_SCOPE}:${SCOPE}:100:120`;
    const headers = { 'x-app-rate-limit': '100:120', 'x-app-rate-limit-count': '3:120' };

    await limiter.observeHeaders(SCOPE, 'm', headers);
    const first = await redis.zrange(key, '0', '-1');
    expect(first).toHaveLength(3);

    // Drop one placeholder, as an acquisition rollback would, then re-sync. The
    // bucket has to climb back to Riot's count, which it cannot do if the second
    // sync picks names the first one already used — same-millisecond syncs would
    // otherwise leave us admitting requests Riot has already counted.
    await redis.zrem(key, first[0]!);
    const second = await redis.zrange(key, '0', '-1');
    expect(second).toHaveLength(2);

    await limiter.observeHeaders(SCOPE, 'm', headers);
    const third = await redis.zrange(key, '0', '-1');
    expect(third).toHaveLength(3);
    // `riot:<token>:<n>` — two syncs padded, so two distinct tokens.
    expect(new Set(third.map((m) => m.split(':')[1])).size).toBe(2);
  });

  /**
   * A development key's real app limits are exactly `BOOTSTRAP_APP_LIMITS`, and
   * `storeConfig` used to skip its write whenever the header matched the local
   * cache — a cache `windowsFor` had just primed with those same bootstrap
   * values. So the app config was never persisted, and the scope never appeared
   * in `knownScopes()`: a dev deployment's dashboard showed no limiter scopes
   * at all, however much traffic it had sent.
   */
  it('persists app limits that equal the bootstrap fallback', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    limiter.clearLocalConfig();

    // No stored config yet: this primes the local cache with the bootstrap
    // windows, exactly the state a fresh dev deployment is in.
    await limiter.acquire(SCOPE, 'm', { waitBudgetMs: 0 });

    await limiter.observeHeaders(SCOPE, 'm', {
      'x-app-rate-limit': '20:1,100:120',
      'x-app-rate-limit-count': '2:1,2:120',
    });

    expect(await redis.get(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`)).toBe('20:1,100:120');
    expect(await limiter.knownScopes()).toContain(SCOPE);
  });

  it('derives known scopes from method configs too', async ({ skip }) => {
    if (!available) return skip();
    // The state older deployments are left in: method configs stored, no app
    // sibling. The scope must still be listed, with its methods.
    await redis.set(`rl:cfg:m:${KEY_SCOPE}:${SCOPE}:match.byId`, '2000:10');
    await redis.set(`rl:cfg:m:${KEY_SCOPE}:${SCOPE}:account.byPuuid`, '1000:60');

    const limiter = new RateLimiter(redis);
    expect(await limiter.knownScopes()).toContain(SCOPE);
    expect((await limiter.knownScopeMethods()).get(SCOPE)).toEqual([
      'account.byPuuid',
      'match.byId',
    ]);
  });

  it('reports per-method usage for the methods it knows', async ({ skip }) => {
    if (!available) return skip();
    const limiter = new RateLimiter(redis);
    await redis.set(`rl:cfg:app:${KEY_SCOPE}:${SCOPE}`, '100:10');
    await redis.set(`rl:cfg:m:${KEY_SCOPE}:${SCOPE}:narrow`, '3:10');
    limiter.clearLocalConfig();

    await limiter.acquire(SCOPE, 'narrow', { waitBudgetMs: 0 });
    await limiter.acquire(SCOPE, 'narrow', { waitBudgetMs: 0 });

    expect(await limiter.methodUsage(SCOPE, ['narrow'])).toEqual([
      { method: 'narrow', windows: [{ window: '3:10', used: 2, limit: 3 }] },
    ]);
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

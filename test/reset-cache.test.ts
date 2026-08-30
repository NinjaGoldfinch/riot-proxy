import './helpers/env.js';
import { Redis } from 'ioredis';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { KEY_SCOPE } from '../src/config.js';
import { runCli } from './helpers/cli.js';

/**
 * `reset:cache` deletes keys, and `--all` is a FLUSHDB, so pointing it at the
 * Redis the developer is actually using would make `npm test` destroy their
 * warm cache. Everything here runs against **logical database 15** instead,
 * which nothing else in this repo touches.
 *
 * Skipped when no Redis is reachable, like the other Redis-backed suites.
 */
const TEST_DB = 15;

function testUrl(): string {
  const url = new URL(process.env['REDIS_URL'] ?? 'redis://localhost:6379');
  url.pathname = `/${TEST_DB}`;
  return url.toString();
}

const REDIS_URL = testUrl();
const FOREIGN = 'ffffffff';

let redis: Redis;
let available = false;

/** Cache-shaped keys this deployment owns — the three the default run claims. */
const OWNED = [`c:${KEY_SCOPE}:account:x`, `neg:${KEY_SCOPE}:summoner:y`, `sf:c:${KEY_SCOPE}:z`];

/** Everything the default run must leave behind. */
const LIMITER = `rl:app:v2:${KEY_SCOPE}:europe`;
const FOREIGN_KEYS = [`c:${FOREIGN}:account:x`, `rl:app:v2:${FOREIGN}:europe`];
const INFRASTRUCTURE = ['bull:archive:meta', `auth:${KEY_SCOPE}:somehash`];

beforeAll(async () => {
  redis = new Redis(REDIS_URL, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    retryStrategy: () => null,
  });
  try {
    await redis.connect();
    await redis.ping();
    available = true;
  } catch {
    available = false;
  }
});

afterAll(async () => {
  if (available) {
    await redis.flushdb();
    await redis.quit();
  }
});

beforeEach(async () => {
  if (!available) return;
  await redis.flushdb();
  const all = [...OWNED, LIMITER, ...FOREIGN_KEYS, ...INFRASTRUCTURE];
  await Promise.all(all.map((key) => redis.set(key, '1')));
});

async function exists(keys: string[]): Promise<boolean[]> {
  return Promise.all(keys.map(async (k) => (await redis.exists(k)) === 1));
}

const run = (args: string[] = [], overrides: Record<string, string> = {}) =>
  runCli('reset-cache', args, { REDIS_URL, ...overrides });

describe.runIf(process.env['SKIP_REDIS_TESTS'] !== '1')('reset:cache', () => {
  it('deletes this scope’s cache and nothing else', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run();
    expect(code).toBe(0);
    expect(stdout).toContain(KEY_SCOPE);

    expect(await exists(OWNED)).toEqual([false, false, false]);
    // The limiter's learned buckets, the queues and the auth cache are not
    // cache: dropping them costs correctness or work, not just a re-fetch.
    expect(await exists([LIMITER, ...INFRASTRUCTURE])).toEqual([true, true, true]);
  });

  /**
   * §7.4 — the whole point of KEY_SCOPE. A Redis shared with another proxy, or
   * still holding the previous Riot key's namespace, has to come out intact.
   */
  it('leaves another key scope alone', async ({ skip }) => {
    if (!available) return skip();
    await run();
    expect(await exists(FOREIGN_KEYS)).toEqual([true, true]);
  });

  it('takes the limiter buckets only when asked, and only its own', async ({ skip }) => {
    if (!available) return skip();
    const { code } = await run(['--limiter']);
    expect(code).toBe(0);
    expect(await exists([LIMITER])).toEqual([false]);
    // `rl:*<scope>*` has to stay anchored on the scope, not match every rl: key.
    expect(await exists([`rl:app:v2:${FOREIGN}:europe`])).toEqual([true]);
  });

  it('--all flushes every scope', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run(['--all']);
    expect(code).toBe(0);
    expect(stdout).toContain('every key, every scope');
    expect(await redis.dbsize()).toBe(0);
  });

  it('refuses under NODE_ENV=production and deletes nothing', async ({ skip }) => {
    if (!available) return skip();
    const { code, stderr } = await run([], { NODE_ENV: 'production' });
    expect(code).toBe(1);
    expect(stderr).toContain('Refusing to reset cache');
    expect(await exists(OWNED)).toEqual([true, true, true]);
  });

  it('--help explains itself without touching anything', async ({ skip }) => {
    if (!available) return skip();
    const { code, stdout } = await run(['--help']);
    expect(code).toBe(0);
    expect(stdout).toContain('Usage: npm run reset:cache');
    expect(await exists(OWNED)).toEqual([true, true, true]);
  });
});

import './helpers/env.js';
import { beforeEach, describe, expect, it } from 'vitest';
import { CacheStore, STALE_MULTIPLIER } from '../src/cache/store.js';
import { FakeRedis } from './helpers/fake-redis.js';

const redis = new FakeRedis();
const store = new CacheStore(redis as never);

beforeEach(() => redis.reset());

describe('cache store (§8.2, §8.5)', () => {
  it('round-trips a value and reports its age', async () => {
    await store.set('k', { hello: 'world' }, 60);
    const entry = await store.get<{ hello: string }>('k');
    expect(entry?.value).toEqual({ hello: 'world' });
    expect(entry?.stale).toBe(false);
    expect(entry?.ageSeconds).toBe(0);
  });

  it('returns undefined for a key that was never written', async () => {
    expect(await store.get('missing')).toBeUndefined();
  });

  it('marks an entry stale past the soft TTL but still serves it (§8.5)', async () => {
    await store.set('k', 'v', 0.05); // 50 ms soft TTL
    await new Promise((r) => setTimeout(r, 80));
    const entry = await store.get<string>('k');
    expect(entry?.value).toBe('v');
    expect(entry?.stale).toBe(true);
  });

  it('sets the hard TTL to soft × 4 so stale reads remain possible', async () => {
    await store.set('k', 'v', 10);
    const ttl = await redis.ttl('k');
    expect(ttl).toBeGreaterThan(10);
    expect(ttl).toBeLessThanOrEqual(10 * STALE_MULTIPLIER);
  });

  it('never expires immutable entries and never marks them stale', async () => {
    await store.set('match', { id: 1 }, Infinity);
    const entry = await store.get<{ id: number }>('match');
    expect(entry?.stale).toBe(false);
    expect(await redis.ttl('match')).toBeGreaterThan(86_400);
  });

  it('drops a corrupted entry rather than serving garbage', async () => {
    await redis.set('k', 'not json');
    expect(await store.get('k')).toBeUndefined();
    expect(await redis.get('k')).toBeNull();
  });

  it('negative-caches 404s under a TTL and ignores a zero TTL (§8.3)', async () => {
    await store.setNegative('neg:a', 30);
    expect(await store.getNegative('neg:a')).toBe(true);

    await store.setNegative('neg:b', 0);
    expect(await store.getNegative('neg:b')).toBe(false);
  });

  it('purges by pattern', async () => {
    await store.set('c:scope:summoner.byPuuid:h:1', 1, 60);
    await store.set('c:scope:summoner.byPuuid:h:2', 2, 60);
    await store.set('c:scope:league.entriesByPuuid:h:3', 3, 60);

    const deleted = await store.purge('c:scope:summoner.byPuuid:*');
    expect(deleted).toBe(2);
    expect(await store.get('c:scope:league.entriesByPuuid:h:3')).toBeDefined();
  });
});

/**
 * A refresh that returns what we already had has not updated anything. If the
 * timestamp moved anyway, `X-Cache-Age` would answer "how long since we last
 * asked" while claiming to answer "how old is this data" — and every label
 * built on it would announce news that did not happen.
 */
describe('unchanged payloads keep their age (§8.2)', () => {
  it('holds the timestamp still when the value is re-written identically', async () => {
    await store.set('k', { lp: 64 }, 60);
    await new Promise((r) => setTimeout(r, 60));
    await store.set('k', { lp: 64 }, 60);

    const entry = await store.get<{ lp: number }>('k');
    expect(entry?.ageSeconds).toBe(0); // rounds to 0s, but the epoch is the old one
    expect(await ageMs('k')).toBeGreaterThanOrEqual(50);
  });

  it('moves the timestamp as soon as the content differs', async () => {
    await store.set('k', { lp: 64 }, 60);
    await new Promise((r) => setTimeout(r, 60));
    await store.set('k', { lp: 71 }, 60);

    expect(await ageMs('k')).toBeLessThan(40);
  });

  it('still refreshes the expiry, so a held timestamp cannot strand an entry', async () => {
    await store.set('k', 'v', 0.05);
    await new Promise((r) => setTimeout(r, 80));
    expect((await store.get<string>('k'))?.stale).toBe(true);

    // Same value, so the age is held — but the soft TTL has to start again or
    // the entry would be permanently stale and re-fetched on every read.
    await store.set('k', 'v', 60);
    expect((await store.get<string>('k'))?.stale).toBe(false);
  });

  it('treats a malformed entry as new data rather than trusting it', async () => {
    await redis.set('k', 'not json', 'EX', 60);
    await store.set('k', 'v', 60);
    expect((await store.get<string>('k'))?.value).toBe('v');
  });
});

/** The envelope's epoch, which `ageSeconds` rounds away at this resolution. */
async function ageMs(key: string): Promise<number> {
  const raw = await redis.get(key);
  const env = JSON.parse(raw as string) as { a: number };
  return Date.now() - env.a;
}

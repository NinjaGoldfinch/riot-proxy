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

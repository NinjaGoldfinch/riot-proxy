import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CacheStore } from '../src/cache/store.js';
import { SingleFlight } from '../src/cache/singleflight.js';
import { Fetcher } from '../src/fetcher.js';
import { ProxyError, RiotError } from '../src/errors.js';
import { build } from '../src/riot/endpoints.js';
import { RateLimitBudgetExceeded } from '../src/riot/limiter.js';
import type { RiotClient } from '../src/riot/client.js';
import { FakeRedis } from './helpers/fake-redis.js';

/**
 * Exercises the read-path composition of §3.2 in isolation: cache → negative
 * cache → single-flight → upstream → cache write.
 */

const redis = new FakeRedis();

function makeFetcher(respond: () => Promise<unknown>) {
  const calls = { n: 0 };
  const client = {
    request: vi.fn(async () => {
      calls.n += 1;
      return { data: await respond(), status: 200, headers: {}, upstreamMs: 1 };
    }),
  } as unknown as RiotClient;

  const store = new CacheStore(redis as never);
  const flight = new SingleFlight(redis as never);
  return { fetcher: new Fetcher(client, store, flight), calls, store, client };
}

// A non-immutable endpoint, so the Postgres archive path is never consulted.
const REQ = build.summonerByPuuid('euw1', 'PUUID-TEST');
const SPECTATOR = build.activeGame('euw1', 'PUUID-TEST');

beforeEach(() => redis.reset());

describe('fetcher read path (§3.2)', () => {
  it('reports MISS then HIT, and only calls upstream once', async () => {
    const { fetcher, calls } = makeFetcher(async () => ({ level: 1 }));

    const first = await fetcher.fetch(REQ);
    expect(first.cache).toBe('MISS');
    expect(first.data).toEqual({ level: 1 });

    const second = await fetcher.fetch(REQ);
    expect(second.cache).toBe('HIT');
    expect(second.data).toEqual({ level: 1 });
    expect(calls.n).toBe(1);
  });

  it('coalesces concurrent misses into exactly one upstream call (§8.4)', async () => {
    const { fetcher, calls } = makeFetcher(async () => {
      await new Promise((r) => setTimeout(r, 20));
      return { level: 2 };
    });

    const results = await Promise.all(Array.from({ length: 100 }, () => fetcher.fetch(REQ)));

    expect(calls.n).toBe(1);
    expect(results.every((r) => r.data && (r.data as { level: number }).level === 2)).toBe(true);
  });

  it('negative-caches a 404 so the second lookup costs no quota (§8.3)', async () => {
    const client = {
      request: vi.fn(async () => {
        throw new RiotError(404, 'spectator.activeGame', 'euw1.api.riotgames.com', 'not found');
      }),
    } as unknown as RiotClient;
    const fetcher = new Fetcher(
      client,
      new CacheStore(redis as never),
      new SingleFlight(redis as never),
    );

    await expect(fetcher.fetch(SPECTATOR)).rejects.toMatchObject({ code: 'NOT_FOUND' });
    await expect(fetcher.fetch(SPECTATOR)).rejects.toMatchObject({ code: 'NOT_FOUND' });

    // The second call was answered from the negative cache, not the upstream.
    expect(client.request).toHaveBeenCalledTimes(1);
  });

  it('bypasses the cache when asked, but still writes through', async () => {
    let n = 0;
    const { fetcher, calls } = makeFetcher(async () => ({ n: (n += 1) }));

    await fetcher.fetch(REQ);
    const bypassed = await fetcher.fetch(REQ, { bypassCache: true });
    expect(calls.n).toBe(2);
    expect(bypassed.data).toEqual({ n: 2 });

    // The bypass refreshed the cache rather than leaving the old value.
    const after = await fetcher.fetch(REQ);
    expect(after.cache).toBe('HIT');
    expect(after.data).toEqual({ n: 2 });
  });

  it('serves the stale value when upstream 5xxs (§8.5)', async () => {
    let fail = false;
    const client = {
      request: vi.fn(async () => {
        if (fail) throw new RiotError(503, 'summoner.byPuuid', 'h', 'down');
        return { data: { level: 9 }, status: 200, headers: {}, upstreamMs: 1 };
      }),
    } as unknown as RiotClient;
    const store = new CacheStore(redis as never);
    const fetcher = new Fetcher(client, store, new SingleFlight(redis as never));

    await fetcher.fetch(REQ);
    fail = true;
    const result = await fetcher.fetch(REQ, { bypassCache: true });
    expect(result.data).toEqual({ level: 9 });
  });

  it('surfaces RATE_LIMITED with a retry hint when the wait budget is blown', async () => {
    const client = {
      request: vi.fn(async () => {
        throw new RateLimitBudgetExceeded(4200, 'frozen');
      }),
    } as unknown as RiotClient;
    const fetcher = new Fetcher(
      client,
      new CacheStore(redis as never),
      new SingleFlight(redis as never),
    );

    const err = (await fetcher.fetch(REQ).catch((e: unknown) => e)) as ProxyError;
    expect(err).toBeInstanceOf(ProxyError);
    expect(err.code).toBe('RATE_LIMITED');
    expect(err.retryAfter).toBe(5);
  });

  it('sanitises an upstream auth failure into UPSTREAM_ERROR (§12.2)', async () => {
    const client = {
      request: vi.fn(async () => {
        throw new RiotError(403, 'summoner.byPuuid', 'h', 'Forbidden: key revoked');
      }),
    } as unknown as RiotClient;
    const fetcher = new Fetcher(
      client,
      new CacheStore(redis as never),
      new SingleFlight(redis as never),
    );

    const err = (await fetcher.fetch(REQ).catch((e: unknown) => e)) as ProxyError;
    expect(err.code).toBe('UPSTREAM_ERROR');
    expect(err.statusCode).toBe(502);
    expect(err.message).not.toMatch(/revoked/);
  });
});

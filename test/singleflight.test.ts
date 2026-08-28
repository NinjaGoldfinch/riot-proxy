import './helpers/env.js';
import { beforeEach, describe, expect, it } from 'vitest';
import { SingleFlight } from '../src/cache/singleflight.js';
import { CacheStore } from '../src/cache/store.js';
import { FakeRedis } from './helpers/fake-redis.js';

const redis = new FakeRedis();

beforeEach(() => redis.reset());

describe('single-flight (§8.4)', () => {
  /** Phase 3 acceptance: 100 concurrent identical requests ⇒ exactly 1 upstream call. */
  it('coalesces 100 concurrent identical misses into one call', async () => {
    const flight = new SingleFlight(redis as never);
    let upstreamCalls = 0;

    const work = async () => {
      upstreamCalls += 1;
      await new Promise((r) => setTimeout(r, 25));
      return { value: 'payload' };
    };

    const results = await Promise.all(
      Array.from({ length: 100 }, () => flight.run('key', { work, peek: async () => undefined })),
    );

    expect(upstreamCalls).toBe(1);
    expect(results).toHaveLength(100);
    for (const r of results) expect(r.value).toEqual({ value: 'payload' });
    // Exactly one caller did the work; the other 99 shared it.
    expect(results.filter((r) => r.didWork)).toHaveLength(1);
  });

  it('releases the in-flight slot so a later request fetches again', async () => {
    const flight = new SingleFlight(redis as never);
    let calls = 0;
    const work = async () => {
      calls += 1;
      return calls;
    };

    await flight.run('key', { work, peek: async () => undefined });
    expect(flight.inflightCount).toBe(0);
    await flight.run('key', { work, peek: async () => undefined });
    expect(calls).toBe(2);
  });

  it('propagates failure to every waiter without leaving the slot occupied', async () => {
    const flight = new SingleFlight(redis as never);
    let calls = 0;
    const work = async () => {
      calls += 1;
      await new Promise((r) => setTimeout(r, 10));
      throw new Error('upstream exploded');
    };

    const settled = await Promise.allSettled(
      Array.from({ length: 10 }, () => flight.run('key', { work, peek: async () => undefined })),
    );

    expect(calls).toBe(1);
    expect(settled.every((s) => s.status === 'rejected')).toBe(true);
    expect(flight.inflightCount).toBe(0);
  });

  it("cross-instance losers read the winner's cache write instead of calling upstream", async () => {
    const store = new CacheStore(redis as never);
    const winner = new SingleFlight(redis as never);
    const loser = new SingleFlight(redis as never);

    let upstreamCalls = 0;
    const winnerWork = async () => {
      upstreamCalls += 1;
      await new Promise((r) => setTimeout(r, 50));
      await store.set('cachekey', { v: 1 }, 60);
      return { v: 1 };
    };

    const loserWork = async () => {
      upstreamCalls += 1;
      return { v: 2 };
    };

    const winnerPromise = winner.run('cachekey', {
      work: winnerWork,
      peek: async () => undefined,
    });
    // Let the winner take the Redis lock first.
    await new Promise((r) => setTimeout(r, 5));
    const loserPromise = loser.run('cachekey', {
      work: loserWork,
      peek: async () => (await store.get<{ v: number }>('cachekey'))?.value,
    });

    const [a, b] = await Promise.all([winnerPromise, loserPromise]);
    expect(upstreamCalls).toBe(1);
    expect(a.value).toEqual({ v: 1 });
    expect(b.value).toEqual({ v: 1 });
  });

  it('falls through to its own call when the winner never writes', async () => {
    const winner = new SingleFlight(redis as never);
    const loser = new SingleFlight(redis as never);
    let calls = 0;

    const stall = winner.run('k', {
      work: async () => {
        calls += 1;
        await new Promise((r) => setTimeout(r, 400));
        return 'winner';
      },
      peek: async () => undefined,
    });
    await new Promise((r) => setTimeout(r, 5));

    // A short poll budget makes the loser give up and do the work itself,
    // rather than failing the request.
    const result = await loser.run('k', {
      work: async () => {
        calls += 1;
        return 'loser';
      },
      peek: async () => undefined,
      pollBudgetMs: 100,
    });

    expect(result.value).toBe('loser');
    expect(calls).toBe(2);
    await stall;
  });
});

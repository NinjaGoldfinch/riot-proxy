import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProxyError } from '../src/errors.js';
import type { BuiltRequest, MethodId } from '../src/riot/endpoints.js';

/**
 * `poll:live` and `poll:rank` are where the event surface consumers actually
 * subscribe to is produced (§11). Both are transition detectors over a snapshot
 * held in Redis, so the things worth pinning are the edges: an event on a
 * change, silence on a repeat, and a first observation that establishes a
 * baseline rather than announcing itself as news.
 *
 * Redis is faked and every upstream call is stubbed, so this needs no services.
 */

const PUUID = 'P'.repeat(78);

vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

const published: { event: string; topic: string; data: Record<string, unknown> }[] = [];
vi.mock('../src/events/index.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/events/index.js')>();
  return {
    ...actual,
    publish: async (event: string, topic: string, data: Record<string, unknown>) => {
      published.push({ event, topic, data });
    },
  };
});

const nudges: { name: string; data: unknown; opts: Record<string, unknown> }[] = [];
vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    pollQueue: {
      add: async (name: string, data: unknown, opts: Record<string, unknown>) => {
        nudges.push({ name, data, opts });
        return { id: name };
      },
    },
  };
});

/** Per-method upstream replies, or a thrown error, for this test. */
let replies = new Map<MethodId, unknown | (() => never)>();

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        const reply = replies.get(req.method);
        if (typeof reply === 'function') (reply as () => never)();
        return { data: reply ?? null, cache: 'MISS' as const, ageSeconds: 0 };
      },
    },
  };
});

const { pollLive, pollRank } = await import('../src/jobs/processors.js');
const { JOB, pollDedupeId } = await import('../src/jobs/queues.js');
const { playerTopic } = await import('../src/events/index.js');
const { redis } = await import('../src/redis.js');
const { KEY_SCOPE } = await import('../src/config.js');

const liveKey = `state:live:${KEY_SCOPE}:${PUUID}`;
const rankKey = `state:rank:${KEY_SCOPE}:${PUUID}`;
const job = { data: { puuid: PUUID, platform: 'oc1' } } as never;

beforeEach(async () => {
  published.length = 0;
  nudges.length = 0;
  replies = new Map();
  await redis.del(liveKey, rankKey);
});

describe('poll:live transitions (§11)', () => {
  const game = (gameId: number) => ({
    gameId,
    gameQueueConfigId: 420,
    participants: [{ puuid: PUUID, championId: 64 }],
  });

  it('publishes game.started for a game id it has not seen', async () => {
    replies.set('spectator.activeGame', game(555));
    await pollLive(job);

    expect(published).toEqual([
      {
        event: 'game.started',
        topic: playerTopic(PUUID),
        data: { puuid: PUUID, platform: 'oc1', gameId: 555, championId: 64, queueId: 420 },
      },
    ]);
    expect(await redis.get(liveKey)).toBe('555');
  });

  it('says nothing on a tick that finds the same game still running', async () => {
    replies.set('spectator.activeGame', game(555));
    await pollLive(job);
    published.length = 0;

    await pollLive(job);
    expect(published).toEqual([]);
  });

  it('treats a 404 as "not in game" rather than a failure (§8.3)', async () => {
    replies.set('spectator.activeGame', () => {
      throw ProxyError.notFound('no active game');
    });

    await expect(pollLive(job)).resolves.toBeUndefined();
    expect(published).toEqual([]);
  });

  it('still fails the job on an upstream error that is not a 404', async () => {
    replies.set('spectator.activeGame', () => {
      throw ProxyError.upstream('spectator-v5 is down');
    });
    await expect(pollLive(job)).rejects.toMatchObject({ code: 'UPSTREAM_ERROR' });
  });

  it('publishes game.ended when a known game disappears, and nudges the match poll', async () => {
    replies.set('spectator.activeGame', game(555));
    await pollLive(job);
    published.length = 0;

    // Next tick: no active game.
    replies.set('spectator.activeGame', () => {
      throw ProxyError.notFound('no active game');
    });
    await pollLive(job);

    expect(published).toEqual([
      { event: 'game.ended', topic: playerTopic(PUUID), data: { puuid: PUUID, gameId: 555 } },
    ]);
    expect(await redis.get(liveKey)).toBeNull();

    // The match lands shortly after the game ends, so the poll is nudged rather
    // than left to its own tick — sharing the fan-out's de-duplication id, so a
    // nudge and a tick cannot both be pending.
    expect(nudges).toEqual([
      {
        name: JOB.pollMatches,
        data: { puuid: PUUID, platform: 'oc1' },
        opts: {
          delay: 60_000,
          deduplication: { id: pollDedupeId(JOB.pollMatches, PUUID) },
        },
      },
    ]);
  });

  it('does not announce an ending for a player it never saw start', async () => {
    replies.set('spectator.activeGame', () => {
      throw ProxyError.notFound('no active game');
    });
    await pollLive(job);

    expect(published).toEqual([]);
    expect(nudges).toEqual([]);
  });
});

describe('poll:rank diffing (§11)', () => {
  const solo = (lp: number) => [
    { queueType: 'RANKED_SOLO_5x5', tier: 'GOLD', rank: 'II', leaguePoints: lp },
  ];

  it('takes the first observation as a baseline, not as a change', async () => {
    replies.set('league.entriesByPuuid', solo(40));
    await pollRank(job);

    expect(published).toEqual([]);
    expect(await redis.get(rankKey)).toContain('RANKED_SOLO_5x5');
  });

  it('publishes rank.changed once LP actually moves', async () => {
    replies.set('league.entriesByPuuid', solo(40));
    await pollRank(job);

    replies.set('league.entriesByPuuid', solo(58));
    await pollRank(job);

    expect(published).toEqual([
      {
        event: 'rank.changed',
        topic: playerTopic(PUUID),
        data: {
          puuid: PUUID,
          queue: 'RANKED_SOLO_5x5',
          before: { tier: 'GOLD', rank: 'II', lp: 40 },
          after: { tier: 'GOLD', rank: 'II', lp: 58 },
        },
      },
    ]);
  });

  it('says nothing when a queue comes back identical', async () => {
    replies.set('league.entriesByPuuid', solo(40));
    await pollRank(job);
    await pollRank(job);
    expect(published).toEqual([]);
  });

  it('reports a queue appearing for the first time against a null before', async () => {
    replies.set('league.entriesByPuuid', solo(40));
    await pollRank(job);

    replies.set('league.entriesByPuuid', [
      ...solo(40),
      { queueType: 'RANKED_FLEX_SR', tier: 'SILVER', rank: 'I', leaguePoints: 12 },
    ]);
    await pollRank(job);

    expect(published).toHaveLength(1);
    expect(published[0]?.data).toMatchObject({ queue: 'RANKED_FLEX_SR', before: null });
  });

  it('publishes one event per queue that moved, not one per poll', async () => {
    replies.set('league.entriesByPuuid', [
      ...solo(40),
      { queueType: 'RANKED_FLEX_SR', tier: 'SILVER', rank: 'I', leaguePoints: 12 },
    ]);
    await pollRank(job);

    replies.set('league.entriesByPuuid', [
      ...solo(75),
      { queueType: 'RANKED_FLEX_SR', tier: 'SILVER', rank: 'I', leaguePoints: 99 },
    ]);
    await pollRank(job);

    expect(published.map((p) => p.data['queue'])).toEqual(['RANKED_SOLO_5x5', 'RANKED_FLEX_SR']);
  });

  it('ignores an entry with no queue type, and a corrupted previous snapshot', async () => {
    replies.set('league.entriesByPuuid', [{ tier: 'GOLD' }]);
    await pollRank(job);
    expect(await redis.get(rankKey)).toBe('{}');

    await redis.set(rankKey, 'not json');
    replies.set('league.entriesByPuuid', solo(40));
    await expect(pollRank(job)).resolves.toBeUndefined();
    expect(published).toEqual([]);
  });
});

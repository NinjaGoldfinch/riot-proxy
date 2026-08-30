import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest, MethodId } from '../src/riot/endpoints.js';

/**
 * The remaining processors, and the switch that reaches them.
 *
 * `dispatch` is the one place where every job's outcome is counted, so a name
 * that routes nowhere or a failure that counts as a completion would make
 * `proxy_jobs_total` — which §13 alerts on — quietly wrong. `archive:match`,
 * `ddragon:sync` and `maintenance` each have exactly one decision worth
 * pinning; they are here for the same reason.
 */

const PUUID = 'A'.repeat(78);

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

const archivedMatches: { matchId: string; region: string }[] = [];
const archivedTimelines: { matchId: string; region: string }[] = [];
vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return {
    ...actual,
    archiveMatch: async (matchId: string, region: string) => {
      archivedMatches.push({ matchId, region });
    },
    archiveTimeline: async (matchId: string, region: string) => {
      archivedTimelines.push({ matchId, region });
    },
  };
});

/** What each processor's upstream leg answers, or throws. */
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

let syncResult: { version: string; changed: boolean; files: string[] } = {
  version: '16.17.1',
  changed: false,
  files: [],
};
const syncCalls: { force?: boolean }[] = [];
vi.mock('../src/static/ddragon.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/static/ddragon.js')>();
  return {
    ...actual,
    syncDdragon: async (opts: { force?: boolean } = {}) => {
      syncCalls.push(opts);
      return syncResult;
    },
  };
});

// `listTrackedPlayers` is the only database read the tick path makes.
vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return { ...actual, listTrackedPlayers: async () => [] };
});

const { archiveMatchJob, ddragonSync, maintenance, dispatch } =
  await import('../src/jobs/processors.js');
const { JOB } = await import('../src/jobs/queues.js');
const { PATCH_TOPIC, playerTopic } = await import('../src/events/index.js');
const { redis } = await import('../src/redis.js');
const { registry } = await import('../src/metrics.js');

async function jobCount(job: string, status: string): Promise<number> {
  const metric = await registry.getSingleMetric('proxy_jobs_total')?.get();
  const match = metric?.values.find(
    (v) => v.labels['job'] === job && v.labels['status'] === status,
  );
  return match?.value ?? 0;
}

beforeEach(() => {
  published.length = 0;
  archivedMatches.length = 0;
  archivedTimelines.length = 0;
  syncCalls.length = 0;
  syncResult = { version: '16.17.1', changed: false, files: [] };
  replies = new Map();
  (redis as unknown as { reset: () => void }).reset();
});

describe('archive:match', () => {
  const matchJob = (data: Record<string, unknown>) => ({ data }) as never;

  it('derives the region from the match id rather than trusting the caller', async () => {
    replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
    await archiveMatchJob(matchJob({ matchId: 'OC1_1', puuid: PUUID }));

    // `OC1_` is an oc1 match, which match-v5 serves from the sea host.
    expect(archivedMatches).toEqual([{ matchId: 'OC1_1', region: 'sea' }]);
    expect(published).toEqual([
      {
        event: 'match.archived',
        topic: playerTopic(PUUID),
        data: { puuid: PUUID, matchId: 'OC1_1' },
      },
    ]);
  });

  it('refuses a match id it cannot place, rather than archiving it somewhere', async () => {
    await expect(archiveMatchJob(matchJob({ matchId: 'nonsense' }))).rejects.toThrow(
      /Cannot derive region/,
    );
    expect(archivedMatches).toEqual([]);
  });

  it('leaves the timeline alone unless the job asked for it', async () => {
    replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
    await archiveMatchJob(matchJob({ matchId: 'OC1_1' }));
    expect(archivedTimelines).toEqual([]);
  });

  it('archives a timeline when asked', async () => {
    replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
    replies.set('match.timeline', { info: { frames: [] } });
    await archiveMatchJob(matchJob({ matchId: 'OC1_1', fetchTimeline: true }));
    expect(archivedTimelines).toEqual([{ matchId: 'OC1_1', region: 'sea' }]);
  });

  it('never fails the archive over a timeline that would not come', async () => {
    replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
    replies.set('match.timeline', () => {
      throw new Error('timeline too large');
    });

    await expect(
      archiveMatchJob(matchJob({ matchId: 'OC1_1', fetchTimeline: true })),
    ).resolves.toBeUndefined();
    // The match is what mattered, and it is stored.
    expect(archivedMatches).toEqual([{ matchId: 'OC1_1', region: 'sea' }]);
    expect(archivedTimelines).toEqual([]);
  });
});

describe('ddragon:sync', () => {
  it('announces a patch only when the sync actually changed something', async () => {
    syncResult = { version: '16.18.1', changed: true, files: ['champion'] };
    await ddragonSync({ data: { force: false } } as never);

    expect(published).toEqual([
      { event: 'patch.new', topic: PATCH_TOPIC, data: { version: '16.18.1' } },
    ]);
  });

  it('stays quiet on the hourly tick that finds the same patch', async () => {
    syncResult = { version: '16.17.1', changed: false, files: [] };
    await ddragonSync({ data: {} } as never);
    expect(published).toEqual([]);
  });

  it('passes force through, and defaults it off', async () => {
    await ddragonSync({ data: { force: true } } as never);
    await ddragonSync({ data: {} } as never);
    expect(syncCalls).toEqual([{ force: true }, { force: false }]);
  });
});

describe('maintenance', () => {
  it('clears single-flight locks that outlived their expiry, and nothing else', async () => {
    // -1 (no expiry) is the leak: a lock whose PX never took, left by a crash.
    await redis.set('sf:c:scope:leaked', '1');
    await redis.set('sf:c:scope:held', '1', 'PX', 5000);
    await redis.set('c:scope:not-a-lock', '1');

    const result = await maintenance();

    expect(result).toEqual({ locksCleared: 1 });
    expect(await redis.get('sf:c:scope:leaked')).toBeNull();
    // A lock still inside its window belongs to a request in flight.
    expect(await redis.get('sf:c:scope:held')).toBe('1');
    expect(await redis.get('c:scope:not-a-lock')).toBe('1');
  });

  it('is a no-op on a namespace with no locks in it', async () => {
    expect(await maintenance()).toEqual({ locksCleared: 0 });
  });
});

describe('dispatch', () => {
  it('routes each job name to its processor', async () => {
    syncResult = { version: '16.17.1', changed: false, files: [] };
    await expect(dispatch({ name: JOB.ddragonSync, data: {} } as never)).resolves.toBeUndefined();
    expect(syncCalls).toHaveLength(1);

    await expect(dispatch({ name: JOB.maintenance, data: {} } as never)).resolves.toEqual({
      locksCleared: 0,
    });
  });

  it('reports a tick as the number of players it fanned out for', async () => {
    await expect(dispatch({ name: JOB.pollLiveTick, data: {} } as never)).resolves.toEqual({
      fannedOut: 0,
    });
  });

  it('refuses a job name it does not know', async () => {
    await expect(dispatch({ name: 'poll:everything', data: {} } as never)).rejects.toThrow(
      /Unknown job/,
    );
  });

  it('counts a completion, and counts a failure as a failure', async () => {
    const before = {
      completed: await jobCount(JOB.maintenance, 'completed'),
      failed: await jobCount('poll:everything', 'failed'),
    };

    await dispatch({ name: JOB.maintenance, data: {} } as never);
    await dispatch({ name: 'poll:everything', data: {} } as never).catch(() => undefined);

    expect(await jobCount(JOB.maintenance, 'completed')).toBe(before.completed + 1);
    // The re-throw is what makes BullMQ retry, so the count and the throw have
    // to happen together — counting a failure as a completion would hide it.
    expect(await jobCount('poll:everything', 'failed')).toBe(before.failed + 1);
  });
});

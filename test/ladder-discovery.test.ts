import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest } from '../src/riot/endpoints.js';
import type { BackfillPlayerJob } from '../src/jobs/queues.js';
import type { Player } from '../src/db/schema.js';

/**
 * Ladder entry → match history (#89), hermetic.
 *
 * The pipeline itself is old — `backfill:player` walks the ids,
 * `filterUnarchived` skips what is stored. What is new is the restraint around
 * the hand-off, and every part of it is here because without it a crawl does
 * something expensive and irreversible: signs thousands of players up for a
 * 60-second poll, re-walks the same ladder every crawl, or walks a player 100
 * matches deep and thereby marks their history "done" forever.
 *
 * The suite pins both backfill limits off, so this file turns them on before
 * anything reads the config.
 */
process.env['LADDER_BACKFILL_LIMIT'] = '100';
process.env['LADDER_BACKFILL_TIER_FLOOR'] = 'MASTER';
process.env['LOOKUP_BACKFILL_LIMIT'] = '10000';

const CRAWL_ID = '00000000-0000-4000-8000-000000000001';
/** Every crawl in this file started here; player stamps move around it. */
const CRAWL_STARTED = new Date('2026-09-01T00:00:00Z');

vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

let apexEntries: unknown[] = [];
/** One page for the paged walk; the second page is always empty. */
let pagedEntries: unknown[] = [];
/** Match-id pages the backfill walk reads, and the query it read them with. */
const idQueries: Record<string, unknown>[] = [];
let history: string[] = [];

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        if (req.method === 'league.entriesByTier') {
          const page = Number(req.query['page'] ?? 1);
          return {
            data: page === 1 ? pagedEntries : [],
            cache: 'MISS' as const,
            ageSeconds: 0,
          };
        }
        if (req.method === 'match.idsByPuuid') {
          idQueries.push(req.query);
          const start = Number(req.query['start'] ?? 0);
          const count = Number(req.query['count'] ?? 100);
          return {
            data: history.slice(start, start + count),
            cache: 'MISS' as const,
            ageSeconds: 0,
          };
        }
        return {
          data: { tier: 'CHALLENGER', entries: apexEntries },
          cache: 'MISS' as const,
          ageSeconds: 0,
        };
      },
    },
  };
});

let crawlStatus = 'running';
vi.mock('../src/db/ladder.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/ladder.js')>();
  return {
    ...actual,
    getCrawl: async () => ({ id: CRAWL_ID, status: crawlStatus, startedAt: CRAWL_STARTED }),
    finishCrawl: async () => undefined,
    bumpCrawlCounters: async (_id: string, by: Record<string, number>) => {
      for (const [k, v] of Object.entries(by)) counters[k] = (counters[k] ?? 0) + v;
    },
    upsertLeagueEntries: async () => 0,
  };
});

const counters: Record<string, number> = {};
/** What `upsertDiscoveredPlayers` was handed, and what it hands back. */
const discovered: { puuid: string; platform: string }[] = [];
/** Stamps the stored rows carry, by puuid. Absent means a player never walked. */
let walkedAt = new Map<string, Date>();
const completions: { puuid: string; depth: number }[] = [];

vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return {
    ...actual,
    upsertDiscoveredPlayers: async (rows: { puuid: string; platform: string }[]) => {
      discovered.push(...rows);
      return rows.map(
        (r) =>
          ({
            puuid: r.puuid,
            keyScope: 'test',
            platform: r.platform,
            gameName: null,
            tagLine: null,
            tracked: false,
            lastSeenMatchId: null,
            historyBackfillStartedAt: walkedAt.get(r.puuid) ?? null,
            historyBackfilledAt: null,
            historyBackfillDepth: null,
            updatedAt: new Date(),
          }) satisfies Player,
      );
    },
    markBackfillStarted: async () => {},
    markBackfillComplete: async (puuid: string, depth: number) => {
      completions.push({ puuid, depth });
    },
  };
});

vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return { ...actual, filterUnarchived: async (ids: string[]) => ids };
});

const enqueued: (BackfillPlayerJob & { priority?: number })[] = [];
vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    ladderQueue: { addBulk: async () => [] },
    archiveQueue: { addBulk: async () => [] },
    enqueueBackfill: async (data: BackfillPlayerJob) => {
      enqueued.push({ ...data, priority: actual.BACKFILL_PRIORITY[data.reason ?? 'admin'] });
      return { jobId: 'j', status: 'queued' as const };
    },
  };
});

const { ladderApex, ladderWalk, backfillPlayer } = await import('../src/jobs/processors.js');
const { BACKFILL_PRIORITY } = await import('../src/jobs/queues.js');
const configModule = await import('../src/config.js');
const { redis } = await import('../src/redis.js');

const entry = (puuid: string, over: Record<string, unknown> = {}) => ({
  puuid,
  rank: 'I',
  leaguePoints: 500,
  wins: 100,
  losses: 90,
  ...over,
});

const apexJob = (tier = 'CHALLENGER') =>
  ({
    data: { crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier },
    attemptsMade: 0,
    opts: { attempts: 3 },
    updateProgress: async () => {},
  }) as never;

const walkJob = (data: Record<string, unknown>) =>
  ({ data: { puuid: 'p', platform: 'oc1', ...data }, updateProgress: async () => {} }) as never;

const ladderWalkJob = (tier: string, division = 'I') =>
  ({
    data: { crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier, division },
    attemptsMade: 0,
    opts: { attempts: 3 },
    updateProgress: async () => {},
  }) as never;

/** `config` is read at call time, so a test can move one field and put it back. */
function withConfig(over: Record<string, unknown>, run: () => Promise<void>) {
  const target = configModule.config as unknown as Record<string, unknown>;
  const before: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(over)) {
    before[k] = target[k];
    target[k] = v;
  }
  return run().finally(() => {
    for (const [k, v] of Object.entries(before)) target[k] = v;
  });
}

beforeEach(() => {
  apexEntries = [];
  pagedEntries = [];
  idQueries.length = 0;
  discovered.length = 0;
  enqueued.length = 0;
  completions.length = 0;
  history = Array.from({ length: 500 }, (_, i) => `OC1_${1000 - i}`);
  walkedAt = new Map();
  crawlStatus = 'running';
  for (const k of Object.keys(counters)) delete counters[k];
  (redis as unknown as { reset: () => void }).reset();
});

describe('discovery', () => {
  it('upserts every eligible player and queues a walk apiece', async () => {
    apexEntries = [entry('chall-1'), entry('chall-2')];

    await ladderApex(apexJob());

    expect(discovered.map((d) => d.puuid)).toEqual(['chall-1', 'chall-2']);
    expect(enqueued).toHaveLength(2);
    expect(enqueued[0]).toMatchObject({
      puuid: 'chall-1',
      platform: 'euw1',
      limit: 100,
      reason: 'ladder',
    });
    expect(counters['playersDiscovered']).toBe(2);
    expect(counters['backfillsEnqueued']).toBe(2);
  });

  it('never asks for a player to be tracked', async () => {
    // The invariant that matters most: `tracked` means a 60-second spectator
    // poll, and a crawl turning it on for thousands of players would drown the
    // limiter. `upsertDiscoveredPlayers` cannot express it — this asserts the
    // caller does not try to smuggle it in another way.
    apexEntries = [entry('chall-1')];
    await ladderApex(apexJob());

    expect(discovered[0]).toEqual({ puuid: 'chall-1', platform: 'euw1' });
    expect(Object.keys(discovered[0]!)).toEqual(['puuid', 'platform']);
  });

  it('stops at the backfill tier floor, which is not the enumeration floor', async () => {
    // The floor here is MASTER. A crawl reaching further down still stores
    // those entries in `league_entries` — it just does not walk their
    // histories, which is the whole reason the two floors are separate knobs.
    pagedEntries = [entry('diamond-1'), entry('diamond-2')];
    await ladderWalk(ladderWalkJob('DIAMOND'));

    expect(discovered).toHaveLength(0);
    expect(enqueued).toHaveLength(0);
    // The entries themselves still went in, and the pages still counted.
    expect(counters['entriesSeen']).toBe(2);

    // The same page one tier up is walked.
    apexEntries = [entry('master-1')];
    await ladderApex(apexJob('MASTER'));
    expect(enqueued.map((e) => e.puuid)).toEqual(['master-1']);
  });

  it('skips a player this crawl has already walked', async () => {
    // Convergence, not thrash: a leg that retries, or a page that overlaps,
    // must not queue the same player twice inside one crawl.
    walkedAt.set('chall-1', new Date(CRAWL_STARTED.getTime() + 60_000));
    apexEntries = [entry('chall-1'), entry('chall-2')];

    await ladderApex(apexJob());

    expect(enqueued.map((e) => e.puuid)).toEqual(['chall-2']);
    // Still discovered — the ladder row and the player row are both current.
    expect(discovered).toHaveLength(2);
    expect(counters['playersDiscovered']).toBe(2);
    expect(counters['backfillsEnqueued']).toBe(1);
  });

  it('walks a player again on the next crawl', async () => {
    // The other half of the same rule. A walk from before this crawl started is
    // stale by definition, and `filterUnarchived` makes the repeat one request
    // plus whatever they have played since.
    walkedAt.set('chall-1', new Date(CRAWL_STARTED.getTime() - 86_400_000));
    apexEntries = [entry('chall-1')];

    await ladderApex(apexJob());
    expect(enqueued.map((e) => e.puuid)).toEqual(['chall-1']);
  });

  it('discovers without walking when the limit is 0', async () => {
    apexEntries = [entry('chall-1')];

    await withConfig({ LADDER_BACKFILL_LIMIT: 0 }, async () => {
      await ladderApex(apexJob());
    });

    // The cheapest useful mode: the ladder lands and the player row exists,
    // and the archive is left to the lookup path.
    expect(discovered.map((d) => d.puuid)).toEqual(['chall-1']);
    expect(enqueued).toHaveLength(0);
    expect(counters['playersDiscovered']).toBe(1);
    expect(counters['backfillsEnqueued']).toBe(0);
  });

  it('ranks the crawl’s walks beneath everything a person set off', async () => {
    apexEntries = [entry('chall-1')];
    await ladderApex(apexJob());

    expect(enqueued[0]?.priority).toBe(BACKFILL_PRIORITY.ladder);
    expect(BACKFILL_PRIORITY.ladder).toBeGreaterThan(BACKFILL_PRIORITY.lookup);
    // Every reason is ranked, because an unprioritized BullMQ job is popped
    // before every prioritized one — leaving `lookup` unranked would have put
    // it ahead of the band meant to outrank the crawl, not behind it.
    expect(Object.values(BACKFILL_PRIORITY).every((p) => p > 0)).toBe(true);
  });
});

describe('what a ladder walk fetches, and what it claims afterwards', () => {
  it('asks match-v5 only for the queue the crawl is about', async () => {
    apexEntries = [entry('chall-1')];
    await ladderApex(apexJob());

    await backfillPlayer(walkJob({ limit: 100, queueId: enqueued[0]?.queueId, reason: 'ladder' }));
    // 420 is ranked solo. The saving is in `match.byId` — one request per
    // match — not in the id page, which costs the same either way.
    expect(enqueued[0]?.queueId).toBe(420);
    expect(idQueries[0]).toMatchObject({ queue: 420 });
  });

  it('does not claim a player’s history from a hundred ranked games', async () => {
    // The regression this guards: `historyBackfilledAt` is what stops the first
    // lookup of a player walking their whole history. A ladder walk that set it
    // would lock every discovered player out of that walk permanently, on the
    // strength of 100 ranked games.
    const result = await backfillPlayer(walkJob({ limit: 100, queueId: 420, reason: 'ladder' }));

    expect(result.complete).toBe(false);
    expect(completions).toEqual([]);
  });

  it('does not claim it even when the ranked ids run out', async () => {
    // Running out of *ranked* ids says nothing about the rest of a history.
    history = ['OC1_1', 'OC1_2'];
    const result = await backfillPlayer(walkJob({ limit: 100, queueId: 420, reason: 'ladder' }));

    expect(result.depth).toBe(2);
    expect(result.complete).toBe(false);
  });

  it('claims it when an unfiltered walk reaches the end of a history', async () => {
    history = ['OC1_1', 'OC1_2'];
    const result = await backfillPlayer(walkJob({ limit: 500, reason: 'lookup' }));

    expect(result.complete).toBe(true);
    expect(completions).toEqual([{ puuid: 'p', depth: 2 }]);
  });

  it('claims it when the walk was allowed to go as deep as a lookup asks', async () => {
    history = Array.from({ length: 300 }, (_, i) => `OC1_${i}`);
    const result = await backfillPlayer(walkJob({ limit: 10_000, reason: 'admin' }));

    expect(result.complete).toBe(true);
  });

  it('does not claim it for a shallow admin walk either', async () => {
    // The same trap predates the ladder: `POST /v1/admin/backfill` defaults to
    // 500, and marking that complete blocked the 10 000-match lookup walk.
    history = Array.from({ length: 900 }, (_, i) => `OC1_${i}`);
    const result = await backfillPlayer(walkJob({ limit: 500, reason: 'admin' }));

    expect(result.depth).toBe(500);
    expect(result.complete).toBe(false);
    expect(completions).toEqual([]);
  });
});

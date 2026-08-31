import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest } from '../src/riot/endpoints.js';
import type { LeagueEntryInput } from '../src/db/ladder.js';

/**
 * The crawl (#88), hermetic: Riot, the ladder tables and the queue are all
 * stubbed, so nothing here needs a service and nothing reaches a worker.
 *
 * The three things worth pinning are the ones that cost real money to get
 * wrong. A walk that stops on a short page loses the rest of a division
 * silently. A walk that advances its cursor before storing a page loses 205
 * players to any crash. And a leg that never releases itself leaves the crawl
 * `running` forever — which, because one live crawl per ladder is a unique
 * index, blocks every future crawl of that ladder too.
 */

const CRAWL_ID = '00000000-0000-4000-8000-000000000001';

/** Pages keyed `TIER:DIVISION:page`; anything unlisted is an empty page. */
let pages = new Map<string, unknown[]>();
/** Apex payloads keyed by tier. */
let apexLeagues = new Map<string, unknown>();
/** Every request the crawl made, as `method path?page`. */
const requests: string[] = [];
/** Which fetch to refuse, 1-based, standing in for the limiter running dry. */
let failAtCall: number | null = null;
/** Refuse with something BullMQ will not retry past its attempt count. */
let failFatally = false;
let fetchCalls = 0;

vi.mock('../src/redis.js', async () => {
  const { FakeRedis } = await import('./helpers/fake-redis.js');
  const redis = new FakeRedis();
  return { redis, publisher: redis, subscriber: () => redis, closeRedis: async () => {} };
});

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  const { RateLimitBudgetExceeded } = await import('../src/riot/limiter.js');
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest, opts?: { priority?: string; waitBudgetMs?: number }) => {
        fetchOpts.push(opts ?? {});
        fetchCalls += 1;
        if (failAtCall !== null && fetchCalls >= failAtCall) {
          throw failFatally
            ? new Error('league-v4 fell over')
            : new RateLimitBudgetExceeded(1000, 'app');
        }
        if (req.method === 'league.entriesByTier') {
          const [, , , , , , tier, division] = req.path.split('/');
          const page = Number(req.query['page'] ?? 1);
          requests.push(`walk ${tier}/${division} p${page}`);
          return {
            data: pages.get(`${tier}:${division}:${page}`) ?? [],
            cache: 'MISS' as const,
            ageSeconds: 0,
          };
        }
        requests.push(`apex ${req.method}`);
        const tier = req.method.split('.')[1]!.toUpperCase();
        return {
          data: apexLeagues.get(tier) ?? { entries: [] },
          cache: 'MISS' as const,
          ageSeconds: 0,
        };
      },
    },
  };
});

const fetchOpts: { priority?: string; waitBudgetMs?: number }[] = [];

/** The crawl row, as the database would hold it. */
const CRAWL_STARTED = new Date('2026-09-01T00:00:00Z');
const baseCrawl = {
  id: CRAWL_ID,
  status: 'running',
  platform: 'euw1',
  queue: 'RANKED_SOLO_5x5',
  startedAt: CRAWL_STARTED,
  finishedAt: null as Date | null,
  entriesSeen: 1870,
  playersDiscovered: 50,
  pagesFetched: 3,
};
let crawlRow: typeof baseCrawl = { ...baseCrawl };
let createResult: { created: boolean } = { created: true };
const upserted: { crawlId: string; entries: LeagueEntryInput[] }[] = [];
const counters: Record<string, number> = {};
const finished: { id: string; status: string }[] = [];

vi.mock('../src/db/ladder.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/ladder.js')>();
  return {
    ...actual,
    createCrawl: async () => ({ crawl: crawlRow, created: createResult.created }),
    getCrawl: async () => crawlRow,
    finishCrawl: async (id: string, status: string) => {
      // The real one is guarded on `status = 'running'`; so is this, because
      // the completion path relies on exactly one caller winning.
      if (crawlRow.status !== 'running') return undefined;
      crawlRow = { ...crawlRow, status, finishedAt: new Date(CRAWL_STARTED.getTime() + 42_000) };
      finished.push({ id, status });
      return crawlRow;
    },
    bumpCrawlCounters: async (_id: string, by: Record<string, number>) => {
      for (const [k, v] of Object.entries(by)) counters[k] = (counters[k] ?? 0) + v;
    },
    upsertLeagueEntries: async (
      crawlId: string,
      _platform: string,
      _queue: string,
      entries: LeagueEntryInput[],
    ) => {
      upserted.push({ crawlId, entries });
      return entries.length;
    },
  };
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

/** Aggregation jobs a completed crawl queued. */
const aggregates: Record<string, unknown>[] = [];

/** Jobs the fan-out added, in the order it added them. */
const added: { name: string; data: Record<string, unknown>; opts: Record<string, unknown> }[] = [];
vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    ladderQueue: {
      addBulk: async (jobs: typeof added) => {
        added.push(...jobs);
        return jobs;
      },
    },
    maintenanceQueue: {
      add: async (_name: string, data: Record<string, unknown>) => {
        aggregates.push(data);
        return {};
      },
    },
  };
});

const { startCrawl, ladderApex, ladderWalk } = await import('../src/jobs/processors.js');
const { JOB, LADDER_PRIORITY, ladderLegId } = await import('../src/jobs/queues.js');
const { getCursor, setCursor, pendingLegs, trackLegs } =
  await import('../src/jobs/ladder-state.js');
const { redis } = await import('../src/redis.js');
const { RateLimitBudgetExceeded } = await import('../src/riot/limiter.js');

/** A job as the processors read it: data, and the retry bookkeeping. */
const job = (data: Record<string, unknown>, attemptsMade = 0, attempts = 3) =>
  ({ data, attemptsMade, opts: { attempts }, updateProgress: async () => {} }) as never;

const entryPage = (n: number, tier = 'DIAMOND') =>
  Array.from({ length: n }, (_, i) => ({
    puuid: `${tier}-${i}`,
    tier,
    rank: 'II',
    leaguePoints: i,
    wins: 10,
    losses: 5,
  }));

beforeEach(() => {
  pages = new Map();
  apexLeagues = new Map();
  requests.length = 0;
  added.length = 0;
  upserted.length = 0;
  finished.length = 0;
  fetchOpts.length = 0;
  failAtCall = null;
  failFatally = false;
  fetchCalls = 0;
  for (const k of Object.keys(counters)) delete counters[k];
  crawlRow = { ...baseCrawl };
  published.length = 0;
  aggregates.length = 0;
  createResult = { created: true };
  (redis as unknown as { reset: () => void }).reset();
});

describe('ladder:crawl — the fan-out', () => {
  it('creates one leg per apex league and one per (tier, division) below', async () => {
    const result = await startCrawl({
      platform: 'euw1',
      queue: 'RANKED_SOLO_5x5',
      tierFloor: 'EMERALD',
    });

    // EMERALD and DIAMOND paged (2 x 4 divisions) + three apex leagues.
    expect(result.created).toBe(true);
    expect(result.legs).toBe(11);
    expect(added).toHaveLength(11);

    const apex = added.filter((j) => j.name === JOB.ladderApex);
    expect(apex.map((j) => j.data['tier'])).toEqual(['MASTER', 'GRANDMASTER', 'CHALLENGER']);

    const walks = added.filter((j) => j.name === JOB.ladderWalk);
    expect(new Set(walks.map((j) => j.data['tier']))).toEqual(new Set(['EMERALD', 'DIAMOND']));
    expect(walks.filter((j) => j.data['tier'] === 'DIAMOND')).toHaveLength(4);
  });

  it('is 28 walk jobs for the whole ladder, not one per page', async () => {
    // The design decision this asserts: a full crawl is ~20 000 pages, and one
    // job per page would be a queue nobody can operate.
    const result = await startCrawl({
      platform: 'euw1',
      queue: 'RANKED_SOLO_5x5',
      tierFloor: 'IRON',
    });
    expect(added.filter((j) => j.name === JOB.ladderWalk)).toHaveLength(28);
    expect(result.legs).toBe(31);
  });

  it('takes the tier floor from config when the caller names none', async () => {
    // LADDER_TIER_FLOOR is pinned to MASTER in the test env, which is apex-only.
    const result = await startCrawl({ platform: 'euw1', queue: 'RANKED_SOLO_5x5' });
    expect(result.legs).toBe(3);
    expect(added.every((j) => j.name === JOB.ladderApex)).toBe(true);
  });

  it('gives every leg an explicit priority, apex ahead of the walks', async () => {
    await startCrawl({ platform: 'euw1', queue: 'RANKED_SOLO_5x5', tierFloor: 'DIAMOND' });
    // The BullMQ trap: an unprioritized job is popped before every prioritized
    // one, so a missing priority here would outrank the whole crawl.
    expect(added.every((j) => typeof j.opts['priority'] === 'number')).toBe(true);
    for (const j of added) {
      const expected = j.name === JOB.ladderApex ? LADDER_PRIORITY.apex : LADDER_PRIORITY.walk;
      expect(j.opts['priority']).toBe(expected);
    }
    expect(LADDER_PRIORITY.apex).toBeLessThan(LADDER_PRIORITY.walk);
  });

  it('de-duplicates on the leg id, so a leg cannot be queued twice', async () => {
    await startCrawl({ platform: 'euw1', queue: 'RANKED_SOLO_5x5', tierFloor: 'DIAMOND' });
    const ids = added.map((j) => (j.opts['deduplication'] as { id: string }).id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids).toContain(ladderLegId(JOB.ladderWalk, CRAWL_ID, 'DIAMOND', 'IV'));
  });

  it('records the legs before queueing them', async () => {
    // The other order is a race: a leg that ran and released itself before the
    // set knew about it would leave the crawl running with nothing to finish it.
    await startCrawl({ platform: 'euw1', queue: 'RANKED_SOLO_5x5', tierFloor: 'DIAMOND' });
    expect(await pendingLegs(CRAWL_ID)).toBe(7);
  });

  it('queues nothing when a crawl is already running, and names the live one', async () => {
    createResult = { created: false };
    const result = await startCrawl({
      platform: 'euw1',
      queue: 'RANKED_SOLO_5x5',
      tierFloor: 'IRON',
    });

    expect(result).toEqual({ crawlId: CRAWL_ID, created: false, legs: 0 });
    expect(added).toHaveLength(0);
    expect(await pendingLegs(CRAWL_ID)).toBe(0);
  });
});

describe('ladder:walk — paging', () => {
  const walkJob = (tier = 'DIAMOND', division = 'I', attemptsMade = 0) =>
    job(
      { crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier, division },
      attemptsMade,
    );

  beforeEach(async () => {
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderWalk, CRAWL_ID, 'DIAMOND', 'I')]);
  });

  it('pages until an empty page, not until a short one', async () => {
    pages.set('DIAMOND:I:1', entryPage(205));
    // Short, because the ladder churned under the walk — there is more behind it.
    pages.set('DIAMOND:I:2', entryPage(180));
    pages.set('DIAMOND:I:3', entryPage(205));
    // Page 4 is unset, i.e. empty: the only reliable terminator.

    const result = await ladderWalk(walkJob());

    expect(result).toEqual({ pages: 3, entries: 590, done: true });
    expect(requests).toEqual([
      'walk DIAMOND/I p1',
      'walk DIAMOND/I p2',
      'walk DIAMOND/I p3',
      'walk DIAMOND/I p4',
    ]);
  });

  it('resumes from the cursor rather than starting over', async () => {
    await setCursor(CRAWL_ID, 'DIAMOND', 'I', 400);
    pages.set('DIAMOND:I:400', entryPage(205));

    await ladderWalk(walkJob());

    expect(requests).toEqual(['walk DIAMOND/I p400', 'walk DIAMOND/I p401']);
  });

  it('advances the cursor one page at a time, after each page is stored', async () => {
    pages.set('DIAMOND:I:1', entryPage(205));
    pages.set('DIAMOND:I:2', entryPage(205));
    // A sibling leg, so the crawl does not finish and clear the cursors out
    // from under the assertion — which is itself the documented behaviour.
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderWalk, CRAWL_ID, 'DIAMOND', 'II')]);

    await ladderWalk(walkJob());
    expect(await getCursor(CRAWL_ID, 'DIAMOND', 'I')).toBe(3);
    expect(upserted).toHaveLength(2);
  });

  it('leaves the cursor where it was when a page is refused', async () => {
    pages.set('DIAMOND:I:1', entryPage(205));
    pages.set('DIAMOND:I:2', entryPage(205));
    await setCursor(CRAWL_ID, 'DIAMOND', 'I', 1);

    // First page lands, then the limiter refuses the second.
    failAtCall = 2;
    await expect(ladderWalk(walkJob())).rejects.toThrow(RateLimitBudgetExceeded);

    // Page 1 was stored and the cursor moved with it; page 2 was not, so the
    // retry asks for page 2 again rather than skipping it.
    expect(await getCursor(CRAWL_ID, 'DIAMOND', 'I')).toBe(2);
    expect(upserted).toHaveLength(1);
  });

  it('keeps the leg outstanding when a refusal will be retried', async () => {
    failAtCall = 1;
    await expect(ladderWalk(walkJob('DIAMOND', 'I', 0))).rejects.toThrow(RateLimitBudgetExceeded);

    // Attempt 1 of 3: BullMQ will hand this back, so the crawl is not over.
    expect(await pendingLegs(CRAWL_ID)).toBe(1);
    expect(finished).toHaveLength(0);
  });

  it('releases the leg when the last attempt fails, so the crawl cannot hang', async () => {
    failAtCall = 1;
    failFatally = true;
    // attemptsMade 2 of 3 attempts: nothing will retry this.
    await expect(ladderWalk(walkJob('DIAMOND', 'I', 2))).rejects.toThrow('fell over');

    expect(await pendingLegs(CRAWL_ID)).toBe(0);
    // And the crawl ends `failed`, not `completed` — it did not see the ladder.
    expect(finished).toEqual([{ id: CRAWL_ID, status: 'failed' }]);
  });

  it('stops when the crawl is no longer running', async () => {
    crawlRow = { ...baseCrawl, status: 'cancelled' };
    pages.set('DIAMOND:I:1', entryPage(205));

    const result = await ladderWalk(walkJob());
    expect(result.pages).toBe(0);
    expect(requests).toHaveLength(0);
  });

  it('counts pages and entries onto the crawl as it goes', async () => {
    pages.set('DIAMOND:I:1', entryPage(205));
    pages.set('DIAMOND:I:2', entryPage(100));
    await ladderWalk(walkJob());

    expect(counters['pagesFetched']).toBe(2);
    expect(counters['entriesSeen']).toBe(305);
  });

  it('waits far longer than a backfill would before giving up on a token', async () => {
    pages.set('DIAMOND:I:1', entryPage(1));
    await ladderWalk(walkJob());
    // Bulk, so it yields to interactive traffic; generous, because failing the
    // job means re-queueing to fetch the very same page.
    expect(fetchOpts[0]?.priority).toBe('bulk');
    expect(fetchOpts[0]?.waitBudgetMs).toBeGreaterThan(120_000);
  });
});

describe('ladder:apex — one request, one league', () => {
  const apexJob = (tier: string) =>
    job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier });

  beforeEach(async () => {
    await trackLegs(CRAWL_ID, [
      ladderLegId(JOB.ladderApex, CRAWL_ID, 'CHALLENGER'),
      ladderLegId(JOB.ladderApex, CRAWL_ID, 'MASTER'),
    ]);
  });

  it('takes the tier from the request, not from the entries', async () => {
    // The shape Riot actually returns: the wrapper carries the tier and every
    // entry omits it. Trusting the entry would write rows with no tier at all.
    apexLeagues.set('CHALLENGER', {
      tier: 'CHALLENGER',
      queue: 'RANKED_SOLO_5x5',
      entries: [
        { puuid: 'chall-1', rank: 'I', leaguePoints: 1500, wins: 300, losses: 200, veteran: true },
        { puuid: 'chall-2', rank: 'I', leaguePoints: 1200, wins: 150, losses: 100 },
      ],
    });

    const result = await ladderApex(apexJob('CHALLENGER'));

    expect(result).toEqual({ entries: 2 });
    expect(upserted[0]?.entries[0]).toMatchObject({
      puuid: 'chall-1',
      tier: 'CHALLENGER',
      division: 'I',
      leaguePoints: 1500,
      veteran: true,
    });
    expect(requests).toEqual(['apex league.challenger']);
  });

  it('drops an entry with no puuid rather than writing a row keyed on nothing', async () => {
    apexLeagues.set('MASTER', {
      tier: 'MASTER',
      entries: [
        { rank: 'I', leaguePoints: 10 },
        { puuid: 'master-1', rank: 'I' },
      ],
    });

    expect(await ladderApex(apexJob('MASTER'))).toEqual({ entries: 1 });
    expect(upserted[0]?.entries.map((e) => e.puuid)).toEqual(['master-1']);
  });
});

describe('completion', () => {
  it('is the last leg that finishes the crawl, and only the last', async () => {
    const legs = ['CHALLENGER', 'GRANDMASTER', 'MASTER'].map((t) =>
      ladderLegId(JOB.ladderApex, CRAWL_ID, t),
    );
    await trackLegs(CRAWL_ID, legs);

    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'CHALLENGER' }),
    );
    expect(finished).toHaveLength(0);
    expect(await pendingLegs(CRAWL_ID)).toBe(2);

    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'GRANDMASTER' }),
    );
    expect(finished).toHaveLength(0);

    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'MASTER' }),
    );
    expect(finished).toEqual([{ id: CRAWL_ID, status: 'completed' }]);
  });

  it('announces the finished crawl and queues its aggregate', async () => {
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderApex, CRAWL_ID, 'MASTER')]);
    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'MASTER' }),
    );

    expect(published).toHaveLength(1);
    expect(published[0]?.event).toBe('ladder.crawl.completed');
    // Admin-scoped, like `metrics`: these are numbers about what the proxy
    // spent its key on.
    expect(published[0]?.topic).toBe('ladder');
    expect(published[0]?.data).toEqual({
      crawlId: CRAWL_ID,
      platform: 'euw1',
      queue: 'RANKED_SOLO_5x5',
      entries: 1870,
      players: 50,
      durationS: 42,
    });

    // The archive only becomes readable once something reads it.
    expect(aggregates).toEqual([{ platform: 'euw1', queue: 'RANKED_SOLO_5x5' }]);
  });

  it('says nothing, and aggregates nothing, for a crawl that gave up', async () => {
    // Half a ladder aggregated as though it were a whole one is worse than
    // leaving the previous numbers in place.
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderWalk, CRAWL_ID, 'DIAMOND', 'I')]);
    failAtCall = 1;
    failFatally = true;
    await expect(
      ladderWalk(
        job(
          {
            crawlId: CRAWL_ID,
            platform: 'euw1',
            queue: 'RANKED_SOLO_5x5',
            tier: 'DIAMOND',
            division: 'I',
          },
          2,
        ),
      ),
    ).rejects.toThrow('fell over');

    expect(finished).toEqual([{ id: CRAWL_ID, status: 'failed' }]);
    expect(published).toHaveLength(0);
    expect(aggregates).toHaveLength(0);
  });

  it("clears the crawl's cursors on the way out", async () => {
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderApex, CRAWL_ID, 'MASTER')]);
    await setCursor(CRAWL_ID, 'DIAMOND', 'I', 12);

    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'MASTER' }),
    );

    expect(finished).toHaveLength(1);
    expect(await getCursor(CRAWL_ID, 'DIAMOND', 'I')).toBe(1);
  });

  it('does not re-finish a crawl a retrying sibling already ended', async () => {
    await trackLegs(CRAWL_ID, [ladderLegId(JOB.ladderApex, CRAWL_ID, 'MASTER')]);
    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'MASTER' }),
    );
    expect(finished).toHaveLength(1);

    // A duplicate delivery of the same leg: the set no longer holds it, so the
    // SREM removes nothing — but the set is also empty, so this caller reads
    // itself as last. `finishCrawl`'s status guard is what stops it.
    await ladderApex(
      job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier: 'MASTER' }),
    );
    expect(finished).toHaveLength(1);
  });
});

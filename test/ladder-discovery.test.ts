import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest } from '../src/riot/endpoints.js';
import type { Player } from '../src/db/schema.js';

/**
 * Ladder entry → match history (#89), hermetic, and staged.
 *
 * The pipeline itself is old — pages of match ids, `filterUnarchived`,
 * `archive:match`. What this file pins is the *order* it runs in, which is the
 * one thing that decides whether a crawl pays for a match once or ten times.
 *
 * A match has ten participants. A crawl that walked a player's history the
 * moment it discovered them would reach one game from ten walks spread across
 * the whole run, and every one of those walks that happened before the match
 * landed in Postgres would fetch it again — `filterUnarchived` can only skip
 * what is already there. So enumeration discovers, the collect stage gathers
 * every id into one set, and only when the last id is in does anything fetch a
 * match.
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
/** Match-id pages the walk reads, and the query it read them with. */
const idQueries: Record<string, unknown>[] = [];
/** Each player's history, newest first. Anyone unlisted has played nothing. */
let histories = new Map<string, string[]>();

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        if (req.method === 'league.entriesByTier') {
          const page = Number(req.query['page'] ?? 1);
          return { data: page === 1 ? pagedEntries : [], cache: 'MISS' as const, ageSeconds: 0 };
        }
        if (req.method === 'match.idsByPuuid') {
          idQueries.push(req.query);
          const puuid = decodeURIComponent(req.path.split('/').at(-2) ?? '');
          const history = histories.get(puuid) ?? [];
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

/** The crawl row, as the database would hold it while it runs. */
const baseCrawl = {
  id: CRAWL_ID,
  status: 'running',
  phase: 'enumerate',
  platform: 'euw1',
  queue: 'RANKED_SOLO_5x5',
  tierFloor: 'MASTER',
  startedAt: CRAWL_STARTED,
  finishedAt: null as Date | null,
  pagesFetched: 0,
  entriesSeen: 0,
  playersDiscovered: 0,
  backfillsEnqueued: 0,
  matchIdsSeen: 0,
  matchesQueued: 0,
};
let crawlRow: typeof baseCrawl = { ...baseCrawl };
const counters: Record<string, number> = {};
const finished: { id: string; status: string }[] = [];
/** Whom the candidate query has to offer, in order. */
let candidates: string[] = [];
const candidateQueries: Record<string, unknown>[] = [];

vi.mock('../src/db/ladder.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/ladder.js')>();
  return {
    ...actual,
    getCrawl: async () => crawlRow,
    finishCrawl: async (id: string, status: string) => {
      if (crawlRow.status !== 'running') return undefined;
      crawlRow = { ...crawlRow, status, finishedAt: new Date(CRAWL_STARTED.getTime() + 42_000) };
      finished.push({ id, status });
      return crawlRow;
    },
    // Guarded exactly like the real one: only the caller that finds the crawl
    // in the stage it is leaving gets to move it on.
    advanceCrawlPhase: async (_id: string, from: string, to: string) => {
      if (crawlRow.status !== 'running' || crawlRow.phase !== from) return undefined;
      crawlRow = { ...crawlRow, phase: to };
      return crawlRow;
    },
    // Keyset, like the real one: the caller hands back the last row it saw
    // rather than a count, so a page cannot shift under it.
    listCrawlBackfillCandidates: async (filter: Record<string, unknown>) => {
      candidateQueries.push(filter);
      const after = filter['after'] as { puuid: string } | undefined;
      const start = after ? candidates.indexOf(after.puuid) + 1 : 0;
      return candidates
        .slice(start, start + Number(filter['limit']))
        .map((puuid, i) => ({ puuid, leaguePoints: 1000 - (start + i) }));
    },
    bumpCrawlCounters: async (_id: string, by: Record<string, number>) => {
      for (const [k, v] of Object.entries(by)) counters[k] = (counters[k] ?? 0) + v;
    },
    upsertLeagueEntries: async () => 0,
  };
});

/** What `upsertDiscoveredPlayers` was handed. */
const discovered: { puuid: string; platform: string }[] = [];
const started: string[] = [];
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
            historyBackfillStartedAt: null,
            historyBackfilledAt: null,
            historyBackfillDepth: null,
            updatedAt: new Date(),
          }) satisfies Player,
      );
    },
    markBackfillStarted: async (puuid: string) => {
      started.push(puuid);
    },
    markBackfillComplete: async (puuid: string, depth: number) => {
      completions.push({ puuid, depth });
    },
  };
});

/** Matches the archive already holds, so the stage can be seen skipping them. */
let archived = new Set<string>();
vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return {
    ...actual,
    filterUnarchived: async (ids: string[]) => ids.filter((id) => !archived.has(id)),
  };
});

interface AddedJob {
  name: string;
  data: Record<string, unknown>;
  opts: Record<string, unknown>;
}
/** Jobs each queue was handed, in order. */
const ladderJobs: AddedJob[] = [];
const archiveJobs: AddedJob[] = [];
const aggregates: Record<string, unknown>[] = [];

vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    ladderQueue: {
      addBulk: async (jobs: AddedJob[]) => {
        ladderJobs.push(...jobs);
        return jobs;
      },
      add: async (name: string, data: Record<string, unknown>, opts: Record<string, unknown>) => {
        ladderJobs.push({ name, data, opts });
        return {};
      },
    },
    archiveQueue: {
      addBulk: async (jobs: AddedJob[]) => {
        archiveJobs.push(...jobs);
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

const { ladderApex, ladderWalk, ladderCollect, ladderArchive, backfillPlayer } =
  await import('../src/jobs/processors.js');
const { ARCHIVE_PRIORITY, JOB, LADDER_PRIORITY, ladderLegId } =
  await import('../src/jobs/queues.js');
const { trackLegs } = await import('../src/jobs/ladder-state.js');
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

const job = (data: Record<string, unknown>) =>
  ({
    data,
    attemptsMade: 0,
    opts: { attempts: 3 },
    updateProgress: async () => {},
  }) as never;

const apexJob = (tier = 'CHALLENGER') =>
  job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier });

const walkJob = (tier: string, division = 'I') =>
  job({ crawlId: CRAWL_ID, platform: 'euw1', queue: 'RANKED_SOLO_5x5', tier, division });

const backfillJob = (data: Record<string, unknown>) =>
  ({ data: { puuid: 'p', platform: 'oc1', ...data }, updateProgress: async () => {} }) as never;

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

/** The legs a crawl's fan-out would have registered, so one can end. */
const legsFor = (...legIds: string[]) => trackLegs(CRAWL_ID, legIds);

const apexLeg = (tier = 'CHALLENGER') => ladderLegId(JOB.ladderApex, CRAWL_ID, tier);

/**
 * Play the worker: run every ladder job that has been queued and not yet run,
 * including the ones a stage transition adds while this loop is running.
 */
async function drainLadder(): Promise<void> {
  for (let i = 0; i < ladderJobs.length; i += 1) {
    const queued = ladderJobs[i]!;
    if (queued.name === JOB.ladderCollect) await ladderCollect(job(queued.data));
    else if (queued.name === JOB.ladderArchive) await ladderArchive(job(queued.data));
  }
}

beforeEach(() => {
  apexEntries = [];
  pagedEntries = [];
  idQueries.length = 0;
  discovered.length = 0;
  started.length = 0;
  completions.length = 0;
  ladderJobs.length = 0;
  archiveJobs.length = 0;
  aggregates.length = 0;
  candidates = [];
  candidateQueries.length = 0;
  finished.length = 0;
  histories = new Map();
  archived = new Set();
  crawlRow = { ...baseCrawl };
  for (const k of Object.keys(counters)) delete counters[k];
  (redis as unknown as { reset: () => void }).reset();
});

describe('enumeration', () => {
  it('records every eligible player and fetches not one match id', async () => {
    // The barrier, from the enumeration side. A page of the ladder produces
    // player rows and nothing else; the walk that costs requests happens after
    // the last leg, not on the page that discovered them.
    apexEntries = [entry('chall-1'), entry('chall-2')];
    await legsFor(apexLeg(), 'another-leg');

    await ladderApex(apexJob());

    expect(discovered.map((d) => d.puuid)).toEqual(['chall-1', 'chall-2']);
    expect(counters['playersDiscovered']).toBe(2);
    expect(idQueries).toHaveLength(0);
    expect(ladderJobs).toHaveLength(0);
  });

  it('never asks for a player to be tracked', async () => {
    // The invariant that matters most: `tracked` means a 60-second spectator
    // poll, and a crawl turning it on for thousands of players would drown the
    // limiter. `upsertDiscoveredPlayers` cannot express it — this asserts the
    // caller does not try to smuggle it in another way.
    apexEntries = [entry('chall-1')];
    await legsFor(apexLeg(), 'another-leg');
    await ladderApex(apexJob());

    expect(discovered[0]).toEqual({ puuid: 'chall-1', platform: 'euw1' });
    expect(Object.keys(discovered[0]!)).toEqual(['puuid', 'platform']);
  });

  it('stops at the backfill tier floor, which is not the enumeration floor', async () => {
    // The floor here is MASTER. A crawl reaching further down still stores
    // those entries in `league_entries` — it just does not walk their
    // histories, which is the whole reason the two floors are separate knobs.
    pagedEntries = [entry('diamond-1'), entry('diamond-2')];
    await legsFor(ladderLegId(JOB.ladderWalk, CRAWL_ID, 'DIAMOND', 'I'), 'another-leg');

    await ladderWalk(walkJob('DIAMOND'));

    expect(discovered).toHaveLength(0);
    // The entries themselves still went in, and the pages still counted.
    expect(counters['entriesSeen']).toBe(2);
  });
});

describe('the collect stage', () => {
  it('waits for the last leg of the enumeration before it starts', async () => {
    // Two legs outstanding. The first to finish must not start collecting:
    // the second is still discovering players whose matches overlap the
    // first's, and collecting them separately is what fetches a match twice.
    candidates = ['chall-1'];
    await legsFor(apexLeg('CHALLENGER'), apexLeg('MASTER'));

    apexEntries = [entry('chall-1')];
    await ladderApex(apexJob('CHALLENGER'));
    expect(crawlRow.phase).toBe('enumerate');
    expect(ladderJobs).toHaveLength(0);

    apexEntries = [entry('master-1')];
    await ladderApex(apexJob('MASTER'));
    expect(crawlRow.phase).toBe('collect');
    expect(ladderJobs.map((j) => j.name)).toEqual([JOB.ladderCollect]);
  });

  it('batches the candidates, and ranks them below the enumeration', async () => {
    candidates = Array.from({ length: 30 }, (_, i) => `p-${i}`);
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    const collect = ladderJobs.filter((j) => j.name === JOB.ladderCollect);
    expect(collect).toHaveLength(2); // 25 + 5
    expect(collect[0]?.data['offset']).toBe(0);
    expect(collect[1]?.data['offset']).toBe(25);
    expect((collect[0]?.data['puuids'] as string[]).length).toBe(25);
    expect((collect[1]?.data['puuids'] as string[]).length).toBe(5);
    expect(collect[0]?.opts['priority']).toBe(LADDER_PRIORITY.collect);
    expect(counters['backfillsEnqueued']).toBe(30);
  });

  it('asks only for players this crawl saw, and not the ones already walked', async () => {
    candidates = ['chall-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    expect(candidateQueries[0]).toMatchObject({
      crawlId: CRAWL_ID,
      platform: 'euw1',
      queue: 'RANKED_SOLO_5x5',
      // Convergence, not thrash: a player walked since this crawl started has
      // already produced their ids for it.
      notWalkedSince: CRAWL_STARTED,
    });
    expect(candidateQueries[0]?.['tiers']).toEqual(['MASTER', 'GRANDMASTER', 'CHALLENGER']);
  });

  it('gathers ids into one set and touches the archive queue not at all', async () => {
    histories.set('p-1', ['EUW1_1', 'EUW1_2']);
    candidates = ['p-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    const collect = ladderJobs.find((j) => j.name === JOB.ladderCollect)!;
    const result = await ladderCollect(job(collect.data));

    expect(result).toMatchObject({ players: 1, ids: 2, newIds: 2 });
    expect(started).toEqual(['p-1']);
    // 420 is ranked solo: the crawl pays for the ladder it is about rather
    // than the player's whole back-catalogue.
    expect(idQueries[0]).toMatchObject({ queue: 420 });
    expect(archiveJobs).toHaveLength(0);
  });

  it('counts a match shared by two players once', async () => {
    // The point of the whole arrangement. Two Challengers who played each
    // other produce the same match id, and the set collapses it — so the
    // archive stage pays for that game once rather than twice.
    histories.set('p-1', ['EUW1_1', 'EUW1_2']);
    histories.set('p-2', ['EUW1_2', 'EUW1_3']);
    candidates = ['p-1', 'p-2'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());
    await drainLadder();

    expect(counters['matchIdsSeen']).toBe(3);
    expect(archiveJobs.map((j) => j.data['matchId']).sort()).toEqual([
      'EUW1_1',
      'EUW1_2',
      'EUW1_3',
    ]);
  });
});

describe('the archive stage', () => {
  it('starts only once every collect job has finished', async () => {
    histories.set('p-1', ['EUW1_1']);
    histories.set('p-26', ['EUW1_2']);
    candidates = Array.from({ length: 30 }, (_, i) => (i === 0 ? 'p-1' : `p-${i + 25}`));
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    const collect = ladderJobs.filter((j) => j.name === JOB.ladderCollect);
    await ladderCollect(job(collect[0]!.data));
    // Half the ids are in. Fetching now is exactly what the stage exists to
    // prevent: the second batch may hold the other side of the same games.
    expect(crawlRow.phase).toBe('collect');
    expect(ladderJobs.some((j) => j.name === JOB.ladderArchive)).toBe(false);
    expect(archiveJobs).toHaveLength(0);

    await ladderCollect(job(collect[1]!.data));
    expect(crawlRow.phase).toBe('archive');
    await drainLadder();
    expect(archiveJobs).toHaveLength(2);
  });

  it('skips what the archive already holds, and ranks the rest last', async () => {
    histories.set('p-1', ['EUW1_1', 'EUW1_2']);
    archived.add('EUW1_1');
    candidates = ['p-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());
    await drainLadder();

    expect(archiveJobs.map((j) => j.data['matchId'])).toEqual(['EUW1_2']);
    expect(archiveJobs[0]?.opts['jobId']).toBe('archive-EUW1_2');
    // Below every depth a lookup walk can reach, so a crawl's forty thousand
    // matches never sit in front of the history somebody is watching fill in.
    expect(archiveJobs[0]?.opts['priority']).toBe(ARCHIVE_PRIORITY.ladder);
    expect(ARCHIVE_PRIORITY.ladder).toBeGreaterThan(ARCHIVE_PRIORITY.live);
    expect(counters['matchIdsSeen']).toBe(2);
    expect(counters['matchesQueued']).toBe(1);
  });

  it('finishes the crawl, announces it and queues the aggregate', async () => {
    histories.set('p-1', ['EUW1_1']);
    candidates = ['p-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());
    await drainLadder();

    expect(finished).toEqual([{ id: CRAWL_ID, status: 'completed' }]);
    expect(aggregates).toEqual([{ platform: 'euw1', queue: 'RANKED_SOLO_5x5' }]);
  });

  it('re-queues a batch it has already queued rather than dropping it', async () => {
    // The ids are removed only once their archive jobs exist, so a crash in
    // between re-reads the batch. Re-queueing costs nothing — the archive job
    // id is the match id — while a pop would lose the matches silently.
    histories.set('p-1', ['EUW1_1']);
    candidates = ['p-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    const collect = ladderJobs.find((j) => j.name === JOB.ladderCollect)!;
    await ladderCollect(job(collect.data));

    const archive = ladderJobs.find((j) => j.name === JOB.ladderArchive)!;
    expect(archive.opts['priority']).toBe(LADDER_PRIORITY.archive);

    await ladderArchive(job(archive.data));
    expect(archiveJobs).toHaveLength(1);
    // The retry BullMQ would give a job that died after queueing: it re-reads
    // a batch it has already queued, and the archive job id absorbs it.
    crawlRow = { ...crawlRow, status: 'running', phase: 'archive' };
    await ladderArchive(job(archive.data));
    expect(archiveJobs).toHaveLength(1);
  });
});

describe('a crawl with nothing to walk', () => {
  it('ends at enumeration when the backfill limit is 0', async () => {
    apexEntries = [entry('chall-1')];
    await legsFor(apexLeg());

    await withConfig({ LADDER_BACKFILL_LIMIT: 0 }, async () => {
      await ladderApex(apexJob());
    });

    // The cheapest useful mode: the ladder lands and the player row exists,
    // and the archive is left to the lookup path.
    expect(discovered.map((d) => d.puuid)).toEqual(['chall-1']);
    expect(ladderJobs).toHaveLength(0);
    expect(idQueries).toHaveLength(0);
    expect(finished).toEqual([{ id: CRAWL_ID, status: 'completed' }]);
  });

  it('ends when every player it saw has been walked since it started', async () => {
    candidates = [];
    await legsFor(apexLeg());
    await ladderApex(apexJob());

    // The fan-out holds a leg of its own precisely so this case releases it:
    // no collect jobs, straight through the empty archive stage, done.
    expect(ladderJobs.map((j) => j.name)).toEqual([JOB.ladderArchive]);
    await drainLadder();
    expect(finished).toEqual([{ id: CRAWL_ID, status: 'completed' }]);
  });
});

describe('what a ladder walk claims afterwards', () => {
  it('does not claim a player’s history from a hundred ranked games', async () => {
    // The regression this guards: `historyBackfilledAt` is what stops the first
    // lookup of a player walking their whole history. A ladder walk that set it
    // would lock every discovered player out of that walk permanently, on the
    // strength of 100 ranked games.
    histories.set(
      'p-1',
      Array.from({ length: 40 }, (_, i) => `EUW1_${i}`),
    );
    candidates = ['p-1'];
    await legsFor(apexLeg());
    await ladderApex(apexJob());
    await drainLadder();

    expect(completions).toEqual([]);
  });

  it('claims it when an unfiltered walk reaches the end of a history', async () => {
    histories.set('p', ['OC1_1', 'OC1_2']);
    const result = await backfillPlayer(backfillJob({ limit: 500, reason: 'lookup' }));

    expect(result.complete).toBe(true);
    expect(completions).toEqual([{ puuid: 'p', depth: 2 }]);
  });

  it('claims it when the walk was allowed to go as deep as a lookup asks', async () => {
    histories.set(
      'p',
      Array.from({ length: 300 }, (_, i) => `OC1_${i}`),
    );
    const result = await backfillPlayer(backfillJob({ limit: 10_000, reason: 'admin' }));

    expect(result.complete).toBe(true);
  });

  it('does not claim it for a shallow admin walk either', async () => {
    // The same trap predates the ladder: `POST /v1/admin/backfill` defaults to
    // 500, and marking that complete blocked the 10 000-match lookup walk.
    histories.set(
      'p',
      Array.from({ length: 900 }, (_, i) => `OC1_${i}`),
    );
    const result = await backfillPlayer(backfillJob({ limit: 500, reason: 'admin' }));

    expect(result.depth).toBe(500);
    expect(result.complete).toBe(false);
    expect(completions).toEqual([]);
  });
});

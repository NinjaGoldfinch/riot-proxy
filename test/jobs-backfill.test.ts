import './helpers/env.js';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest } from '../src/riot/endpoints.js';
import type { ArchiveMatchJob } from '../src/jobs/queues.js';

/**
 * `backfillPlayer` walks a player's whole history into the archive, and the two
 * stamps it writes are the only thing that stops the next lookup walking them
 * again (#44). That makes a partial walk writing a completion the failure to
 * guard against: it would lock a player out of ever being backfilled, silently,
 * which is the bug #44 was filed to fix — reintroduced one layer down.
 *
 * Hermetic: the id pages, the archive and the queue are all stubbed, so nothing
 * here needs a service and nothing reaches a worker.
 */

/** Newest first, the way match-v5 returns them. */
const HISTORY = Array.from({ length: 250 }, (_, i) => `OC1_${1000 - i}`);

/** Every id-list page this walk asked for, as `[start, count]`. */
const pages: [number, number][] = [];
const queued: { matchId: string; priority: number; fetchTimeline?: boolean }[] = [];
/** Ordered record of the calls that matter, so "before" can be asserted. */
const events: string[] = [];
const completions: { puuid: string; depth: number }[] = [];

/** Ids already in the archive, which the walk must rank around but not queue. */
let archived = new Set<string>();
/** Page index to blow up on, standing in for the job dying mid-history. */
let failAtPage: number | null = null;

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        const start = Number(req.query['start'] ?? 0);
        const count = Number(req.query['count'] ?? 100);
        if (failAtPage !== null && pages.length === failAtPage) {
          throw new Error('match-v5 fell over mid-walk');
        }
        pages.push([start, count]);
        events.push(`page:${start}`);
        return { data: HISTORY.slice(start, start + count), cache: 'MISS' as const, ageSeconds: 0 };
      },
    },
  };
});

vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return {
    ...actual,
    filterUnarchived: async (ids: string[]) => ids.filter((id) => !archived.has(id)),
  };
});

vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return {
    ...actual,
    markBackfillStarted: async () => {
      events.push('started');
    },
    markBackfillComplete: async (puuid: string, depth: number) => {
      events.push('complete');
      completions.push({ puuid, depth });
    },
  };
});

vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    archiveQueue: {
      addBulk: async (jobs: { data: ArchiveMatchJob; opts: { priority: number } }[]) => {
        for (const job of jobs) {
          queued.push({
            matchId: job.data.matchId,
            priority: job.opts.priority,
            ...(job.data.fetchTimeline !== undefined
              ? { fetchTimeline: job.data.fetchTimeline }
              : {}),
          });
        }
        return jobs;
      },
    },
  };
});

const { backfillPlayer } = await import('../src/jobs/processors.js');
const { backfillPriority } = await import('../src/jobs/queues.js');

const progress: number[] = [];
const walk = (data: Record<string, unknown> = {}) =>
  backfillPlayer({
    data: { puuid: 'p', platform: 'oc1', ...data },
    updateProgress: async (n: number) => {
      progress.push(n);
    },
  } as never);

beforeEach(() => {
  pages.length = 0;
  queued.length = 0;
  events.length = 0;
  completions.length = 0;
  progress.length = 0;
  archived = new Set();
  failAtPage = null;
});

describe('backfill:player (§10)', () => {
  it('pages the history a hundred at a time, up to the limit it was given', async () => {
    const result = await walk({ limit: 250 });

    expect(pages).toEqual([
      [0, 100],
      [100, 100],
      [200, 50],
    ]);
    expect(result).toEqual({ queued: 250, depth: 250 });
    expect(queued).toHaveLength(250);
  });

  it('stops on a short page rather than asking past the end of a history', async () => {
    const result = await walk({ limit: 1000 });

    // 250 games exist; the third page comes back short and ends the walk.
    expect(pages).toEqual([
      [0, 100],
      [100, 100],
      [200, 100],
    ]);
    expect(result.depth).toBe(250);
  });

  it('claims the walk before doing any of it, and completes it on the way out', async () => {
    await walk({ limit: 100 });
    // #44 — a job that dies leaves a start without a completion, which reads as
    // "tried, did not finish". Recording the start afterwards would lose that.
    expect(events).toEqual(['started', 'page:0', 'complete']);
  });

  it('never records a completion for a walk that died mid-history', async () => {
    failAtPage = 1;
    await expect(walk({ limit: 250 })).rejects.toThrow('fell over mid-walk');

    // The stamp that stops the next lookup queueing this player again must not
    // be written by a walk that only got half way.
    expect(events).toEqual(['started', 'page:0']);
    expect(completions).toEqual([]);
  });

  it('records how deep it actually got, so a raised limit is distinguishable', async () => {
    await walk({ limit: 150 });
    expect(completions).toEqual([{ puuid: 'p', depth: 150 }]);
  });

  it('ranks each match by its position in the history, not in the filtered page', async () => {
    // Ten already-archived matches at the top: skipping them must not promote
    // the eleventh into the band a freshly finished game occupies.
    archived = new Set(HISTORY.slice(0, 10));
    await walk({ limit: 100 });

    expect(queued.map((q) => q.matchId)).not.toContain(HISTORY[0]);
    expect(queued.find((q) => q.matchId === HISTORY[10])?.priority).toBe(backfillPriority(10));
    expect(queued.find((q) => q.matchId === HISTORY[99])?.priority).toBe(backfillPriority(99));
  });

  it('queues nothing when the whole page is already archived', async () => {
    archived = new Set(HISTORY);
    const result = await walk({ limit: 100 });

    expect(queued).toEqual([]);
    // Still a completed walk: the archive holding everything is the goal state.
    expect(result).toEqual({ queued: 0, depth: 100 });
    expect(completions).toEqual([{ puuid: 'p', depth: 100 }]);
  });

  it('passes the job’s own timeline choice down to each archive job', async () => {
    await walk({ limit: 100, fetchTimeline: true });
    expect(queued.every((q) => q.fetchTimeline === true)).toBe(true);
  });

  it('reports progress as a percentage of the limit, never past 100', async () => {
    await walk({ limit: 250 });
    expect(progress).toEqual([40, 80, 100]);
  });
});

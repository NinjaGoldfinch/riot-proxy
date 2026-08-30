import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest } from '../src/riot/endpoints.js';
import type { BackfillPlayerJob } from '../src/jobs/queues.js';

/**
 * A match poll used to read a fixed window off the top of the history, so
 * anything that fell past it between one tick and the next was lost for good
 * (#46). Play rate was never the risk — nobody finishes five games in five
 * minutes — but a redeploy or a stalled queue stops the ticks, and nothing else
 * walks a tracked player, so the gap never healed.
 *
 * `last_seen_match_id` was written by every tick and read by nothing. These
 * tests are about it finally being read.
 */
process.env['TRACK_CATCHUP_LIMIT'] = '120';

/** Newest first, the way match-v5 returns them. */
const HISTORY = Array.from({ length: 400 }, (_, i) => `OC1_${1000 - i}`);

/** Every id-list page this poll asked for, as `[start, count]`. */
const pages: [number, number][] = [];
const archived: { matchId: string; priority: number }[] = [];
const enqueued: BackfillPlayerJob[] = [];
let cursor: string | null = null;
let lastSeenWrites: string[] = [];

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        const start = Number(req.query['start'] ?? 0);
        const count = Number(req.query['count'] ?? 20);
        pages.push([start, count]);
        return {
          data: HISTORY.slice(start, start + count),
          cache: 'MISS' as const,
          ageSeconds: 0,
        };
      },
    },
  };
});

vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return {
    ...actual,
    getPlayer: async () => ({ puuid: 'p', platform: 'oc1', lastSeenMatchId: cursor }),
    setLastSeenMatch: async (_puuid: string, matchId: string) => {
      lastSeenWrites.push(matchId);
    },
  };
});

vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  // Nothing is stored yet, so every id the poll finds is work it must queue.
  return { ...actual, filterUnarchived: async (ids: string[]) => ids };
});

vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    archiveQueue: {
      addBulk: async (jobs: { data: { matchId: string }; opts: { priority: number } }[]) => {
        for (const j of jobs) archived.push({ matchId: j.data.matchId, priority: j.opts.priority });
        return jobs;
      },
    },
    enqueueBackfill: async (data: BackfillPlayerJob) => {
      enqueued.push(data);
      return { jobId: `backfill-${data.puuid}`, status: 'queued' as const };
    },
  };
});

const { pollMatches } = await import('../src/jobs/processors.js');
const { config } = await import('../src/config.js');
const { ARCHIVE_PRIORITY, backfillPriority } = await import('../src/jobs/queues.js');
const { closeRedis, redis } = await import('../src/redis.js');

let available = false;

const poll = () =>
  pollMatches({
    data: { puuid: 'p', platform: 'oc1' },
    // The real job only ever has its progress read by BullMQ.
    updateProgress: async () => {},
  } as never);

beforeAll(async () => {
  try {
    await redis.ping();
    available = true;
  } catch {
    available = false;
  }
});

afterAll(async () => {
  await closeRedis();
});

beforeEach(() => {
  pages.length = 0;
  archived.length = 0;
  enqueued.length = 0;
  lastSeenWrites = [];
  cursor = null;
});

describe('match poll catch-up', () => {
  it('costs one call when nothing has gone wrong', async ({ skip }) => {
    if (!available) return skip();
    // The cursor is the second-newest match: one game since the last tick.
    cursor = HISTORY[1] ?? null;
    await poll();

    expect(pages).toEqual([[0, 5]]);
    // Only what is actually new — the cursor itself is already archived.
    expect(archived.map((a) => a.matchId)).toEqual([HISTORY[0]]);
    expect(lastSeenWrites).toEqual([HISTORY[0]]);
  });

  it('reads one page and stops for a player it has never polled', async ({ skip }) => {
    if (!available) return skip();
    cursor = null;
    await poll();

    // No cursor is not a reason to walk a history; that is the backfill's job.
    expect(pages).toEqual([[0, 5]]);
    expect(archived).toHaveLength(5);
  });

  it('pages back to the cursor when the ticks stopped for a while', async ({ skip }) => {
    if (!available) return skip();
    // 40 games happened while the worker was down — eight times the old window.
    cursor = HISTORY[40] ?? null;
    await poll();

    expect(pages.length).toBeGreaterThan(1);
    expect(pages[0]).toEqual([0, 5]);
    // Every one of them, not just the newest five.
    expect(archived.map((a) => a.matchId)).toEqual(HISTORY.slice(0, 40));
    expect(enqueued).toEqual([]);
  });

  it('does not re-queue the cursor itself', async ({ skip }) => {
    if (!available) return skip();
    cursor = HISTORY[40] ?? null;
    await poll();
    expect(archived.map((a) => a.matchId)).not.toContain(cursor);
  });

  it('keeps a freshly finished game ahead of the catch-up tail', async ({ skip }) => {
    if (!available) return skip();
    cursor = HISTORY[40] ?? null;
    await poll();

    const newest = archived.find((a) => a.matchId === HISTORY[0]);
    const tail = archived.find((a) => a.matchId === HISTORY[30]);
    // A long tail must not swamp the live band it shares with real-time games.
    expect(newest?.priority).toBe(ARCHIVE_PRIORITY.live);
    expect(tail?.priority).toBe(backfillPriority(30));
    expect(newest?.priority).toBeLessThan(tail?.priority ?? 0);
  });

  it('hands a gap deeper than the limit to a backfill', async ({ skip }) => {
    if (!available) return skip();
    // Further behind than a tick should chase inline.
    cursor = HISTORY[300] ?? null;
    await poll();

    expect(enqueued).toEqual([
      {
        puuid: 'p',
        platform: 'oc1',
        limit: config.LOOKUP_BACKFILL_LIMIT,
        reason: 'catchup',
      },
    ]);
    // It still archives what it did find rather than dropping the work.
    expect(archived.length).toBeGreaterThan(0);
    // And it stopped at the limit rather than walking the whole history.
    const deepest = Math.max(...pages.map(([start]) => start));
    expect(deepest).toBeLessThan(config.TRACK_CATCHUP_LIMIT + 100);
  });

  it('advances the cursor to the newest match it saw', async ({ skip }) => {
    if (!available) return skip();
    cursor = HISTORY[40] ?? null;
    await poll();
    expect(lastSeenWrites).toEqual([HISTORY[0]]);
  });

  it('leaves the cursor alone when there is nothing new', async ({ skip }) => {
    if (!available) return skip();
    cursor = HISTORY[0] ?? null;
    await poll();

    expect(pages).toEqual([[0, 5]]);
    expect(archived).toEqual([]);
    // Nothing moved, so nothing to write.
    expect(lastSeenWrites).toEqual([]);
  });
});

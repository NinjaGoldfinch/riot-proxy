import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest, MethodId } from '../src/riot/endpoints.js';
import type { BackfillPlayerJob } from '../src/jobs/queues.js';

/**
 * The first time anyone asks for a player, their whole history goes into the
 * archive — matches are immutable, so a match stored now is one nobody ever
 * spends quota on again. The suite pins `LOOKUP_BACKFILL_LIMIT` off, so this
 * file turns it on before anything reads the config.
 */
process.env['LOOKUP_BACKFILL_LIMIT'] = '250';

const replies = new Map<MethodId, unknown>();
const enqueued: BackfillPlayerJob[] = [];
let unarchived: (ids: string[]) => string[] = (ids) => ids;

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => ({
        data: replies.get(req.method) ?? null,
        cache: 'MISS' as const,
        ageSeconds: 0,
      }),
    },
  };
});

// The real one would put a job in front of whatever worker is running on this
// machine, for a player that does not exist.
vi.mock('../src/jobs/queues.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/jobs/queues.js')>();
  return {
    ...actual,
    enqueueBackfill: async (data: BackfillPlayerJob) => {
      enqueued.push(data);
      return { jobId: `backfill-${data.puuid}`, status: 'queued' as const };
    },
  };
});

vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return { ...actual, filterUnarchived: async (ids: string[]) => unarchived(ids) };
});

const { buildApp } = await import('../src/app.js');
const { config } = await import('../src/config.js');
const { closeDb, pingDb } = await import('../src/db/index.js');
const { closeRedis, redis } = await import('../src/redis.js');
const { wsHub } = await import('../src/ws/index.js');
const { createTestConsumer, removeTestConsumers, testConsumerName } =
  await import('./helpers/consumers.js');

let app: Awaited<ReturnType<typeof buildApp>> | undefined;
let key = '';
let available = false;

const PUUID = 'L'.repeat(78);
const auth = () => ({ authorization: `Bearer ${key}` });
const page = (start = 0, count = 10) =>
  `/v1/players/${PUUID}/matches?platform=oc1&start=${start}&count=${count}`;

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const consumer = await createTestConsumer({
    name: testConsumerName('lookup-backfill'),
    scopes: ['read'],
    quotaPerMin: 10_000,
  });
  key = consumer?.key ?? '';
  app = await buildApp();
  await app.ready();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  if (available) await removeTestConsumers();
  await Promise.allSettled([closeRedis(), closeDb()]);
});

beforeEach(async () => {
  enqueued.length = 0;
  unarchived = (ids) => ids;
  replies.clear();
  replies.set('match.idsByPuuid', ['OC1_1', 'OC1_2']);
  replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
  if (available) {
    const keys = await redis.keys(`refresh:${config.KEY_SCOPE}:*`);
    if (keys.length) await redis.del(...keys);
  }
});

describe('first-lookup backfill', () => {
  it('is configured to walk the whole history, not just the page asked for', ({ skip }) => {
    if (!available) return skip();
    expect(config.LOOKUP_BACKFILL_LIMIT).toBe(250);
  });

  it('queues the player’s history the first time they are looked up', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });

    expect(res.statusCode).toBe(200);
    expect(enqueued).toEqual([{ puuid: PUUID, platform: 'oc1', limit: 250, reason: 'lookup' }]);
    // The caller is told, so a UI can say the history is on its way.
    expect(res.json().backfill).toMatchObject({ status: 'queued', limit: 250 });
  });

  it('leaves a player alone once their recent matches are already stored', async ({ skip }) => {
    if (!available || !app) return skip();
    unarchived = () => [];

    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(enqueued).toEqual([]);
    expect(res.json().backfill).toBeNull();
  });

  it('never judges "new to us" from a deep page', async ({ skip }) => {
    if (!available || !app) return skip();
    // Page 5 being unarchived says nothing: a backfill already running has
    // simply not reached it yet.
    await app.inject({ method: 'GET', url: page(50), headers: auth() });
    expect(enqueued).toEqual([]);
  });

  it('serves the page even when the queue is unreachable', async ({ skip }) => {
    if (!available || !app) return skip();
    unarchived = () => {
      throw new Error('postgres is down');
    };

    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(res.statusCode).toBe(200);
    expect(res.json().matchIds).toEqual(['OC1_1', 'OC1_2']);
    expect(res.json().backfill).toBeNull();
  });
});

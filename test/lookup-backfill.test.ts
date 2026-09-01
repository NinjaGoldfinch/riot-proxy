import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { probeServices } from './helpers/services.js';
import type { BuiltRequest, MethodId } from '../src/riot/endpoints.js';
import type { BackfillPlayerJob } from '../src/jobs/queues.js';
import type { UpsertPlayerInput } from '../src/db/players.js';
import type { Player } from '../src/db/schema.js';

/**
 * The first time anyone asks for a player, their whole history goes into the
 * archive — matches are immutable, so a match stored now is one nobody ever
 * spends quota on again. The suite pins `LOOKUP_BACKFILL_LIMIT` off, so this
 * file turns it on before anything reads the config.
 *
 * "First time" is read off the player row, not off the archive (#44). These
 * tests therefore drive `upsertPlayer`'s return value rather than pretending
 * matches are or are not stored — that distinction is the whole point.
 */
process.env['LOOKUP_BACKFILL_LIMIT'] = '250';

const replies = new Map<MethodId, unknown>();
const enqueued: BackfillPlayerJob[] = [];
const upserted: UpsertPlayerInput[] = [];

const PUUID = 'L'.repeat(78);

/** The stored row this lookup will find. Each test sets the parts it cares about. */
let stored: Partial<Player> = {};
/** Set to make the next upsert blow up, standing in for Postgres being down. */
let upsertFails = false;

const row = (input: UpsertPlayerInput): Player => ({
  puuid: input.puuid,
  keyScope: 'test',
  platform: input.platform,
  gameName: null,
  tagLine: null,
  tracked: false,
  lastSeenMatchId: null,
  historyBackfillStartedAt: null,
  historyBackfilledAt: null,
  historyBackfillDepth: null,
  updatedAt: new Date(),
  ...stored,
});

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

vi.mock('../src/db/players.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/players.js')>();
  return {
    ...actual,
    upsertPlayer: async (input: UpsertPlayerInput) => {
      if (upsertFails) throw new Error('postgres is down');
      upserted.push(input);
      return row(input);
    },
  };
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

const auth = () => ({ authorization: `Bearer ${key}` });
const page = (start = 0, count = 10) =>
  `/v1/players/${PUUID}/matches?platform=oc1&start=${start}&count=${count}`;

beforeAll(async () => {
  available = await probeServices('lookup-backfill.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
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
  upserted.length = 0;
  stored = {};
  upsertFails = false;
  replies.clear();
  replies.set('match.idsByPuuid', ['OC1_1', 'OC1_2']);
  replies.set('match.byId', { metadata: { matchId: 'OC1_1' } });
  if (available) {
    // Scoped to this file's own PUUID: `refresh:*` is shared with every other
    // suite running against the same Redis, and a wider wipe here can delete
    // another file's in-flight claim out from under it.
    const keys = await redis.keys(`refresh:${config.KEY_SCOPE}:*:${PUUID}`);
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

  it('records the player, so the answer survives the request', async ({ skip }) => {
    if (!available || !app) return skip();
    await app.inject({ method: 'GET', url: page(), headers: auth() });
    // Until #44 a lookup wrote no player row at all; only an admin track did.
    expect(upserted).toEqual([{ puuid: PUUID, platform: 'oc1' }]);
  });

  it('still walks a player whose recent matches a teammate already archived', async ({ skip }) => {
    if (!available || !app) return skip();
    // The regression this replaced: matches are shared between ten players, so
    // the archive holding this player's games says nothing about whether anyone
    // walked *them*. Only a completed walk does, and there is none.
    stored = { historyBackfilledAt: null };

    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(enqueued).toHaveLength(1);
    expect(res.json().backfill).toMatchObject({ status: 'queued' });
  });

  it('leaves a player alone once their history has actually been walked', async ({ skip }) => {
    if (!available || !app) return skip();
    stored = { historyBackfilledAt: new Date(), historyBackfillDepth: 250 };

    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(enqueued).toEqual([]);
    expect(res.json().backfill).toBeNull();
  });

  it('retries a walk that started and never finished', async ({ skip }) => {
    if (!available || !app) return skip();
    // Started but never completed: the job died mid-history. A half-walk must
    // not read as a finished one, or the gap it left is permanent.
    stored = { historyBackfillStartedAt: new Date(), historyBackfilledAt: null };

    await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(enqueued).toHaveLength(1);
  });

  it('never judges "new to us" from a deep page', async ({ skip }) => {
    if (!available || !app) return skip();
    // Page 5 is someone paging through a history whose first page already had
    // its chance to trigger this.
    await app.inject({ method: 'GET', url: page(50), headers: auth() });
    expect(enqueued).toEqual([]);
    expect(upserted).toEqual([]);
  });

  it('serves the page even when the bookkeeping is unreachable', async ({ skip }) => {
    if (!available || !app) return skip();
    upsertFails = true;

    const res = await app.inject({ method: 'GET', url: page(), headers: auth() });
    expect(res.statusCode).toBe(200);
    expect(res.json().matchIds).toEqual(['OC1_1', 'OC1_2']);
    expect(res.json().backfill).toBeNull();
  });
});

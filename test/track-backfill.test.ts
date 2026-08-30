import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { BackfillPlayerJob } from '../src/jobs/queues.js';
import type { UpsertPlayerInput } from '../src/db/players.js';
import type { Player } from '../src/db/schema.js';

/**
 * Tracking a player used to archive only what the poller happened to catch from
 * that moment on, so a player tracked today had no history behind them and
 * nothing to reconcile a gap against (#46). They get the same walk a first
 * lookup triggers (#44), for the same reason: matches are immutable, so it is
 * quota spent once.
 */
process.env['LOOKUP_BACKFILL_LIMIT'] = '250';

const enqueued: BackfillPlayerJob[] = [];
let stored: Partial<Player> = {};

const PUUID = 'T'.repeat(78);

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
    upsertPlayer: async (input: UpsertPlayerInput): Promise<Player> => ({
      puuid: input.puuid,
      keyScope: 'test',
      platform: input.platform,
      gameName: input.gameName ?? null,
      tagLine: input.tagLine ?? null,
      tracked: input.tracked ?? false,
      lastSeenMatchId: null,
      historyBackfillStartedAt: null,
      historyBackfilledAt: null,
      historyBackfillDepth: null,
      updatedAt: new Date(),
      ...stored,
    }),
  };
});

const { buildApp } = await import('../src/app.js');
const { closeDb, pingDb } = await import('../src/db/index.js');
const { closeRedis, redis } = await import('../src/redis.js');
const { wsHub } = await import('../src/ws/index.js');
const { createTestConsumer, removeTestConsumers, testConsumerName } =
  await import('./helpers/consumers.js');

let app: Awaited<ReturnType<typeof buildApp>> | undefined;
let key = '';
let available = false;

const track = (body: Record<string, unknown>) => ({
  method: 'POST' as const,
  url: '/v1/admin/tracked-players',
  headers: { authorization: `Bearer ${key}` },
  payload: { platform: 'oc1', puuid: PUUID, ...body },
});

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const consumer = await createTestConsumer({
    name: testConsumerName('track-backfill'),
    scopes: ['read', 'admin'],
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

beforeEach(() => {
  enqueued.length = 0;
  stored = {};
});

describe('tracking a player walks their history', () => {
  it('queues a walk for a player nobody has walked', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject(track({}));

    expect(res.statusCode).toBe(200);
    expect(enqueued).toEqual([{ puuid: PUUID, platform: 'oc1', limit: 250, reason: 'track' }]);
    // Reported back, so an operator can see the work was started.
    expect(res.json().backfill).toMatchObject({ status: 'queued' });
  });

  it('leaves a player alone if their history has already been walked', async ({ skip }) => {
    if (!available || !app) return skip();
    stored = { historyBackfilledAt: new Date(), historyBackfillDepth: 250 };

    const res = await app.inject(track({}));
    expect(enqueued).toEqual([]);
    expect(res.json().backfill).toBeNull();
  });

  it('does not walk a player who is being untracked', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject(track({ tracked: false }));

    expect(res.statusCode).toBe(200);
    expect(enqueued).toEqual([]);
  });

  it('still tracks the player when the queue is unreachable', async ({ skip }) => {
    if (!available || !app) return skip();
    const queues = await import('../src/jobs/queues.js');
    const spy = vi
      .spyOn(queues, 'enqueueBackfill')
      .mockRejectedValueOnce(new Error('redis is down'));

    const res = await app.inject(track({}));
    // Tracking is the thing that was asked for; the walk is an optimisation.
    expect(res.statusCode).toBe(200);
    expect(res.json().tracked).toBe(true);
    expect(res.json().backfill).toBeNull();
    spy.mockRestore();
  });
});

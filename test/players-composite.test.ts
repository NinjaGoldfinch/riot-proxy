import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { BuiltRequest, MethodId } from '../src/riot/endpoints.js';
import { ProxyError } from '../src/errors.js';

/**
 * The composites (§6.3) are the only routes that do more than hand a built
 * request to the fetcher, so they are the only ones whose *shape* can be wrong:
 * a part that should degrade to a warning failing the document, an account
 * fetched twice, a match page that claims there is more behind it. The fetcher
 * itself is stubbed — what is under test is the fan-out, not the read path.
 */

type Reply = { data: unknown } | { error: unknown };

const calls: BuiltRequest[] = [];
let replies = new Map<MethodId, Reply | ((req: BuiltRequest) => Reply)>();

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest) => {
        calls.push(req);
        const entry = replies.get(req.method);
        const reply = typeof entry === 'function' ? entry(req) : entry;
        if (!reply) throw ProxyError.notFound(`no stub for ${req.method}`);
        if ('error' in reply) throw reply.error;
        return { data: reply.data, cache: 'MISS' as const, ageSeconds: 0 };
      },
    },
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

const PUUID = 'P'.repeat(78);
const auth = () => ({ authorization: `Bearer ${key}` });
const methodsCalled = () => calls.map((c) => c.method);

beforeAll(async () => {
  try {
    await redis.ping();
    available = await pingDb();
  } catch {
    available = false;
  }
  if (!available) return;

  const consumer = await createTestConsumer({
    name: testConsumerName('composite'),
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

beforeEach(() => {
  calls.length = 0;
  replies = new Map();
});

describe('composite profile (§6.3)', () => {
  it('resolves a Riot ID and reuses that account as the composite part', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('account.byRiotId', {
      data: { puuid: PUUID, gameName: 'NinjaGoldfinch', tagLine: 'OCENZ' },
    });
    replies.set('summoner.byPuuid', { data: { summonerLevel: 412 } });
    replies.set('league.entriesByPuuid', { data: [{ queueType: 'RANKED_SOLO_5x5' }] });
    replies.set('mastery.topByPuuid', { data: [{ championId: 266 }] });

    const res = await app.inject({
      method: 'GET',
      url: '/v1/players/by-riot-id/NinjaGoldfinch/OCENZ/profile?platform=oc1',
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({
      puuid: PUUID,
      platform: 'oc1',
      region: 'sea',
      account: { gameName: 'NinjaGoldfinch' },
      summoner: { summonerLevel: 412 },
      warnings: [],
    });

    // The whole point of resolving here: one account call, not two.
    expect(methodsCalled()).toContain('account.byRiotId');
    expect(methodsCalled()).not.toContain('account.byPuuid');
    // SEA has no account-v1 host, so the lookup must land on asia (§5.1).
    expect(calls.find((c) => c.method === 'account.byRiotId')?.scope).toBe('asia');
  });

  it('404s a Riot ID that resolves to no PUUID', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('account.byRiotId', { data: {} });

    const res = await app.inject({
      method: 'GET',
      url: '/v1/players/by-riot-id/Ghost/NONE/profile?platform=euw1',
      headers: auth(),
    });
    expect(res.statusCode).toBe(404);
    expect(res.json().error.code).toBe('NOT_FOUND');
  });

  it('degrades a failed part to a warning rather than failing the document', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('account.byPuuid', { data: { gameName: 'Someone' } });
    replies.set('summoner.byPuuid', { data: { summonerLevel: 30 } });
    replies.set('league.entriesByPuuid', { data: [] });
    replies.set('mastery.topByPuuid', { error: ProxyError.upstream('mastery timed out') });

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    const body = res.json();
    expect(body.mastery).toBeNull();
    expect(body.warnings).toEqual([expect.stringContaining('mastery unavailable')]);
  });

  it('404s when every part fails', async ({ skip }) => {
    if (!available || !app) return skip();
    // No stubs registered: every part rejects.
    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile`,
      headers: auth(),
    });
    expect(res.statusCode).toBe(404);
    expect(res.json().error.code).toBe('NOT_FOUND');
  });
});

describe('composite match page (§6.3)', () => {
  const page = (ids: string[]) => {
    replies.set('match.idsByPuuid', { data: ids });
    replies.set('match.byId', (req) => ({
      data: { metadata: { matchId: req.path.split('/').pop() } },
    }));
  };

  it('hydrates every id on the page and reports whether more follow', async ({ skip }) => {
    if (!available || !app) return skip();
    page(['OC1_1', 'OC1_2']);

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&start=4&count=2`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({
      region: 'sea',
      start: 4,
      count: 2,
      matchIds: ['OC1_1', 'OC1_2'],
      hasMore: true,
      warnings: [],
    });
    expect(res.json().matches).toHaveLength(2);
    expect(methodsCalled().filter((m) => m === 'match.byId')).toHaveLength(2);
    // `start` has to reach match-v5 or every page returns the same matches.
    expect(calls.find((c) => c.method === 'match.idsByPuuid')?.query).toMatchObject({
      start: 4,
      count: 2,
    });
  });

  it('stops claiming more pages once one comes back short', async ({ skip }) => {
    if (!available || !app) return skip();
    page(['OC1_1']);

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=10`,
      headers: auth(),
    });
    expect(res.json().hasMore).toBe(false);
  });

  it('drops an unavailable match into warnings and still serves the page', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('match.idsByPuuid', { data: ['OC1_1', 'OC1_2'] });
    replies.set('match.byId', (req) =>
      req.path.endsWith('OC1_2')
        ? { error: ProxyError.upstream('match gone') }
        : { data: { metadata: { matchId: 'OC1_1' } } },
    );

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=2`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    expect(res.json().matches).toHaveLength(1);
    expect(res.json().warnings).toEqual([expect.stringContaining('match OC1_2 unavailable')]);
  });

  it('fails the page when the id lookup itself fails', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('match.idsByPuuid', { error: ProxyError.upstream('match-v5 down') });

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1`,
      headers: auth(),
    });
    expect(res.statusCode).toBe(502);
    expect(res.json().error.code).toBe('UPSTREAM_ERROR');
  });

  it('caps the fan-out well below match-v5’s own limit of 100', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=50`,
      headers: auth(),
    });
    expect(res.statusCode).toBe(400);
    expect(res.json().error.code).toBe('VALIDATION');
    expect(calls).toHaveLength(0);
  });
});

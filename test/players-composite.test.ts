import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { probeServices } from './helpers/services.js';
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
const bypassFlags: { method: MethodId; bypass: boolean }[] = [];
let replies = new Map<MethodId, Reply | ((req: BuiltRequest) => Reply)>();

vi.mock('../src/fetcher.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/fetcher.js')>();
  return {
    ...actual,
    fetcher: {
      fetch: async (req: BuiltRequest, opts?: { bypassCache?: boolean }) => {
        calls.push(req);
        bypassFlags.push({ method: req.method, bypass: opts?.bypassCache === true });
        const entry = replies.get(req.method);
        const reply = typeof entry === 'function' ? entry(req) : entry;
        if (!reply) throw ProxyError.notFound(`no stub for ${req.method}`);
        if ('error' in reply) throw reply.error;
        return { data: reply.data, cache: 'MISS' as const, ageSeconds: 0 };
      },
    },
  };
});

/**
 * The archive read the match page primes itself with (#54). Empty by default,
 * so every other test in this file exercises the fan-out exactly as before;
 * the ids each call asked for are recorded, because "one query, not twenty" is
 * the thing being fixed and is otherwise invisible from the response.
 */
const archiveQueries: string[][] = [];
let archivedMatches = new Map<string, unknown>();

vi.mock('../src/db/matches.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/db/matches.js')>();
  return {
    ...actual,
    getArchivedMatches: async (ids: string[]) => {
      archiveQueries.push(ids);
      return new Map(
        ids.filter((id) => archivedMatches.has(id)).map((id) => [id, archivedMatches.get(id)]),
      );
    },
  };
});

const { buildApp } = await import('../src/app.js');
const { closeDb, pingDb } = await import('../src/db/index.js');
const { closeRedis, redis } = await import('../src/redis.js');
const { config } = await import('../src/config.js');
const { wsHub } = await import('../src/ws/index.js');
const { registry } = await import('../src/metrics.js');
const { createTestConsumer, removeTestConsumers, testConsumerName } =
  await import('./helpers/consumers.js');

let app: Awaited<ReturnType<typeof buildApp>> | undefined;
let key = '';
let available = false;

const PUUID = 'P'.repeat(78);
const auth = () => ({ authorization: `Bearer ${key}` });
const methodsCalled = () => calls.map((c) => c.method);

beforeAll(async () => {
  available = await probeServices('players-composite.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
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

beforeEach(async () => {
  calls.length = 0;
  bypassFlags.length = 0;
  archiveQueries.length = 0;
  archivedMatches = new Map();
  replies = new Map();
  if (available) {
    // Each test starts with the refresh window open.
    const keys = await redis.keys(`refresh:${config.KEY_SCOPE}:*`);
    if (keys.length) await redis.del(...keys);
  }
});

/** `proxy_cache_reads_total{state="hit"}`, across every label set. */
async function cacheHits(): Promise<number> {
  const metric = await registry.getSingleMetric('proxy_cache_reads_total')?.get();
  return (metric?.values ?? [])
    .filter((v) => v.labels['state'] === 'hit')
    .reduce((sum, v) => sum + v.value, 0);
}

const bypassed = (method: MethodId) =>
  calls.filter((c) => c.method === method).length > 0 &&
  bypassFlags.filter((f) => f.method === method).every((f) => f.bypass);

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
  /** A match-v5 payload, with enough of the bulk to prove the page sheds it. */
  const riotMatch = (matchId: string, puuid = PUUID) => ({
    metadata: { matchId, participants: [puuid, 'other'] },
    info: {
      queueId: 420,
      gameMode: 'CLASSIC',
      gameDuration: 1834,
      gameEndTimestamp: 1_756_001_894_000,
      teams: [{ teamId: 100, win: true, bans: [{ championId: 64 }] }],
      participants: [
        {
          puuid,
          win: true,
          championId: 64,
          championName: 'LeeSin',
          kills: 8,
          deaths: 3,
          assists: 11,
          challenges: { kda: 6.33, soloKills: 2 },
        },
        { puuid: 'other', win: false, championId: 266, championName: 'Aatrox' },
      ],
    },
  });

  const page = (ids: string[]) => {
    replies.set('match.idsByPuuid', { data: ids });
    replies.set('match.byId', (req) => ({
      data: riotMatch(req.path.split('/').pop() ?? ''),
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
        : { data: riotMatch('OC1_1') },
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

  it('returns a summary of the player’s own line, not the match behind it', async ({ skip }) => {
    if (!available || !app) return skip();
    page(['OC1_1']);

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=1`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    expect(res.json().matches[0]).toEqual({
      matchId: 'OC1_1',
      queueId: 420,
      gameMode: 'CLASSIC',
      gameDuration: 1834,
      gameEndTimestamp: 1_756_001_894_000,
      player: {
        puuid: PUUID,
        win: true,
        championId: 64,
        championName: 'LeeSin',
        kills: 8,
        deaths: 3,
        assists: 11,
      },
    });

    // The bulk an overview panel never reads: the other nine players, their
    // challenges, the team objectives. Fetched and archived, just not returned.
    expect(res.body).not.toContain('challenges');
    expect(res.body).not.toContain('Aatrox');
    expect(res.body).not.toContain('bans');
  });

  it('names a match that does not mention the player rather than serving it', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('match.idsByPuuid', { data: ['OC1_1', 'OC1_2'] });
    replies.set('match.byId', (req) =>
      req.path.endsWith('OC1_2')
        ? { data: riotMatch('OC1_2', 'somebody-else') }
        : { data: riotMatch('OC1_1') },
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

  /**
   * Paging a history is affordable *because* matches are archived — but the
   * fan-out asked the archive for them one at a time, so a fully archived page
   * of twenty issued twenty single-row queries against a pool of ten, and half
   * of them waited on the other half before the page could be assembled (#54).
   */
  it('asks the archive for the whole page in one query', async ({ skip }) => {
    if (!available || !app) return skip();
    const ids = Array.from({ length: 20 }, (_, i) => `OC1_${i}`);
    replies.set('match.idsByPuuid', { data: ids });
    archivedMatches = new Map(ids.map((id) => [id, riotMatch(id)]));

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=20`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    expect(res.json().matches).toHaveLength(20);
    expect(archiveQueries).toEqual([ids]);
    // Nothing was left for the fetcher: an archived match is the answer.
    expect(methodsCalled()).not.toContain('match.byId');
  });

  it('still counts each archived match as a cache hit', async ({ skip }) => {
    if (!available || !app) return skip();
    const ids = ['OC1_1', 'OC1_2'];
    replies.set('match.idsByPuuid', { data: ids });
    archivedMatches = new Map(ids.map((id) => [id, riotMatch(id)]));
    const before = await cacheHits();

    await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=2`,
      headers: auth(),
    });

    // The fetcher counted these when it made the query; moving the query must
    // not quietly drop the hit rate §13 watches.
    expect(await cacheHits()).toBe(before + 2);
  });

  it('fans out for exactly the matches the archive did not have', async ({ skip }) => {
    if (!available || !app) return skip();
    page(['OC1_1', 'OC1_2', 'OC1_3']);
    archivedMatches = new Map([['OC1_2', riotMatch('OC1_2')]]);

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&count=3`,
      headers: auth(),
    });

    expect(res.statusCode).toBe(200);
    // Reassembled in id order, so `matches[]` still lines up with `matchIds[]`.
    expect(res.json().matches.map((m: { matchId: string }) => m.matchId)).toEqual([
      'OC1_1',
      'OC1_2',
      'OC1_3',
    ]);
    expect(
      calls.filter((c) => c.method === 'match.byId').map((c) => c.path.split('/').pop()),
    ).toEqual(['OC1_1', 'OC1_3']);
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

/**
 * A profile view must not spend upstream quota on a player nobody asked to
 * re-read — that is what the cache is for — and the manual override that does
 * spend it has to be metered where a browser cannot route around it.
 */
describe('manual refresh (60s per player)', () => {
  const stubProfile = () => {
    replies.set('account.byRiotId', { data: { puuid: PUUID, gameName: 'N', tagLine: 'OCE' } });
    replies.set('account.byPuuid', { data: { puuid: PUUID } });
    replies.set('summoner.byPuuid', { data: { summonerLevel: 1 } });
    replies.set('league.entriesByPuuid', { data: [] });
    replies.set('mastery.topByPuuid', { data: [] });
  };

  it('reads through the cache when no refresh is asked for', async ({ skip }) => {
    if (!available || !app) return skip();
    stubProfile();

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile`,
      headers: auth(),
    });

    expect(res.json()).toMatchObject({ refreshed: false, refreshAvailableIn: 0 });
    expect(bypassFlags.every((f) => !f.bypass)).toBe(true);
  });

  it('bypasses the cache on the first refresh and refuses the next for 60s', async ({ skip }) => {
    if (!available || !app) return skip();
    stubProfile();

    const first = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile?refresh=true`,
      headers: auth(),
    });
    expect(first.json()).toMatchObject({ refreshed: true, refreshAvailableIn: 60 });
    expect(bypassed('summoner.byPuuid')).toBe(true);

    bypassFlags.length = 0;
    const second = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile?refresh=true`,
      headers: auth(),
    });
    // Not an error: whoever won the window wrote these values seconds ago.
    expect(second.statusCode).toBe(200);
    expect(second.json().refreshed).toBe(false);
    expect(second.json().refreshAvailableIn).toBeGreaterThan(0);
    expect(second.json().refreshAvailableIn).toBeLessThanOrEqual(60);
    expect(bypassFlags.every((f) => !f.bypass)).toBe(true);
  });

  it('reports the running cooldown to a plain lookup, so a UI can show it', async ({ skip }) => {
    if (!available || !app) return skip();
    stubProfile();

    await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile?refresh=true`,
      headers: auth(),
    });
    const plain = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile`,
      headers: auth(),
    });
    expect(plain.json().refreshAvailableIn).toBeGreaterThan(0);
  });

  it('keys the window on the PUUID, whichever way the profile was entered', async ({ skip }) => {
    if (!available || !app) return skip();
    stubProfile();

    const byRiotId = await app.inject({
      method: 'GET',
      url: '/v1/players/by-riot-id/N/OCE/profile?platform=oc1&refresh=true',
      headers: auth(),
    });
    expect(byRiotId.json().refreshed).toBe(true);
    // The Riot ID mapping is only consulted for the PUUID it carries, so the
    // account part is re-read by PUUID along with everything else.
    expect(methodsCalled()).toContain('account.byPuuid');

    const byPuuid = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile?refresh=true`,
      headers: auth(),
    });
    expect(byPuuid.json().refreshed).toBe(false);
  });

  it('refreshes only the id list, never the immutable matches behind it', async ({ skip }) => {
    if (!available || !app) return skip();
    replies.set('match.idsByPuuid', { data: ['OC1_1'] });
    replies.set('match.byId', { data: { metadata: { matchId: 'OC1_1' } } });

    const res = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&refresh=true`,
      headers: auth(),
    });

    expect(res.json()).toMatchObject({ refreshed: true, refreshAvailableIn: 60 });
    expect(bypassed('match.idsByPuuid')).toBe(true);
    // Re-downloading an archived match would cost quota to learn nothing.
    expect(bypassFlags.filter((f) => f.method === 'match.byId').every((f) => !f.bypass)).toBe(true);
  });

  it('meters matches and profile independently, so one Update covers both', async ({ skip }) => {
    if (!available || !app) return skip();
    stubProfile();
    replies.set('match.idsByPuuid', { data: [] });

    const profile = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/profile?refresh=true`,
      headers: auth(),
    });
    const matches = await app.inject({
      method: 'GET',
      url: `/v1/players/${PUUID}/matches?platform=oc1&refresh=true`,
      headers: auth(),
    });

    expect(profile.json().refreshed).toBe(true);
    expect(matches.json().refreshed).toBe(true);
  });
});

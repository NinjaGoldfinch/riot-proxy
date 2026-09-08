import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import type { App } from '../src/app.js';
import { buildApp } from '../src/app.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { matchParticipants, matches } from '../src/db/schema.js';
import { listPlayerChampions } from '../src/db/analytics.js';
import { createTestConsumer, removeTestConsumers, testConsumerName } from './helpers/consumers.js';
import { closeRedis, redis } from '../src/redis.js';
import { wsHub } from '../src/ws/index.js';

/**
 * A player's champion pool (#113), against the real Postgres — the deliverable
 * is one grouped query, so there is nothing left to test once it is mocked.
 *
 * The fixture stays small enough to check by hand: every count below can be
 * worked out from the seed, which is the only way to know the grouping counts
 * the right rows rather than merely counting.
 */

/**
 * PUUIDs long enough to pass `PuuidParam`'s 60-character floor, since these go
 * through the route as well as the query.
 */
const PUUID = 'pooltestpuuid'.padEnd(78, '0');
const STRANGER = 'pooltestother'.padEnd(78, '0');

const AHRI = 103;
const GAREN = 86;

/** Ids are platform-prefixed for real, because the platform filter reads them. */
const EUW = (n: number) => `EUW1_99000000${n}`;
const NA = (n: number) => `NA1_99000000${n}`;

let app: App | undefined;
let readKey = '';
let available = false;
const seeded = new Set<string>();

interface Participant {
  puuid: string;
  championId: number;
  win: boolean;
  kills?: number;
  deaths?: number;
  assists?: number;
  cs?: number;
}

async function seedMatch(
  matchId: string,
  participants: Participant[],
  over: { queueId?: number; gameVersion?: string; gameDuration?: number; gameEndTs?: number } = {},
): Promise<void> {
  seeded.add(matchId);
  await db.insert(matches).values({
    matchId,
    region: 'europe',
    data: {
      metadata: { matchId },
      info: {
        queueId: over.queueId ?? 420,
        gameVersion: over.gameVersion ?? '16.13.790.6961',
        gameEndTimestamp: over.gameEndTs ?? 1_756_000_000_000,
        gameDuration: over.gameDuration ?? 1800,
      },
    },
  });
  await db.insert(matchParticipants).values(
    participants.map((p) => ({
      matchId,
      puuid: p.puuid,
      championId: p.championId,
      win: p.win,
      kills: p.kills ?? null,
      deaths: p.deaths ?? null,
      assists: p.assists ?? null,
      cs: p.cs ?? null,
    })),
  );
}

async function wipe(): Promise<void> {
  if (seeded.size > 0) {
    await db.delete(matches).where(inArray(matches.matchId, [...seeded]));
    seeded.clear();
  }
  // The route caches its document, so a suite that seeds different rows under
  // the same PUUID would otherwise assert against the previous test's answer.
  const keys = await redis.keys('d:*:pool:*');
  if (keys.length > 0) await redis.del(...keys);
}

beforeAll(async () => {
  available = await probeServices('player-champions.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (!available) return;

  const read = await createTestConsumer({ name: testConsumerName('pool'), scopes: ['read'] });
  readKey = read?.key ?? '';
  app = await buildApp();
  await app.ready();
});

beforeEach(async () => {
  if (available) await wipe();
});

afterAll(async () => {
  if (app) await app.close();
  await wsHub.stop();
  if (available) {
    await wipe();
    await removeTestConsumers();
  }
  await Promise.allSettled([closeRedis(), closeDb()]);
});

describe('listPlayerChampions', () => {
  it('groups one player’s games by champion, most played first', async ({ skip }) => {
    if (!available) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }]);
    await seedMatch(EUW(2), [{ puuid: PUUID, championId: AHRI, win: false }]);
    await seedMatch(EUW(3), [{ puuid: PUUID, championId: GAREN, win: true }]);

    const rows = await listPlayerChampions({ puuid: PUUID });
    expect(rows.map((r) => [r.championId, r.games, r.wins])).toEqual([
      [AHRI, 2, 1],
      [GAREN, 1, 1],
    ]);
  });

  it('counts only the player asked about, not everyone in their games', async ({ skip }) => {
    if (!available) return skip();
    // One match, two participants: the stranger's Garen must not reach the
    // pool, and the shared match must not count twice for either of them.
    await seedMatch(EUW(1), [
      { puuid: PUUID, championId: AHRI, win: true },
      { puuid: STRANGER, championId: GAREN, win: false },
    ]);

    expect((await listPlayerChampions({ puuid: PUUID })).map((r) => r.championId)).toEqual([AHRI]);
    expect((await listPlayerChampions({ puuid: STRANGER })).map((r) => r.championId)).toEqual([
      GAREN,
    ]);
  });

  it('narrows to one platform by the match id’s own prefix', async ({ skip }) => {
    if (!available) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }]);
    await seedMatch(NA(2), [{ puuid: PUUID, championId: GAREN, win: true }]);

    expect(
      (await listPlayerChampions({ puuid: PUUID, platform: 'euw1' })).map((r) => r.championId),
    ).toEqual([AHRI]);
    expect(
      (await listPlayerChampions({ puuid: PUUID, platform: 'na1' })).map((r) => r.championId),
    ).toEqual([GAREN]);
    // Unfiltered spans both, which is the point of not defaulting it.
    expect(await listPlayerChampions({ puuid: PUUID })).toHaveLength(2);
  });

  it('narrows by queue and by patch', async ({ skip }) => {
    if (!available) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }], { queueId: 420 });
    await seedMatch(EUW(2), [{ puuid: PUUID, championId: GAREN, win: true }], { queueId: 450 });
    await seedMatch(EUW(3), [{ puuid: PUUID, championId: GAREN, win: true }], {
      gameVersion: '16.12.1.1',
    });

    expect(
      (await listPlayerChampions({ puuid: PUUID, queueId: 450 })).map((r) => r.championId),
    ).toEqual([GAREN]);
    // 16.13 holds the Ahri game and the queue-450 Garen; 16.12 holds the third.
    expect(await listPlayerChampions({ puuid: PUUID, patch: '16.13' })).toHaveLength(2);
    expect(
      (await listPlayerChampions({ puuid: PUUID, patch: '16.12' })).map((r) => r.games),
    ).toEqual([1]);
  });

  it('sums the facts and reports the swept rows separately', async ({ skip }) => {
    if (!available) return skip();
    await seedMatch(
      EUW(1),
      [{ puuid: PUUID, championId: AHRI, win: true, kills: 10, deaths: 2, assists: 4, cs: 200 }],
      { gameDuration: 1200 },
    );
    // Same champion, archived before C2 extracted the facts — counted as a
    // game, excluded from every average it never contributed to.
    await seedMatch(EUW(2), [{ puuid: PUUID, championId: AHRI, win: false }], {
      gameDuration: 1800,
    });

    const [row] = await listPlayerChampions({ puuid: PUUID });
    expect(row).toMatchObject({ games: 2, wins: 1, statedGames: 1, kills: 10, cs: 200 });
    // Only the swept game's duration is in the denominator, so cs/min is
    // 200 over 20 minutes rather than 200 over 50.
    expect(row?.durationS).toBe(1200);
  });

  it('reports the most recent game, and null when none carries an end', async ({ skip }) => {
    if (!available) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }], {
      gameEndTs: 1_700_000_000_000,
    });
    await seedMatch(EUW(2), [{ puuid: PUUID, championId: AHRI, win: true }], {
      gameEndTs: 1_760_000_000_000,
    });

    const [row] = await listPlayerChampions({ puuid: PUUID });
    expect(row?.lastPlayedAt).toBe(1_760_000_000_000);
  });
});

describe('GET /v1/players/{puuid}/champions', () => {
  const get = (puuid: string, query = '') =>
    app!.inject({
      method: 'GET',
      url: `/v1/players/${puuid}/champions${query ? `?${query}` : ''}`,
      headers: { authorization: `Bearer ${readKey}` },
    });

  it('derives winRate, avgKda and csPerMin from the sums', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedMatch(
      EUW(1),
      [{ puuid: PUUID, championId: AHRI, win: true, kills: 8, deaths: 2, assists: 4, cs: 200 }],
      { gameDuration: 1200 },
    );
    await seedMatch(
      EUW(2),
      [{ puuid: PUUID, championId: AHRI, win: false, kills: 2, deaths: 2, assists: 0, cs: 100 }],
      { gameDuration: 1200 },
    );

    const res = await get(PUUID);
    expect(res.statusCode).toBe(200);
    const body = res.json() as {
      archivedGames: number;
      champions: { championId: number; winRate: number; avgKda: number; csPerMin: number }[];
    };

    expect(body.archivedGames).toBe(2);
    // (8+2 kills + 4+0 assists) / (2+2 deaths) = 3.5, over 300 cs in 40 minutes.
    expect(body.champions[0]).toMatchObject({ championId: AHRI, winRate: 0.5, avgKda: 3.5 });
    expect(body.champions[0]?.csPerMin).toBeCloseTo(7.5, 4);
  });

  it('omits the averages rather than zeroing them on an unswept archive', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }]);

    const champion = (await get(PUUID)).json().champions[0] as Record<string, unknown>;
    // A pre-C2 row has no kills to average and no duration it may claim, so
    // both are absent — an `avgKda: 0` would read as "fed every game".
    expect(champion).toMatchObject({ championId: AHRI, games: 1, winRate: 1 });
    expect(champion).not.toHaveProperty('avgKda');
    expect(champion).not.toHaveProperty('csPerMin');
  });

  it('answers 200 with an empty pool for a player nobody has walked', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await get(STRANGER);
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ archivedGames: 0, champions: [] });
  });

  it('echoes the filters it applied', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }]);

    expect((await get(PUUID)).json()).toMatchObject({
      platform: null,
      queue: null,
      patch: null,
    });
    expect((await get(PUUID, 'platform=euw1&queue=420&patch=16.13')).json()).toMatchObject({
      platform: 'euw1',
      queue: 420,
      patch: '16.13',
    });
  });

  it('serves the second read from the cache, keyed on the filters', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedMatch(EUW(1), [{ puuid: PUUID, championId: AHRI, win: true }]);

    expect((await get(PUUID)).headers['x-cache']).toBe('MISS');
    expect((await get(PUUID)).headers['x-cache']).toBe('HIT');
    // A different question is a different document, not a stale hit on the
    // first one.
    expect((await get(PUUID, 'queue=420')).headers['x-cache']).toBe('MISS');
  });

  it('rejects a patch that is not a patch, and a platform that is not one', async ({ skip }) => {
    if (!available || !app) return skip();
    expect((await get(PUUID, 'patch=16.13.790.6961')).statusCode).toBe(400);
    expect((await get(PUUID, 'platform=euw9')).statusCode).toBe(400);
  });
});

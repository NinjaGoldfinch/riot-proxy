import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { eq, inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import type { App } from '../src/app.js';
import { buildApp } from '../src/app.js';
import { KEY_SCOPE } from '../src/config.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { championStats, leagueEntries, matchParticipants, matches } from '../src/db/schema.js';
import { latestPatch, listChampionStats, recomputeChampionStats } from '../src/db/analytics.js';
import { createTestConsumer, removeTestConsumers, testConsumerName } from './helpers/consumers.js';
import { closeRedis, redis } from '../src/redis.js';
import { wsHub } from '../src/ws/index.js';

/**
 * Reading the archive back (#90), against the real Postgres — the deliverable
 * *is* a SQL statement, so there is nothing left to test once it is mocked.
 *
 * The fixture is deliberately small and hand-checkable: every count asserted
 * below can be worked out from the seed by hand, which is the only way to know
 * the join is counting the right thing rather than merely counting.
 */
/**
 * A real platform, because the route validates it against the closed enum —
 * and one nothing else in the suite or a dev database touches, since every
 * query here is scoped by it.
 */
const PLATFORM = 'vn2';
const QUEUE = 'RANKED_SOLO_5x5';
const MATCH_PREFIX = 'ANALYTICS_TEST_';

let app: App | undefined;
let readKey = '';
let available = false;

/** Champion ids, named so the assertions read as something. */
const AHRI = 103;
const GAREN = 86;

interface SeedMatch {
  id: string;
  queueId: number;
  gameVersion: string;
  /** puuid → [championId, win] */
  players: Record<string, [number, boolean]>;
}

async function seed(seedMatches: SeedMatch[], ladder: Record<string, string>): Promise<void> {
  for (const m of seedMatches) {
    await db.insert(matches).values({
      matchId: m.id,
      region: 'europe',
      data: {
        metadata: { matchId: m.id },
        info: {
          queueId: m.queueId,
          gameVersion: m.gameVersion,
          gameEndTimestamp: 1_756_000_000_000,
        },
      },
    });
    await db.insert(matchParticipants).values(
      Object.entries(m.players).map(([puuid, [championId, win]]) => ({
        matchId: m.id,
        puuid,
        championId,
        win,
      })),
    );
  }

  const crawlId = '00000000-0000-4000-8000-0000000000aa';
  for (const [puuid, tier] of Object.entries(ladder)) {
    await db.insert(leagueEntries).values({
      keyScope: KEY_SCOPE,
      platform: PLATFORM,
      queue: QUEUE,
      puuid,
      tier,
      division: 'I',
      leaguePoints: 0,
      wins: 0,
      losses: 0,
      firstSeenCrawlId: crawlId,
      lastSeenCrawlId: crawlId,
    });
  }
}

async function wipe(): Promise<void> {
  const ids = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .where(eq(matches.region, 'europe'));
  const mine = ids.map((r) => r.matchId).filter((id) => id.startsWith(MATCH_PREFIX));
  if (mine.length > 0) await db.delete(matches).where(inArray(matches.matchId, mine));
  await db.delete(leagueEntries).where(eq(leagueEntries.platform, PLATFORM));
  await db.delete(championStats).where(eq(championStats.platform, PLATFORM));
}

beforeAll(async () => {
  available = await probeServices('ladder-analytics.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (!available) return;

  const read = await createTestConsumer({
    name: testConsumerName('analytics'),
    scopes: ['read'],
  });
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

const m = (
  id: string,
  players: Record<string, [number, boolean]>,
  over: Partial<SeedMatch> = {},
): SeedMatch => ({
  id: `${MATCH_PREFIX}${id}`,
  queueId: 420,
  gameVersion: '16.13.790.6961',
  players,
  ...over,
});

describe('recomputing champion aggregates', () => {
  it('counts a champion once per participant, at that player’s tier', async ({ skip }) => {
    if (!available) return skip();
    await seed(
      [
        m('1', { 'chall-a': [AHRI, true], 'diamond-a': [GAREN, false] }),
        m('2', { 'chall-a': [AHRI, false] }),
      ],
      { 'chall-a': 'CHALLENGER', 'diamond-a': 'DIAMOND' },
    );

    const result = await recomputeChampionStats(PLATFORM, QUEUE);
    expect(result.games).toBe(3);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows).toHaveLength(2);
    expect(rows.find((r) => r.championId === AHRI)).toMatchObject({
      tier: 'CHALLENGER',
      patch: '16.13',
      games: 2,
      wins: 1,
    });
    expect(rows.find((r) => r.championId === GAREN)).toMatchObject({
      tier: 'DIAMOND',
      games: 1,
      wins: 0,
    });
  });

  it('splits the same match across the tiers its players sit in', async ({ skip }) => {
    if (!available) return skip();
    // The whole reason the ladder is joined in: one game contributes to two
    // tiers, because a match is not played "in" a tier.
    await seed([m('1', { 'chall-a': [AHRI, true], 'diamond-a': [AHRI, false] })], {
      'chall-a': 'CHALLENGER',
      'diamond-a': 'DIAMOND',
    });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => `${r.tier}:${r.wins}/${r.games}`).sort()).toEqual([
      'CHALLENGER:1/1',
      'DIAMOND:0/1',
    ]);
  });

  it('ignores players the ladder has never seen', async ({ skip }) => {
    if (!available) return skip();
    // Nine of ten participants in a Challenger game are usually not in the
    // crawl's tier floor — and an unplaced player has no tier to be counted at.
    await seed([m('1', { 'chall-a': [AHRI, true], stranger: [GAREN, false] })], {
      'chall-a': 'CHALLENGER',
    });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows.map((r) => r.championId)).toEqual([AHRI]);
  });

  it('groups by the patch, not the build behind it', async ({ skip }) => {
    if (!available) return skip();
    // The two builds are the same patch; the third is not.
    await seed(
      [
        m('1', { 'chall-a': [AHRI, true] }, { gameVersion: '16.13.790.6961' }),
        m('2', { 'chall-a': [AHRI, true] }, { gameVersion: '16.13.802.4387' }),
        m('3', { 'chall-a': [AHRI, false] }, { gameVersion: '16.14.111.2222' }),
      ],
      { 'chall-a': 'CHALLENGER' },
    );
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows.map((r) => `${r.patch}:${r.wins}/${r.games}`).sort()).toEqual([
      '16.13:2/2',
      '16.14:0/1',
    ]);
  });

  it('counts only the queue the ladder is about', async ({ skip }) => {
    if (!available) return skip();
    await seed(
      [
        m('1', { 'chall-a': [AHRI, true] }),
        // ARAM, and a flex game: neither belongs in a solo-queue aggregate.
        m('2', { 'chall-a': [AHRI, true] }, { queueId: 450 }),
        m('3', { 'chall-a': [AHRI, true] }, { queueId: 440 }),
      ],
      { 'chall-a': 'CHALLENGER' },
    );
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.games).toBe(1);
  });

  it('replaces the slice rather than adding to it', async ({ skip }) => {
    if (!available) return skip();
    await seed([m('1', { 'chall-a': [AHRI, true] })], { 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);
    await recomputeChampionStats(PLATFORM, QUEUE);

    // Idempotent, which is what makes recompute-from-archive the strategy
    // rather than a fallback: running it twice is running it once.
    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.games).toBe(1);
  });

  it('drops rows the archive no longer supports', async ({ skip }) => {
    if (!available) return skip();
    await seed([m('1', { 'chall-a': [GAREN, true] })], { 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);
    expect(await listChampionStats({ platform: PLATFORM, queue: QUEUE })).toHaveLength(1);

    // The player left the ladder — the crawl no longer places them at a tier,
    // so their games no longer belong to one. An upsert would have left the
    // old row sitting there looking current.
    await db.delete(leagueEntries).where(eq(leagueEntries.platform, PLATFORM));
    await recomputeChampionStats(PLATFORM, QUEUE);
    expect(await listChampionStats({ platform: PLATFORM, queue: QUEUE })).toEqual([]);
  });

  it('follows a player who was promoted since the last crawl', async ({ skip }) => {
    if (!available) return skip();
    // Recompute-from-archive exists for this: an incremental counter written at
    // archive time could never go back and move a game to a new tier.
    await seed([m('1', { climber: [AHRI, true] })], { climber: 'DIAMOND' });
    await recomputeChampionStats(PLATFORM, QUEUE);
    expect((await listChampionStats({ platform: PLATFORM, queue: QUEUE }))[0]?.tier).toBe(
      'DIAMOND',
    );

    await db
      .update(leagueEntries)
      .set({ tier: 'MASTER' })
      .where(eq(leagueEntries.platform, PLATFORM));
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rows = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.tier).toBe('MASTER');
  });

  it('picks the newest patch by number, not by string order', async ({ skip }) => {
    if (!available) return skip();
    // '16.9' sorts after '16.10' as text, which would make the default slice
    // the wrong patch for two weeks of every year.
    await seed(
      [
        m('1', { 'chall-a': [AHRI, true] }, { gameVersion: '16.9.700.1' }),
        m('2', { 'chall-a': [AHRI, true] }, { gameVersion: '16.10.700.1' }),
      ],
      { 'chall-a': 'CHALLENGER' },
    );
    await recomputeChampionStats(PLATFORM, QUEUE);

    expect(await latestPatch(PLATFORM, QUEUE)).toBe('16.10');
  });
});

describe('matches generated columns (#109)', () => {
  async function insertMatch(id: string, info: Record<string, unknown>): Promise<string> {
    const matchId = `${MATCH_PREFIX}${id}`;
    await db.insert(matches).values({
      matchId,
      region: 'europe',
      data: { metadata: { matchId }, info },
    });
    return matchId;
  }

  it('derives patch as major.minor, ignoring the build behind it', async ({ skip }) => {
    if (!available) return skip();
    const matchId = await insertMatch('patch-present', {
      queueId: 420,
      gameVersion: '16.13.790.6961',
      gameEndTimestamp: 1_756_000_000_000,
    });

    const [row] = await db
      .select({ patch: matches.patch })
      .from(matches)
      .where(eq(matches.matchId, matchId));
    expect(row?.patch).toBe('16.13');
  });

  it('leaves patch null when gameVersion is absent', async ({ skip }) => {
    if (!available) return skip();
    const matchId = await insertMatch('patch-absent', {
      queueId: 420,
      gameEndTimestamp: 1_756_000_000_000,
    });

    const [row] = await db
      .select({ patch: matches.patch })
      .from(matches)
      .where(eq(matches.matchId, matchId));
    expect(row?.patch).toBeNull();
  });

  it('derives game_duration in seconds from info.gameDuration', async ({ skip }) => {
    if (!available) return skip();
    const matchId = await insertMatch('duration-present', {
      queueId: 420,
      gameVersion: '16.13.790.6961',
      gameDuration: 1823,
      gameEndTimestamp: 1_756_000_000_000,
    });

    const [row] = await db
      .select({ gameDuration: matches.gameDuration })
      .from(matches)
      .where(eq(matches.matchId, matchId));
    expect(row?.gameDuration).toBe(1823);
  });

  it('leaves game_duration null when info.gameDuration is absent', async ({ skip }) => {
    if (!available) return skip();
    const matchId = await insertMatch('duration-absent', {
      queueId: 420,
      gameVersion: '16.13.790.6961',
      gameEndTimestamp: 1_756_000_000_000,
    });

    const [row] = await db
      .select({ gameDuration: matches.gameDuration })
      .from(matches)
      .where(eq(matches.matchId, matchId));
    expect(row?.gameDuration).toBeNull();
  });
});

describe('GET /v1/lol/analytics/champions', () => {
  const get = (query: string) =>
    app!.inject({
      method: 'GET',
      url: `/v1/lol/analytics/champions?platform=${PLATFORM}&${query}`,
      headers: { authorization: `Bearer ${readKey}` },
    });

  it('serves the newest patch by default, most played first', async ({ skip }) => {
    if (!available || !app) return skip();
    await seed(
      [
        m('1', { 'chall-a': [AHRI, true] }),
        m('2', { 'chall-a': [AHRI, false] }),
        m('3', { 'chall-a': [GAREN, true] }),
        m('4', { 'chall-a': [AHRI, true] }, { gameVersion: '16.12.1.1' }),
      ],
      { 'chall-a': 'CHALLENGER' },
    );
    await recomputeChampionStats(PLATFORM, QUEUE);

    const res = await get('');
    expect(res.statusCode).toBe(200);
    const body = res.json() as {
      patch: string;
      tier: string | null;
      totalGames: number;
      computedAt: string;
      champions: { championId: number; games: number; winRate: number; share: number }[];
    };

    expect(body.patch).toBe('16.13');
    expect(body.tier).toBeNull();
    // The 16.12 game is a different patch and is not in this slice.
    expect(body.totalGames).toBe(3);
    expect(body.champions.map((c) => c.championId)).toEqual([AHRI, GAREN]);
    expect(body.champions[0]).toMatchObject({ games: 2, wins: 1, winRate: 0.5 });
    expect(body.champions[0]?.share).toBeCloseTo(2 / 3, 4);
    expect(body.computedAt).toBeTruthy();
  });

  it('narrows to one tier when asked', async ({ skip }) => {
    if (!available || !app) return skip();
    await seed([m('1', { 'chall-a': [AHRI, true], 'diamond-a': [GAREN, true] })], {
      'chall-a': 'CHALLENGER',
      'diamond-a': 'DIAMOND',
    });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const body = (await get('tier=DIAMOND')).json() as {
      tier: string;
      champions: { championId: number; share: number }[];
    };
    expect(body.tier).toBe('DIAMOND');
    expect(body.champions.map((c) => c.championId)).toEqual([GAREN]);
    // The share is of the slice, so one champion in a one-tier slice is all of it.
    expect(body.champions[0]?.share).toBe(1);
  });

  it('answers honestly before anything has been aggregated', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await get('');
    expect(res.statusCode).toBe(200);
    // Not a 404: the endpoint exists, the numbers do not yet.
    expect(res.json()).toMatchObject({
      patch: null,
      totalGames: 0,
      champions: [],
      computedAt: null,
    });
  });

  it('is cacheable, because a recompute replaces a slice wholesale', async ({ skip }) => {
    if (!available || !app) return skip();
    const res = await get('');
    expect(res.headers['cache-control']).toContain('max-age=300');
  });

  it('rejects a patch that is not a patch, and a tier that is not a tier', async ({ skip }) => {
    if (!available || !app) return skip();
    expect((await get('patch=16.13.790.6961')).statusCode).toBe(400);
    expect((await get('tier=WOOD')).statusCode).toBe(400);
  });
});

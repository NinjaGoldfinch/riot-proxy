import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { eq, inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import type { App } from '../src/app.js';
import { buildApp } from '../src/app.js';
import { KEY_SCOPE } from '../src/config.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import {
  analyticsSlices,
  championBans,
  championStats,
  leagueEntries,
  matchBans,
  matchParticipants,
  matches,
} from '../src/db/schema.js';
import {
  latestPatch,
  listAnalyticsSlices,
  listChampionBans,
  listChampionStats,
  recomputeChampionStats,
} from '../src/db/analytics.js';
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
  await db.delete(analyticsSlices).where(eq(analyticsSlices.platform, PLATFORM));
  await db.delete(championBans).where(eq(championBans.platform, PLATFORM));
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

const FACTS_CRAWL_ID = '00000000-0000-4000-8000-0000000000bb';

interface SeededParticipant {
  championId: number;
  win: boolean;
  teamPosition?: string;
  kills?: number;
  deaths?: number;
  assists?: number;
  cs?: number;
  gold?: number;
  damage?: number;
  vision?: number;
}

/**
 * A match with full control over the C2 fact columns and optional bans —
 * `seed`/`m` above only carry `championId`/`win`, which is not enough to
 * exercise C3's role/sum/denominator columns.
 */
async function seedFacts(
  id: string,
  participants: Record<string, SeededParticipant>,
  bans: { teamId: number; pickTurn: number; championId: number }[] = [],
  over: { gameDuration?: number } = {},
): Promise<void> {
  const matchId = `${MATCH_PREFIX}${id}`;
  await db.insert(matches).values({
    matchId,
    region: 'europe',
    data: {
      metadata: { matchId },
      info: {
        queueId: 420,
        gameVersion: '16.13.790.6961',
        gameEndTimestamp: 1_756_000_000_000,
        gameDuration: over.gameDuration ?? 1800,
      },
    },
  });
  await db.insert(matchParticipants).values(
    Object.entries(participants).map(([puuid, p]) => ({
      matchId,
      puuid,
      championId: p.championId,
      win: p.win,
      teamPosition: p.teamPosition ?? null,
      kills: p.kills ?? null,
      deaths: p.deaths ?? null,
      assists: p.assists ?? null,
      cs: p.cs ?? null,
      gold: p.gold ?? null,
      damage: p.damage ?? null,
      vision: p.vision ?? null,
    })),
  );
  if (bans.length > 0) {
    await db.insert(matchBans).values(bans.map((b) => ({ matchId, ...b })));
  }
}

async function ladder(entries: Record<string, string>): Promise<void> {
  for (const [puuid, tier] of Object.entries(entries)) {
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
      firstSeenCrawlId: FACTS_CRAWL_ID,
      lastSeenCrawlId: FACTS_CRAWL_ID,
    });
  }
}

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

/**
 * The role dimension, the fact sums, and the honest denominators C3 adds
 * (#111) — against the real Postgres, for the same reason as the block
 * above: the deliverable is what the recompute's SQL actually does with a
 * mix of swept and unswept participant rows, which nothing but a real
 * database can answer.
 */
describe('champion_stats v2 — role, sums and denominators', () => {
  it('stores one row per role, and sums every role at read time when none is asked for', async ({
    skip,
  }) => {
    if (!available) return skip();
    await seedFacts('role-1', { 'chall-a': { championId: AHRI, win: true, teamPosition: 'MIDDLE' } });
    await seedFacts('role-2', { 'chall-a': { championId: AHRI, win: false, teamPosition: 'UTILITY' } });
    await ladder({ 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rolled = await listChampionStats({ platform: PLATFORM, queue: QUEUE });
    expect(rolled.find((r) => r.championId === AHRI)).toMatchObject({ games: 2, wins: 1 });

    const mid = await listChampionStats({ platform: PLATFORM, queue: QUEUE, role: 'MIDDLE' });
    expect(mid.find((r) => r.championId === AHRI)).toMatchObject({ games: 1, wins: 1 });

    const support = await listChampionStats({ platform: PLATFORM, queue: QUEUE, role: 'UTILITY' });
    expect(support.find((r) => r.championId === AHRI)).toMatchObject({ games: 1, wins: 0 });
  });

  it('sums facts only from swept rows, counting them separately in statedGames', async ({ skip }) => {
    if (!available) return skip();
    await seedFacts('stated-1', {
      'chall-a': {
        championId: AHRI,
        win: true,
        kills: 10,
        deaths: 2,
        assists: 5,
        cs: 200,
        gold: 14_000,
        damage: 20_000,
        vision: 30,
      },
    });
    // A pre-C2-shaped row: champion and win only, nothing else — exactly what
    // an archive `facts:reextract` has not reached yet looks like.
    await seedFacts('stated-2', { 'chall-a': { championId: AHRI, win: false } });
    await ladder({ 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const row = (await listChampionStats({ platform: PLATFORM, queue: QUEUE })).find(
      (r) => r.championId === AHRI,
    );
    expect(row).toMatchObject({
      games: 2,
      statedGames: 1,
      kills: 10,
      deaths: 2,
      assists: 5,
      cs: 200,
      gold: 14_000,
      damage: 20_000,
      vision: 30,
      // Only the stated game's 1800s, not the unswept game's too — otherwise
      // csPerMin at the route would be understated by a game that contributed
      // duration but no cs.
      durationS: 1800,
    });
  });

  it('counts matchesPicked as distinct matches, not participant rows', async ({ skip }) => {
    if (!available) return skip();
    // A mirror matchup: the same champion on both sides of one game.
    await seedFacts('mirror-1', {
      'chall-a': { championId: AHRI, win: true },
      'chall-b': { championId: AHRI, win: false },
    });
    await ladder({ 'chall-a': 'CHALLENGER', 'chall-b': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const row = (await listChampionStats({ platform: PLATFORM, queue: QUEUE })).find(
      (r) => r.championId === AHRI,
    );
    expect(row).toMatchObject({ games: 2, matchesPicked: 1 });
  });

  it('computes a slice as distinct matches with a participant at that tier', async ({ skip }) => {
    if (!available) return skip();
    await seedFacts('slice-1', {
      'chall-a': { championId: AHRI, win: true },
      'chall-b': { championId: GAREN, win: false },
    });
    await seedFacts('slice-2', { 'diamond-a': { championId: AHRI, win: true } });
    await ladder({ 'chall-a': 'CHALLENGER', 'chall-b': 'CHALLENGER', 'diamond-a': 'DIAMOND' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const slices = await listAnalyticsSlices({ platform: PLATFORM, queue: QUEUE, patch: '16.13' });
    // One Challenger match (both its picks are the same match), one Diamond.
    expect(slices.find((s) => s.tier === 'CHALLENGER')?.matches).toBe(1);
    expect(slices.find((s) => s.tier === 'DIAMOND')?.matches).toBe(1);
  });

  it('attributes a ban to every tier the match touched', async ({ skip }) => {
    if (!available) return skip();
    // `championId: -1` (no pick made in that slot) never reaches `match_bans`
    // at all — `extractBans` (#110) skips it before a row is ever written, so
    // there is nothing for the recompute to filter; that skip is covered in
    // `test/matches-facts.test.ts`.
    await seedFacts(
      'ban-1',
      { 'chall-a': { championId: AHRI, win: true }, 'diamond-a': { championId: GAREN, win: false } },
      [{ teamId: 100, pickTurn: 1, championId: 64 }],
    );
    await ladder({ 'chall-a': 'CHALLENGER', 'diamond-a': 'DIAMOND' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const bans = await listChampionBans({ platform: PLATFORM, queue: QUEUE, patch: '16.13' });
    expect(bans.map((b) => `${b.tier}:${b.championId}:${b.bans}`).sort()).toEqual([
      'CHALLENGER:64:1',
      'DIAMOND:64:1',
    ]);
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

  it('rolls up every role by default, and narrows with a role filter', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedFacts('role-a', { 'chall-a': { championId: AHRI, win: true, teamPosition: 'MIDDLE' } });
    await seedFacts('role-b', { 'chall-a': { championId: AHRI, win: false, teamPosition: 'UTILITY' } });
    await ladder({ 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const rolled = (await get('')).json() as {
      role: string | null;
      champions: { championId: number; games: number }[];
    };
    expect(rolled.role).toBeNull();
    expect(rolled.champions.find((c) => c.championId === AHRI)?.games).toBe(2);

    const narrowed = (await get('role=MIDDLE')).json() as {
      role: string | null;
      champions: { championId: number; games: number }[];
    };
    expect(narrowed.role).toBe('MIDDLE');
    expect(narrowed.champions.find((c) => c.championId === AHRI)?.games).toBe(1);
  });

  it('computes pickRate from the slice, and banRate for a champion that was banned', async ({
    skip,
  }) => {
    if (!available || !app) return skip();
    await seedFacts(
      'rate-1',
      { 'chall-a': { championId: AHRI, win: true }, 'chall-b': { championId: GAREN, win: false } },
      [{ teamId: 200, pickTurn: 1, championId: 64 }],
    );
    await ladder({ 'chall-a': 'CHALLENGER', 'chall-b': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const body = (await get('')).json() as {
      champions: { championId: number; pickRate?: number; banRate?: number }[];
    };
    // One Challenger match total, and Ahri was picked in all of it.
    expect(body.champions.find((c) => c.championId === AHRI)).toMatchObject({ pickRate: 1 });
    // Never banned — a computed 0, not an unknown: the slice both rates
    // divide into is known the moment the champion has a stats row at all,
    // since both tables come from the same recompute transaction (#111).
    expect(body.champions.find((c) => c.championId === AHRI)?.banRate).toBe(0);
  });

  it('clamps pickRate at 1 rather than double-counting a cross-role mirror pick', async ({
    skip,
  }) => {
    if (!available || !app) return skip();
    // One match, one champion picked twice — once in each of two different
    // roles. `matchesPicked` is stored (and summed) per role, so the rolled-up
    // read would otherwise report this one match as two.
    await seedFacts('cross-role-1', {
      'chall-a': { championId: AHRI, win: true, teamPosition: 'JUNGLE' },
      'chall-b': { championId: AHRI, win: false, teamPosition: 'UTILITY' },
    });
    await ladder({ 'chall-a': 'CHALLENGER', 'chall-b': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const body = (await get('')).json() as { champions: { championId: number; pickRate?: number }[] };
    expect(body.champions.find((c) => c.championId === AHRI)?.pickRate).toBe(1);
  });

  it('computes the performance averages only once a champion has stated games', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedFacts(
      'perf-1',
      {
        'chall-a': {
          championId: AHRI,
          win: true,
          kills: 10,
          deaths: 2,
          assists: 6,
          cs: 200,
          gold: 12_000,
          damage: 20_000,
          vision: 30,
        },
      },
      [],
      { gameDuration: 1200 }, // 20 minutes
    );
    // No facts at all — the pre-C2 shape.
    await seedFacts('perf-2', { 'chall-b': { championId: GAREN, win: true } });
    await ladder({ 'chall-a': 'CHALLENGER', 'chall-b': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const body = (await get('')).json() as {
      champions: {
        championId: number;
        avgKda?: number;
        csPerMin?: number;
        goldPerMin?: number;
        avgDamage?: number;
        avgVision?: number;
      }[];
    };

    const ahri = body.champions.find((c) => c.championId === AHRI);
    expect(ahri?.avgKda).toBeCloseTo((10 + 6) / 2, 4);
    expect(ahri?.csPerMin).toBeCloseTo(200 / 20, 4);
    expect(ahri?.goldPerMin).toBeCloseTo(12_000 / 20, 4);
    expect(ahri?.avgDamage).toBe(20_000);
    expect(ahri?.avgVision).toBe(30);

    const garen = body.champions.find((c) => c.championId === GAREN);
    expect(garen?.avgKda).toBeUndefined();
    expect(garen?.csPerMin).toBeUndefined();
  });

  it('trims champions below minGames', async ({ skip }) => {
    if (!available || !app) return skip();
    await seedFacts('min-1', { 'chall-a': { championId: AHRI, win: true } });
    await seedFacts('min-2', { 'chall-a': { championId: AHRI, win: true } });
    await seedFacts('min-3', { 'chall-a': { championId: GAREN, win: true } });
    await ladder({ 'chall-a': 'CHALLENGER' });
    await recomputeChampionStats(PLATFORM, QUEUE);

    const trimmed = (await get('minGames=2')).json() as { champions: { championId: number }[] };
    expect(trimmed.champions.map((c) => c.championId)).toEqual([AHRI]);

    const untrimmed = (await get('minGames=0')).json() as { champions: { championId: number }[] };
    expect(untrimmed.champions.map((c) => c.championId).sort()).toEqual([AHRI, GAREN].sort());
  });
});

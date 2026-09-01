import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { eq, inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import { KEY_SCOPE } from '../src/config.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { matchParticipants, matches, players } from '../src/db/schema.js';
import { backfillNamesFromArchive, getPlayer } from '../src/db/players.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * Naming a crawl's PUUIDs out of the archive, against the real Postgres — the
 * deliverable is one statement, and a mocked Postgres would only prove that
 * the string was passed along.
 *
 * The whole point of the feature is that it costs no upstream request, so
 * nothing here stubs a Riot call: there is none to stub. What the fixture
 * varies is the thing the query has to get right — *which* archived match a
 * name is read from.
 *
 * A platform nothing else touches, because every assertion below is a `players`
 * row and a dev database sharing this Postgres is full of real ones.
 */
const PLATFORM = 'names-test';
const MATCH_PREFIX = 'NAMES_TEST_';
const REGION = 'names-test-region';

let available = false;

interface SeedParticipant {
  puuid: string;
  gameName?: string | null;
  tagLine?: string | null;
  /** The pre-Riot-ID field, for the "not a fallback" case. */
  summonerName?: string;
  /** The transition-era spelling of `riotIdGameName`. */
  legacyName?: string;
}

/** One archived match. `endedAt` is what orders "most recent". */
async function archive(
  id: string,
  endedAt: number,
  participants: SeedParticipant[],
): Promise<void> {
  await db.insert(matches).values({
    matchId: MATCH_PREFIX + id,
    region: REGION,
    data: {
      metadata: { matchId: MATCH_PREFIX + id, participants: participants.map((p) => p.puuid) },
      info: {
        queueId: 420,
        gameVersion: '16.13.790.6961',
        gameEndTimestamp: endedAt,
        participants: participants.map((p) => ({
          puuid: p.puuid,
          championId: 103,
          win: true,
          ...(p.gameName !== undefined ? { riotIdGameName: p.gameName } : {}),
          ...(p.tagLine !== undefined ? { riotIdTagline: p.tagLine } : {}),
          ...(p.summonerName !== undefined ? { summonerName: p.summonerName } : {}),
          ...(p.legacyName !== undefined ? { riotIdName: p.legacyName } : {}),
        })),
      },
    },
  });

  await db.insert(matchParticipants).values(
    participants.map((p) => ({
      matchId: MATCH_PREFIX + id,
      puuid: p.puuid,
      championId: 103,
      win: true,
    })),
  );
}

/** A discovered player: a PUUID, a platform, and nothing else. */
async function discover(puuid: string, over: { gameName?: string; tagLine?: string } = {}) {
  await db.insert(players).values({ puuid, keyScope: KEY_SCOPE, platform: PLATFORM, ...over });
}

async function wipe(): Promise<void> {
  const ids = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .where(eq(matches.region, REGION));
  const mine = ids.map((r) => r.matchId);
  if (mine.length > 0) await db.delete(matches).where(inArray(matches.matchId, mine));
  await db.delete(players).where(eq(players.platform, PLATFORM));
}

beforeAll(async () => {
  available = await probeServices('player-names.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
});

beforeEach(async () => {
  if (available) await wipe();
});

afterAll(async () => {
  if (available) await wipe();
  await closeDb();
  await closeRedis();
});

describe('backfillNamesFromArchive', () => {
  it('names a discovered PUUID from its most recent archived match', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_p1');
    await archive('older', 1_756_000_000_000, [
      { puuid: 'NAMES_TEST_p1', gameName: 'OldName', tagLine: 'OCE' },
    ]);
    await archive('newer', 1_756_900_000_000, [
      { puuid: 'NAMES_TEST_p1', gameName: 'NewName', tagLine: 'OCE' },
    ]);

    const result = await backfillNamesFromArchive();
    expect(result.named).toBeGreaterThanOrEqual(1);

    const player = await getPlayer('NAMES_TEST_p1');
    // The newest match, not merely *a* match: a name is what the player was
    // called during that game, so the ordering is the accuracy.
    expect(player?.gameName).toBe('NewName');
    expect(player?.tagLine).toBe('OCE');
  });

  it('takes each player their own name out of a shared match', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_a');
    await discover('NAMES_TEST_b');
    await archive('shared', 1_756_000_000_000, [
      { puuid: 'NAMES_TEST_a', gameName: 'Ayy', tagLine: 'OCE' },
      { puuid: 'NAMES_TEST_b', gameName: 'Bee', tagLine: 'NA1' },
    ]);

    await backfillNamesFromArchive();

    expect((await getPlayer('NAMES_TEST_a'))?.gameName).toBe('Ayy');
    expect((await getPlayer('NAMES_TEST_b'))?.gameName).toBe('Bee');
  });

  it('leaves a player who already has a name alone', async ({ skip }) => {
    if (!available) skip();

    // What `account-v1` said, via a profile lookup. Authoritative in a way a
    // past game is not — a rename shows up here first.
    await discover('NAMES_TEST_named', { gameName: 'CurrentName', tagLine: 'OCE' });
    await archive('stale', 1_756_900_000_000, [
      { puuid: 'NAMES_TEST_named', gameName: 'NameFromLastWeek', tagLine: 'OCE' },
    ]);

    await backfillNamesFromArchive();

    expect((await getPlayer('NAMES_TEST_named'))?.gameName).toBe('CurrentName');
  });

  it('looks past recent matches whose Riot ID fields came back empty', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_empty');
    await archive('e1', 1_756_900_000_000, [
      { puuid: 'NAMES_TEST_empty', gameName: '', tagLine: '' },
    ]);
    await archive('e2', 1_756_800_000_000, [
      { puuid: 'NAMES_TEST_empty', gameName: '', tagLine: '' },
    ]);
    await archive('e3', 1_756_700_000_000, [
      { puuid: 'NAMES_TEST_empty', gameName: 'FoundIt', tagLine: 'OCE' },
    ]);

    await backfillNamesFromArchive();

    // Without the window this player would stay nameless forever: the target
    // set is deterministic, so every later pass would read the same two empty
    // matches and give up in the same place.
    expect((await getPlayer('NAMES_TEST_empty'))?.gameName).toBe('FoundIt');
  });

  it('reads the transition-era spelling, but never summonerName', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_legacy');
    await discover('NAMES_TEST_ancient');
    await archive('legacy', 1_756_000_000_000, [
      { puuid: 'NAMES_TEST_legacy', legacyName: 'Transitional', tagLine: 'OCE' },
      // No Riot ID at all — only the display name Riot stopped using. A name
      // with no tag is not the identity anything looks a player up by.
      { puuid: 'NAMES_TEST_ancient', summonerName: 'PreRiotId' },
    ]);

    await backfillNamesFromArchive();

    expect((await getPlayer('NAMES_TEST_legacy'))?.gameName).toBe('Transitional');
    expect((await getPlayer('NAMES_TEST_ancient'))?.gameName).toBeNull();
  });

  it('leaves a player with nothing archived nameless, and says so', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_unarchived');

    const result = await backfillNamesFromArchive();

    expect((await getPlayer('NAMES_TEST_unarchived'))?.gameName).toBeNull();
    // Counted as outstanding rather than failed: the archive stage may simply
    // not have reached them yet, and tomorrow's pass will.
    expect(result.unnamed).toBeGreaterThanOrEqual(1);
  });

  it('spends no upstream request', async ({ skip }) => {
    if (!available) skip();

    await discover('NAMES_TEST_quota');
    await archive('quota', 1_756_000_000_000, [
      { puuid: 'NAMES_TEST_quota', gameName: 'Free', tagLine: 'OCE' },
    ]);

    // The proof that this is not a Riot call in disguise: it resolves with the
    // limiter untouched, which is the entire reason to prefer it to
    // `account-v1` on a ladder-sized set of players.
    const before = await redis.keys('rl:*');
    await backfillNamesFromArchive();
    const after = await redis.keys('rl:*');

    expect(after).toEqual(before);
    expect((await getPlayer('NAMES_TEST_quota'))?.gameName).toBe('Free');
  });
});

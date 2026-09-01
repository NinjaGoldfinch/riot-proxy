import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { desc, eq, inArray } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { matchBans, matchParticipants, matches } from '../src/db/schema.js';
import { archiveMatch, reextractBatch, type RiotMatch } from '../src/db/matches.js';
import {
  clearReextractCursor,
  getReextractCursor,
  setReextractCursor,
} from '../src/jobs/facts-state.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * C2 (#110): the widened `match_participants` columns, `match_bans`, and the
 * `facts:reextract` walk that backfills both for rows archived before either
 * existed. Against the real Postgres and Redis — the deliverable is the SQL
 * `onConflictDoUpdate`/`onConflictDoNothing` actually does on a real conflict,
 * and the cursor's resumability is a Redis fact, not a TypeScript one.
 */
const REGION = 'facts-test';
/**
 * `reextractBatch` walks the *whole* `matches` table — by design, since fact
 * columns are universal, not key-scoped — so its pagination tests share the
 * table with whatever real archive a developer's own `npm run dev`/worker
 * has built. A leading `Z` sorts after every real Riot match id (they all
 * start with an uppercase platform code, none of which reaches `Z`), so a
 * batch that starts after the id captured by `latestMatchId()` below sees
 * only this suite's own rows regardless of how much other data exists.
 */
const MATCH_PREFIX = 'ZFACTS_TEST_';

let available = false;

async function wipe(): Promise<void> {
  const ids = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .where(eq(matches.region, REGION));
  const mine = ids.map((r) => r.matchId).filter((id) => id.startsWith(MATCH_PREFIX));
  if (mine.length > 0) await db.delete(matches).where(inArray(matches.matchId, mine));
}

beforeAll(async () => {
  available = await probeServices('matches-facts.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (available) await wipe();
});

beforeEach(async () => {
  if (available) {
    await wipe();
    await clearReextractCursor();
  }
});

afterAll(async () => {
  if (available) {
    await wipe();
    await clearReextractCursor();
  }
  await Promise.allSettled([closeRedis(), closeDb()]);
});

/** One participant, shaped like Riot's real payload (verified field names). */
function participant(puuid: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    puuid,
    championId: 103,
    win: true,
    teamId: 100,
    teamPosition: 'MIDDLE',
    kills: 8,
    deaths: 2,
    assists: 5,
    totalMinionsKilled: 180,
    neutralMinionsKilled: 20,
    goldEarned: 14_200,
    visionScore: 24,
    totalDamageDealtToChampions: 21_000,
    item0: 3020,
    item1: 4645,
    item2: 3157,
    item3: 3135,
    item4: 3089,
    item5: 3165,
    // The trinket. Never extracted — there is no item6 column.
    item6: 3364,
    summoner1Id: 4,
    summoner2Id: 14,
    perks: {
      styles: [
        { description: 'primaryStyle', style: 8200, selections: [{ perk: 8214 }] },
        { description: 'subStyle', style: 8100, selections: [{ perk: 8138 }] },
      ],
    },
    ...over,
  };
}

function match(
  id: string,
  participants: Record<string, unknown>[],
  teams: Record<string, unknown>[] = [],
): RiotMatch {
  return {
    metadata: { matchId: id, participants: participants.map((p) => p.puuid as string) },
    info: {
      participants,
      teams,
      queueId: 420,
      gameEndTimestamp: 1_756_000_000_000,
    },
  } as unknown as RiotMatch;
}

const puuid = (n: number) => `facts-test-puuid-${String(n).padStart(4, '0')}`;

/** The current end of the whole archive, so a batch starting just after it
 * sees only rows this test seeds — see the `MATCH_PREFIX` comment above. */
async function latestMatchId(): Promise<string | null> {
  const rows = await db
    .select({ matchId: matches.matchId })
    .from(matches)
    .orderBy(desc(matches.matchId))
    .limit(1);
  return rows[0]?.matchId ?? null;
}

async function participantRow(matchId: string, p: string) {
  const rows = await db
    .select()
    .from(matchParticipants)
    .where(eq(matchParticipants.matchId, matchId));
  return rows.find((r) => r.puuid === p);
}

describe('extracting participant facts', () => {
  it('extracts every widened column from a full payload', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}full-1`;
    await archiveMatch(id, REGION, match(id, [participant(puuid(1))]));

    const row = await participantRow(id, puuid(1));
    expect(row).toMatchObject({
      championId: 103,
      win: true,
      teamId: 100,
      teamPosition: 'MIDDLE',
      kills: 8,
      deaths: 2,
      assists: 5,
      cs: 200, // 180 + 20
      gold: 14_200,
      damage: 21_000,
      vision: 24,
      item0: 3020,
      item1: 4645,
      item2: 3157,
      item3: 3135,
      item4: 3089,
      item5: 3165,
      // The keystone is the primary style's first selection; sub_style_id is
      // the *tree*, not a specific rune.
      keystoneId: 8214,
      subStyleId: 8100,
      spell1: 4,
      spell2: 14,
    });
  });

  it('leaves placement and subteam_id null outside Arena', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}non-arena`;
    await archiveMatch(id, REGION, match(id, [participant(puuid(2))]));

    const row = await participantRow(id, puuid(2));
    expect(row?.placement).toBeNull();
    expect(row?.subteamId).toBeNull();
  });

  it("reads Arena's placement and playerSubteamId, not subteamId", async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}arena-1`;
    await archiveMatch(
      id,
      REGION,
      match(id, [
        participant(puuid(3), { placement: 2, playerSubteamId: 4, teamPosition: '' }),
      ]),
    );

    const row = await participantRow(id, puuid(3));
    expect(row?.placement).toBe(2);
    expect(row?.subteamId).toBe(4);
    expect(row?.teamPosition).toBe('');
  });

  it('leaves every fact column null for the metadata-only fallback', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}metadata-only`;
    // No info.participants at all — the shape `archiveMatch` sees when only
    // match ids, not full bodies, are known.
    await archiveMatch(id, REGION, { metadata: { matchId: id, participants: [puuid(4)] } });

    const row = await participantRow(id, puuid(4));
    expect(row).toMatchObject({
      championId: null,
      win: null,
      teamId: null,
      teamPosition: null,
      kills: null,
      cs: null,
      item0: null,
      keystoneId: null,
      placement: null,
    });
  });

  it('self-heals a pre-C2 row on re-archive, via onConflictDoUpdate', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}self-heal`;
    // Simulate what L5 archived: the row exists with only the four original
    // columns, because that is all the extraction wrote at the time.
    await db.insert(matches).values({ matchId: id, region: REGION, data: {} });
    await db
      .insert(matchParticipants)
      .values({ matchId: id, puuid: puuid(5), championId: 103, win: true });

    await archiveMatch(id, REGION, match(id, [participant(puuid(5))]));

    const row = await participantRow(id, puuid(5));
    expect(row).toMatchObject({ teamId: 100, kills: 8, gold: 14_200, keystoneId: 8214 });
  });
});

describe('extracting bans', () => {
  it('writes one row per ban, skipping empty slots', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}bans-1`;
    await archiveMatch(
      id,
      REGION,
      match(
        id,
        [participant(puuid(6))],
        [
          {
            teamId: 100,
            bans: [
              { championId: 64, pickTurn: 1 },
              { championId: -1, pickTurn: 2 }, // no pick made — skipped
            ],
          },
          { teamId: 200, bans: [{ championId: 238, pickTurn: 3 }] },
        ],
      ),
    );

    const rows = await db.select().from(matchBans).where(eq(matchBans.matchId, id));
    expect(rows.map((r) => `${r.teamId}:${r.pickTurn}:${r.championId}`).sort()).toEqual([
      '100:1:64',
      '200:3:238',
    ]);
  });

  it('writes nothing when there are no teams', async ({ skip }) => {
    if (!available) return skip();
    const id = `${MATCH_PREFIX}bans-none`;
    await archiveMatch(id, REGION, match(id, [participant(puuid(7))]));
    expect(await db.select().from(matchBans).where(eq(matchBans.matchId, id))).toEqual([]);
  });
});

describe('facts:reextract batches', () => {
  /** A pre-C2 row: the match and its bare participant row, no facts. */
  async function seedPreC2(id: string, p: string, richData: RiotMatch): Promise<void> {
    await db.insert(matches).values({ matchId: id, region: REGION, data: richData });
    await db.insert(matchParticipants).values({ matchId: id, puuid: p, championId: 103, win: true });
  }

  it('walks matches in id order and backfills their facts', async ({ skip }) => {
    if (!available) return skip();
    const baseline = await latestMatchId();
    const a = `${MATCH_PREFIX}reex-a`;
    const b = `${MATCH_PREFIX}reex-b`;
    const c = `${MATCH_PREFIX}reex-c`;
    await seedPreC2(a, puuid(10), match(a, [participant(puuid(10))]));
    await seedPreC2(b, puuid(11), match(b, [participant(puuid(11))]));
    await seedPreC2(c, puuid(12), match(c, [participant(puuid(12))]));

    const first = await reextractBatch(baseline, 2);
    expect(first.matchIds).toEqual([a, b]);
    expect(first.cursor).toBe(b);
    // Only what this batch covered is backfilled — proof the walk has not
    // silently touched the rest of the archive ahead of its cursor.
    expect((await participantRow(a, puuid(10)))?.gold).toBe(14_200);
    expect((await participantRow(b, puuid(11)))?.gold).toBe(14_200);
    expect((await participantRow(c, puuid(12)))?.gold).toBeNull();

    const second = await reextractBatch(first.cursor, 2);
    expect(second.matchIds).toEqual([c]);
    expect(second.cursor).toBe(c);
    expect((await participantRow(c, puuid(12)))?.gold).toBe(14_200);

    const third = await reextractBatch(second.cursor, 2);
    expect(third).toEqual({ matchIds: [], cursor: null });
  });

  it('resumes from a cursor rather than restarting the walk', async ({ skip }) => {
    if (!available) return skip();
    const baseline = await latestMatchId();
    const a = `${MATCH_PREFIX}resume-a`;
    const b = `${MATCH_PREFIX}resume-b`;
    await seedPreC2(a, puuid(20), match(a, [participant(puuid(20))]));
    await seedPreC2(b, puuid(21), match(b, [participant(puuid(21))]));

    // A crash after committing the first batch: the cursor is what a restart
    // reads to know it need not touch `a` again.
    const batch = await reextractBatch(baseline, 1);
    await setReextractCursor(batch.cursor!);

    const resumed = await getReextractCursor();
    const next = await reextractBatch(resumed, 10);
    expect(next.matchIds).toEqual([b]);
  });
});

describe('the reextract cursor', () => {
  it('starts at null', async ({ skip }) => {
    if (!available) return skip();
    expect(await getReextractCursor()).toBeNull();
  });

  it('remembers where a run left off, and clears on demand', async ({ skip }) => {
    if (!available) return skip();
    await setReextractCursor(`${MATCH_PREFIX}some-match`);
    expect(await getReextractCursor()).toBe(`${MATCH_PREFIX}some-match`);

    await clearReextractCursor();
    expect(await getReextractCursor()).toBeNull();
  });
});

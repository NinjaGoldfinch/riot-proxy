import './helpers/env.js';
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { and, eq, ne } from 'drizzle-orm';
import { probeServices } from './helpers/services.js';
import { KEY_SCOPE } from '../src/config.js';
import { closeDb, db, pingDb } from '../src/db/index.js';
import { ladderCrawls, leagueEntries, players } from '../src/db/schema.js';
import { getPlayer, setTracked, upsertDiscoveredPlayers, upsertPlayer } from '../src/db/players.js';
import {
  bumpCrawlCounters,
  countLeagueEntries,
  createCrawl,
  finishCrawl,
  getCrawl,
  getLatestCompletedCrawl,
  getLeagueEntry,
  getRunningCrawl,
  listCrawlBackfillCandidates,
  listCrawls,
  listLeagueEntries,
  upsertLeagueEntries,
  type LeagueEntryInput,
} from '../src/db/ladder.js';
import { CURSOR_TTL_S, clearCursors, getCursor, setCursor } from '../src/jobs/ladder-state.js';
import { closeRedis, redis } from '../src/redis.js';

/**
 * The ladder's storage (#87), against the real Postgres — the two rules worth
 * proving are both database behaviour, not TypeScript: one live crawl per
 * ladder is a partial unique index, and `first_seen`/`last_seen` diverging
 * across crawls is what an `ON CONFLICT` clause does or does not overwrite.
 * Neither survives being mocked.
 *
 * A platform nobody uses keeps this out of the way of dev data sharing the
 * database — and out of the way of the live-crawl index, which would otherwise
 * refuse a real crawl while the suite runs.
 */
const PLATFORM = 'ladder-test';
const QUEUE = 'RANKED_SOLO_5x5' as const;

let available = false;

const entry = (puuid: string, over: Partial<LeagueEntryInput> = {}): LeagueEntryInput => ({
  puuid,
  tier: 'DIAMOND',
  division: 'I',
  leaguePoints: 50,
  wins: 100,
  losses: 90,
  ...over,
});

async function wipe(): Promise<void> {
  await db.delete(leagueEntries).where(eq(leagueEntries.platform, PLATFORM));
  await db.delete(ladderCrawls).where(eq(ladderCrawls.platform, PLATFORM));
  await db.delete(players).where(eq(players.platform, PLATFORM));
}

beforeAll(async () => {
  available = await probeServices('ladder-storage.test.ts', async () => {
    await redis.ping();
    return pingDb();
  });
  if (available) await wipe();
});

beforeEach(async () => {
  if (available) await wipe();
});

afterAll(async () => {
  if (available) await wipe();
  await Promise.allSettled([closeRedis(), closeDb()]);
});

describe('crawl runs', () => {
  it('starts a crawl running, with its counters at zero', async ({ skip }) => {
    if (!available) return skip();
    const { crawl, created } = await createCrawl({
      platform: PLATFORM,
      queue: QUEUE,
      tierFloor: 'MASTER',
    });

    expect(created).toBe(true);
    expect(crawl.status).toBe('running');
    expect(crawl.keyScope).toBe(KEY_SCOPE);
    expect(crawl.tierFloor).toBe('MASTER');
    expect(crawl.finishedAt).toBeNull();
    expect([
      crawl.pagesFetched,
      crawl.entriesSeen,
      crawl.playersDiscovered,
      crawl.backfillsEnqueued,
    ]).toEqual([0, 0, 0, 0]);

    expect((await getCrawl(crawl.id))?.id).toBe(crawl.id);
    expect((await getRunningCrawl(PLATFORM, QUEUE))?.id).toBe(crawl.id);
  });

  it('refuses a second live crawl on the same ladder, and names the one running', async ({
    skip,
  }) => {
    if (!available) return skip();
    const first = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });

    // The floor differs, so this is not deduplication by payload — it is the
    // index refusing a second running row for the ladder.
    const second = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'IRON' });

    expect(second.created).toBe(false);
    expect(second.crawl.id).toBe(first.crawl.id);
    expect(second.crawl.tierFloor).toBe('MASTER');
    expect(await countCrawlRows()).toBe(1);
  });

  it('lets two workers race the same trigger without starting two crawls', async ({ skip }) => {
    if (!available) return skip();
    // The check the index replaces is read-then-insert, which both of these
    // would pass: they are in flight at the same time and neither can see the
    // other's row yet.
    const results = await Promise.all([
      createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' }),
      createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' }),
    ]);

    expect(results.filter((r) => r.created)).toHaveLength(1);
    expect(new Set(results.map((r) => r.crawl.id)).size).toBe(1);
    expect(await countCrawlRows()).toBe(1);
  });

  it('allows the next crawl once the last one ends — the index is partial', async ({ skip }) => {
    if (!available) return skip();
    const first = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    await finishCrawl(first.crawl.id, 'completed');

    const second = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    expect(second.created).toBe(true);
    expect(second.crawl.id).not.toBe(first.crawl.id);
    expect(await countCrawlRows()).toBe(2);
  });

  it('keeps separate ladders independent', async ({ skip }) => {
    if (!available) return skip();
    await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    const flex = await createCrawl({
      platform: PLATFORM,
      queue: 'RANKED_FLEX_SR',
      tierFloor: 'MASTER',
    });
    expect(flex.created).toBe(true);
    expect(await countCrawlRows()).toBe(2);
  });

  it('will not let a finished crawl be finished again', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    const cancelled = await finishCrawl(crawl.id, 'cancelled');
    expect(cancelled?.status).toBe('cancelled');
    expect(cancelled?.finishedAt).toBeInstanceOf(Date);

    // The last walk job finishing a moment after the cancel must not resurrect
    // the run as a success.
    expect(await finishCrawl(crawl.id, 'completed')).toBeUndefined();
    expect((await getCrawl(crawl.id))?.status).toBe('cancelled');
  });

  it('adds to counters in SQL, so concurrent walks do not overwrite each other', async ({
    skip,
  }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });

    // Read-modify-write would land on 1: every one of these reads zero.
    await Promise.all(
      Array.from({ length: 10 }, () => bumpCrawlCounters(crawl.id, { pagesFetched: 1 })),
    );
    await bumpCrawlCounters(crawl.id, { entriesSeen: 205, playersDiscovered: 3 });

    const after = await getCrawl(crawl.id);
    expect(after?.pagesFetched).toBe(10);
    expect(after?.entriesSeen).toBe(205);
    expect(after?.playersDiscovered).toBe(3);
    expect(after?.backfillsEnqueued).toBe(0);
  });

  it('lists runs newest first, and finds the newest completed one', async ({ skip }) => {
    if (!available) return skip();
    const older = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    await finishCrawl(older.crawl.id, 'completed');
    const newer = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    await finishCrawl(newer.crawl.id, 'completed');
    const running = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });

    const listed = await listCrawls({ platform: PLATFORM, queue: QUEUE });
    expect(listed.map((c) => c.id)).toEqual([running.crawl.id, newer.crawl.id, older.crawl.id]);

    // The running one is newer, and is deliberately not the answer: "the
    // ladder as it stands" means the last crawl that actually finished.
    expect((await getLatestCompletedCrawl(PLATFORM, QUEUE))?.id).toBe(newer.crawl.id);
  });
});

describe('league entries', () => {
  it('writes a page of entries and reads them back best first', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });

    const written = await upsertLeagueEntries(crawl.id, PLATFORM, QUEUE, [
      entry('puuid-low', { leaguePoints: 10 }),
      entry('puuid-high', { leaguePoints: 900, hotStreak: true }),
      entry('puuid-mid', { leaguePoints: 400 }),
    ]);
    expect(written).toBe(3);

    const ladder = await listLeagueEntries({ platform: PLATFORM, queue: QUEUE });
    expect(ladder.map((e) => e.puuid)).toEqual(['puuid-high', 'puuid-mid', 'puuid-low']);
    expect(ladder[0]?.hotStreak).toBe(true);
    expect(ladder[0]?.veteran).toBe(false);
    expect(ladder[0]?.keyScope).toBe(KEY_SCOPE);
    expect(ladder[0]?.firstSeenCrawlId).toBe(crawl.id);
    expect(ladder[0]?.lastSeenCrawlId).toBe(crawl.id);
  });

  it('batches past the 100-row limit rather than building one huge statement', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });

    // A Riot page is ~205 entries and these rows are 16 columns wide, so a
    // single insert would be over 3 000 bind parameters.
    const page = Array.from({ length: 205 }, (_, i) =>
      entry(`puuid-${String(i).padStart(3, '0')}`, { leaguePoints: i }),
    );
    expect(await upsertLeagueEntries(crawl.id, PLATFORM, QUEUE, page)).toBe(205);
    expect(await countLeagueEntries({ platform: PLATFORM, queue: QUEUE })).toBe(205);
  });

  it('writes nothing, and asks nothing, for an empty page', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    expect(await upsertLeagueEntries(crawl.id, PLATFORM, QUEUE, [])).toBe(0);
  });

  it('keeps first_seen and moves last_seen when a later crawl sees the same player', async ({
    skip,
  }) => {
    if (!available) return skip();
    const first = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    await upsertLeagueEntries(first.crawl.id, PLATFORM, QUEUE, [
      entry('puuid-climber', { tier: 'DIAMOND', division: 'IV', leaguePoints: 12, wins: 100 }),
    ]);
    await finishCrawl(first.crawl.id, 'completed');

    const second = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    await upsertLeagueEntries(second.crawl.id, PLATFORM, QUEUE, [
      entry('puuid-climber', {
        tier: 'MASTER',
        division: 'I',
        leaguePoints: 340,
        wins: 118,
        losses: 91,
        veteran: true,
      }),
    ]);

    const row = await getLeagueEntry(PLATFORM, QUEUE, 'puuid-climber');
    expect(row?.firstSeenCrawlId).toBe(first.crawl.id);
    expect(row?.lastSeenCrawlId).toBe(second.crawl.id);
    // Latest state, not history: the row is the player now, not a delta.
    expect(row?.tier).toBe('MASTER');
    expect(row?.division).toBe('I');
    expect(row?.leaguePoints).toBe(340);
    expect(row?.wins).toBe(118);
    expect(row?.veteran).toBe(true);
    expect(await countLeagueEntries({ platform: PLATFORM, queue: QUEUE })).toBe(1);
  });

  it('separates the current ladder from the players who fell off it', async ({ skip }) => {
    if (!available) return skip();
    const first = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    await upsertLeagueEntries(first.crawl.id, PLATFORM, QUEUE, [
      entry('puuid-stayed'),
      entry('puuid-decayed'),
    ]);
    await finishCrawl(first.crawl.id, 'completed');

    const second = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    await upsertLeagueEntries(second.crawl.id, PLATFORM, QUEUE, [
      entry('puuid-stayed'),
      entry('puuid-promoted'),
    ]);
    await finishCrawl(second.crawl.id, 'completed');

    const latest = await getLatestCompletedCrawl(PLATFORM, QUEUE);
    expect(latest?.id).toBe(second.crawl.id);

    const current = await listLeagueEntries({
      platform: PLATFORM,
      queue: QUEUE,
      crawlId: latest!.id,
    });
    expect(current.map((e) => e.puuid).sort()).toEqual(['puuid-promoted', 'puuid-stayed']);

    // The complement — the rows the newest crawl did not restamp — is the
    // "dropped or decayed" half, off the same index.
    const dropped = await db
      .select({ puuid: leagueEntries.puuid })
      .from(leagueEntries)
      .where(
        and(eq(leagueEntries.platform, PLATFORM), ne(leagueEntries.lastSeenCrawlId, latest!.id)),
      );
    expect(dropped.map((r) => r.puuid)).toEqual(['puuid-decayed']);
  });

  it('filters a slice of the ladder by tier and division', async ({ skip }) => {
    if (!available) return skip();
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'IRON' });
    await upsertLeagueEntries(crawl.id, PLATFORM, QUEUE, [
      entry('puuid-d1', { tier: 'DIAMOND', division: 'I' }),
      entry('puuid-d4', { tier: 'DIAMOND', division: 'IV' }),
      entry('puuid-iron', { tier: 'IRON', division: 'IV' }),
    ]);

    expect(await countLeagueEntries({ platform: PLATFORM, queue: QUEUE, tier: 'DIAMOND' })).toBe(2);
    const d4 = await listLeagueEntries({
      platform: PLATFORM,
      queue: QUEUE,
      tier: 'DIAMOND',
      division: 'IV',
    });
    expect(d4.map((e) => e.puuid)).toEqual(['puuid-d4']);
  });

  it('keeps the two queues apart for the same player', async ({ skip }) => {
    if (!available) return skip();
    const solo = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'DIAMOND' });
    const flex = await createCrawl({
      platform: PLATFORM,
      queue: 'RANKED_FLEX_SR',
      tierFloor: 'DIAMOND',
    });
    await upsertLeagueEntries(solo.crawl.id, PLATFORM, QUEUE, [
      entry('puuid-both', { tier: 'MASTER', leaguePoints: 300 }),
    ]);
    await upsertLeagueEntries(flex.crawl.id, PLATFORM, 'RANKED_FLEX_SR', [
      entry('puuid-both', { tier: 'GOLD', leaguePoints: 12 }),
    ]);

    expect((await getLeagueEntry(PLATFORM, QUEUE, 'puuid-both'))?.tier).toBe('MASTER');
    expect((await getLeagueEntry(PLATFORM, 'RANKED_FLEX_SR', 'puuid-both'))?.tier).toBe('GOLD');
  });
});

describe('page cursors', () => {
  const CRAWL = 'cursor-test-crawl';

  beforeEach(async () => {
    if (available) await clearCursors(CRAWL);
  });

  it('starts every walk at page 1', async ({ skip }) => {
    if (!available) return skip();
    expect(await getCursor(CRAWL, 'DIAMOND', 'I')).toBe(1);
  });

  it('remembers where a walk got to, per tier and division', async ({ skip }) => {
    if (!available) return skip();
    await setCursor(CRAWL, 'DIAMOND', 'I', 42);
    await setCursor(CRAWL, 'DIAMOND', 'IV', 7);

    expect(await getCursor(CRAWL, 'DIAMOND', 'I')).toBe(42);
    expect(await getCursor(CRAWL, 'DIAMOND', 'IV')).toBe(7);
    // A division nobody has walked is still at the start.
    expect(await getCursor(CRAWL, 'IRON', 'II')).toBe(1);
  });

  it('expires on its own, so a crashed crawl strands nothing permanently', async ({ skip }) => {
    if (!available) return skip();
    await setCursor(CRAWL, 'GOLD', 'III', 5);
    const ttl = await redis.ttl(`ladder:cursor:${KEY_SCOPE}:${CRAWL}:GOLD:III`);
    expect(ttl).toBeGreaterThan(CURSOR_TTL_S - 60);
    expect(ttl).toBeLessThanOrEqual(CURSOR_TTL_S);
  });

  it("clears one crawl's cursors and leaves another's alone", async ({ skip }) => {
    if (!available) return skip();
    const other = 'cursor-test-other';
    await setCursor(CRAWL, 'GOLD', 'I', 3);
    await setCursor(CRAWL, 'GOLD', 'II', 9);
    await setCursor(other, 'GOLD', 'I', 11);

    expect(await clearCursors(CRAWL)).toBe(2);
    expect(await getCursor(CRAWL, 'GOLD', 'I')).toBe(1);
    expect(await getCursor(other, 'GOLD', 'I')).toBe(11);

    await clearCursors(other);
  });
});

async function countCrawlRows(): Promise<number> {
  const rows = await db.select().from(ladderCrawls).where(eq(ladderCrawls.platform, PLATFORM));
  return rows.length;
}

/**
 * Discovery's half of the storage (#89). These are here rather than beside the
 * job tests because the rule that matters is not something the caller does —
 * it is what the `ON CONFLICT` clause is allowed to touch, which only a real
 * Postgres can answer.
 */
describe('discovered players', () => {
  const puuid = (n: number) => `ladder-test-puuid-${String(n).padStart(4, '0')}`;

  it('creates rows for players nobody had seen, untracked', async ({ skip }) => {
    if (!available) return skip();
    const rows = await upsertDiscoveredPlayers([
      { puuid: puuid(1), platform: PLATFORM },
      { puuid: puuid(2), platform: PLATFORM },
    ]);

    expect(rows.map((r) => r.puuid)).toEqual([puuid(1), puuid(2)]);
    expect(rows.every((r) => r.tracked === false)).toBe(true);
    expect(rows.every((r) => r.keyScope === KEY_SCOPE)).toBe(true);
    // Nothing has walked them yet, which is what a repeat crawl reads.
    expect(rows.every((r) => r.historyBackfillStartedAt === null)).toBe(true);
  });

  it('cannot turn tracking on, and cannot turn it off either', async ({ skip }) => {
    if (!available) return skip();
    // `tracked` is a 60-second spectator poll per player. A crawl that could
    // set it would sign up thousands of them; a crawl that could clear it
    // would silently stop polling someone an admin asked for.
    await upsertPlayer({
      puuid: puuid(3),
      platform: PLATFORM,
      gameName: 'Tracked',
      tagLine: 'OCE',
    });
    await setTracked(puuid(3), true);

    await upsertDiscoveredPlayers([{ puuid: puuid(3), platform: PLATFORM }]);

    const after = await getPlayer(puuid(3));
    expect(after?.tracked).toBe(true);
    // And the identity an admin track put there survives, the same way
    // `upsertPlayer` leaves absent fields alone.
    expect(after?.gameName).toBe('Tracked');
    expect(after?.tagLine).toBe('OCE');
  });

  it('reports what it found, so a repeat crawl knows whom it has walked', async ({ skip }) => {
    if (!available) return skip();
    const walkedAt = new Date('2026-08-01T00:00:00Z');
    await db
      .insert(players)
      .values({
        puuid: puuid(4),
        keyScope: KEY_SCOPE,
        platform: PLATFORM,
        historyBackfillStartedAt: walkedAt,
      })
      .onConflictDoNothing();

    const [row] = await upsertDiscoveredPlayers([{ puuid: puuid(4), platform: PLATFORM }]);
    expect(row?.historyBackfillStartedAt?.toISOString()).toBe(walkedAt.toISOString());
  });

  it('takes a Riot page in one go rather than 205 round trips', async ({ skip }) => {
    if (!available) return skip();
    const page = Array.from({ length: 205 }, (_, i) => ({
      puuid: puuid(100 + i),
      platform: PLATFORM,
    }));

    const rows = await upsertDiscoveredPlayers(page);
    expect(rows).toHaveLength(205);
    expect(new Set(rows.map((r) => r.puuid)).size).toBe(205);
  });

  it('writes nothing for an empty page', async ({ skip }) => {
    if (!available) return skip();
    expect(await upsertDiscoveredPlayers([])).toEqual([]);
  });
});

/**
 * Whom the collect stage will walk (§6 of docs/ladder-crawl-plan.md). The
 * query is the whole hand-off between the two stages: enumeration leaves the
 * ladder stamped with the crawl id, and this reads it back out.
 *
 * Against the real Postgres because every clause in it is a join or an index
 * predicate — the left join in particular, which is the difference between
 * finding the players a crawl just discovered and finding none of them.
 */
describe('the collect stage’s candidates', () => {
  const puuid = (n: number) => `ladder-cand-puuid-${String(n).padStart(4, '0')}`;

  /** A crawl with a ladder under it, ready to be read back. */
  async function stamped(entries: LeagueEntryInput[]) {
    const { crawl } = await createCrawl({ platform: PLATFORM, queue: QUEUE, tierFloor: 'MASTER' });
    await upsertLeagueEntries(crawl.id, PLATFORM, QUEUE, entries);
    return crawl;
  }

  const page = (crawlId: string, over: Record<string, unknown> = {}) =>
    listCrawlBackfillCandidates({
      crawlId,
      platform: PLATFORM,
      queue: QUEUE,
      notWalkedSince: new Date(),
      limit: 100,
      ...over,
    });

  const candidates = async (crawlId: string, over: Record<string, unknown> = {}) =>
    (await page(crawlId, over)).map((c) => c.puuid);

  it('finds the players this crawl just discovered', async ({ skip }) => {
    if (!available) return skip();
    // The left join earns its keep here: these players have no backfill stamp
    // at all, and an inner join would return nobody.
    const crawl = await stamped([
      entry(puuid(1), { tier: 'CHALLENGER', leaguePoints: 900 }),
      entry(puuid(2), { tier: 'MASTER', leaguePoints: 300 }),
    ]);
    await upsertDiscoveredPlayers([
      { puuid: puuid(1), platform: PLATFORM },
      { puuid: puuid(2), platform: PLATFORM },
    ]);

    // Best first, so a cancelled crawl has at least collected the top.
    expect(await candidates(crawl.id)).toEqual([puuid(1), puuid(2)]);
  });

  it('offers every tier the crawl stored, not just the apex', async ({ skip }) => {
    if (!available) return skip();
    // There is no backfill floor: whoever enumeration stored is walked. LP
    // still orders the page, so the Master outranks the Diamond here.
    const crawl = await stamped([
      entry(puuid(1), { tier: 'MASTER', leaguePoints: 300 }),
      entry(puuid(2), { tier: 'DIAMOND', leaguePoints: 99 }),
    ]);

    expect(await candidates(crawl.id)).toEqual([puuid(1), puuid(2)]);
  });

  it('skips a player already walked since the crawl started', async ({ skip }) => {
    if (!available) return skip();
    // Convergence, not thrash: a lookup that walked this player while the
    // crawl was enumerating has already produced their ids.
    const crawl = await stamped([
      entry(puuid(1), { tier: 'CHALLENGER', leaguePoints: 900 }),
      entry(puuid(2), { tier: 'CHALLENGER', leaguePoints: 800 }),
    ]);
    await upsertPlayer({ puuid: puuid(1), platform: PLATFORM });
    await db
      .update(players)
      .set({ historyBackfillStartedAt: new Date(Date.now() + 60_000) })
      .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid(1))));

    expect(await candidates(crawl.id)).toEqual([puuid(2)]);
  });

  it('walks a player again on the next crawl', async ({ skip }) => {
    if (!available) return skip();
    // The other half of the same rule. A walk from before this crawl started
    // is stale by definition, and the archive makes the repeat one request
    // plus whatever they have played since.
    const crawl = await stamped([entry(puuid(1), { tier: 'CHALLENGER', leaguePoints: 900 })]);
    await upsertPlayer({ puuid: puuid(1), platform: PLATFORM });
    await db
      .update(players)
      .set({ historyBackfillStartedAt: new Date(Date.now() - 86_400_000) })
      .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, puuid(1))));

    expect(await candidates(crawl.id)).toEqual([puuid(1)]);
  });

  it('offers only the ladder this crawl saw', async ({ skip }) => {
    if (!available) return skip();
    // A player who fell off between runs keeps their `league_entries` row —
    // that is what makes "dropped since" answerable — and must not be walked
    // by a crawl that did not see them.
    const first = await stamped([entry(puuid(1), { tier: 'CHALLENGER', leaguePoints: 900 })]);
    await finishCrawl(first.id, 'completed');
    const second = await stamped([entry(puuid(2), { tier: 'CHALLENGER', leaguePoints: 800 })]);

    expect(await candidates(second.id)).toEqual([puuid(2)]);
  });

  it('pages by cursor, without showing one player twice', async ({ skip }) => {
    if (!available) return skip();
    // Every entry on the same LP, which is what a sort key without a tie-break
    // cannot page through at all: the ties have no defined order, so "after
    // 500 LP" is either everyone or nobody.
    const crawl = await stamped(
      Array.from({ length: 30 }, (_, i) =>
        entry(puuid(200 + i), { tier: 'CHALLENGER', leaguePoints: 500 }),
      ),
    );

    const first = await page(crawl.id, { limit: 25 });
    const second = await page(crawl.id, { limit: 25, after: first.at(-1) });
    expect(first).toHaveLength(25);
    expect(second).toHaveLength(5);
    expect(new Set([...first, ...second].map((c) => c.puuid)).size).toBe(30);
  });

  it('does not skip a player when an earlier page has since been walked', async ({ skip }) => {
    if (!available) return skip();
    // The failure a cursor exists to prevent. The jobs this paging feeds start
    // running while it is still paging, and each stamps the players it takes —
    // which removes them from this query. Under OFFSET the remaining rows
    // shift left by however many were stamped, and the next page skips exactly
    // that many players, silently.
    const crawl = await stamped(
      Array.from({ length: 6 }, (_, i) =>
        entry(puuid(300 + i), { tier: 'CHALLENGER', leaguePoints: 900 - i }),
      ),
    );

    const first = await page(crawl.id, { limit: 3 });
    expect(first).toHaveLength(3);

    // The workers get through the first page while the fan-out is mid-flight.
    for (const c of first) {
      await upsertPlayer({ puuid: c.puuid, platform: PLATFORM });
      await db
        .update(players)
        .set({ historyBackfillStartedAt: new Date(Date.now() + 60_000) })
        .where(and(eq(players.keyScope, KEY_SCOPE), eq(players.puuid, c.puuid)));
    }

    const second = await page(crawl.id, { limit: 3, after: first.at(-1) });
    expect(second.map((c) => c.puuid)).toEqual([puuid(303), puuid(304), puuid(305)]);
  });
});

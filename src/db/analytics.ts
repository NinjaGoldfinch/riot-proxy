import { and, desc, eq, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import { QUEUE_IDS, type RankedQueue, type Tier } from '../riot/ladder.js';
import { db, sql } from './index.js';
import { championStats, type ChampionStat } from './schema.js';

/**
 * Reading the archive back (#90). Everything here is derived — `champion_stats`
 * holds nothing that could not be recomputed from `matches`,
 * `match_participants` and `league_entries`, which is what makes recomputing
 * it the whole strategy rather than a fallback.
 */

export interface RecomputeResult {
  platform: string;
  queue: RankedQueue;
  rows: number;
  games: number;
}

/**
 * Recompute one ladder's champion aggregates from the archive.
 *
 * One statement, and the join in the middle is the point of the whole feature:
 * `match_participants` knows which champion won, `matches` knows which game it
 * was, and `league_entries` is the only thing that can say at which *tier* it
 * was played.
 *
 * `mp.puuid = le.puuid` is safe across a key rotation without a `key_scope`
 * column on `match_participants`, and deliberately so. Match ids are not
 * encrypted, so the archive outlives a rotation; PUUIDs are, so participants
 * written under an old key simply fail to join the new key's ladder. Stale
 * rows fall out rather than being counted under a scope they never belonged
 * to.
 *
 * Delete-then-insert inside one transaction, scoped to this (platform, queue).
 * An upsert would leave behind rows for a (tier, patch, champion) combination
 * the recompute no longer produces — a champion disabled for a patch, or a
 * tier the crawl no longer reaches — and those would sit there looking current
 * forever.
 */
export async function recomputeChampionStats(
  platform: string,
  queue: RankedQueue,
): Promise<RecomputeResult> {
  const queueId = QUEUE_IDS[queue];

  const inserted = await sql.begin(async (tx) => {
    await tx`
      delete from champion_stats
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;

    return tx<{ games: number }[]>`
      insert into champion_stats
        (key_scope, platform, queue, tier, patch, champion_id, games, wins, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        le.tier,
        -- '16.13.790.6961' -> '16.13'. The build numbers behind it change
        -- within a patch and would shatter every group.
        split_part(m.data->'info'->>'gameVersion', '.', 1) || '.' ||
          split_part(m.data->'info'->>'gameVersion', '.', 2) as patch,
        mp.champion_id,
        count(*)::int as games,
        count(*) filter (where mp.win)::int as wins,
        now()
      from match_participants mp
      join matches m on m.match_id = mp.match_id
      join league_entries le
        on le.puuid = mp.puuid
       and le.key_scope = ${KEY_SCOPE}
       and le.platform = ${platform}
       and le.queue = ${queue}
      where mp.champion_id is not null
        and mp.win is not null
        and m.queue_id = ${queueId}
        and m.data->'info'->>'gameVersion' is not null
      group by 1, 2, 3, 4, 5, 6
      returning games
    `;
  });

  return {
    platform,
    queue,
    rows: inserted.length,
    games: inserted.reduce((total, row) => total + row.games, 0),
  };
}

export interface ChampionStatsFilter {
  platform: string;
  queue: string;
  tier?: Tier;
  patch?: string;
  limit?: number;
}

/** The newest patch this key scope has aggregated for a ladder. */
export async function latestPatch(platform: string, queue: string): Promise<string | undefined> {
  const rows = await db
    .select({ patch: championStats.patch })
    .from(championStats)
    .where(
      and(
        eq(championStats.keyScope, KEY_SCOPE),
        eq(championStats.platform, platform),
        eq(championStats.queue, queue),
      ),
    )
    .groupBy(championStats.patch)
    // Patches are `major.minor`, so a string sort puts 16.9 above 16.10.
    // Sorting on the numbers is the difference between "the latest patch" and
    // "the latest patch that happens to sort last".
    .orderBy(
      desc(raw`split_part(${championStats.patch}, '.', 1)::int`),
      desc(raw`split_part(${championStats.patch}, '.', 2)::int`),
    )
    .limit(1);
  return rows[0]?.patch;
}

/** One slice of the aggregate, most-played first. */
export async function listChampionStats(filter: ChampionStatsFilter): Promise<ChampionStat[]> {
  const where = [
    eq(championStats.keyScope, KEY_SCOPE),
    eq(championStats.platform, filter.platform),
    eq(championStats.queue, filter.queue),
  ];
  if (filter.tier) where.push(eq(championStats.tier, filter.tier));
  if (filter.patch) where.push(eq(championStats.patch, filter.patch));

  return db
    .select()
    .from(championStats)
    .where(and(...where))
    .orderBy(desc(championStats.games))
    .limit(filter.limit ?? 500);
}

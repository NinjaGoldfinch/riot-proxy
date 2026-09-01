import { and, desc, eq, gte, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import { QUEUE_IDS, type RankedQueue, type Tier } from '../riot/ladder.js';
import { db, sql } from './index.js';
import { analyticsSlices, championBans, championStats } from './schema.js';

/**
 * Reading the archive back (#90). Everything here is derived — nothing in
 * `champion_stats`, `analytics_slices` or `champion_bans` could not be
 * recomputed from `matches`, `match_participants`, `match_bans` and
 * `league_entries`, which is what makes recomputing them the whole strategy
 * rather than a fallback.
 */

export interface RecomputeResult {
  platform: string;
  queue: RankedQueue;
  rows: number;
  games: number;
}

/**
 * Recompute one ladder's champion aggregates from the archive (#111).
 *
 * Three delete-then-insert steps in one transaction, scoped to this
 * (platform, queue) — an upsert at any of the three would leave behind rows
 * for a (tier, patch, …) combination the recompute no longer produces (a
 * champion disabled for a patch, or a tier the crawl no longer reaches), and
 * those would sit there looking current forever.
 *
 * 1. `analytics_slices` — for each (tier, patch), how many distinct matches
 *    had a participant the ladder placed at that tier. The denominator every
 *    rate below divides into.
 * 2. `champion_stats` — one row per (tier, patch, champion, role), summing
 *    facts only where `match_participants` actually has them (`sum` skips
 *    `null` on its own); `stated_games` counts how many rows did. `duration_s`
 *    is masked the same way `kills` is, deliberately: a pre-C2 row's game
 *    still has a duration in `matches`, and letting it into the denominator
 *    while its `cs`/`gold` stay excluded from the numerator would understate
 *    every per-minute stat rather than merely omitting the unswept games.
 * 3. `champion_bans` — bans joined through `match_participants` to reach
 *    `league_entries`' tier, the same way a match reaches a tier for the
 *    slice above. A ban has no role and no champion pick to key off, which is
 *    why it is not a role row of `champion_stats`.
 *
 * `mp.puuid = le.puuid` is safe across a key rotation without a `key_scope`
 * column on `match_participants`, and deliberately so — see `matches.ts`.
 */
export async function recomputeChampionStats(
  platform: string,
  queue: RankedQueue,
): Promise<RecomputeResult> {
  const queueId = QUEUE_IDS[queue];

  const inserted = await sql.begin(async (tx) => {
    await tx`
      delete from analytics_slices
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    await tx`
      insert into analytics_slices (key_scope, platform, queue, tier, patch, matches, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        le.tier,
        m.patch,
        count(distinct m.match_id)::int as matches,
        now()
      from match_participants mp
      join matches m on m.match_id = mp.match_id
      join league_entries le
        on le.puuid = mp.puuid
       and le.key_scope = ${KEY_SCOPE}
       and le.platform = ${platform}
       and le.queue = ${queue}
      where m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5
    `;

    await tx`
      delete from champion_stats
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    const statsRows = await tx<{ games: number }[]>`
      insert into champion_stats
        (key_scope, platform, queue, tier, patch, champion_id, role,
         games, wins, matches_picked, stated_games,
         kills, deaths, assists, cs, gold, damage, vision, duration_s,
         computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        le.tier,
        m.patch,
        mp.champion_id,
        coalesce(mp.team_position, '') as role,
        count(*)::int as games,
        count(*) filter (where mp.win)::int as wins,
        count(distinct m.match_id)::int as matches_picked,
        count(*) filter (where mp.kills is not null)::int as stated_games,
        -- sum() over an all-null group is null, not 0 — every column here is
        -- NOT NULL, so a champion/tier/patch bucket with no swept rows yet
        -- needs the coalesce as much as a genuinely empty one would.
        coalesce(sum(mp.kills), 0)::bigint,
        coalesce(sum(mp.deaths), 0)::bigint,
        coalesce(sum(mp.assists), 0)::bigint,
        coalesce(sum(mp.cs), 0)::bigint,
        coalesce(sum(mp.gold), 0)::bigint,
        coalesce(sum(mp.damage), 0)::bigint,
        coalesce(sum(mp.vision), 0)::bigint,
        coalesce(sum(m.game_duration) filter (where mp.kills is not null), 0)::bigint,
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
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6, 7
      returning games
    `;

    await tx`
      delete from champion_bans
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    await tx`
      insert into champion_bans (key_scope, platform, queue, tier, patch, champion_id, bans, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        le.tier,
        m.patch,
        mb.champion_id,
        count(distinct mb.match_id)::int as bans,
        now()
      from match_bans mb
      join matches m on m.match_id = mb.match_id
      join match_participants mp on mp.match_id = mb.match_id
      join league_entries le
        on le.puuid = mp.puuid
       and le.key_scope = ${KEY_SCOPE}
       and le.platform = ${platform}
       and le.queue = ${queue}
      where m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6
    `;

    return statsRows;
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
  /** `match_participants.team_position` verbatim, or `''`. Omitted sums every
   * role's rows into one — see `listChampionStats`. */
  role?: string;
  /** Groups whose summed `games` falls short are dropped, not zeroed. */
  minGames?: number;
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

export interface ChampionStatRow {
  championId: number;
  tier: string;
  patch: string;
  games: number;
  wins: number;
  matchesPicked: number;
  statedGames: number;
  kills: number;
  deaths: number;
  assists: number;
  cs: number;
  gold: number;
  damage: number;
  vision: number;
  durationS: number;
  computedAt: Date;
}

/**
 * One slice of the aggregate, most-played first (#111).
 *
 * Always grouped by `(champion_id, tier, patch)` and summed, whether or not
 * `role` is filtered: with a role in the `WHERE`, the primary key guarantees
 * at most one stored row per group, so the sum is that row's own value.
 * Without one, the same query sums every role's row into one — the "role
 * rollup" the plan calls for, with no separate rollup rows ever stored (§6.1)
 * and so nothing that can double-count on a careless read.
 */
export async function listChampionStats(filter: ChampionStatsFilter): Promise<ChampionStatRow[]> {
  const where = [
    eq(championStats.keyScope, KEY_SCOPE),
    eq(championStats.platform, filter.platform),
    eq(championStats.queue, filter.queue),
  ];
  if (filter.tier) where.push(eq(championStats.tier, filter.tier));
  if (filter.patch) where.push(eq(championStats.patch, filter.patch));
  if (filter.role !== undefined) where.push(eq(championStats.role, filter.role));

  return db
    .select({
      championId: championStats.championId,
      tier: championStats.tier,
      patch: championStats.patch,
      games: raw<number>`sum(${championStats.games})`.mapWith(Number),
      wins: raw<number>`sum(${championStats.wins})`.mapWith(Number),
      matchesPicked: raw<number>`sum(${championStats.matchesPicked})`.mapWith(Number),
      statedGames: raw<number>`sum(${championStats.statedGames})`.mapWith(Number),
      kills: raw<number>`sum(${championStats.kills})`.mapWith(Number),
      deaths: raw<number>`sum(${championStats.deaths})`.mapWith(Number),
      assists: raw<number>`sum(${championStats.assists})`.mapWith(Number),
      cs: raw<number>`sum(${championStats.cs})`.mapWith(Number),
      gold: raw<number>`sum(${championStats.gold})`.mapWith(Number),
      damage: raw<number>`sum(${championStats.damage})`.mapWith(Number),
      vision: raw<number>`sum(${championStats.vision})`.mapWith(Number),
      durationS: raw<number>`sum(${championStats.durationS})`.mapWith(Number),
      // `sql<Date>` is a compile-time hint only, not a runtime parser — this
      // came back a string when tried without the explicit mapper, so the
      // conversion stays here rather than trusting it implicitly.
      computedAt: raw<string>`max(${championStats.computedAt})`.mapWith((v) => new Date(v)),
    })
    .from(championStats)
    .where(and(...where))
    .groupBy(championStats.championId, championStats.tier, championStats.patch)
    .having(gte(raw`sum(${championStats.games})`, filter.minGames ?? 0))
    .orderBy(desc(raw`sum(${championStats.games})`))
    .limit(filter.limit ?? 500);
}

export interface AnalyticsSliceFilter {
  platform: string;
  queue: string;
  patch: string;
  tier?: Tier;
}

/** Slice denominators for one patch, every tier the recompute reached — the
 * caller builds a per-tier lookup rather than asking once per row. */
export async function listAnalyticsSlices(
  filter: AnalyticsSliceFilter,
): Promise<{ tier: string; matches: number }[]> {
  const where = [
    eq(analyticsSlices.keyScope, KEY_SCOPE),
    eq(analyticsSlices.platform, filter.platform),
    eq(analyticsSlices.queue, filter.queue),
    eq(analyticsSlices.patch, filter.patch),
  ];
  if (filter.tier) where.push(eq(analyticsSlices.tier, filter.tier));

  return db
    .select({ tier: analyticsSlices.tier, matches: analyticsSlices.matches })
    .from(analyticsSlices)
    .where(and(...where));
}

/** Same shape as `listAnalyticsSlices` — one lookup, every tier's ban counts. */
export async function listChampionBans(
  filter: AnalyticsSliceFilter,
): Promise<{ tier: string; championId: number; bans: number }[]> {
  const where = [
    eq(championBans.keyScope, KEY_SCOPE),
    eq(championBans.platform, filter.platform),
    eq(championBans.queue, filter.queue),
    eq(championBans.patch, filter.patch),
  ];
  if (filter.tier) where.push(eq(championBans.tier, filter.tier));

  return db
    .select({
      tier: championBans.tier,
      championId: championBans.championId,
      bans: championBans.bans,
    })
    .from(championBans)
    .where(and(...where));
}

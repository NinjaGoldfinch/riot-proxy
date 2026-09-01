import { and, desc, eq, gte, sql as raw } from 'drizzle-orm';
import { KEY_SCOPE } from '../config.js';
import { QUEUE_IDS, type RankedQueue, type Tier } from '../riot/ladder.js';
import { db, sql } from './index.js';
import {
  analyticsSlices,
  championBans,
  championItems,
  championMatchups,
  championRunes,
  championSpells,
  championStats,
} from './schema.js';

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

export interface RecomputeCountResult {
  platform: string;
  queue: RankedQueue;
  rows: number;
}

/**
 * Recompute one ladder's lane matchups (#112) — its own transaction, in the
 * same maintenance run as `recomputeChampionStats` rather than inside it: a
 * crash here leaves matchups stale behind a fresh `champion_stats`, which
 * `computed_at` makes visible, and the next run converges (§9.1 of the plan).
 *
 * A self-join on `match_participants`, matched by `match_id` and lane
 * (`team_position`), opposite `team_id`. The join is symmetric — for any pair
 * it matches (A, B) it also matches (B, A) — so both directions are stored
 * without a second pass: `champion_id` is always "whose row this is",
 * `opponent_id` the lane rival.
 *
 * `lane_counts` guards the one way a self-join like this silently lies:
 * Riot's own position inference is supposed to assign each `team_position`
 * to exactly one participant per team, but nothing here enforces that
 * upstream, and a match where it fails would otherwise fan out — two
 * `MIDDLE` players on one team each joining both `MIDDLE` players on the
 * other counts one real match as up to four matchups. Requiring exactly one
 * occupant on both sides of the join excludes the ambiguous lane rather than
 * guessing at it; the match's other, unambiguous lanes are unaffected.
 */
export async function recomputeChampionMatchups(
  platform: string,
  queue: RankedQueue,
): Promise<RecomputeCountResult> {
  const queueId = QUEUE_IDS[queue];

  const inserted = await sql.begin(async (tx) => {
    await tx`
      delete from champion_matchups
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    return tx<{ championId: number }[]>`
      with lane_counts as (
        select match_id, team_id, team_position, count(*) as n
        from match_participants
        where team_position is not null and team_position <> '' and team_id is not null
        group by match_id, team_id, team_position
      )
      insert into champion_matchups
        (key_scope, platform, queue, patch, role, champion_id, opponent_id, games, wins, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        m.patch,
        a.team_position,
        a.champion_id,
        b.champion_id,
        count(*)::int as games,
        count(*) filter (where a.win)::int as wins,
        now()
      from match_participants a
      join match_participants b
        on a.match_id = b.match_id
       and a.team_position = b.team_position
       and a.team_id <> b.team_id
      join lane_counts la
        on la.match_id = a.match_id and la.team_id = a.team_id and la.team_position = a.team_position
      join lane_counts lb
        on lb.match_id = b.match_id and lb.team_id = b.team_id and lb.team_position = b.team_position
      join matches m on m.match_id = a.match_id
      join league_entries le
        on le.puuid = a.puuid
       and le.key_scope = ${KEY_SCOPE}
       and le.platform = ${platform}
       and le.queue = ${queue}
      where a.champion_id is not null
        and b.champion_id is not null
        and a.win is not null
        and a.team_position is not null
        and a.team_position <> ''
        and la.n = 1
        and lb.n = 1
        and m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6, 7
      returning champion_id
    `;
  });

  return { platform, queue, rows: inserted.length };
}

export interface RecomputeBuildsResult {
  platform: string;
  queue: RankedQueue;
  items: number;
  runes: number;
  spells: number;
}

/**
 * Recompute one ladder's build frequency tables (#112) — items, runes and
 * spells, each its own transaction (§9.1 of the plan), same reasoning as
 * `recomputeChampionMatchups`.
 *
 * Unlike matchups, `role` is `coalesce`d to `''` here: a build choice applies
 * to every participant, laned or not, the same way `champion_stats` treats it.
 */
export async function recomputeChampionBuilds(
  platform: string,
  queue: RankedQueue,
): Promise<RecomputeBuildsResult> {
  const queueId = QUEUE_IDS[queue];

  const items = await sql.begin(async (tx) => {
    await tx`
      delete from champion_items
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    // Final items only: one row per non-empty item0..5 slot, but a
    // participant who holds the same item in two slots (a second Control
    // Ward, a second potion) must still count as one game for that item —
    // `count(distinct (match_id, puuid))` rather than `count(*)` is what
    // keeps "games" meaning "games this was in the build" and not "slots it
    // filled", the same convention every other count in this feature uses.
    // The trinket (item6) is never a build choice, so it is not in the array
    // to unnest.
    return tx<{ championId: number }[]>`
      insert into champion_items
        (key_scope, platform, queue, patch, champion_id, role, item_id, games, wins, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        m.patch,
        mp.champion_id,
        coalesce(mp.team_position, ''),
        item.id,
        count(distinct (mp.match_id, mp.puuid))::int as games,
        count(distinct (mp.match_id, mp.puuid)) filter (where mp.win)::int as wins,
        now()
      from match_participants mp
      join matches m on m.match_id = mp.match_id
      join league_entries le
        on le.puuid = mp.puuid
       and le.key_scope = ${KEY_SCOPE}
       and le.platform = ${platform}
       and le.queue = ${queue}
      cross join lateral unnest(array[mp.item0, mp.item1, mp.item2, mp.item3, mp.item4, mp.item5]) as item(id)
      where mp.champion_id is not null
        and mp.win is not null
        and item.id is not null
        and item.id <> 0
        and m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6, 7
      returning champion_id
    `;
  });

  const runes = await sql.begin(async (tx) => {
    await tx`
      delete from champion_runes
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    return tx<{ championId: number }[]>`
      insert into champion_runes
        (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id,
         games, wins, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        m.patch,
        mp.champion_id,
        coalesce(mp.team_position, ''),
        mp.keystone_id,
        mp.sub_style_id,
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
        and mp.keystone_id is not null
        and mp.sub_style_id is not null
        and m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6, 7, 8
      returning champion_id
    `;
  });

  const spells = await sql.begin(async (tx) => {
    await tx`
      delete from champion_spells
      where key_scope = ${KEY_SCOPE} and platform = ${platform} and queue = ${queue}
    `;
    // least/greatest normalise the pair's order, so the two summoner-spell
    // slots collapse to one row regardless of which slot each spell landed in.
    return tx<{ championId: number }[]>`
      insert into champion_spells
        (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b,
         games, wins, computed_at)
      select
        le.key_scope,
        le.platform,
        le.queue,
        m.patch,
        mp.champion_id,
        coalesce(mp.team_position, ''),
        least(mp.spell1, mp.spell2),
        greatest(mp.spell1, mp.spell2),
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
        and mp.spell1 is not null
        and mp.spell2 is not null
        and m.queue_id = ${queueId}
        and m.patch is not null
      group by 1, 2, 3, 4, 5, 6, 7, 8
      returning champion_id
    `;
  });

  return { platform, queue, items: items.length, runes: runes.length, spells: spells.length };
}

export interface ChampionStatsFilter {
  platform: string;
  queue: string;
  tier?: Tier;
  patch?: string;
  /** `match_participants.team_position` verbatim, or `''`. Omitted sums every
   * role's rows into one — see `listChampionStats`. */
  role?: string;
  /** Narrows to one champion — the detail composite's own stat row(s). */
  championId?: number;
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
  if (filter.championId !== undefined) where.push(eq(championStats.championId, filter.championId));

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

export interface ChampionMatchupFilter {
  platform: string;
  queue: string;
  patch: string;
  championId: number;
  role?: string;
  minGames?: number;
  limit?: number;
}

export interface ChampionMatchupRow {
  role: string;
  opponentId: number;
  games: number;
  wins: number;
  computedAt: Date;
}

/**
 * One champion's lane matchups, most-played first (#112).
 *
 * Read verbatim, not summed like `listChampionStats`: a matchup is a
 * statement about one lane, and "Ahri vs Yasuo" in `MIDDLE` and a same-named
 * pairing that happened to occur in `JUNGLE` are different matchups, not the
 * same one counted twice — so `role` stays a per-row field rather than
 * something to roll up when it is not filtered.
 */
export async function listChampionMatchups(
  filter: ChampionMatchupFilter,
): Promise<ChampionMatchupRow[]> {
  const where = [
    eq(championMatchups.keyScope, KEY_SCOPE),
    eq(championMatchups.platform, filter.platform),
    eq(championMatchups.queue, filter.queue),
    eq(championMatchups.patch, filter.patch),
    eq(championMatchups.championId, filter.championId),
  ];
  if (filter.role !== undefined) where.push(eq(championMatchups.role, filter.role));
  if (filter.minGames) where.push(gte(championMatchups.games, filter.minGames));

  return db
    .select({
      role: championMatchups.role,
      opponentId: championMatchups.opponentId,
      games: championMatchups.games,
      wins: championMatchups.wins,
      computedAt: championMatchups.computedAt,
    })
    .from(championMatchups)
    .where(and(...where))
    .orderBy(desc(championMatchups.games))
    .limit(filter.limit ?? 50);
}

export interface ChampionBuildFilter {
  platform: string;
  queue: string;
  patch: string;
  championId: number;
  role?: string;
  minGames?: number;
  limit?: number;
}

/**
 * A champion's most-held final items, most-played first (#112).
 *
 * Grouped and summed like `listChampionStats`, for the same reason: unlike a
 * lane matchup, "what does this champion build" is a meaningful question
 * whether or not a role is filtered, so an unfiltered read sums every role's
 * rows into one rather than returning a role-shredded list.
 */
export async function listChampionItems(
  filter: ChampionBuildFilter,
): Promise<{ itemId: number; games: number; wins: number }[]> {
  const where = [
    eq(championItems.keyScope, KEY_SCOPE),
    eq(championItems.platform, filter.platform),
    eq(championItems.queue, filter.queue),
    eq(championItems.patch, filter.patch),
    eq(championItems.championId, filter.championId),
  ];
  if (filter.role !== undefined) where.push(eq(championItems.role, filter.role));

  return db
    .select({
      itemId: championItems.itemId,
      games: raw<number>`sum(${championItems.games})`.mapWith(Number),
      wins: raw<number>`sum(${championItems.wins})`.mapWith(Number),
    })
    .from(championItems)
    .where(and(...where))
    .groupBy(championItems.itemId)
    .having(gte(raw`sum(${championItems.games})`, filter.minGames ?? 0))
    .orderBy(desc(raw`sum(${championItems.games})`))
    .limit(filter.limit ?? 10);
}

/** A champion's most-run keystone/sub-style pairs — see `listChampionItems`. */
export async function listChampionRunes(
  filter: ChampionBuildFilter,
): Promise<{ keystoneId: number; subStyleId: number; games: number; wins: number }[]> {
  const where = [
    eq(championRunes.keyScope, KEY_SCOPE),
    eq(championRunes.platform, filter.platform),
    eq(championRunes.queue, filter.queue),
    eq(championRunes.patch, filter.patch),
    eq(championRunes.championId, filter.championId),
  ];
  if (filter.role !== undefined) where.push(eq(championRunes.role, filter.role));

  return db
    .select({
      keystoneId: championRunes.keystoneId,
      subStyleId: championRunes.subStyleId,
      games: raw<number>`sum(${championRunes.games})`.mapWith(Number),
      wins: raw<number>`sum(${championRunes.wins})`.mapWith(Number),
    })
    .from(championRunes)
    .where(and(...where))
    .groupBy(championRunes.keystoneId, championRunes.subStyleId)
    .having(gte(raw`sum(${championRunes.games})`, filter.minGames ?? 0))
    .orderBy(desc(raw`sum(${championRunes.games})`))
    .limit(filter.limit ?? 10);
}

/** A champion's most-run summoner spell pairs — see `listChampionItems`. */
export async function listChampionSpells(
  filter: ChampionBuildFilter,
): Promise<{ spellA: number; spellB: number; games: number; wins: number }[]> {
  const where = [
    eq(championSpells.keyScope, KEY_SCOPE),
    eq(championSpells.platform, filter.platform),
    eq(championSpells.queue, filter.queue),
    eq(championSpells.patch, filter.patch),
    eq(championSpells.championId, filter.championId),
  ];
  if (filter.role !== undefined) where.push(eq(championSpells.role, filter.role));

  return db
    .select({
      spellA: championSpells.spellA,
      spellB: championSpells.spellB,
      games: raw<number>`sum(${championSpells.games})`.mapWith(Number),
      wins: raw<number>`sum(${championSpells.wins})`.mapWith(Number),
    })
    .from(championSpells)
    .where(and(...where))
    .groupBy(championSpells.spellA, championSpells.spellB)
    .having(gte(raw`sum(${championSpells.games})`, filter.minGames ?? 0))
    .orderBy(desc(raw`sum(${championSpells.games})`))
    .limit(filter.limit ?? 10);
}

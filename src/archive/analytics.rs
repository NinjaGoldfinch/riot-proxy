//! The analytics tables (V0005, ADR-056; v1 `db/analytics.ts`): rebuilt from
//! `match_facts` per ladder (platform, queue) and read by `/v1/lol/analytics/*`.
//!
//! Every participant of the platform's archived matches counts (ADR-105), at
//! the tier stamped when the match was archived (`match_facts.tier`, THR-02,
//! ADR-127, ADR-138): the newer of the ladder's and the last league lookup's then, or
//! [`UNKNOWN_TIER`] when neither had them. A match with players in several
//! tiers counts in each (v1); `analytics_match_totals` and
//! `champion_ban_totals` count it once, for rows summed over every tier
//! (ADR-123). The per-participant tables (stats, matchups, items, runes,
//! spells, set builds) are keyed by the row's own player's tier and side as
//! well (FLT-01, ADR-129), so a read sums over whichever it doesn't name;
//! `champion_stats_totals` holds the stats already summed over tier, for the
//! reads that name none (PERF-02, ADR-168). Every table is
//! rebuilt wholesale for the newest `AGGREGATE_PATCH_LIMIT` patches (0: all)
//! and keeps older patches' rows. Every row carries `remake`, so a read can
//! leave remakes out (the default) or add them back.

use std::collections::BTreeSet;

use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};

use crate::db::{Db, DbError};
use crate::riot::ladder;

pub mod delta;

/// One ladder's rebuild.
#[derive(Debug, Clone)]
pub struct Scope {
    pub key_scope: String,
    pub platform: String,
    pub queue: String,
    /// match-v5's queue id for `queue` (420 / 440).
    pub queue_id: i64,
    /// The patches rebuilt, newest first; `None` rebuilds every patch.
    pub patches: Option<Vec<String>>,
    pub now: i64,
}

/// SQL ordering for `major.minor`: string order puts 16.9 above 16.10 (v1).
pub(crate) const PATCH_DESC: &str = "CAST(substr(patch, 1, instr(patch, '.') - 1) AS INTEGER) DESC, \
     CAST(substr(patch, instr(patch, '.') + 1) AS INTEGER) DESC";

/// The newest `limit` patches archived for a queue, any platform (v1
/// `recentPatches`); `None` when `limit` is 0, meaning every patch.
pub fn recent_patches(c: &Connection, queue_id: i64, limit: u32) -> Result<Option<Vec<String>>, DbError> {
    if limit == 0 {
        return Ok(None);
    }
    let mut stmt = c.prepare(&format!(
        "SELECT patch FROM (SELECT DISTINCT patch FROM matches WHERE queue_id = ?1 AND patch IS NOT NULL)
          ORDER BY {PATCH_DESC} LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![queue_id, limit], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(Some(rows))
}

impl Scope {
    /// The rebuilt patches as a JSON array for `json_each`, or `NULL` for all.
    fn patches_json(&self) -> Option<String> {
        self.patches
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "[]".into()))
    }

    /// Delete one table's rows for this ladder and its rebuilt patches.
    fn clear(&self, c: &Connection, table: &str) -> Result<(), DbError> {
        c.execute(
            &format!(
                "DELETE FROM {table} WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
                   AND (?4 IS NULL OR patch IN (SELECT value FROM json_each(?4)))"
            ),
            params![self.key_scope, self.platform, self.queue, self.patches_json()],
        )?;
        Ok(())
    }

    /// Run an `INSERT … SELECT` whose `?1`–`?6` are this scope's key scope,
    /// platform, queue, queue id, patches and timestamp. Returns rows written.
    fn insert(&self, c: &Connection, sql: &str) -> Result<i64, DbError> {
        let n = c.execute(
            sql,
            params![
                self.key_scope,
                self.platform,
                self.queue,
                self.queue_id,
                self.patches_json(),
                self.now
            ],
        )?;
        Ok(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

/// The tier of a participant neither the ladder nor a league lookup has
/// placed (ADR-105). Not one of Riot's tiers.
pub const UNKNOWN_TIER: &str = "UNKNOWN";

/// A fact's tier in [`ladder_facts`]: its stamp (THR-02, on the row since DSK-04).
const TIER: &str = "f.tier";

/// A fact's side in [`ladder_facts`] (FLT-01): team 100 is blue, 200 red
/// (design/10 §Scoreboard layout).
const SIDE: &str = "CASE f.team_id WHEN 100 THEN 'blue' ELSE 'red' END";

/// The platform's matches only: a match id starts with its platform (`OC1_…`).
/// Binds `?2` platform.
const ON_PLATFORM: &str = "substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'";

/// Which facts a step's statements read (THR-03): every fact of the ladder,
/// as a rebuild does, or a delta batch's copy in a temp table, narrowed by
/// `only` (a further `AND …` on `f.match_id`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Facts<'a> {
    /// `match_facts`, or a temp table with its columns.
    pub table: &'a str,
    pub only: &'a str,
}

/// Every fact: a rebuild's source.
pub(crate) const EVERY_FACT: Facts<'static> = Facts {
    table: "match_facts",
    only: "",
};

/// Facts of the platform's archived matches in this queue, each with its
/// player's stamped [`TIER`] and their [`SIDE`], plus any further `join`. A
/// fact with no stamp (a match `tiers:backfill` hasn't reached) doesn't
/// count, nor one of a team other than Summoner's Rift's two. Binds `?1` key
/// scope, `?2` platform, `?3` queue, `?4` queue id, `?5` patches.
#[cfg(test)]
fn ladder_facts(join: &str) -> String {
    ladder_facts_in(EVERY_FACT, join)
}

/// [`ladder_facts`] from `src`.
fn ladder_facts_in(src: Facts, join: &str) -> String {
    let (table, only) = (src.table, src.only);
    format!(
        "FROM {table} f
         JOIN matches m ON m.match_id = f.match_id
         {join}
        WHERE f.key_scope = ?1 AND m.queue_id = ?4 AND m.patch IS NOT NULL AND {ON_PLATFORM}
          AND f.tier IS NOT NULL AND f.team_id IN (100, 200)
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5))){only}"
    )
}

/// Rows written per table by one rebuild step.
pub type Written = Vec<(&'static str, i64)>;

/// The tables a run reports rows for (`analytics.updated`, the dashboard's
/// runs), in v1's set plus the set builds'.
pub const REPORTED: [&str; 9] = [
    "champion_stats",
    "champion_matchups",
    "champion_items",
    "champion_runes",
    "champion_spells",
    "champion_builds",
    "champion_build_parts",
    "champion_matchup_builds",
    "champion_matchup_build_parts",
];

/// One statement of a step (THR-03): the rebuild runs it as an `INSERT`, a
/// delta as an upsert or into a temp table first.
pub(crate) enum Stmt {
    /// Rows for `table`, its `cols` in order, from `select` (which may begin
    /// with a `WITH`). Takes the six parameters of [`Scope::insert`].
    Into {
        table: &'static str,
        cols: &'static str,
        select: String,
    },
    /// `champion_stats_totals` from the stats this step wrote (PERF-02).
    Totals,
    /// A temp table made (it names `?6`, so it takes all six parameters) or
    /// dropped (it takes none).
    Exec(String),
}

impl Stmt {
    fn rows(table: &'static str, cols: &'static str, select: String) -> Self {
        Self::Into { table, cols, select }
    }
}

/// A rebuild step's transaction: its stamp bit in `analytics_counted.bits`,
/// the tables it replaces, and its statements.
pub(crate) struct Step {
    pub bit: i64,
    pub tables: &'static [&'static str],
    pub sql: fn(Facts) -> Vec<Stmt>,
}

/// The rebuild's six transactions, in order. A match counted by all six has
/// [`ALL_STEPS`].
pub(crate) const STEPS: [Step; 6] = [
    // champions
    Step {
        bit: 1,
        tables: &[
            "analytics_slices",
            "analytics_match_totals",
            "champion_stats",
            "champion_stats_totals",
            "champion_bans",
            "champion_ban_totals",
            "analytics_floor_totals",
            "champion_ban_floor_totals",
        ],
        sql: champions_sql,
    },
    // matchups
    Step {
        bit: 2,
        tables: &["champion_matchups"],
        sql: matchups_sql,
    },
    // items
    Step {
        bit: 4,
        tables: &["champion_items"],
        sql: items_sql,
    },
    // runes
    Step {
        bit: 8,
        tables: &["champion_runes"],
        sql: runes_sql,
    },
    // spells
    Step {
        bit: 16,
        tables: &["champion_spells"],
        sql: spells_sql,
    },
    // sets
    Step {
        bit: 32,
        tables: &[
            "champion_builds",
            "champion_build_parts",
            "champion_matchup_builds",
            "champion_matchup_build_parts",
        ],
        sql: sets_sql,
    },
];

/// Every step's bit: a counted match.
pub const ALL_STEPS: i64 = 63;

/// The set-build step's bit: the one a match's first `match_builds` rows clear.
pub const SETS_STEP: i64 = 32;

/// `champion_stats_totals` from `champion_stats` rows (PERF-02, ADR-168), the
/// rebuild's own or a delta batch's. Binds as [`Scope::insert`].
pub(crate) fn totals_from(stats: &str) -> String {
    format!(
        "INSERT INTO champion_stats_totals (key_scope, platform, queue, patch, champion_id, role, remake, side,
           games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
           duration_s, computed_at)
         SELECT key_scope, platform, queue, patch, champion_id, role, remake, side,
           sum(games), sum(wins), sum(matches_picked), sum(stated_games), sum(kills), sum(deaths), sum(assists),
           sum(cs), sum(gold), sum(damage), sum(vision), sum(duration_s), ?6
           FROM {stats}
          WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
            AND (?5 IS NULL OR patch IN (SELECT value FROM json_each(?5)))
          GROUP BY patch, champion_id, role, remake, side"
    )
}

/// Run one step as a rebuild, in the caller's transaction: clear its tables
/// for the scope, write them from every fact, and stamp what it counted
/// (THR-03). Returns the rows written to the [`REPORTED`] tables.
pub(crate) fn rebuild_step(c: &Connection, s: &Scope, step: &Step) -> Result<Written, DbError> {
    for t in step.tables {
        s.clear(c, t)?;
    }
    let mut out = vec![];
    for stmt in (step.sql)(EVERY_FACT) {
        match stmt {
            Stmt::Into { table, cols, select } => {
                let n = s.insert(c, &format!("INSERT INTO {table} ({cols}) {select}"))?;
                if REPORTED.contains(&table) {
                    out.push((table, n));
                }
            }
            // From the rows just written, not the facts again, so the two
            // can't drift; every one of them carries ?6.
            Stmt::Totals => {
                s.insert(c, &totals_from("champion_stats"))?;
            }
            Stmt::Exec(sql) if sql.contains("?6") => {
                s.insert(c, &sql)?;
            }
            Stmt::Exec(sql) => c.execute_batch(&sql)?,
        }
    }
    delta::stamp_step(c, s, step.bit)?;
    Ok(out)
}

/// Slices, champion stats and bans, their totals over every tier (SITE-08,
/// PERF-02) and per tier floor (FLT-03), in one transaction (v1's champion
/// step).
pub fn rebuild_champions(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let tx = c.transaction()?;
    let out = rebuild_step(&tx, s, &STEPS[0])?;
    tx.commit()?;
    Ok(out)
}

fn champions_sql(src: Facts) -> Vec<Stmt> {
    let facts = ladder_facts_in(src, "");
    // A ban counts once per match, in every tier the match had a player in (v1).
    let bans = ladder_facts_in(src, "JOIN match_bans b ON b.match_id = f.match_id");
    let mut out = vec![
        Stmt::rows(
            "analytics_slices",
            "key_scope, platform, queue, tier, patch, remake, matches, computed_at",
            format!(
                "SELECT ?1, ?2, ?3, {TIER}, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
                 {facts}
                 GROUP BY {TIER}, m.patch, coalesce(m.remake, 0)"
            ),
        ),
        // The same matches once each, whatever tiers their players are in: the
        // denominator of a row summed over every tier (ADR-123).
        Stmt::rows(
            "analytics_match_totals",
            "key_scope, platform, queue, patch, remake, matches, computed_at",
            format!(
                "SELECT ?1, ?2, ?3, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
                 {facts}
                 GROUP BY m.patch, coalesce(m.remake, 0)"
            ),
        ),
        Stmt::rows(
            "champion_stats",
            "key_scope, platform, queue, tier, patch, champion_id, role, remake, side,
               games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
               duration_s, computed_at",
            format!(
                "SELECT ?1, ?2, ?3, {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0),
                   {SIDE}, count(*), sum(f.win), count(DISTINCT m.match_id), count(f.kills),
                   coalesce(sum(f.kills), 0), coalesce(sum(f.deaths), 0), coalesce(sum(f.assists), 0),
                   coalesce(sum(f.cs), 0), coalesce(sum(f.gold), 0), coalesce(sum(f.damage), 0),
                   coalesce(sum(f.vision), 0),
                   coalesce(sum(m.game_duration) FILTER (WHERE f.kills IS NOT NULL), 0), ?6
                 {facts}
                 GROUP BY {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0), {SIDE}"
            ),
        ),
        // The same rows summed over tier, for the reads that name none.
        Stmt::Totals,
        Stmt::rows(
            "champion_bans",
            "key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at",
            format!(
                "SELECT ?1, ?2, ?3, {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
                 {bans}
                 GROUP BY {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0)"
            ),
        ),
        // And once per match over every tier.
        Stmt::rows(
            "champion_ban_totals",
            "key_scope, platform, queue, patch, champion_id, remake, bans, computed_at",
            format!(
                "SELECT ?1, ?2, ?3, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
                 {bans}
                 GROUP BY m.patch, b.champion_id, coalesce(m.remake, 0)"
            ),
        ),
    ];
    out.extend(floors_sql(src));
    out
}

/// Riot's tiers with their rank, lowest first, as an SQL `floors(rank, tier)`
/// table: the floors of a tier range (FLT-03). Code constants from
/// [`ladder::tiers`], not caller text; `UNKNOWN` is no floor.
pub(crate) fn floors() -> String {
    let rows = ladder::tiers()
        .enumerate()
        .map(|(i, t)| format!("({i}, '{t}')"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("floors(rank, tier) AS (VALUES {rows})")
}

/// The pick- and ban-rate denominators of a tier range (FLT-03, ADR-170): a
/// match counts once in every floor at or below its highest stamped tier,
/// so a range's matches aren't the sum of its tiers' slices. Each match is
/// reduced to that tier once (`floor_top`), counted per tier, and only then
/// spread over the floors below it: grouping per floor first sorted every
/// match up to ten times over (about 4× slower, ADR-170). A match whose
/// players are all `UNKNOWN` or unstamped is in no floor.
fn floors_sql(src: Facts) -> Vec<Stmt> {
    let floors = floors();
    let facts = ladder_facts_in(src, "JOIN floors fl ON fl.tier = f.tier");
    vec![
        Stmt::Exec(format!(
            "CREATE TEMP TABLE floor_top AS WITH {floors}
             SELECT m.match_id, m.patch, coalesce(m.remake, 0) AS remake, max(fl.rank) AS rank, ?6 AS computed_at
             {facts}
             GROUP BY m.match_id"
        )),
        Stmt::rows(
            "analytics_floor_totals",
            "key_scope, platform, queue, floor_tier, patch, remake, matches, computed_at",
            format!(
                "WITH {floors},
                   g AS MATERIALIZED (SELECT patch, remake, rank, count(*) AS n FROM temp.floor_top
                                       GROUP BY patch, remake, rank)
                 SELECT ?1, ?2, ?3, fl.tier, g.patch, g.remake, sum(g.n), ?6
                   FROM g JOIN floors fl ON fl.rank <= g.rank
                  GROUP BY fl.tier, g.patch, g.remake"
            ),
        ),
        // A champion banned by both teams counts once per match, as in
        // champion_ban_totals; a match has one highest tier, so the per-tier
        // counts are disjoint and their sum over a floor is exact.
        Stmt::rows(
            "champion_ban_floor_totals",
            "key_scope, platform, queue, floor_tier, patch, champion_id, remake, bans, computed_at",
            format!(
                "WITH {floors},
                   g AS MATERIALIZED (SELECT t.patch, b.champion_id, t.remake, t.rank, count(DISTINCT t.match_id) AS n
                                        FROM temp.floor_top t JOIN match_bans b ON b.match_id = t.match_id
                                       GROUP BY t.patch, b.champion_id, t.remake, t.rank)
                 SELECT ?1, ?2, ?3, fl.tier, g.patch, g.champion_id, g.remake, sum(g.n), ?6
                   FROM g JOIN floors fl ON fl.rank <= g.rank
                  GROUP BY fl.tier, g.patch, g.champion_id, g.remake"
            ),
        ),
        Stmt::Exec("DROP TABLE temp.floor_top".into()),
    ]
}

/// Lane matchups (v1): two players of opposite teams in the same lane, each
/// lane held by exactly one player per team, mirror lanes excluded. Recorded
/// from both sides, since every participant counts (ADR-105), each at its
/// own player's stamped tier and side (FLT-01): the opponent's tier may
/// differ. A player with no stamp has no row, as in [`ladder_facts`]; the
/// lane's head count is taken before that, from every fact.
///
/// Each laned fact is read once, with its lane's head count from a window,
/// and that set is joined to itself (ADR-101). The earlier shape (two
/// `match_facts`, `matches` and a grouped lane count joined twice) let SQLite,
/// which has no `ANALYZE` statistics, loop over the whole ladder for every
/// fact: 44 s for a 16,600-game ladder.
fn matchups_sql(src: Facts) -> Vec<Stmt> {
    let (lanes, pairs) = (lanes_in(src, ""), one_on_one(""));
    vec![Stmt::rows(
        "champion_matchups",
        "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side, games, wins,
           computed_at",
        format!(
            "{lanes}
             SELECT ?1, ?2, ?3, a.patch, a.champion_id, a.position, b.champion_id, a.remake, a.tier,
               CASE a.team_id WHEN 100 THEN 'blue' ELSE 'red' END, count(*), sum(a.win), ?6
               {pairs}
              GROUP BY a.patch, a.position, a.champion_id, b.champion_id, a.remake, a.tier, a.team_id"
        ),
    )]
}

/// The rebuild's matchups statement, whole (its query plan is tested).
#[cfg(test)]
fn matchups() -> String {
    match matchups_sql(EVERY_FACT).remove(0) {
        Stmt::Into { table, cols, select } => format!("INSERT INTO {table} ({cols}) {select}"),
        _ => unreachable!("matchups_sql is one INSERT"),
    }
}

/// `lane`: the platform's laned facts in this queue, each with its lane's head
/// count, plus the `extra` columns (`", f.runes"`). A fact with no stamp is
/// in it, so it counts in the head count; [`one_on_one`] drops it. Binds as
/// [`ladder_facts`]; begins a `WITH`. Widening the matchups' own lane with the
/// set builds' columns slowed them by 30 % (FLT-04), hence `extra`.
fn lanes_in(src: Facts, extra: &str) -> String {
    let (table, only) = (src.table, src.only);
    format!(
        "WITH lane AS (
           SELECT f.match_id, f.puuid, f.team_id, f.position, f.champion_id, f.win, f.tier, m.patch,
             coalesce(m.remake, 0) AS remake{extra},
             count(*) OVER (PARTITION BY f.match_id, f.team_id, f.position) AS n
             FROM {table} f JOIN matches m ON m.match_id = f.match_id
            WHERE f.key_scope = ?1 AND f.position IS NOT NULL AND m.queue_id = ?4 AND m.patch IS NOT NULL
              AND substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'
              AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5))){only}
         )"
    )
}

/// Each stamped laner `a` of [`lanes_in`] against `b`, the other team's player
/// in its lane, where each team has one player there and they differ; plus
/// any further `join`.
fn one_on_one(join: &str) -> String {
    format!(
        "FROM lane a
         JOIN lane b ON b.match_id = a.match_id AND b.position = a.position AND b.team_id <> a.team_id
         {join}
        WHERE a.n = 1 AND b.n = 1 AND a.champion_id <> b.champion_id AND a.team_id IN (100, 200)
          AND a.tier IS NOT NULL"
    )
}

pub fn rebuild_matchups(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let tx = c.transaction()?;
    let out = rebuild_step(&tx, s, &STEPS[1])?;
    tx.commit()?;
    Ok(out)
}

/// A set build part's value for `k.part` of a player `p` (BLD-02, ADR-120):
/// `NULL` when the player has nothing to count there, so the part is left
/// out. Shared by `champion_build_parts` and `champion_matchup_build_parts`.
const PART_VALUE: &str = "CASE k.part
       WHEN 'item3' THEN CAST(json_extract(p.items, '$[2]') AS TEXT)
       WHEN 'item4' THEN CAST(json_extract(p.items, '$[3]') AS TEXT)
       WHEN 'item5' THEN CAST(json_extract(p.items, '$[4]') AS TEXT)
       WHEN 'starter' THEN nullif(p.starter, '[]')
       WHEN 'boots' THEN CAST(p.boots AS TEXT)
       WHEN 'skill_order' THEN nullif(p.skill_order, '')
       WHEN 'runes' THEN json_extract(p.runes, '$[1]') || ':' || json_extract(p.runes, '$[2]')
       WHEN 'spells' THEN
         min(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]')) || ':'
           || max(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]'))
     END";

/// Items, runes and spells, a transaction each (v1's builds step), then set
/// builds and their parts in one more (BLD-02), with the set builds against
/// each lane opponent (FLT-04).
pub fn rebuild_builds(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let mut out = vec![];
    for step in &STEPS[2..] {
        let tx = c.transaction()?;
        out.extend(rebuild_step(&tx, s, step)?);
        tx.commit()?;
    }
    Ok(out)
}

/// Items: slots 0–5, empty slots skipped, a player's duplicate item once.
fn items_sql(src: Facts) -> Vec<Stmt> {
    let facts = ladder_facts_in(src, "JOIN json_each(f.items) i");
    vec![Stmt::rows(
        "champion_items",
        "key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side, games, wins,
           computed_at",
        format!(
            "SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(DISTINCT f.match_id || ' ' || f.puuid),
               count(DISTINCT CASE WHEN f.win THEN f.match_id || ' ' || f.puuid END), ?6
             {facts}
               AND i.value IS NOT NULL AND i.value <> 0
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER), coalesce(m.remake, 0),
               {TIER}, {SIDE}"
        ),
    )]
}

fn runes_sql(src: Facts) -> Vec<Stmt> {
    let facts = ladder_facts_in(src, "");
    vec![Stmt::rows(
        "champion_runes",
        "key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side,
           games, wins, computed_at",
        format!(
            "SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               json_extract(f.runes, '$[1]'), json_extract(f.runes, '$[2]'),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.runes, '$[1]') IS NOT NULL
               AND json_extract(f.runes, '$[2]') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), json_extract(f.runes, '$[1]'),
               json_extract(f.runes, '$[2]'), coalesce(m.remake, 0), {TIER}, {SIDE}"
        ),
    )]
}

/// Spells as an unordered pair: Flash on D and on F is one build (v1).
fn spells_sql(src: Facts) -> Vec<Stmt> {
    let facts = ladder_facts_in(src, "");
    vec![Stmt::rows(
        "champion_spells",
        "key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side, games, wins,
           computed_at",
        format!(
            "SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0), {TIER}, {SIDE}, count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.summoners, '$[0]') IS NOT NULL
               AND json_extract(f.summoners, '$[1]') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0), {TIER}, {SIDE}"
        ),
    )]
}

/// Set builds and their parts (BLD-02), then the set builds against each lane
/// opponent (FLT-04).
fn sets_sql(src: Facts) -> Vec<Stmt> {
    let players = set_build_players_in(src);
    let mut out = vec![
        Stmt::rows(
            "champion_builds",
            "key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side, games, wins, computed_at",
            format!(
                "{players}
                 SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, tier, side, count(*), sum(win), ?6
                   FROM p GROUP BY patch, champion_id, role, remake, core, tier, side"
            ),
        ),
        Stmt::rows(
            "champion_build_parts",
            "key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side, games,
               wins, computed_at",
            format!(
                "{players},
                 k (part) AS (VALUES ('item3'), ('item4'), ('item5'), ('starter'), ('boots'), ('skill_order'),
                   ('runes'), ('spells')),
                 v AS (
                   SELECT p.patch, p.champion_id, p.role, p.remake, p.core, p.tier, p.side, p.win, k.part,
                     {PART_VALUE} AS value
                     FROM p CROSS JOIN k
                 )
                 SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, part, value, tier, side, count(*),
                   sum(win), ?6
                   FROM v WHERE value IS NOT NULL
                  GROUP BY patch, champion_id, role, remake, core, part, value, tier, side"
            ),
        ),
    ];
    out.extend(matchup_builds_sql(src));
    out
}

/// Set builds against one lane opponent (FLT-04, ADR-171): the players of
/// [`set_build_players`] who are also `a` in [`one_on_one`], so each row lies
/// inside one `champion_builds` row and one `champion_matchups` row. The
/// players are gathered once into a temp table and both inserts read it
/// (11 s against 19 s for restating the lanes, FLT-04 §8). Only four parts.
fn matchup_builds_sql(src: Facts) -> Vec<Stmt> {
    let lanes = lanes_in(src, ", f.runes, f.summoners");
    let pairs = one_on_one(
        "JOIN match_builds mb ON mb.match_id = a.match_id AND mb.key_scope = ?1 AND mb.puuid = a.puuid",
    );
    vec![
        Stmt::Exec(format!(
            "CREATE TEMP TABLE lane_builds AS {lanes}
             SELECT a.patch, a.champion_id, a.position AS role, b.champion_id AS opponent_id, a.remake, a.tier,
               CASE a.team_id WHEN 100 THEN 'blue' ELSE 'red' END AS side, a.win,
               json_array(json_extract(mb.items, '$[0]'), json_extract(mb.items, '$[1]')) AS core,
               mb.items, mb.starter, mb.boots, mb.skill_order, a.runes, a.summoners, ?6 AS computed_at
               {pairs}
               AND json_array_length(mb.items) >= 2"
        )),
        Stmt::rows(
            "champion_matchup_builds",
            "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, tier, side, games,
               wins, computed_at",
            "SELECT ?1, ?2, ?3, patch, champion_id, role, opponent_id, remake, core, tier, side, count(*), sum(win), ?6
               FROM temp.lane_builds
              GROUP BY patch, champion_id, role, opponent_id, remake, core, tier, side"
                .into(),
        ),
        Stmt::rows(
            "champion_matchup_build_parts",
            "key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, part, value, tier,
               side, games, wins, computed_at",
            format!(
                "WITH k (part) AS (VALUES ('item3'), ('boots'), ('runes'), ('spells')),
                 v AS (
                   SELECT p.patch, p.champion_id, p.role, p.opponent_id, p.remake, p.core, p.tier, p.side, p.win,
                     k.part, {PART_VALUE} AS value
                     FROM temp.lane_builds p CROSS JOIN k
                 )
                 SELECT ?1, ?2, ?3, patch, champion_id, role, opponent_id, remake, core, part, value, tier, side,
                   count(*), sum(win), ?6
                   FROM v WHERE value IS NOT NULL
                  GROUP BY patch, champion_id, role, opponent_id, remake, core, part, value, tier, side"
            ),
        ),
        Stmt::Exec("DROP TABLE temp.lane_builds".into()),
    ]
}

/// Set builds (BLD-02, ADR-120): `p`, the facts of the players whose
/// `match_builds` row has two or more finished items, each with its `core`,
/// the first two as `"[a,b]"`. A player with fewer counts in no build and no
/// part. Binds as [`ladder_facts`]; begins a `WITH`.
#[cfg(test)]
fn set_build_players() -> String {
    set_build_players_in(EVERY_FACT)
}

fn set_build_players_in(src: Facts) -> String {
    let facts = ladder_facts_in(
        src,
        "JOIN match_builds b ON b.match_id = f.match_id AND b.key_scope = f.key_scope AND b.puuid = f.puuid",
    );
    format!(
        "WITH p AS (
           SELECT m.patch, f.champion_id, coalesce(f.position, '') AS role, coalesce(m.remake, 0) AS remake,
             {TIER} AS tier, {SIDE} AS side, f.win,
             json_array(json_extract(b.items, '$[0]'), json_extract(b.items, '$[1]')) AS core,
             b.items, b.starter, b.boots, b.skill_order, f.runes, f.summoners
           {facts}
             AND json_array_length(b.items) >= 2
         )"
    )
}

/// Whether a ladder still holds rows V0017 copied without a tier and side
/// (`analytics_unsplit`, ADR-129). Its next rebuild then covers every patch.
pub fn unsplit(c: &Connection, key_scope: &str, platform: &str, queue: &str) -> Result<bool, DbError> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row(
        "SELECT 1 FROM analytics_unsplit WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3",
        params![key_scope, platform, queue],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// Take a ladder off `analytics_unsplit`, once every step has rebuilt every
/// patch of it.
pub fn mark_split(c: &Connection, key_scope: &str, platform: &str, queue: &str) -> Result<(), DbError> {
    c.execute(
        "DELETE FROM analytics_unsplit WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3",
        params![key_scope, platform, queue],
    )?;
    Ok(())
}

/// One key scope's ladders still on `analytics_unsplit`, as `(platform, queue)`.
pub fn unsplit_ladders(c: &Connection, key_scope: &str) -> Result<Vec<(String, String)>, DbError> {
    let mut stmt = c.prepare(
        "SELECT platform, queue FROM analytics_unsplit WHERE key_scope = ?1 ORDER BY platform, queue",
    )?;
    let rows = stmt
        .query_map([key_scope], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── Reads ───────────────────────────────────────────────────────────────────

/// A team's side of the map, as the per-participant tables store it (FLT-01):
/// team 100 is blue, 200 red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Blue,
    Red,
}

impl Side {
    /// `blue` / `red`: the query value, the stored value and the JSON value.
    pub const ALL: [&'static str; 2] = ["blue", "red"];

    pub fn parse(v: &str) -> Option<Self> {
        match v {
            "blue" => Some(Self::Blue),
            "red" => Some(Self::Red),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Red => "red",
        }
    }
}

/// Which tiers a read sums (FLT-03): one of these, so a read can't name a
/// tier and a range at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierFilter {
    /// Every tier, summed (ADR-123).
    Any,
    /// One tier: Riot's, or [`UNKNOWN_TIER`].
    One(String),
    /// This tier and every one above it, in [`ladder::tiers`]'s order;
    /// `UNKNOWN` is never in a range.
    AtOrAbove(&'static str),
}

impl TierFilter {
    /// The one tier asked for, if that is what was asked.
    pub fn one(&self) -> Option<&str> {
        match self {
            Self::One(t) => Some(t),
            _ => None,
        }
    }

    /// The floor of the range asked for, if that is what was asked.
    pub fn floor(&self) -> Option<&'static str> {
        match self {
            Self::AtOrAbove(f) => Some(f),
            _ => None,
        }
    }
}

/// Which slice a read asks for.
#[derive(Debug, Clone)]
pub struct Read {
    pub key_scope: String,
    /// `None`: every platform summed (ADR-065).
    pub platform: Option<String>,
    pub queue: String,
    /// `None`: every patch summed (`?patch=all`, ADR-094).
    pub patch: Option<String>,
    /// Every tier, one, or a range (ADR-123, FLT-03).
    pub tier: TierFilter,
    /// `None`: both sides summed (FLT-02). The per-match tables (slices,
    /// bans, totals) have no side and ignore it.
    pub side: Option<Side>,
    /// `None`: every role summed; `Some("")` is the roleless rows.
    pub role: Option<String>,
    pub champion_id: Option<i64>,
    pub min_games: i64,
    pub limit: i64,
    /// Add remakes' rows back in (`?remakes=include`).
    pub remakes: bool,
    /// The lane opponent a set build read is against (FLT-04): read from
    /// `champion_matchup_builds` instead, where `role` is a lane. Only
    /// [`build_source`]'s reads take it.
    pub opponent_id: Option<i64>,
}

/// The newest patch this ladder has stats for (v1 `latestPatch`): the slices'
/// patches, which are the stats' (ADR-167).
pub async fn latest_patch(
    db: &Db,
    key_scope: &str,
    platform: Option<&str>,
    queue: &str,
) -> Result<Option<String>, DbError> {
    let r = Read::slice(key_scope, platform, queue);
    db.read(move |c| Ok(newest_patch(domain(c, &r)?.patches))).await
}

/// The newest of `patches`, compared as `major.minor` numbers.
fn newest_patch(patches: Vec<String>) -> Option<String> {
    let num = |p: &str| -> (i64, i64) {
        let (a, b) = p.split_once('.').unwrap_or((p, ""));
        (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
    };
    patches.into_iter().max_by_key(|p| num(p))
}

/// One aggregated patch: its games (participant rows, as `totalGames`) and
/// recompute time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchRow {
    pub patch: String,
    pub games: i64,
    pub computed_at: i64,
}

/// Every patch this ladder (or every ladder, without a platform) has stats
/// for, newest first (ADR-094); with a champion, only the patches it was
/// played on, and its games (ADR-100); with a side, only that side's games
/// (FLT-02).
pub async fn patches(
    db: &Db,
    key_scope: &str,
    platform: Option<&str>,
    queue: &str,
    champion_id: Option<i64>,
    side: Option<Side>,
    remakes: bool,
) -> Result<Vec<PatchRow>, DbError> {
    let r = Read {
        champion_id,
        side,
        remakes,
        ..Read::slice(key_scope, platform, queue)
    };
    db.read(move |c| {
        let (table, keys) = stats_source(&r, false);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        let mut stmt = c.prepare(&format!(
            "SELECT patch, sum(games), max(computed_at) FROM {table} WHERE {}
              GROUP BY patch ORDER BY {PATCH_DESC}",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.args), |r| {
                Ok(PatchRow {
                    patch: r.get(0)?,
                    games: r.get(1)?,
                    computed_at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

impl Read {
    /// Every patch, tier, side and role of one queue's slice, remakes left
    /// out: the base the patch reads fill in.
    fn slice(key_scope: &str, platform: Option<&str>, queue: &str) -> Self {
        Self {
            key_scope: key_scope.to_string(),
            platform: platform.map(str::to_string),
            queue: queue.to_string(),
            patch: None,
            tier: TierFilter::Any,
            side: None,
            role: None,
            champion_id: None,
            min_games: 0,
            limit: 0,
            remakes: false,
            opponent_id: None,
        }
    }
}

/// The platforms, tiers and patches a slice has rows for (PERF-01, ADR-167):
/// what a filter a read leaves open stands for. Read from `analytics_slices`,
/// a few hundred rows: every row of the other tables comes from a fact whose
/// match is counted there, at the same platform, tier and patch, and the
/// slices are rewritten first, in the stats' transaction.
#[derive(Debug, Default)]
pub(crate) struct Domain {
    platforms: Vec<String>,
    tiers: Vec<String>,
    patches: Vec<String>,
}

pub(crate) fn domain(c: &Connection, r: &Read) -> Result<Domain, DbError> {
    let platform = if r.platform.is_some() {
        " AND platform = ?3"
    } else {
        ""
    };
    let mut stmt = c.prepare_cached(&format!(
        "SELECT DISTINCT platform, tier, patch FROM analytics_slices WHERE key_scope = ?1 AND queue = ?2{platform}"
    ))?;
    let (mut platforms, mut tiers, mut patches) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut args: Vec<Value> = vec![r.key_scope.clone().into(), r.queue.clone().into()];
    args.extend(r.platform.clone().map(Value::from));
    let mut rows = stmt.query(params_from_iter(args))?;
    while let Some(row) = rows.next()? {
        platforms.insert(row.get::<_, String>(0)?);
        tiers.insert(row.get::<_, String>(1)?);
        patches.insert(row.get::<_, String>(2)?);
    }
    Ok(Domain {
        platforms: platforms.into_iter().collect(),
        tiers: tiers.into_iter().collect(),
        patches: patches.into_iter().collect(),
    })
}

/// Where a table keeps its tier, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TierKey {
    /// No tier column: the totals over every tier.
    None,
    /// `(…, queue, tier, patch, …)`: an open tier is an `IN`, so the key
    /// still reaches the patch.
    BeforePatch,
    /// After the champion, role and facet: an open tier is left out.
    AfterPatch,
    /// `(…, queue, floor_tier, patch, …)`: the floor totals (FLT-03), read
    /// only for a range, by its floor.
    Floor,
}

/// The filter columns a table has besides `key_scope, platform, queue, patch, remake`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Keys {
    tier: TierKey,
    side: bool,
    role: bool,
    champion: bool,
    /// `opponent_id` after the champion, and an open role is an `IN` over the
    /// lanes so the key reaches it (FLT-04).
    opponent: bool,
}

const SLICES: Keys = Keys {
    tier: TierKey::BeforePatch,
    side: false,
    role: false,
    champion: false,
    opponent: false,
};
const MATCH_TOTALS: Keys = Keys {
    tier: TierKey::None,
    side: false,
    role: false,
    champion: false,
    opponent: false,
};
const STATS: Keys = Keys {
    tier: TierKey::BeforePatch,
    side: true,
    role: true,
    champion: true,
    opponent: false,
};
/// `champion_stats` summed over tier (PERF-02, ADR-168): no tier column.
const STATS_TOTALS: Keys = Keys {
    tier: TierKey::None,
    side: true,
    role: true,
    champion: true,
    opponent: false,
};
const BANS: Keys = Keys {
    tier: TierKey::BeforePatch,
    side: false,
    role: false,
    champion: true,
    opponent: false,
};
const BAN_TOTALS: Keys = Keys {
    tier: TierKey::None,
    side: false,
    role: false,
    champion: true,
    opponent: false,
};
/// Distinct matches per floor (FLT-03, V0026).
const FLOOR_TOTALS: Keys = Keys {
    tier: TierKey::Floor,
    side: false,
    role: false,
    champion: false,
    opponent: false,
};
/// Bans per floor (FLT-03, V0026).
const BAN_FLOOR_TOTALS: Keys = Keys {
    tier: TierKey::Floor,
    side: false,
    role: false,
    champion: true,
    opponent: false,
};
/// Matchups, items, runes, spells, set builds and their parts.
const FACETS: Keys = Keys {
    tier: TierKey::AfterPatch,
    side: true,
    role: true,
    champion: true,
    opponent: false,
};

/// Set builds against one lane opponent and their parts (FLT-04, V0027).
const MATCHUP_BUILDS: Keys = Keys {
    tier: TierKey::AfterPatch,
    side: true,
    role: true,
    champion: true,
    opponent: true,
};

/// The lanes, as `champion_matchups` and `champion_matchup_builds` store a
/// role (v1 `LANE_POSITIONS`).
pub const LANES: [&str; 5] = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];

/// The set builds a build read sums, their parts and their keys: the builds
/// against the read's opponent when it names one (FLT-04, ADR-171), else
/// every set build. The only way a read reaches the opponent tables.
pub(crate) fn build_source(r: &Read) -> (&'static str, &'static str, Keys) {
    if r.opponent_id.is_some() {
        (
            "champion_matchup_builds",
            "champion_matchup_build_parts",
            MATCHUP_BUILDS,
        )
    } else {
        ("champion_builds", "champion_build_parts", FACETS)
    }
}

/// The table a stats read sums and its keys: `champion_stats_totals` when the
/// read names no tier and wants no row per tier (PERF-02, ADR-168), else
/// `champion_stats`. The totals have no tier, so a read of one tier or of a
/// range (FLT-03) must never reach them.
pub(crate) fn stats_source(r: &Read, by_tier: bool) -> (&'static str, Keys) {
    if by_tier || r.tier != TierFilter::Any {
        ("champion_stats", STATS)
    } else {
        ("champion_stats_totals", STATS_TOTALS)
    }
}

/// A read's `WHERE` and its arguments, in order (PERF-01, ADR-167). A filter
/// that is set is an equality; one left open is an `IN` over the slice's
/// values where the key needs it to reach the columns after it, and
/// otherwise absent. Never `(? IS NULL OR col = ?)`: SQLite can't seek on
/// it, so every read scanned its key scope. A side is written into the SQL:
/// it is one of two constants.
pub(crate) struct Filter {
    pub(crate) sql: String,
    pub(crate) args: Vec<Value>,
}

impl Filter {
    pub(crate) fn new(r: &Read, d: &Domain, k: Keys) -> Self {
        let mut f = Self {
            sql: String::new(),
            args: vec![],
        };
        f.eq("key_scope", r.key_scope.clone().into());
        f.one_of("platform", r.platform.as_ref(), &d.platforms);
        f.eq("queue", r.queue.clone().into());
        match (k.tier, &r.tier) {
            (TierKey::None, _) | (TierKey::AfterPatch, TierFilter::Any) => {}
            (TierKey::BeforePatch, TierFilter::Any) => f.one_of("tier", None, &d.tiers),
            (TierKey::BeforePatch | TierKey::AfterPatch, TierFilter::One(t)) => {
                f.eq("tier", t.clone().into())
            }
            // A range is its tiers the slice has: a set, so the key still
            // reaches the patch where tier comes before it.
            (TierKey::BeforePatch | TierKey::AfterPatch, TierFilter::AtOrAbove(floor)) => {
                let range = ladder::tiers_at_or_above(floor);
                let tiers: Vec<String> = d
                    .tiers
                    .iter()
                    .filter(|t| range.contains(&t.as_str()))
                    .cloned()
                    .collect();
                f.one_of("tier", None, &tiers);
            }
            (TierKey::Floor, TierFilter::AtOrAbove(floor)) => f.eq("floor_tier", floor.to_string().into()),
            // The floor totals answer a range only.
            (TierKey::Floor, _) => f.push("0"),
        }
        f.one_of("patch", r.patch.as_ref(), &d.patches);
        if !r.remakes {
            f.push("remake = 0");
        }
        if let (true, Some(side)) = (k.side, r.side) {
            f.push(&format!("side = '{}'", side.as_str()));
        }
        match (k.role, &r.role) {
            (true, Some(role)) => f.eq("role", role.clone().into()),
            // Every row is a lane: the IN is no filter, only the way to the
            // opponent.
            (true, None) if k.opponent => {
                let lanes: Vec<String> = LANES.iter().map(|l| l.to_string()).collect();
                f.one_of("role", None, &lanes);
            }
            _ => {}
        }
        if let (true, Some(id)) = (k.champion, r.champion_id) {
            f.eq("champion_id", id.into());
        }
        if let (true, Some(id)) = (k.opponent, r.opponent_id) {
            f.eq("opponent_id", id.into());
        }
        f
    }

    fn push(&mut self, term: &str) {
        if !self.sql.is_empty() {
            self.sql.push_str(" AND ");
        }
        self.sql.push_str(term);
    }

    fn eq(&mut self, col: &str, v: Value) {
        self.push(&format!("{col} = ?"));
        self.args.push(v);
    }

    /// `col = ?` when set, else `col IN (…)` over `all` (`IN ()` when the
    /// slice has nothing: no rows, as before).
    fn one_of(&mut self, col: &str, set: Option<&String>, all: &[String]) {
        if let Some(v) = set {
            return self.eq(col, v.clone().into());
        }
        let marks = vec!["?"; all.len()].join(", ");
        self.push(&format!("{col} IN ({marks})"));
        self.args.extend(all.iter().map(|v| Value::from(v.clone())));
    }

    /// The arguments with `more` after them, for the `?`s that follow the
    /// `WHERE`.
    fn with(mut self, more: impl IntoIterator<Item = Value>) -> Vec<Value> {
        self.args.extend(more);
        self.args
    }
}

/// One champion at one tier, or summed over every tier (`tier` `None`), its
/// roles summed unless a role is asked for; every patch summed when the read
/// names none (`patch` is then `"all"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatRow {
    pub champion_id: i64,
    pub tier: Option<String>,
    pub patch: String,
    pub games: i64,
    pub wins: i64,
    pub matches_picked: i64,
    pub stated_games: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub cs: i64,
    pub gold: i64,
    pub damage: i64,
    pub vision: i64,
    pub duration_s: i64,
    pub computed_at: i64,
}

/// Champion stats, most played first: one row per champion summed over every
/// tier when the read names none (ADR-123), else that tier's rows (v1
/// `listChampionStats`). `minGames` and `limit` apply to the rows returned.
pub async fn stats(db: &Db, r: Read) -> Result<Vec<StatRow>, DbError> {
    let by_tier = matches!(r.tier, TierFilter::One(_));
    stat_rows(db, r, by_tier).await
}

/// Champion stats a row per (champion, tier), the named tier's or every
/// tier's, most played first.
pub async fn stats_by_tier(db: &Db, r: Read) -> Result<Vec<StatRow>, DbError> {
    stat_rows(db, r, true).await
}

async fn stat_rows(db: &Db, r: Read, by_tier: bool) -> Result<Vec<StatRow>, DbError> {
    let (tier, group) = if by_tier {
        ("tier", "champion_id, tier")
    } else {
        ("NULL", "champion_id")
    };
    db.read(move |c| {
        let (table, keys) = stats_source(&r, by_tier);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        // Summed over tier, the rows are first summed per platform and patch
        // in key order, which needs no sort, so the final GROUP BY sorts a
        // few thousand rows instead of every role and side's (ADR-168). The
        // same integers, so the same answer. A range sums champion_stats,
        // whose key has the tier before the patch, so its first sum keeps
        // the tier too (FLT-03: 121 ms instead of 190 on the synthetic copy).
        let keyed = if table == "champion_stats_totals" {
            "platform, patch, champion_id"
        } else {
            "platform, tier, patch, champion_id"
        };
        let source = if by_tier {
            format!("{table} WHERE {}", w.sql)
        } else {
            format!(
                "(SELECT {keyed}, sum(games) AS games, sum(wins) AS wins,
                         sum(matches_picked) AS matches_picked, sum(stated_games) AS stated_games,
                         sum(kills) AS kills, sum(deaths) AS deaths, sum(assists) AS assists, sum(cs) AS cs,
                         sum(gold) AS gold, sum(damage) AS damage, sum(vision) AS vision,
                         sum(duration_s) AS duration_s, max(computed_at) AS computed_at
                    FROM {table} WHERE {} GROUP BY {keyed})",
                w.sql
            )
        };
        let patch = r.patch.clone().unwrap_or_else(|| "all".into());
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, {tier}, sum(games), sum(wins), sum(matches_picked),
                    sum(stated_games), sum(kills), sum(deaths), sum(assists), sum(cs), sum(gold), sum(damage),
                    sum(vision), sum(duration_s), max(computed_at)
               FROM {source}
              GROUP BY {group} HAVING sum(games) >= ?
              ORDER BY sum(games) DESC, {group} LIMIT ?"
        ))?;
        let rows = stmt
            .query_map(
                params_from_iter(w.with([r.min_games.into(), r.limit.into()])),
                |x| {
                    Ok(StatRow {
                        champion_id: x.get(0)?,
                        tier: x.get(1)?,
                        patch: patch.clone(),
                        games: x.get(2)?,
                        wins: x.get(3)?,
                        matches_picked: x.get(4)?,
                        stated_games: x.get(5)?,
                        kills: x.get(6)?,
                        deaths: x.get(7)?,
                        assists: x.get(8)?,
                        cs: x.get(9)?,
                        gold: x.get(10)?,
                        damage: x.get(11)?,
                        vision: x.get(12)?,
                        duration_s: x.get(13)?,
                        computed_at: x.get(14)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Matches per tier: pick- and ban-rate denominators (v1 `listAnalyticsSlices`).
pub async fn slices(db: &Db, r: Read) -> Result<Vec<(String, i64)>, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, SLICES);
        let mut stmt = c.prepare(&format!(
            "SELECT tier, sum(matches) FROM analytics_slices WHERE {} GROUP BY tier",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.args), |x| Ok((x.get(0)?, x.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Bans per (tier, champion) (v1 `listChampionBans`).
pub async fn bans(db: &Db, r: Read) -> Result<Vec<(String, i64, i64)>, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, BANS);
        let mut stmt = c.prepare(&format!(
            "SELECT tier, champion_id, sum(bans) FROM champion_bans WHERE {} GROUP BY tier, champion_id",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.args), |x| {
                Ok((x.get(0)?, x.get(1)?, x.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Distinct matches in the slice, every tier and role (ADR-123): the pick- and
/// ban-rate denominator of a row summed over every tier.
pub async fn match_totals(db: &Db, r: Read) -> Result<i64, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, MATCH_TOTALS);
        Ok(c.query_row(
            &format!(
                "SELECT coalesce(sum(matches), 0) FROM analytics_match_totals WHERE {}",
                w.sql
            ),
            params_from_iter(w.args),
            |x| x.get(0),
        )?)
    })
    .await
}

/// The newest `computed_at` of a slice's match totals (THR-03, ADR-182):
/// every match a rebuild or a delta counts upserts its patch's row, so this
/// moves whenever anything in the slice does, the pick and ban rates'
/// denominators included. `None` for no rows.
pub async fn totals_stamp(
    db: &Db,
    key_scope: &str,
    platform: Option<&str>,
    queue: &str,
    patch: Option<&str>,
    remakes: bool,
) -> Result<Option<i64>, DbError> {
    let (scope, platform, queue, patch) = (
        key_scope.to_string(),
        platform.map(str::to_string),
        queue.to_string(),
        patch.map(str::to_string),
    );
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT max(computed_at) FROM analytics_match_totals
              WHERE key_scope = ?1 AND queue = ?3 AND (?2 IS NULL OR platform = ?2)
                AND (?4 IS NULL OR patch = ?4) AND (?5 OR remake = 0)",
            params![scope, platform, queue, patch, remakes],
            |r| r.get(0),
        )?)
    })
    .await
}

/// Bans per champion over every tier, a match once (ADR-123): `(champion, bans)`.
pub async fn ban_totals(db: &Db, r: Read) -> Result<Vec<(i64, i64)>, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, BAN_TOTALS);
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, sum(bans) FROM champion_ban_totals WHERE {} GROUP BY champion_id",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.args), |x| Ok((x.get(0)?, x.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Distinct matches in the slice with a player at or above the read's floor,
/// each once (FLT-03, V0026): the pick- and ban-rate denominator of a row
/// summed over a tier range. 0 for a read that names no range.
pub async fn floor_totals(db: &Db, r: Read) -> Result<i64, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, FLOOR_TOTALS);
        Ok(c.query_row(
            &format!(
                "SELECT coalesce(sum(matches), 0) FROM analytics_floor_totals WHERE {}",
                w.sql
            ),
            params_from_iter(w.args),
            |x| x.get(0),
        )?)
    })
    .await
}

/// Bans per champion among the read's floor's matches, a match once (FLT-03):
/// `(champion, bans)`.
pub async fn ban_floor_totals(db: &Db, r: Read) -> Result<Vec<(i64, i64)>, DbError> {
    db.read(move |c| {
        let w = Filter::new(&r, &domain(c, &r)?, BAN_FLOOR_TOTALS);
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, sum(bans) FROM champion_ban_floor_totals WHERE {} GROUP BY champion_id",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.args), |x| Ok((x.get(0)?, x.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// One facet row of a champion: a matchup, item, rune pair or spell pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetRow {
    /// Matchups: the lane. Empty for builds.
    pub role: String,
    /// The facet's id(s): opponent; item; keystone and sub-style; spell pair.
    pub ids: Vec<i64>,
    pub games: i64,
    pub wins: i64,
    pub computed_at: i64,
}

/// The facets of `champion_matchups` / `champion_items` / `_runes` / `_spells`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Matchups,
    Items,
    Runes,
    Spells,
}

impl Facet {
    fn table(self) -> &'static str {
        match self {
            Self::Matchups => "champion_matchups",
            Self::Items => "champion_items",
            Self::Runes => "champion_runes",
            Self::Spells => "champion_spells",
        }
    }

    fn columns(self) -> &'static str {
        match self {
            // Matchups keep their lane: a row per (lane, opponent) (v1).
            Self::Matchups => "role, opponent_id",
            Self::Items => "item_id",
            Self::Runes => "keystone_id, sub_style_id",
            Self::Spells => "spell_a, spell_b",
        }
    }
}

/// v1 `listChampionMatchups` / `Items` / `Runes` / `Spells`: one champion's
/// facet rows, roles summed unless a role is asked for, most played first.
pub async fn facet(db: &Db, facet: Facet, r: Read) -> Result<Vec<FacetRow>, DbError> {
    db.read(move |c| {
        let cols = facet.columns();
        let w = Filter::new(&r, &domain(c, &r)?, FACETS);
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}, sum(games), sum(wins), max(computed_at) FROM {} WHERE {}
              GROUP BY {cols} HAVING sum(games) >= ?
              ORDER BY sum(games) DESC, {cols} LIMIT ?",
            facet.table(),
            w.sql
        ))?;
        let rows = stmt
            .query_map(
                params_from_iter(w.with([r.min_games.into(), r.limit.into()])),
                |x| {
                    let (role, ids, at) = match facet {
                        Facet::Matchups => (x.get::<_, String>(0)?, vec![x.get(1)?], 2),
                        Facet::Items => (String::new(), vec![x.get(0)?], 1),
                        Facet::Runes | Facet::Spells => (String::new(), vec![x.get(0)?, x.get(1)?], 2),
                    };
                    Ok(FacetRow {
                        role,
                        ids,
                        games: x.get(at)?,
                        wins: x.get(at + 1)?,
                        computed_at: x.get(at + 2)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// One set build of a champion (BLD-02): its first two finished items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRow {
    pub core: [i64; 2],
    pub games: i64,
    pub wins: i64,
    pub computed_at: i64,
}

/// One champion's set builds, roles summed unless a role is asked for, most
/// played first; against one opponent when the read names one (FLT-04).
pub async fn builds(db: &Db, r: Read) -> Result<Vec<BuildRow>, DbError> {
    db.read(move |c| {
        let (table, _, keys) = build_source(&r);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        let mut stmt = c.prepare(&format!(
            "SELECT json_extract(core, '$[0]'), json_extract(core, '$[1]'), sum(games), sum(wins), max(computed_at)
               FROM {table} WHERE {}
              GROUP BY core HAVING sum(games) >= ?
              ORDER BY sum(games) DESC, core LIMIT ?",
            w.sql
        ))?;
        let rows = stmt
            .query_map(
                params_from_iter(w.with([r.min_games.into(), r.limit.into()])),
                |x| {
                    Ok(BuildRow {
                        core: [x.get(0)?, x.get(1)?],
                        games: x.get(2)?,
                        wins: x.get(3)?,
                        computed_at: x.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// One champion's games with a set build in the slice, every core summed,
/// and the newest recompute time of those rows: `pickRate`'s denominator.
pub async fn build_totals(db: &Db, r: Read) -> Result<(i64, Option<i64>), DbError> {
    db.read(move |c| {
        let (table, _, keys) = build_source(&r);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        Ok(c.query_row(
            &format!(
                "SELECT coalesce(sum(games), 0), max(computed_at) FROM {table} WHERE {}",
                w.sql
            ),
            params_from_iter(w.args),
            |x| Ok((x.get(0)?, x.get(1)?)),
        )?)
    })
    .await
}

/// The role one champion was played in most in the slice, every tier summed
/// (`champion_stats_totals`) or the named tier's; `None` when it has no games. A tie goes to a lane
/// over the roleless rows, then to the role that sorts first.
pub async fn top_role(db: &Db, r: Read) -> Result<Option<String>, DbError> {
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        let r = Read { role: None, ..r };
        let (table, keys) = stats_source(&r, false);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        Ok(c.query_row(
            &format!(
                "SELECT role FROM {table} WHERE {}
                  GROUP BY role ORDER BY sum(games) DESC, role = '', role LIMIT 1",
                w.sql
            ),
            params_from_iter(w.args),
            |x| x.get(0),
        )
        .optional()?)
    })
    .await
}

/// The lane one champion's set builds against `r.opponent_id` have the most
/// games in (FLT-04); `None` when it has none. Every row is a lane, so a tie
/// goes to the lane that sorts first (ADR-121's second rule).
pub async fn top_lane(db: &Db, r: Read) -> Result<Option<String>, DbError> {
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        let r = Read { role: None, ..r };
        let w = Filter::new(&r, &domain(c, &r)?, MATCHUP_BUILDS);
        Ok(c.query_row(
            &format!(
                "SELECT role FROM champion_matchup_builds WHERE {}
                  GROUP BY role ORDER BY sum(games) DESC, role LIMIT 1",
                w.sql
            ),
            params_from_iter(w.args),
            |x| x.get(0),
        )
        .optional()?)
    })
    .await
}

/// What the players of one set build chose besides its core: `part` is
/// `item3`–`item5`, `starter`, `boots`, `skill_order`, `runes` or `spells`,
/// and `value` is as stored in `champion_build_parts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPartRow {
    pub core: [i64; 2],
    pub part: String,
    pub value: String,
    pub games: i64,
    pub wins: i64,
}

/// The parts of one champion's `cores`, summed as [`builds`] sums them; by
/// core and part, then most played first.
pub async fn build_parts(db: &Db, r: Read, cores: Vec<[i64; 2]>) -> Result<Vec<BuildPartRow>, DbError> {
    // A core is stored as `json_array`'s text, which is what `json_each`
    // gives back for each inner array.
    let cores = serde_json::to_string(&cores).unwrap_or_else(|_| "[]".into());
    db.read(move |c| {
        let (_, table, keys) = build_source(&r);
        let w = Filter::new(&r, &domain(c, &r)?, keys);
        let mut stmt = c.prepare(&format!(
            "SELECT json_extract(core, '$[0]'), json_extract(core, '$[1]'), part, value, sum(games), sum(wins)
               FROM {table} WHERE {}
                AND core IN (SELECT j.value FROM json_each(?) j)
              GROUP BY core, part, value
              ORDER BY core, part, sum(games) DESC, value",
            w.sql
        ))?;
        let rows = stmt
            .query_map(params_from_iter(w.with([cores.into()])), |x| {
                Ok(BuildPartRow {
                    core: [x.get(0)?, x.get(1)?],
                    part: x.get(2)?,
                    value: x.get(3)?,
                    games: x.get(4)?,
                    wins: x.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Rows per analytics table for one ladder: the dashboard's and the
/// `proxy_aggregate_rows` gauge's view.
pub fn count(c: &Connection, s: &Scope, table: &str) -> Result<i64, DbError> {
    Ok(c.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3"),
        params![s.key_scope, s.platform, s.queue],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests;

//! The analytics tables (V0005, ADR-056; v1 `db/analytics.ts`): rebuilt from
//! `match_facts` per ladder (platform, queue) and read by `/v1/lol/analytics/*`.
//!
//! Every participant of the platform's archived matches counts (ADR-105), at
//! the tier stamped when the match was archived (`match_tiers`, THR-02,
//! ADR-127): the newer of the ladder's and the last league lookup's then, or
//! [`UNKNOWN_TIER`] when neither had them. A match with players in several
//! tiers counts in each (v1); `analytics_match_totals` and
//! `champion_ban_totals` count it once, for rows summed over every tier
//! (ADR-123). Every table is
//! rebuilt wholesale for the newest `AGGREGATE_PATCH_LIMIT` patches (0: all)
//! and keeps older patches' rows. Every row carries `remake`, so a read can
//! leave remakes out (the default) or add them back.

use rusqlite::{Connection, params};

use crate::db::{Db, DbError};

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
const PATCH_DESC: &str = "CAST(substr(patch, 1, instr(patch, '.') - 1) AS INTEGER) DESC, \
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

/// A fact's tier in [`ladder_facts`]: its stamp (THR-02).
const TIER: &str = "mt.tier";

/// The platform's matches only: a match id starts with its platform (`OC1_…`).
/// Binds `?2` platform.
const ON_PLATFORM: &str = "substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'";

/// Facts of the platform's archived matches in this queue, each with its
/// player's stamped [`TIER`], plus any further `join`. A fact with no stamp
/// (a match `tiers:backfill` hasn't reached) doesn't count. Binds `?1` key
/// scope, `?2` platform, `?3` queue, `?4` queue id, `?5` patches.
fn ladder_facts(join: &str) -> String {
    format!(
        "FROM match_facts f
         JOIN matches m ON m.match_id = f.match_id
         JOIN match_tiers mt ON mt.match_id = f.match_id AND mt.puuid = f.puuid
           AND mt.key_scope = f.key_scope AND mt.platform = ?2 AND mt.queue = ?3
         {join}
        WHERE f.key_scope = ?1 AND m.queue_id = ?4 AND m.patch IS NOT NULL AND {ON_PLATFORM}
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))"
    )
}

/// Rows written per table by one rebuild step.
pub type Written = Vec<(&'static str, i64)>;

/// Slices, champion stats and bans, and their totals over every tier
/// (SITE-08), in one transaction (v1's champion step).
pub fn rebuild_champions(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let facts = ladder_facts("");
    let tx = c.transaction()?;
    for t in [
        "analytics_slices",
        "analytics_match_totals",
        "champion_stats",
        "champion_bans",
        "champion_ban_totals",
    ] {
        s.clear(&tx, t)?;
    }
    s.insert(
        &tx,
        &format!(
            "INSERT INTO analytics_slices (key_scope, platform, queue, tier, patch, remake, matches, computed_at)
             SELECT ?1, ?2, ?3, {TIER}, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
             {facts}
             GROUP BY {TIER}, m.patch, coalesce(m.remake, 0)"
        ),
    )?;
    // The same matches once each, whatever tiers their players are in: the
    // denominator of a row summed over every tier (ADR-123).
    s.insert(
        &tx,
        &format!(
            "INSERT INTO analytics_match_totals (key_scope, platform, queue, patch, remake, matches, computed_at)
             SELECT ?1, ?2, ?3, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
             {facts}
             GROUP BY m.patch, coalesce(m.remake, 0)"
        ),
    )?;
    let stats = s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake,
               games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
               duration_s, computed_at)
             SELECT ?1, ?2, ?3, {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0),
               count(*), sum(f.win), count(DISTINCT m.match_id), count(f.kills),
               coalesce(sum(f.kills), 0), coalesce(sum(f.deaths), 0), coalesce(sum(f.assists), 0),
               coalesce(sum(f.cs), 0), coalesce(sum(f.gold), 0), coalesce(sum(f.damage), 0),
               coalesce(sum(f.vision), 0),
               coalesce(sum(m.game_duration) FILTER (WHERE f.kills IS NOT NULL), 0), ?6
             {facts}
             GROUP BY {TIER}, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0)"
        ),
    )?;
    // A ban counts once per match, in every tier the match had a player in (v1).
    let facts = ladder_facts("JOIN match_bans b ON b.match_id = f.match_id");
    s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_bans (key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at)
             SELECT ?1, ?2, ?3, {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
             {facts}
             GROUP BY {TIER}, m.patch, b.champion_id, coalesce(m.remake, 0)"
        ),
    )?;
    // And once per match over every tier.
    s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_ban_totals (key_scope, platform, queue, patch, champion_id, remake, bans, computed_at)
             SELECT ?1, ?2, ?3, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
             {facts}
             GROUP BY m.patch, b.champion_id, coalesce(m.remake, 0)"
        ),
    )?;
    tx.commit()?;
    Ok(vec![("champion_stats", stats)])
}

/// Lane matchups (v1): two players of opposite teams in the same lane, each
/// lane held by exactly one player per team, mirror lanes excluded. Recorded
/// from both sides, since every participant counts (ADR-105).
///
/// Each laned fact is read once, with its lane's head count from a window,
/// and that set is joined to itself (ADR-101). The earlier shape (two
/// `match_facts`, `matches` and a grouped lane count joined twice) let SQLite,
/// which has no `ANALYZE` statistics, loop over the whole ladder for every
/// fact: 44 s for a 16,600-game ladder.
const MATCHUPS: &str = "WITH lane AS (
       SELECT f.match_id, f.puuid, f.team_id, f.position, f.champion_id, f.win, m.patch,
         coalesce(m.remake, 0) AS remake,
         count(*) OVER (PARTITION BY f.match_id, f.team_id, f.position) AS n
         FROM match_facts f JOIN matches m ON m.match_id = f.match_id
        WHERE f.key_scope = ?1 AND f.position IS NOT NULL AND m.queue_id = ?4 AND m.patch IS NOT NULL
          AND substr(m.match_id, 1, length(?2) + 1) = upper(?2) || '_'
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))
     )
     INSERT INTO champion_matchups (key_scope, platform, queue, patch, champion_id, role, opponent_id,
       remake, games, wins, computed_at)
     SELECT ?1, ?2, ?3, a.patch, a.champion_id, a.position, b.champion_id, a.remake, count(*), sum(a.win), ?6
       FROM lane a
       JOIN lane b ON b.match_id = a.match_id AND b.position = a.position AND b.team_id <> a.team_id
      WHERE a.n = 1 AND b.n = 1 AND a.champion_id <> b.champion_id
      GROUP BY a.patch, a.position, a.champion_id, b.champion_id, a.remake";

pub fn rebuild_matchups(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let tx = c.transaction()?;
    s.clear(&tx, "champion_matchups")?;
    let n = s.insert(&tx, MATCHUPS)?;
    tx.commit()?;
    Ok(vec![("champion_matchups", n)])
}

/// Items, runes and spells, a transaction each (v1's builds step), then set
/// builds and their parts in one more (BLD-02).
pub fn rebuild_builds(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let mut out = vec![];
    // Items: slots 0–5, empty slots skipped, a player's duplicate item once.
    let facts = ladder_facts("JOIN json_each(f.items) i");
    let tx = c.transaction()?;
    s.clear(&tx, "champion_items")?;
    let n = s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_items (key_scope, platform, queue, patch, champion_id, role, item_id, remake,
               games, wins, computed_at)
             SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER),
               coalesce(m.remake, 0), count(DISTINCT f.match_id || ' ' || f.puuid),
               count(DISTINCT CASE WHEN f.win THEN f.match_id || ' ' || f.puuid END), ?6
             {facts}
               AND i.value IS NOT NULL AND i.value <> 0
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), CAST(i.value AS INTEGER), coalesce(m.remake, 0)"
        ),
    )?;
    tx.commit()?;
    out.push(("champion_items", n));

    let facts = ladder_facts("");
    let tx = c.transaction()?;
    s.clear(&tx, "champion_runes")?;
    let n = s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_runes (key_scope, platform, queue, patch, champion_id, role, keystone_id,
               sub_style_id, remake, games, wins, computed_at)
             SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               json_extract(f.runes, '$.keystone'), json_extract(f.runes, '$.subStyle'),
               coalesce(m.remake, 0), count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.runes, '$.keystone') IS NOT NULL
               AND json_extract(f.runes, '$.subStyle') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''), json_extract(f.runes, '$.keystone'),
               json_extract(f.runes, '$.subStyle'), coalesce(m.remake, 0)"
        ),
    )?;
    tx.commit()?;
    out.push(("champion_runes", n));

    // Spells as an unordered pair: Flash on D and on F is one build (v1).
    let tx = c.transaction()?;
    s.clear(&tx, "champion_spells")?;
    let n = s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_spells (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b,
               remake, games, wins, computed_at)
             SELECT ?1, ?2, ?3, m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0), count(*), sum(f.win), ?6
             {facts}
               AND json_extract(f.summoners, '$[0]') IS NOT NULL
               AND json_extract(f.summoners, '$[1]') IS NOT NULL
             GROUP BY m.patch, f.champion_id, coalesce(f.position, ''),
               min(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               max(json_extract(f.summoners, '$[0]'), json_extract(f.summoners, '$[1]')),
               coalesce(m.remake, 0)"
        ),
    )?;
    tx.commit()?;
    out.push(("champion_spells", n));

    let tx = c.transaction()?;
    for t in ["champion_builds", "champion_build_parts"] {
        s.clear(&tx, t)?;
    }
    let players = set_build_players();
    let n = s.insert(
        &tx,
        &format!(
            "{players}
             INSERT INTO champion_builds (key_scope, platform, queue, patch, champion_id, role, remake, core,
               games, wins, computed_at)
             SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, count(*), sum(win), ?6
               FROM p GROUP BY patch, champion_id, role, remake, core"
        ),
    )?;
    out.push(("champion_builds", n));
    let n = s.insert(
        &tx,
        &format!(
            "{players},
             k (part) AS (VALUES ('item3'), ('item4'), ('item5'), ('starter'), ('boots'), ('skill_order'),
               ('runes'), ('spells')),
             v AS (
               SELECT p.patch, p.champion_id, p.role, p.remake, p.core, p.win, k.part,
                 CASE k.part
                   WHEN 'item3' THEN CAST(json_extract(p.items, '$[2]') AS TEXT)
                   WHEN 'item4' THEN CAST(json_extract(p.items, '$[3]') AS TEXT)
                   WHEN 'item5' THEN CAST(json_extract(p.items, '$[4]') AS TEXT)
                   WHEN 'starter' THEN nullif(p.starter, '[]')
                   WHEN 'boots' THEN CAST(p.boots AS TEXT)
                   WHEN 'skill_order' THEN nullif(p.skill_order, '')
                   WHEN 'runes' THEN json_extract(p.runes, '$.keystone') || ':' || json_extract(p.runes, '$.subStyle')
                   WHEN 'spells' THEN
                     min(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]')) || ':'
                       || max(json_extract(p.summoners, '$[0]'), json_extract(p.summoners, '$[1]'))
                 END AS value
                 FROM p CROSS JOIN k
             )
             INSERT INTO champion_build_parts (key_scope, platform, queue, patch, champion_id, role, remake, core,
               part, value, games, wins, computed_at)
             SELECT ?1, ?2, ?3, patch, champion_id, role, remake, core, part, value, count(*), sum(win), ?6
               FROM v WHERE value IS NOT NULL
              GROUP BY patch, champion_id, role, remake, core, part, value"
        ),
    )?;
    tx.commit()?;
    out.push(("champion_build_parts", n));
    Ok(out)
}

/// Set builds (BLD-02, ADR-120): `p`, the facts of the players whose
/// `match_builds` row has two or more finished items, each with its `core`,
/// the first two as `"[a,b]"`. A player with fewer counts in no build and no
/// part. Binds as [`ladder_facts`]; begins a `WITH`.
fn set_build_players() -> String {
    let facts = ladder_facts(
        "JOIN match_builds b ON b.match_id = f.match_id AND b.key_scope = f.key_scope AND b.puuid = f.puuid",
    );
    format!(
        "WITH p AS (
           SELECT m.patch, f.champion_id, coalesce(f.position, '') AS role, coalesce(m.remake, 0) AS remake, f.win,
             json_array(json_extract(b.items, '$[0]'), json_extract(b.items, '$[1]')) AS core,
             b.items, b.starter, b.boots, b.skill_order, f.runes, f.summoners
           {facts}
             AND json_array_length(b.items) >= 2
         )"
    )
}

// ── Reads ───────────────────────────────────────────────────────────────────

/// Which slice a read asks for.
#[derive(Debug, Clone)]
pub struct Read {
    pub key_scope: String,
    /// `None`: every platform summed (ADR-065).
    pub platform: Option<String>,
    pub queue: String,
    /// `None`: every patch summed (`?patch=all`, ADR-094).
    pub patch: Option<String>,
    /// `None`: every tier, summed by [`stats`] (ADR-123).
    pub tier: Option<String>,
    /// `None`: every role summed; `Some("")` is the roleless rows.
    pub role: Option<String>,
    pub champion_id: Option<i64>,
    pub min_games: i64,
    pub limit: i64,
    /// Add remakes' rows back in (`?remakes=include`).
    pub remakes: bool,
}

/// The newest patch this ladder has stats for (v1 `latestPatch`).
pub async fn latest_patch(
    db: &Db,
    key_scope: &str,
    platform: Option<&str>,
    queue: &str,
) -> Result<Option<String>, DbError> {
    let (scope, platform, queue) = (
        key_scope.to_string(),
        platform.map(str::to_string),
        queue.to_string(),
    );
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            &format!(
                "SELECT patch FROM (SELECT DISTINCT patch FROM champion_stats
                  WHERE key_scope = ?1 AND (?2 IS NULL OR platform = ?2) AND queue = ?3)
                 ORDER BY {PATCH_DESC} LIMIT 1"
            ),
            params![scope, platform, queue],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
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
/// played on, and its games (ADR-100).
pub async fn patches(
    db: &Db,
    key_scope: &str,
    platform: Option<&str>,
    queue: &str,
    champion_id: Option<i64>,
    remakes: bool,
) -> Result<Vec<PatchRow>, DbError> {
    let (scope, platform, queue) = (
        key_scope.to_string(),
        platform.map(str::to_string),
        queue.to_string(),
    );
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT patch, sum(games), max(computed_at) FROM champion_stats
              WHERE key_scope = ?1 AND (?2 IS NULL OR platform = ?2) AND queue = ?3 AND (?4 OR remake = 0)
                AND (?5 IS NULL OR champion_id = ?5)
              GROUP BY patch ORDER BY {PATCH_DESC}"
        ))?;
        let rows = stmt
            .query_map(params![scope, platform, queue, remakes, champion_id], |r| {
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

/// The filter a read uses: `?1`–`?4` scope/platform/queue/patch (NULL platform
/// or patch: all) and `?8`
/// whether remakes count always; `?5` tier, `?6` role and `?7` champion for
/// the tables that have those columns. Unused numbers are bound but unread.
fn read_where(tier: bool, role: bool, champion: bool) -> String {
    let mut w = String::from(
        "key_scope = ?1 AND (?2 IS NULL OR platform = ?2) AND queue = ?3 AND (?4 IS NULL OR patch = ?4)
           AND (?8 OR remake = 0)",
    );
    if tier {
        w.push_str(" AND (?5 IS NULL OR tier = ?5)");
    }
    if role {
        w.push_str(" AND (?6 IS NULL OR role = ?6)");
    }
    if champion {
        w.push_str(" AND (?7 IS NULL OR champion_id = ?7)");
    }
    w
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
    let by_tier = r.tier.is_some();
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
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, {tier}, coalesce(?4, 'all'), sum(games), sum(wins), sum(matches_picked),
                    sum(stated_games), sum(kills), sum(deaths), sum(assists), sum(cs), sum(gold), sum(damage),
                    sum(vision), sum(duration_s), max(computed_at)
               FROM champion_stats WHERE {}
              GROUP BY {group} HAVING sum(games) >= ?9
              ORDER BY sum(games) DESC, {group} LIMIT ?10",
            read_where(true, true, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    r.tier,
                    r.role,
                    r.champion_id,
                    r.remakes,
                    r.min_games,
                    r.limit
                ],
                |x| {
                    Ok(StatRow {
                        champion_id: x.get(0)?,
                        tier: x.get(1)?,
                        patch: x.get(2)?,
                        games: x.get(3)?,
                        wins: x.get(4)?,
                        matches_picked: x.get(5)?,
                        stated_games: x.get(6)?,
                        kills: x.get(7)?,
                        deaths: x.get(8)?,
                        assists: x.get(9)?,
                        cs: x.get(10)?,
                        gold: x.get(11)?,
                        damage: x.get(12)?,
                        vision: x.get(13)?,
                        duration_s: x.get(14)?,
                        computed_at: x.get(15)?,
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
        let mut stmt = c.prepare(&format!(
            "SELECT tier, sum(matches) FROM analytics_slices WHERE {} GROUP BY tier",
            read_where(true, false, false)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    r.tier,
                    None::<String>,
                    None::<i64>,
                    r.remakes
                ],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Bans per (tier, champion) (v1 `listChampionBans`).
pub async fn bans(db: &Db, r: Read) -> Result<Vec<(String, i64, i64)>, DbError> {
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT tier, champion_id, sum(bans) FROM champion_bans WHERE {} GROUP BY tier, champion_id",
            read_where(true, false, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    r.tier,
                    None::<String>,
                    r.champion_id,
                    r.remakes
                ],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Distinct matches in the slice, every tier and role (ADR-123): the pick- and
/// ban-rate denominator of a row summed over every tier.
pub async fn match_totals(db: &Db, r: Read) -> Result<i64, DbError> {
    db.read(move |c| {
        Ok(c.query_row(
            &format!(
                "SELECT coalesce(sum(matches), 0) FROM analytics_match_totals WHERE {}",
                read_where(false, false, false)
            ),
            params![
                r.key_scope,
                r.platform,
                r.queue,
                r.patch,
                None::<String>,
                None::<String>,
                None::<i64>,
                r.remakes
            ],
            |x| x.get(0),
        )?)
    })
    .await
}

/// Bans per champion over every tier, a match once (ADR-123): `(champion, bans)`.
pub async fn ban_totals(db: &Db, r: Read) -> Result<Vec<(i64, i64)>, DbError> {
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, sum(bans) FROM champion_ban_totals WHERE {} GROUP BY champion_id",
            read_where(false, false, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    None::<String>,
                    None::<String>,
                    r.champion_id,
                    r.remakes
                ],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )?
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
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}, sum(games), sum(wins), max(computed_at) FROM {} WHERE {}
              GROUP BY {cols} HAVING sum(games) >= ?9
              ORDER BY sum(games) DESC, {cols} LIMIT ?10",
            facet.table(),
            read_where(false, true, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    None::<String>,
                    r.role,
                    r.champion_id,
                    r.remakes,
                    r.min_games,
                    r.limit
                ],
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
/// played first.
pub async fn builds(db: &Db, r: Read) -> Result<Vec<BuildRow>, DbError> {
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT json_extract(core, '$[0]'), json_extract(core, '$[1]'), sum(games), sum(wins), max(computed_at)
               FROM champion_builds WHERE {}
              GROUP BY core HAVING sum(games) >= ?9
              ORDER BY sum(games) DESC, core LIMIT ?10",
            read_where(false, true, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    None::<String>,
                    r.role,
                    r.champion_id,
                    r.remakes,
                    r.min_games,
                    r.limit
                ],
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
        Ok(c.query_row(
            &format!(
                "SELECT coalesce(sum(games), 0), max(computed_at) FROM champion_builds WHERE {}",
                read_where(false, true, true)
            ),
            params![
                r.key_scope,
                r.platform,
                r.queue,
                r.patch,
                None::<String>,
                r.role,
                r.champion_id,
                r.remakes
            ],
            |x| Ok((x.get(0)?, x.get(1)?)),
        )?)
    })
    .await
}

/// The role one champion was played in most in the slice, every tier summed
/// (`champion_stats`); `None` when it has no games. A tie goes to a lane
/// over the roleless rows, then to the role that sorts first.
pub async fn top_role(db: &Db, r: Read) -> Result<Option<String>, DbError> {
    db.read(move |c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            &format!(
                "SELECT role FROM champion_stats WHERE {}
                  GROUP BY role ORDER BY sum(games) DESC, role = '', role LIMIT 1",
                read_where(false, false, true)
            ),
            params![
                r.key_scope,
                r.platform,
                r.queue,
                r.patch,
                None::<String>,
                None::<String>,
                r.champion_id,
                r.remakes
            ],
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
        let mut stmt = c.prepare(&format!(
            "SELECT json_extract(core, '$[0]'), json_extract(core, '$[1]'), part, value, sum(games), sum(wins)
               FROM champion_build_parts WHERE {}
                AND core IN (SELECT j.value FROM json_each(?9) j)
              GROUP BY core, part, value
              ORDER BY core, part, sum(games) DESC, value",
            read_where(false, true, true)
        ))?;
        let rows = stmt
            .query_map(
                params![
                    r.key_scope,
                    r.platform,
                    r.queue,
                    r.patch,
                    None::<String>,
                    r.role,
                    r.champion_id,
                    r.remakes,
                    cores
                ],
                |x| {
                    Ok(BuildPartRow {
                        core: [x.get(0)?, x.get(1)?],
                        part: x.get(2)?,
                        value: x.get(3)?,
                        games: x.get(4)?,
                        wins: x.get(5)?,
                    })
                },
            )?
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

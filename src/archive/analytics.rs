//! The analytics tables (V0005, ADR-056; v1 `db/analytics.ts`): rebuilt from
//! `match_facts` per ladder (platform, queue) and read by `/v1/lol/analytics/*`.
//!
//! Each participant counts at the tier the ladder holds them at
//! (`ladder_entries`); participants the ladder does not hold are left out, and
//! a match with players in several tiers counts in each (v1). Every table is
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

/// Facts of this ladder's archived matches, each joined to its player's tier,
/// plus any further `join`. Binds `?1` key scope, `?2` platform, `?3` queue,
/// `?4` queue id, `?5` patches.
fn ladder_facts(join: &str) -> String {
    format!(
        "FROM match_facts f
         JOIN matches m ON m.match_id = f.match_id
         JOIN ladder_entries le ON le.key_scope = f.key_scope AND le.platform = ?2
           AND le.queue = ?3 AND le.puuid = f.puuid
         {join}
        WHERE f.key_scope = ?1 AND m.queue_id = ?4 AND m.patch IS NOT NULL
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))"
    )
}

/// Rows written per table by one rebuild step.
pub type Written = Vec<(&'static str, i64)>;

/// Slices, champion stats and bans, in one transaction (v1's champion step).
pub fn rebuild_champions(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let facts = ladder_facts("");
    let tx = c.transaction()?;
    for t in ["analytics_slices", "champion_stats", "champion_bans"] {
        s.clear(&tx, t)?;
    }
    s.insert(
        &tx,
        &format!(
            "INSERT INTO analytics_slices (key_scope, platform, queue, tier, patch, remake, matches, computed_at)
             SELECT ?1, ?2, ?3, le.tier, m.patch, coalesce(m.remake, 0), count(DISTINCT m.match_id), ?6
             {facts}
             GROUP BY le.tier, m.patch, coalesce(m.remake, 0)"
        ),
    )?;
    let stats = s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake,
               games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
               duration_s, computed_at)
             SELECT ?1, ?2, ?3, le.tier, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0),
               count(*), sum(f.win), count(DISTINCT m.match_id), count(f.kills),
               coalesce(sum(f.kills), 0), coalesce(sum(f.deaths), 0), coalesce(sum(f.assists), 0),
               coalesce(sum(f.cs), 0), coalesce(sum(f.gold), 0), coalesce(sum(f.damage), 0),
               coalesce(sum(f.vision), 0),
               coalesce(sum(m.game_duration) FILTER (WHERE f.kills IS NOT NULL), 0), ?6
             {facts}
             GROUP BY le.tier, m.patch, f.champion_id, coalesce(f.position, ''), coalesce(m.remake, 0)"
        ),
    )?;
    // A ban counts once per match, in every tier the match had a player in (v1).
    let facts = ladder_facts("JOIN match_bans b ON b.match_id = f.match_id");
    s.insert(
        &tx,
        &format!(
            "INSERT INTO champion_bans (key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at)
             SELECT ?1, ?2, ?3, le.tier, m.patch, b.champion_id, coalesce(m.remake, 0), count(DISTINCT b.match_id), ?6
             {facts}
             GROUP BY le.tier, m.patch, b.champion_id, coalesce(m.remake, 0)"
        ),
    )?;
    tx.commit()?;
    Ok(vec![("champion_stats", stats)])
}

/// Lane matchups (v1): two players of opposite teams in the same lane, each
/// lane held by exactly one player per team, mirror lanes excluded. Recorded
/// from the side of the player the ladder holds.
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
          AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))
     )
     INSERT INTO champion_matchups (key_scope, platform, queue, patch, champion_id, role, opponent_id,
       remake, games, wins, computed_at)
     SELECT ?1, ?2, ?3, a.patch, a.champion_id, a.position, b.champion_id, a.remake, count(*), sum(a.win), ?6
       FROM lane a
       JOIN lane b ON b.match_id = a.match_id AND b.position = a.position AND b.team_id <> a.team_id
       JOIN ladder_entries le ON le.key_scope = ?1 AND le.platform = ?2 AND le.queue = ?3
         AND le.puuid = a.puuid
      WHERE a.n = 1 AND b.n = 1 AND a.champion_id <> b.champion_id
      GROUP BY a.patch, a.position, a.champion_id, b.champion_id, a.remake";

pub fn rebuild_matchups(c: &mut Connection, s: &Scope) -> Result<Written, DbError> {
    let tx = c.transaction()?;
    s.clear(&tx, "champion_matchups")?;
    let n = s.insert(&tx, MATCHUPS)?;
    tx.commit()?;
    Ok(vec![("champion_matchups", n)])
}

/// Items, runes and spells, a transaction each (v1's builds step).
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
    Ok(out)
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

/// One champion at one tier, its roles summed unless a role is asked for;
/// every patch summed when the read names none (`patch` is then `"all"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatRow {
    pub champion_id: i64,
    pub tier: String,
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

/// v1 `listChampionStats`: most played first.
pub async fn stats(db: &Db, r: Read) -> Result<Vec<StatRow>, DbError> {
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT champion_id, tier, coalesce(?4, 'all'), sum(games), sum(wins), sum(matches_picked), sum(stated_games),
                    sum(kills), sum(deaths), sum(assists), sum(cs), sum(gold), sum(damage), sum(vision),
                    sum(duration_s), max(computed_at)
               FROM champion_stats WHERE {}
              GROUP BY champion_id, tier HAVING sum(games) >= ?9
              ORDER BY sum(games) DESC, champion_id, tier LIMIT ?10",
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

//! A player's champion pool, grouped at read time from `match_facts` (v1
//! `listPlayerChampions`, #113). It only knows the games this deployment has
//! archived, and never calls Riot.

use crate::db::{Db, DbError};

#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// A platform (`euw1`), matched against each match id's own prefix (v1).
    pub platform: Option<String>,
    pub queue_id: Option<i64>,
    /// `major.minor`, as `matches.patch`.
    pub patch: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChampionRow {
    pub champion_id: i64,
    pub games: i64,
    pub wins: i64,
    /// Games whose facts carry a K/D/A: the averages' denominator (v1 `statedGames`).
    pub stated_games: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    /// CS summed over the games whose facts carry CS and whose match carries a
    /// length; `cs_seconds` is those games' total length, so `cs / minutes` is
    /// CS per minute over the same games.
    pub cs: i64,
    pub cs_seconds: i64,
    /// The newest archived game's end, unix ms.
    pub last_played_ms: Option<i64>,
}

/// Most played first; ties by champion id so the order is stable.
pub async fn champions(
    db: &Db,
    key_scope: &str,
    puuid: &str,
    f: Filter,
) -> Result<Vec<ChampionRow>, DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.read(move |c| {
        let prefix = f.platform.map(|p| format!("{}_", p.to_ascii_uppercase()));
        let mut stmt = c.prepare_cached(
            "SELECT f.champion_id, count(*), sum(f.win), count(f.kills),
                    coalesce(sum(f.kills), 0), coalesce(sum(f.deaths), 0), coalesce(sum(f.assists), 0),
                    max(m.game_end_ms),
                    coalesce(sum(f.cs) FILTER (WHERE m.game_duration IS NOT NULL), 0),
                    coalesce(sum(m.game_duration) FILTER (WHERE f.cs IS NOT NULL), 0)
             FROM match_facts f JOIN matches m ON m.match_id = f.match_id
             WHERE f.key_scope = ?1 AND f.puuid = ?2
               AND (?3 IS NULL OR substr(f.match_id, 1, length(?3)) = ?3)
               AND (?4 IS NULL OR m.queue_id = ?4)
               AND (?5 IS NULL OR m.patch = ?5)
             GROUP BY f.champion_id
             ORDER BY count(*) DESC, f.champion_id
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![scope, puuid, prefix, f.queue_id, f.patch, f.limit],
            |r| {
                Ok(ChampionRow {
                    champion_id: r.get(0)?,
                    games: r.get(1)?,
                    wins: r.get(2)?,
                    stated_games: r.get(3)?,
                    kills: r.get(4)?,
                    deaths: r.get(5)?,
                    assists: r.get(6)?,
                    last_played_ms: r.get(7)?,
                    cs: r.get(8)?,
                    cs_seconds: r.get(9)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    })
    .await
}

#[cfg(test)]
mod tests;

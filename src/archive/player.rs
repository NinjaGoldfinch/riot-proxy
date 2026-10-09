//! One player's slice of the archive, for the admin player view (DEV-03,
//! ADR-074): what is stored for them, read from `match_facts` (one row per
//! participant, indexed by `(key_scope, puuid)`). Exact counts, no caps.

use rusqlite::params;

use crate::db::{Db, DbError};

/// Totals over every archived match the player appears in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub matches: i64,
    /// Matches Riot flagged as remakes (`matches.remake = 1`).
    pub remakes: i64,
    pub wins: i64,
    /// Of those matches, how many also have a stored timeline.
    pub timelines: i64,
    pub oldest_game_end_ms: Option<i64>,
    pub newest_game_end_ms: Option<i64>,
    /// `(queue_id, matches)`, most matches first.
    pub by_queue: Vec<(i64, i64)>,
}

pub async fn summary(db: &Db, key_scope: &str, puuid: &str) -> Result<Summary, DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.read(move |c| {
        let mut s = c.query_row(
            "SELECT count(*), coalesce(sum(m.remake = 1), 0), coalesce(sum(f.win), 0),
                    coalesce(sum(t.match_id IS NOT NULL), 0), min(m.game_end_ms), max(m.game_end_ms)
               FROM match_facts f
               JOIN matches m ON m.match_id = f.match_id
               LEFT JOIN timelines t ON t.match_id = f.match_id
              WHERE f.key_scope = ?1 AND f.puuid = ?2",
            params![scope, puuid],
            |r| {
                Ok(Summary {
                    matches: r.get(0)?,
                    remakes: r.get(1)?,
                    wins: r.get(2)?,
                    timelines: r.get(3)?,
                    oldest_game_end_ms: r.get(4)?,
                    newest_game_end_ms: r.get(5)?,
                    by_queue: Vec::new(),
                })
            },
        )?;
        let mut q = c.prepare(
            "SELECT m.queue_id, count(*) FROM match_facts f JOIN matches m ON m.match_id = f.match_id
              WHERE f.key_scope = ?1 AND f.puuid = ?2
              GROUP BY m.queue_id ORDER BY count(*) DESC, m.queue_id",
        )?;
        s.by_queue = q
            .query_map(params![scope, puuid], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(s)
    })
    .await
}

/// The player's line in one archived match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub match_id: String,
    pub queue_id: i64,
    pub game_end_ms: i64,
    /// Seconds; null for rows archived before V0003 and not yet re-extracted.
    pub game_duration: Option<i64>,
    /// Null until facts:reextract has looked (V0005).
    pub remake: Option<bool>,
    pub champion_id: i64,
    pub position: Option<String>,
    pub win: bool,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub cs: Option<i64>,
}

/// One page of the player's archived matches, newest first, and how many
/// there are in all (for the same filter).
pub async fn lines(
    db: &Db,
    key_scope: &str,
    puuid: &str,
    queue: Option<i64>,
    start: i64,
    count: i64,
) -> Result<(i64, Vec<Line>), DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.read(move |c| {
        let total = c.query_row(
            "SELECT count(*) FROM match_facts f JOIN matches m ON m.match_id = f.match_id
              WHERE f.key_scope = ?1 AND f.puuid = ?2 AND (?3 IS NULL OR m.queue_id = ?3)",
            params![scope, puuid, queue],
            |r| r.get(0),
        )?;
        let mut s = c.prepare(
            "SELECT f.match_id, m.queue_id, m.game_end_ms, m.game_duration, m.remake, f.champion_id,
                    f.position, f.win, f.kills, f.deaths, f.assists, f.cs
               FROM match_facts f JOIN matches m ON m.match_id = f.match_id
              WHERE f.key_scope = ?1 AND f.puuid = ?2 AND (?3 IS NULL OR m.queue_id = ?3)
              ORDER BY m.game_end_ms DESC, f.match_id DESC
              LIMIT ?4 OFFSET ?5",
        )?;
        let rows = s
            .query_map(params![scope, puuid, queue, count, start], |r| {
                Ok(Line {
                    match_id: r.get(0)?,
                    queue_id: r.get(1)?,
                    game_end_ms: r.get(2)?,
                    game_duration: r.get(3)?,
                    remake: r.get::<_, Option<i64>>(4)?.map(|v| v != 0),
                    champion_id: r.get(5)?,
                    position: r.get(6)?,
                    win: r.get::<_, i64>(7)? != 0,
                    kills: r.get(8)?,
                    deaths: r.get(9)?,
                    assists: r.get(10)?,
                    cs: r.get(11)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok((total, rows))
    })
    .await
}

/// The ids of the player's archived games on one champion, newest first, in
/// `region` (match-v5's routing value, as `matches.region`): the match page's
/// champion filter (SITE-02). At most `count` ids from `start`.
#[allow(clippy::too_many_arguments)]
pub async fn champion_match_ids(
    db: &Db,
    key_scope: &str,
    puuid: &str,
    champion_id: i64,
    region: &str,
    queue: Option<i64>,
    start: i64,
    count: i64,
) -> Result<Vec<String>, DbError> {
    let (scope, puuid, region) = (key_scope.to_string(), puuid.to_string(), region.to_string());
    db.read(move |c| {
        let mut s = c.prepare_cached(
            "SELECT f.match_id
               FROM match_facts f JOIN matches m ON m.match_id = f.match_id
              WHERE f.key_scope = ?1 AND f.puuid = ?2 AND f.champion_id = ?3 AND m.region = ?4
                AND (?5 IS NULL OR m.queue_id = ?5)
              ORDER BY m.game_end_ms DESC, f.match_id DESC
              LIMIT ?6 OFFSET ?7",
        )?;
        let ids = s
            .query_map(
                params![scope, puuid, champion_id, region, queue, count, start],
                |r| r.get(0),
            )?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    })
    .await
}

#[cfg(test)]
mod tests;

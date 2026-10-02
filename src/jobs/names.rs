//! `names:backfill` (v1 `jobs/player-names.ts`, `backfillNamesFromArchive`):
//! Riot IDs for players known only by PUUID, which a crawl creates by the
//! thousand, read out of matches the archive already holds. No upstream call.
//!
//! A name is what the player was called *in that game*, so it comes from
//! their most recent archived match that carries one (of the last three:
//! some bodies have the Riot ID fields empty), and it only ever fills a null.
//! A name already set came from account-v1 or an admin, which outrank a past
//! game (v1).

use std::collections::{HashMap, HashSet};

use futures_util::future::BoxFuture;
use rusqlite::params;
use serde::Deserialize;
use serde_json::json;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{Enqueued, Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};

/// v1's safety valve: players examined per pass, freshest first.
const TARGET_LIMIT: i64 = 50_000;
/// Players resolved per read of the archive.
const CHUNK: usize = 500;
/// Matches examined per player (v1).
const RECENT: i64 = 3;

/// The job: one at a time, since a queued pass reads the same tables as the
/// one that would replace it (v1).
pub fn job() -> NewJob {
    NewJob::new(kinds::NAMES_BACKFILL, priority::MAINTENANCE, json!({})).dedupe(kinds::NAMES_BACKFILL)
}

pub async fn enqueue(queue: &Queue) -> Result<Enqueued, DbError> {
    queue.enqueue(job()).await
}

/// Players of this key scope known only by PUUID (v1 `countUnnamedPlayers`).
pub async fn count_unnamed(db: &Db, key_scope: &str) -> Result<i64, DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM players WHERE key_scope = ?1 AND game_name IS NULL",
            [scope],
            |r| r.get(0),
        )?)
    })
    .await
}

#[derive(Deserialize)]
struct Body {
    info: Info,
}

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    participants: Vec<Participant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Participant {
    puuid: Option<String>,
    riot_id_game_name: Option<String>,
    /// What matches from the Riot ID transition call the same field (v1).
    riot_id_name: Option<String>,
    riot_id_tagline: Option<String>,
}

/// `(puuid → (gameName, tagLine))` from one body. `summonerName` is not a
/// fallback: a name without a tag is not the identity anything looks a
/// player up by (v1).
fn names_in(body: &[u8]) -> HashMap<String, (String, Option<String>)> {
    let Ok(body) = serde_json::from_slice::<Body>(body) else {
        return HashMap::new();
    };
    let present = |s: Option<String>| s.filter(|v| !v.is_empty());
    body.info
        .participants
        .into_iter()
        .filter_map(|p| {
            let name = present(p.riot_id_game_name).or_else(|| present(p.riot_id_name))?;
            Some((p.puuid?, (name, present(p.riot_id_tagline))))
        })
        .collect()
}

/// One pass. Returns `(named, unnamed)`: players given a name now, and those
/// still without one.
pub async fn backfill(db: &Db, key_scope: &str) -> Result<(usize, i64), JobError> {
    let store = |e: &dyn std::fmt::Display| JobError::Retry(format!("store: {e}"));
    let scope = key_scope.to_string();
    let targets: Vec<String> = db
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT puuid FROM players WHERE key_scope = ?1 AND game_name IS NULL
                  ORDER BY updated_at DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![scope, TARGET_LIMIT], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .map_err(|e| store(&e))?;

    let mut named = 0;
    for chunk in targets.chunks(CHUNK) {
        // Each player's most recent matches, newest first.
        let (scope, players) = (key_scope.to_string(), chunk.to_vec());
        let recent: Vec<(String, Vec<String>)> = db
            .read(move |c| {
                let mut stmt = c.prepare_cached(
                    "SELECT f.match_id FROM match_facts f JOIN matches m ON m.match_id = f.match_id
                      WHERE f.key_scope = ?1 AND f.puuid = ?2
                      ORDER BY m.game_end_ms DESC, f.match_id DESC LIMIT ?3",
                )?;
                players
                    .into_iter()
                    .map(|p| {
                        let ids = stmt
                            .query_map(params![scope, p, RECENT], |r| r.get(0))?
                            .collect::<Result<Vec<String>, _>>()?;
                        Ok((p, ids))
                    })
                    .collect::<Result<Vec<_>, DbError>>()
            })
            .await
            .map_err(|e| store(&e))?;
        let wanted: Vec<String> = recent
            .iter()
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            continue;
        }
        // Ten players share a match: each body is read once per chunk.
        let bodies = crate::archive::matches::get_many(db, &wanted)
            .await
            .map_err(|e| store(&e))?;
        let parsed: HashMap<&str, HashMap<String, (String, Option<String>)>> =
            bodies.iter().map(|(id, b)| (id.as_str(), names_in(b))).collect();
        let found: Vec<(String, String, Option<String>)> = recent
            .iter()
            .filter_map(|(puuid, ids)| {
                ids.iter().find_map(|id| {
                    parsed
                        .get(id.as_str())
                        .and_then(|names| names.get(puuid))
                        .map(|(n, t)| (puuid.clone(), n.clone(), t.clone()))
                })
            })
            .collect();
        if found.is_empty() {
            continue;
        }
        let (scope, now) = (key_scope.to_string(), Clock::now().unix_ms);
        named += db
            .write(move |c| {
                let tx = c.transaction()?;
                let mut n = 0;
                {
                    let mut stmt = tx.prepare_cached(
                        "UPDATE players SET game_name = ?3, tag_line = ?4, updated_at = ?5
                          WHERE key_scope = ?1 AND puuid = ?2 AND game_name IS NULL",
                    )?;
                    for (puuid, name, tag) in &found {
                        n += stmt.execute(params![scope, puuid, name, tag, now])?;
                    }
                }
                tx.commit()?;
                Ok::<_, DbError>(n)
            })
            .await
            .map_err(|e| store(&e))?;
    }
    let unnamed = count_unnamed(db, key_scope).await.map_err(|e| store(&e))?;
    Ok((named, unnamed))
}

pub struct NamesBackfill {
    pub db: Db,
    pub key_scope: String,
}

impl NamesBackfill {
    pub async fn run(&self, _job: &Job) -> Result<(), JobError> {
        let started = std::time::Instant::now();
        let (named, unnamed) = backfill(&self.db, &self.key_scope).await?;
        tracing::info!(
            named,
            unnamed,
            ms = started.elapsed().as_millis(),
            "player names backfilled from archive"
        );
        Ok(())
    }
}

pub struct NamesBackfillHandler(pub std::sync::Arc<NamesBackfill>);

impl Handler for NamesBackfillHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.run(job))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_riot_id_fields_only() {
        let body = json!({"info": {"participants": [
            {"puuid": "A", "riotIdGameName": "Faker", "riotIdTagline": "KR1"},
            {"puuid": "B", "riotIdGameName": "", "riotIdName": "Old Name", "riotIdTagline": ""},
            {"puuid": "C", "riotIdGameName": "", "summonerName": "NoTag"},
            {"riotIdGameName": "Nobody"}
        ]}});
        let names = names_in(body.to_string().as_bytes());
        assert_eq!(names.len(), 2);
        assert_eq!(names["A"], ("Faker".to_string(), Some("KR1".to_string())));
        assert_eq!(names["B"], ("Old Name".to_string(), None));
        assert!(names_in(b"{}").is_empty());
    }
}

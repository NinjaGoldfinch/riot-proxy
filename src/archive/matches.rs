//! The match archive (docs/design/04 §Schema, §Cache tiers): completed matches
//! are immutable, so Riot is asked for each one once and the archive answers
//! after that. Bodies are stored as zstd blobs and come back byte-identical.
//!
//! `patch`, `queue_id` and `game_end_ms` are pulled out of the body on insert so
//! analytics and stats never open the blob. Timelines go in their own table,
//! only with `ARCHIVE_TIMELINES=true`, and only once their match is archived
//! (the foreign key; v1 skipped a timeline that arrived first the same way).
//!
//! Archiving a match also writes its `match_facts` in the same transaction
//! (v1 `archiveMatch`), for the key scope whose PUUIDs the body carries.

use bytes::Bytes;
use rusqlite::{OptionalExtension, params_from_iter};
use serde::Deserialize;

use crate::archive::facts::{self, Fact};
use crate::db::{Db, DbError};

/// design/04: zstd, level 3.
pub const ZSTD_LEVEL: i32 = 3;
/// Ids per `IN (…)` query in [`filter_unarchived`], well inside SQLite's limit.
const FILTER_CHUNK: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("zstd: {0}")]
    Zstd(#[from] std::io::Error),
    #[error("not a match-v5 body: {0}")]
    NotAMatch(String),
    #[error("a compression task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// The columns extracted from a match-v5 body on insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchMeta {
    /// `major.minor` of `info.gameVersion`, e.g. "14.18" (v1 migration 0005).
    pub patch: String,
    pub queue_id: i64,
    pub game_end_ms: i64,
}

#[derive(Deserialize)]
struct Body {
    info: Info,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    game_version: Option<String>,
    queue_id: Option<i64>,
    game_end_timestamp: Option<i64>,
}

/// Read the indexed columns out of a match body. Every one is `NOT NULL` in the
/// schema, so a body missing any of them is refused rather than guessed at; the
/// caller still gets the body, it just is not archived.
pub fn extract(body: &[u8]) -> Result<MatchMeta, ArchiveError> {
    let b: Body = serde_json::from_slice(body).map_err(|e| ArchiveError::NotAMatch(e.to_string()))?;
    let missing = |f: &str| ArchiveError::NotAMatch(format!("info.{f} is missing"));
    let version = b.info.game_version.ok_or_else(|| missing("gameVersion"))?;
    let mut parts = version.split('.');
    let patch = match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if !major.is_empty() && !minor.is_empty() => format!("{major}.{minor}"),
        _ => {
            return Err(ArchiveError::NotAMatch(format!(
                "gameVersion {version:?} has no major.minor"
            )));
        }
    };
    Ok(MatchMeta {
        patch,
        queue_id: b.info.queue_id.ok_or_else(|| missing("queueId"))?,
        game_end_ms: b
            .info
            .game_end_timestamp
            .ok_or_else(|| missing("gameEndTimestamp"))?,
    })
}

async fn compress(body: Bytes) -> Result<Vec<u8>, ArchiveError> {
    Ok(tokio::task::spawn_blocking(move || zstd::encode_all(body.as_ref(), ZSTD_LEVEL)).await??)
}

/// Parse and compress off the async runtime: a match body is ~100 KB.
async fn prepare(body: Bytes) -> Result<(MatchMeta, Vec<Fact>, Vec<u8>), ArchiveError> {
    tokio::task::spawn_blocking(move || {
        let meta = extract(&body)?;
        // Facts are a derivation: a body they cannot read is still archived.
        let rows = facts::extract(&body).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "match facts not extracted");
            Vec::new()
        });
        let blob = zstd::encode_all(body.as_ref(), ZSTD_LEVEL)?;
        Ok((meta, rows, blob))
    })
    .await?
}

fn decompress(blob: &[u8]) -> Result<Bytes, ArchiveError> {
    Ok(Bytes::from(zstd::decode_all(blob)?))
}

/// The archived match body, byte-identical to what Riot sent.
pub async fn get(db: &Db, match_id: &str) -> Result<Option<Bytes>, ArchiveError> {
    let id = match_id.to_string();
    db.read(move |conn| {
        let blob: Option<Vec<u8>> = conn
            .query_row("SELECT body_zstd FROM matches WHERE match_id = ?1", [&id], |r| {
                r.get(0)
            })
            .optional()?;
        blob.map(|b| decompress(&b)).transpose()
    })
    .await
}

/// Archive a match body and its facts. Idempotent: archiving it again rewrites
/// the same rows (v1 upserted too). `region` is the routing value the match came
/// from; `key_scope` is the key whose PUUIDs are in the body.
pub async fn put(
    db: &Db,
    match_id: &str,
    region: &str,
    key_scope: &str,
    body: Bytes,
    now_ms: i64,
) -> Result<MatchMeta, ArchiveError> {
    let size = body.len();
    let (meta, rows, blob) = prepare(body).await?;
    tracing::debug!(
        match_id,
        raw_bytes = size,
        zstd_bytes = blob.len(),
        ratio = format!("{:.1}", size as f64 / blob.len().max(1) as f64),
        "match archived"
    );
    let (id, region, scope, row) = (
        match_id.to_string(),
        region.to_string(),
        key_scope.to_string(),
        meta.clone(),
    );
    db.write(move |conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (match_id) DO UPDATE SET
               region = excluded.region, patch = excluded.patch, queue_id = excluded.queue_id,
               game_end_ms = excluded.game_end_ms, body_zstd = excluded.body_zstd,
               body_size = excluded.body_size, archived_at = excluded.archived_at",
            rusqlite::params![id, region, row.patch, row.queue_id, row.game_end_ms, blob, size as i64, now_ms],
        )?;
        facts::write(&tx, &id, &scope, &rows)?;
        tx.commit()?;
        Ok::<_, ArchiveError>(())
    })
    .await?;
    metrics::counter!(crate::metrics::ARCHIVED_MATCHES_TOTAL).increment(1);
    Ok(meta)
}

/// The archived timeline body, if any.
pub async fn get_timeline(db: &Db, match_id: &str) -> Result<Option<Bytes>, ArchiveError> {
    let id = match_id.to_string();
    db.read(move |conn| {
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT body_zstd FROM timelines WHERE match_id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        blob.map(|b| decompress(&b)).transpose()
    })
    .await
}

/// Archive a timeline. Returns `false`, storing nothing, when its match is not
/// archived yet: the row would have no parent.
pub async fn put_timeline(db: &Db, match_id: &str, body: Bytes) -> Result<bool, ArchiveError> {
    let blob = compress(body).await?;
    let id = match_id.to_string();
    db.write(move |conn| {
        let n = conn.execute(
            "INSERT INTO timelines (match_id, body_zstd)
             SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM matches WHERE match_id = ?1)
             ON CONFLICT (match_id) DO UPDATE SET body_zstd = excluded.body_zstd",
            rusqlite::params![id, blob],
        )?;
        Ok::<_, ArchiveError>(n > 0)
    })
    .await
}

/// The ids not yet archived, in their input order (duplicates kept). What the
/// backfill and the ladder crawl use to skip matches they already have.
pub async fn filter_unarchived(db: &Db, match_ids: &[String]) -> Result<Vec<String>, ArchiveError> {
    if match_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = match_ids.to_vec();
    db.read(move |conn| {
        let mut known = std::collections::HashSet::new();
        for chunk in ids.chunks(FILTER_CHUNK) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT match_id FROM matches WHERE match_id IN ({marks})"
            ))?;
            let rows = stmt.query_map(params_from_iter(chunk), |r| r.get::<_, String>(0))?;
            for id in rows {
                known.insert(id?);
            }
        }
        Ok::<_, ArchiveError>(ids.into_iter().filter(|id| !known.contains(id)).collect())
    })
    .await
}

#[cfg(test)]
mod tests;

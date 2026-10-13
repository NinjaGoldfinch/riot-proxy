//! The match archive (docs/design/04 §Schema, §Cache tiers): completed matches
//! are immutable, so Riot is asked for each one once and the archive answers
//! after that. Bodies are stored as zstd blobs and come back byte-identical.
//!
//! `patch`, `queue_id` and `game_end_ms` are pulled out of the body on insert so
//! analytics and stats never open the blob. Timelines go in their own table,
//! with no foreign key to their match, so one fetched first is kept (TL-01;
//! v1, and v2 until then, dropped it).
//!
//! Archiving a match also writes its `match_facts` in the same transaction
//! (v1 `archiveMatch`), for the key scope whose PUUIDs the body carries, and
//! a ranked match's participants' tiers (`match_facts.tier`, THR-02, DSK-04).
//!
//! A body may live in a pack in object storage instead (design/12, OBJ-02):
//! its row then holds the empty blob and `body_locations` says where it is.
//! Reads fetch it by byte range and check its digest; writes always land in
//! SQLite, and a re-archived body is local again.

use std::collections::{BTreeMap, HashMap};

use bytes::Bytes;
use futures_util::StreamExt;
use rusqlite::params_from_iter;
use serde::Deserialize;
use ulid::Ulid;

use crate::archive::facts::{self, Fact};
use crate::archive::objects::Objects;
use crate::archive::pack::{self, Kind};
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
    /// `info.gameDuration`, seconds. Riot switched it from milliseconds at 11.20,
    /// the same patch that added `gameEndTimestamp`, which [`extract`] requires,
    /// so an archived match always has it in seconds.
    pub game_duration_s: Option<i64>,
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
    game_duration: Option<i64>,
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
        game_duration_s: b.info.game_duration,
    })
}

async fn compress(body: Bytes) -> Result<Vec<u8>, ArchiveError> {
    Ok(tokio::task::spawn_blocking(move || zstd::encode_all(body.as_ref(), ZSTD_LEVEL)).await??)
}

/// Everything derived from a body: its columns, facts and extras.
pub type Derived = (MatchMeta, Vec<Fact>, facts::Extras);

/// Derive what the archive stores beside a body. Facts are a derivation: a
/// body they cannot read is still archived.
pub fn derive(body: &[u8]) -> Result<Derived, ArchiveError> {
    let meta = extract(body)?;
    let rows = facts::extract(body).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "match facts not extracted");
        Vec::new()
    });
    let extras = facts::extras(body).unwrap_or_default();
    Ok((meta, rows, extras))
}

/// Parse and compress off the async runtime: a match body is ~100 KB.
async fn prepare(body: Bytes) -> Result<(Derived, Vec<u8>), ArchiveError> {
    tokio::task::spawn_blocking(move || {
        let derived = derive(&body)?;
        let blob = zstd::encode_all(body.as_ref(), ZSTD_LEVEL)?;
        Ok((derived, blob))
    })
    .await?
}

/// Write a match's derived rows: its `game_duration` and `remake` columns,
/// its facts for `key_scope`, and its bans. Shared by archiving and
/// `facts:reextract`.
pub fn write_derived(
    tx: &rusqlite::Connection,
    match_id: &str,
    key_scope: &str,
    (meta, rows, extras): &Derived,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE matches SET game_duration = ?2, remake = ?3, facts_version = ?4 WHERE match_id = ?1",
        rusqlite::params![
            match_id,
            meta.game_duration_s,
            extras.remake,
            facts::FACTS_VERSION
        ],
    )?;
    facts::write(tx, match_id, key_scope, rows)?;
    facts::write_bans(tx, match_id, &extras.bans)
}

fn decompress(blob: &[u8]) -> Result<Bytes, ArchiveError> {
    Ok(Bytes::from(zstd::decode_all(blob)?))
}

/// Where an offloaded body is (`body_locations`).
#[derive(Debug, Clone)]
struct Located {
    pack_id: String,
    offset: u64,
    len: u64,
    sha256: Vec<u8>,
}

/// A row as read: its body, or (when the local copy was freed) where to
/// fetch it, if anywhere.
enum Stored {
    Local(Bytes),
    Offloaded(Option<Located>),
}

/// The rows of `ids` in `kind`'s table, bodies decompressed. An empty
/// `body_zstd` is an offloaded row (design/12 §SQLite): never a body.
fn read_rows(
    conn: &rusqlite::Connection,
    kind: Kind,
    ids: &[String],
) -> Result<Vec<(String, Stored)>, ArchiveError> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(FILTER_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT t.match_id, t.body_zstd, l.pack_id, l.offset, l.len, l.sha256
               FROM {} t LEFT JOIN body_locations l ON l.match_id = t.match_id AND l.kind = '{}'
              WHERE t.match_id IN ({marks})",
            kind.table(),
            kind.as_str()
        ))?;
        let mut rows = stmt.query(params_from_iter(chunk))?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            let stored = if blob.is_empty() {
                let pack_id: Option<String> = r.get(2)?;
                Stored::Offloaded(match pack_id {
                    Some(pack_id) => Some(Located {
                        pack_id,
                        offset: r.get::<_, i64>(3)?.try_into().unwrap_or(0),
                        len: r.get::<_, i64>(4)?.try_into().unwrap_or(0),
                        sha256: r.get(5)?,
                    }),
                    None => None,
                })
            } else {
                Stored::Local(decompress(&blob)?)
            };
            out.push((id, stored));
        }
    }
    Ok(out)
}

/// Bodies of `ids` in `kind`: local ones from SQLite, offloaded ones by
/// byte range from their packs. A body that cannot be fetched or fails its
/// digest is left out with a warning, so the caller goes to Riot.
async fn read_bodies(db: &Db, kind: Kind, ids: &[String]) -> Result<HashMap<String, Bytes>, ArchiveError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let wanted = ids.to_vec();
    let rows = db.read(move |conn| read_rows(conn, kind, &wanted)).await?;
    let mut found = HashMap::with_capacity(rows.len());
    let mut away: BTreeMap<String, Vec<(String, Located)>> = BTreeMap::new();
    for (id, stored) in rows {
        match stored {
            Stored::Local(body) => {
                found.insert(id, body);
            }
            Stored::Offloaded(Some(at)) => away.entry(at.pack_id.clone()).or_default().push((id, at)),
            Stored::Offloaded(None) => {
                tracing::warn!(match_id = %id, %kind, "offloaded body has no location; reading from Riot");
            }
        }
    }
    if away.is_empty() {
        return Ok(found);
    }
    let Some(objects) = db.objects() else {
        tracing::warn!(
            %kind,
            bodies = away.values().map(Vec::len).sum::<usize>(),
            "offloaded bodies but no ARCHIVE_OBJECT_URL; reading from Riot"
        );
        return Ok(found);
    };
    let fetched: Vec<Vec<(String, Bytes)>> = futures_util::stream::iter(away)
        .map(|(pack_id, bodies)| fetch_from_pack(objects, kind, pack_id, bodies))
        .buffer_unordered(objects.concurrency())
        .collect()
        .await;
    found.extend(fetched.into_iter().flatten());
    Ok(found)
}

/// One pack's share of a read: one `get_ranges` call (close ranges merged by
/// `object_store`), each body checked against its digest, then decompressed
/// off the runtime. Failures are logged and dropped.
async fn fetch_from_pack(
    objects: &Objects,
    kind: Kind,
    pack_id: String,
    bodies: Vec<(String, Located)>,
) -> Vec<(String, Bytes)> {
    let Ok(id) = Ulid::from_string(&pack_id) else {
        tracing::warn!(%pack_id, %kind, bodies = bodies.len(), "pack id is not a ULID; reading from Riot");
        return Vec::new();
    };
    let key = pack::key(kind, id);
    let ranges: Vec<_> = bodies
        .iter()
        .map(|(_, at)| at.offset..at.offset + at.len)
        .collect();
    let parts = match objects.get_ranges(&key, &ranges).await {
        Ok(parts) => parts,
        Err(e) => {
            tracing::warn!(error = %e, %key, bodies = bodies.len(), "pack read failed; reading from Riot");
            return Vec::new();
        }
    };
    let checked: Vec<(String, Bytes)> = bodies
        .into_iter()
        .zip(parts)
        .filter_map(|((match_id, at), part)| {
            if pack::sha256(&part).as_slice() == at.sha256.as_slice() {
                Some((match_id, part))
            } else {
                tracing::warn!(%match_id, %key, "offloaded body failed its sha256; reading from Riot");
                None
            }
        })
        .collect();
    let decoded = tokio::task::spawn_blocking(move || {
        checked
            .into_iter()
            .filter_map(|(match_id, part)| match decompress(&part) {
                Ok(body) => Some((match_id, body)),
                Err(e) => {
                    tracing::warn!(%match_id, error = %e, "offloaded body did not decompress; reading from Riot");
                    None
                }
            })
            .collect()
    })
    .await;
    decoded.unwrap_or_else(|e| {
        tracing::warn!(error = %e, %key, "offloaded body decompression task failed");
        Vec::new()
    })
}

/// The archived match body, byte-identical to what Riot sent, wherever it
/// is kept.
pub async fn get(db: &Db, match_id: &str) -> Result<Option<Bytes>, ArchiveError> {
    let mut found = read_bodies(db, Kind::Match, &[match_id.to_string()]).await?;
    Ok(found.remove(match_id))
}

/// The archived bodies among `match_ids`, by id; ids not archived are absent.
/// One query for a whole page (v1 `getArchivedMatches`, #54) rather than one
/// archive read per match; offloaded ones cost a ranged GET per pack.
pub async fn get_many(db: &Db, match_ids: &[String]) -> Result<HashMap<String, Bytes>, ArchiveError> {
    read_bodies(db, Kind::Match, match_ids).await
}

/// The archived timelines among `match_ids`, by id, as [`get_many`] reads
/// match bodies (`builds:extract`, BLD-01).
pub async fn get_timelines(db: &Db, match_ids: &[String]) -> Result<HashMap<String, Bytes>, ArchiveError> {
    read_bodies(db, Kind::Timeline, match_ids).await
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
    let (derived, blob) = prepare(body).await?;
    let meta = derived.0.clone();
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
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at, game_duration)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (match_id) DO UPDATE SET
               region = excluded.region, patch = excluded.patch, queue_id = excluded.queue_id,
               game_end_ms = excluded.game_end_ms, body_zstd = excluded.body_zstd,
               body_size = excluded.body_size, archived_at = excluded.archived_at,
               game_duration = excluded.game_duration, offloaded_at = NULL",
            rusqlite::params![
                id,
                region,
                row.patch,
                row.queue_id,
                row.game_end_ms,
                blob,
                size as i64,
                now_ms,
                row.game_duration_s
            ],
        )?;
        write_derived(&tx, &id, &scope, &derived)?;
        // Each participant's tier as it is now, kept from then on (THR-02).
        crate::archive::tiers::stamp(&tx, &id, row.queue_id)?;
        tx.commit()?;
        Ok::<_, ArchiveError>(())
    })
    .await?;
    metrics::counter!(crate::metrics::ARCHIVED_MATCHES_TOTAL).increment(1);
    Ok(meta)
}

/// The archived timeline body, if any, wherever it is kept.
pub async fn get_timeline(db: &Db, match_id: &str) -> Result<Option<Bytes>, ArchiveError> {
    let mut found = read_bodies(db, Kind::Timeline, &[match_id.to_string()]).await?;
    Ok(found.remove(match_id))
}

/// Archive a timeline, whether or not its match is archived yet (TL-01): a
/// client that asks for a match and its timeline at once often gets the
/// timeline first. [`crate::archive::SqliteArchive`] queues the missing match.
pub async fn put_timeline(db: &Db, match_id: &str, body: Bytes) -> Result<(), ArchiveError> {
    let blob = compress(body).await?;
    let id = match_id.to_string();
    db.write(move |conn| {
        conn.execute(
            "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)
             ON CONFLICT (match_id) DO UPDATE SET body_zstd = excluded.body_zstd, offloaded_at = NULL",
            rusqlite::params![id, blob],
        )?;
        Ok::<_, ArchiveError>(())
    })
    .await
}

/// How many bodies of one kind are in each state (design/12 §SQLite).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyStates {
    /// Only in SQLite.
    pub local: i64,
    /// In a verified pack and still in SQLite.
    pub packed: i64,
    /// Only in the bucket.
    pub offloaded: i64,
}

/// Archive totals for `/v1/admin/stats`. The archive is not key-scoped: match
/// ids are not encrypted, so it survives a key rotation (v1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub matches: i64,
    pub timelines: i64,
    /// zstd bytes of match bodies in SQLite (freed ones count 0), and what
    /// every match body decompresses to.
    pub stored_bytes: i64,
    pub raw_bytes: i64,
    pub match_bodies: BodyStates,
    pub timeline_bodies: BodyStates,
    /// Pack objects, their size, and the bytes in them still referenced.
    pub packs: i64,
    pub pack_bytes: i64,
    pub pack_live_bytes: i64,
}

/// The state counts of `kind` with `total` rows. Each count reads one partial
/// index (V0023): the state columns sit after the body in the row, so reading
/// them row by row would read every body.
fn body_states(c: &rusqlite::Connection, kind: Kind, total: i64) -> Result<BodyStates, ArchiveError> {
    let table = kind.table();
    let local: i64 = c.query_row(
        &format!("SELECT count(*) FROM {table} WHERE packed_at IS NULL"),
        [],
        |r| r.get(0),
    )?;
    let packed: i64 = c.query_row(
        &format!("SELECT count(*) FROM {table} WHERE packed_at IS NOT NULL AND offloaded_at IS NULL"),
        [],
        |r| r.get(0),
    )?;
    Ok(BodyStates {
        local,
        packed,
        offloaded: (total - local - packed).max(0),
    })
}

pub async fn stats(db: &Db) -> Result<Stats, ArchiveError> {
    db.read(|c| {
        let (matches, stored_bytes, raw_bytes) = c.query_row(
            "SELECT count(*), coalesce(sum(length(body_zstd)), 0), coalesce(sum(body_size), 0) FROM matches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let timelines = c.query_row("SELECT count(*) FROM timelines", [], |r| r.get(0))?;
        let (packs, pack_bytes, pack_live_bytes) = c.query_row(
            "SELECT count(*), coalesce(sum(bytes), 0), coalesce(sum(live_bytes), 0) FROM body_packs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok::<_, ArchiveError>(Stats {
            matches,
            timelines,
            stored_bytes,
            raw_bytes,
            match_bodies: body_states(c, Kind::Match, matches)?,
            timeline_bodies: body_states(c, Kind::Timeline, timelines)?,
            packs,
            pack_bytes,
            pack_live_bytes,
        })
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

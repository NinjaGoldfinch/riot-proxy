//! The two steps that move bodies out of SQLite (design/12 §Offload job):
//! write a pack of local bodies to the bucket, and free the local copies of
//! packed ones. `archive:offload` (OBJ-03) decides which bodies and when;
//! these do the work and keep the database consistent.
//!
//! Uploading and freeing are separate. A pack is recorded only after its
//! object is verified, in one transaction; a crash before that leaves the rows
//! unpacked and an orphan object (OBJ-06 sweeps it). Freeing is a database
//! update with no store call, so it cannot fail halfway.

use bytes::Bytes;
use rusqlite::params_from_iter;
use ulid::Ulid;

use crate::archive::matches::ArchiveError;
use crate::archive::objects::{Objects, ObjectsError};
use crate::archive::pack::{self, Kind, Pack, PackError};
use crate::db::Db;

/// Ids per `IN (…)` query, well inside SQLite's limit.
const CHUNK: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum OffloadError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Objects(#[from] ObjectsError),
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error("pack {key} failed verification: {why}")]
    Verify { key: String, why: String },
    #[error("a packing task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// What [`write_pack`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub key: String,
    pub bodies: usize,
    pub bytes: u64,
}

/// Pack the local bodies of `ids` in `kind`, upload the pack, check the stored
/// object, and record it: `body_packs`, a location per body and `packed_at`,
/// in one transaction. Ids with no local body are skipped; `None` when none
/// had one. The local copies stay.
pub async fn write_pack(
    db: &Db,
    objects: &Objects,
    kind: Kind,
    ids: &[String],
    now_ms: i64,
) -> Result<Option<Written>, OffloadError> {
    let wanted = ids.to_vec();
    let bodies = db
        .read(move |c| local_bodies(c, kind, &wanted))
        .await
        .map_err(ArchiveError::from)?;
    if bodies.is_empty() {
        return Ok(None);
    }
    let id = Ulid::from_parts(u64::try_from(now_ms).unwrap_or(0), rand_u128());
    let built: Pack = tokio::task::spawn_blocking(move || pack::build(id, kind, bodies)).await??;
    let key = built.key();
    let metadata = built.metadata();
    let meta: Vec<(&str, &str)> = metadata.iter().map(|(k, v)| (*k, v.as_str())).collect();
    objects.put(&key, built.bytes.clone(), &meta).await?;
    verify(objects, &built).await?;

    let size = built.bytes.len() as u64;
    let count = built.entries.len();
    let (entries, digest) = (built.entries, built.sha256);
    db.write(move |c| {
        let tx = c.transaction()?;
        pack::record_on(&tx, id, kind, size, &digest, &entries, now_ms)?;
        tx.commit()?;
        Ok::<_, ArchiveError>(())
    })
    .await?;
    Ok(Some(Written {
        key,
        bodies: count,
        bytes: size,
    }))
}

/// The stored object must be the pack: the same size, and the same sha256 in
/// its metadata. A store that keeps no metadata (a local directory) is read
/// back and hashed instead.
async fn verify(objects: &Objects, built: &Pack) -> Result<(), OffloadError> {
    let key = built.key();
    let fail = |why: String| OffloadError::Verify {
        key: key.clone(),
        why,
    };
    let size = built.bytes.len() as u64;
    let Some(head) = objects.head(&key).await? else {
        return Err(fail("not found after the upload".into()));
    };
    if head.size != size {
        return Err(fail(format!("{} bytes stored, {size} written", head.size)));
    }
    let want = hex::encode(built.sha256);
    match head.metadata.get("sha256") {
        Some(got) if *got == want => Ok(()),
        Some(got) => Err(fail(format!("sha256 metadata {got}, expected {want}"))),
        None => {
            let stored = objects.get_range(&key, 0..size).await?;
            if hex::encode(pack::sha256(&stored)) == want {
                Ok(())
            } else {
                Err(fail("stored bytes differ from the pack".into()))
            }
        }
    }
}

/// `(match_id, body_zstd)` of the ids whose body is still in SQLite.
fn local_bodies(
    c: &rusqlite::Connection,
    kind: Kind,
    ids: &[String],
) -> Result<Vec<(String, Bytes)>, crate::db::DbError> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt = c.prepare(&format!(
            "SELECT match_id, body_zstd FROM {} WHERE match_id IN ({marks}) AND length(body_zstd) > 0",
            kind.table()
        ))?;
        let rows = stmt.query_map(params_from_iter(chunk), |r| {
            Ok((r.get::<_, String>(0)?, Bytes::from(r.get::<_, Vec<u8>>(1)?)))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// Free the local copies of the packed bodies among `ids`: `body_zstd`
/// becomes `X''` and `offloaded_at` is set. Ids that are not packed, or have
/// no location, are left alone, so a body is never freed before it is safely
/// in the bucket. Returns how many were freed.
pub async fn free(db: &Db, kind: Kind, ids: &[String], now_ms: i64) -> Result<usize, ArchiveError> {
    let ids = ids.to_vec();
    db.write(move |c| {
        let tx = c.transaction()?;
        let mut freed = 0;
        for chunk in ids.chunks(CHUNK) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "UPDATE {table} SET body_zstd = X'', offloaded_at = ?1
                  WHERE match_id IN ({marks}) AND packed_at IS NOT NULL AND offloaded_at IS NULL
                    AND EXISTS (SELECT 1 FROM body_locations l
                                 WHERE l.match_id = {table}.match_id AND l.kind = '{kind}')",
                table = kind.table(),
                kind = kind.as_str(),
            );
            let mut params: Vec<rusqlite::types::Value> = vec![now_ms.into()];
            params.extend(chunk.iter().map(|id| rusqlite::types::Value::from(id.clone())));
            freed += tx.execute(&sql, params_from_iter(params))?;
        }
        tx.commit()?;
        Ok::<_, ArchiveError>(freed)
    })
    .await
}

/// Bodies freed per transaction.
pub const FREE_BATCH: usize = 2_000;

/// The ids of the next pack of `kind`, or `None` when none is due.
///
/// Unpacked bodies are taken oldest first (matches by `archived_at`,
/// timelines in match-id order, each the order of its "to pack" index), up
/// to `target_bytes`; a single larger body is a pack of its own. That is a
/// full pack. Fewer bytes than that are packed only when one of them was
/// archived before `cutoff_ms`, so freeing it is not held up; otherwise they
/// wait for more. A timeline's archive time is its match's; one with no
/// match yet counts as new.
pub async fn next_pack(
    db: &Db,
    kind: Kind,
    target_bytes: u64,
    cutoff_ms: i64,
) -> Result<Option<Vec<String>>, ArchiveError> {
    db.read(move |c| {
        let sql = match kind {
            Kind::Match => {
                "SELECT match_id, length(body_zstd), archived_at FROM matches
                  WHERE packed_at IS NULL ORDER BY archived_at"
            }
            Kind::Timeline => {
                "SELECT t.match_id, length(t.body_zstd), m.archived_at
                   FROM timelines t LEFT JOIN matches m ON m.match_id = t.match_id
                  WHERE t.packed_at IS NULL ORDER BY t.match_id"
            }
        };
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query([])?;
        let (mut ids, mut bytes, mut old) = (Vec::new(), 0u64, false);
        while let Some(r) = rows.next()? {
            let len = u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0);
            if len == 0 {
                continue;
            }
            if !ids.is_empty() && bytes + len > target_bytes {
                return Ok(Some(ids));
            }
            ids.push(r.get::<_, String>(0)?);
            bytes += len;
            old |= r.get::<_, Option<i64>>(2)?.is_some_and(|at| at < cutoff_ms);
            if bytes >= target_bytes {
                return Ok(Some(ids));
            }
        }
        Ok::<_, ArchiveError>((old && !ids.is_empty()).then_some(ids))
    })
    .await
}

/// Up to `limit` packed, located bodies of `kind` still held locally whose
/// match was archived before `cutoff_ms`: the next batch to [`free`].
pub async fn next_free(
    db: &Db,
    kind: Kind,
    cutoff_ms: i64,
    limit: usize,
) -> Result<Vec<String>, ArchiveError> {
    db.read(move |c| {
        let sql = match kind {
            Kind::Match => {
                "SELECT m.match_id FROM matches m
                  WHERE m.packed_at IS NOT NULL AND m.offloaded_at IS NULL AND m.archived_at < ?1
                    AND EXISTS (SELECT 1 FROM body_locations l WHERE l.match_id = m.match_id AND l.kind = 'match')
                  LIMIT ?2"
            }
            Kind::Timeline => {
                "SELECT t.match_id FROM timelines t JOIN matches m ON m.match_id = t.match_id
                  WHERE t.packed_at IS NOT NULL AND t.offloaded_at IS NULL AND m.archived_at < ?1
                    AND EXISTS (SELECT 1 FROM body_locations l WHERE l.match_id = t.match_id AND l.kind = 'timeline')
                  LIMIT ?2"
            }
        };
        let mut stmt = c.prepare(sql)?;
        let ids = stmt
            .query_map(rusqlite::params![cutoff_ms, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, ArchiveError>(ids)
    })
    .await
}

/// What is left to do, for the `archive_offload_pending` gauges: unpacked
/// bodies of each kind, and packed ones past `cutoff_ms` not yet freed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backlog {
    pub to_pack: i64,
    pub to_free: i64,
    /// zstd bytes of the unpacked bodies: the trigger's counter.
    pub unpacked_bytes: u64,
}

pub async fn backlog(db: &Db, kind: Kind, cutoff_ms: i64) -> Result<Backlog, ArchiveError> {
    db.read(move |c| {
        let table = kind.table();
        let (to_pack, unpacked): (i64, i64) = c.query_row(
            &format!(
                "SELECT count(*), coalesce(sum(length(body_zstd)), 0) FROM {table} WHERE packed_at IS NULL"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let to_free: i64 = c.query_row(
            match kind {
                Kind::Match => {
                    "SELECT count(*) FROM matches
                      WHERE packed_at IS NOT NULL AND offloaded_at IS NULL AND archived_at < ?1"
                }
                Kind::Timeline => {
                    "SELECT count(*) FROM timelines t JOIN matches m ON m.match_id = t.match_id
                      WHERE t.packed_at IS NOT NULL AND t.offloaded_at IS NULL AND m.archived_at < ?1"
                }
            },
            [cutoff_ms],
            |r| r.get(0),
        )?;
        Ok::<_, ArchiveError>(Backlog {
            to_pack,
            to_free,
            unpacked_bytes: u64::try_from(unpacked).unwrap_or(0),
        })
    })
    .await
}

fn rand_u128() -> u128 {
    let mut buf = [0u8; 16];
    // A ULID's random half only has to make two packs written in the same
    // millisecond differ; on the rare failure the clock half still orders it.
    let _ = getrandom::fill(&mut buf);
    u128::from_le_bytes(buf)
}

#[cfg(test)]
mod tests;

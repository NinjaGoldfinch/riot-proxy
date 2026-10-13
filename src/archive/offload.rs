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

fn rand_u128() -> u128 {
    let mut buf = [0u8; 16];
    // A ULID's random half only has to make two packs written in the same
    // millisecond differ; on the rare failure the clock half still orders it.
    let _ = getrandom::fill(&mut buf);
    u128::from_le_bytes(buf)
}

#[cfg(test)]
mod tests;

//! Recovering the map of where every packed body is (design/12 §Replica and
//! backups, OBJ-04). The map is `body_packs` and `body_locations` in SQLite;
//! two more copies of it live in the bucket:
//!
//! - **Daily snapshots** under `index/`: both tables as zstd-compressed JSON
//!   lines, one object each (`index/packs-<date>.jsonl.zst`,
//!   `index/locations-<date>.jsonl.zst`). A bucket lifecycle rule expires them
//!   after 14 days, so a corrupt one is never the only copy.
//! - **Each pack's own index**, which [`reindex`] reads: the trailer and the
//!   index, two ranged GETs a pack.
//!
//! [`restore_index`] loads the newest snapshot that reads cleanly (falling
//! back day by day), then reindexes only the packs written after it: ULIDs
//! sort by time, so that is a listing and a few packs. With no readable
//! snapshot it reindexes every pack.

use std::collections::{BTreeMap, HashMap};

use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::archive::matches::ArchiveError;
use crate::archive::objects::{ObjectInfo, Objects, ObjectsError};
use crate::archive::pack::{self, Digest32, Entry, Kind, PackError, TRAILER_LEN};
use crate::db::Db;

/// Where snapshots live in the bucket.
pub const INDEX_PREFIX: &str = "index/";
const PACKS_PREFIX: &str = "packs/";
/// As for the pack index (design/04).
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Objects(#[from] ObjectsError),
    #[error("{key}: {why}")]
    Bad { key: String, why: String },
}

impl From<crate::db::DbError> for RecoveryError {
    fn from(e: crate::db::DbError) -> Self {
        Self::Archive(e.into())
    }
}

impl From<rusqlite::Error> for RecoveryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Archive(e.into())
    }
}

fn bad(key: &str, why: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Bad {
        key: key.to_string(),
        why: why.to_string(),
    }
}

/// A `body_packs` row, and a line of `index/packs-<date>.jsonl.zst`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackRow {
    pub pack_id: String,
    pub kind: Kind,
    pub bytes: i64,
    pub bodies: i64,
    pub live_bytes: i64,
    /// Hex, of the whole object.
    pub sha256: String,
    pub created_at: i64,
}

/// A `body_locations` row, and a line of `index/locations-<date>.jsonl.zst`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationRow {
    pub match_id: String,
    pub kind: Kind,
    pub pack_id: String,
    pub offset: i64,
    pub len: i64,
    /// Hex, of the body as stored.
    pub sha256: String,
}

/// Both tables, in primary-key order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tables {
    pub packs: Vec<PackRow>,
    pub locations: Vec<LocationRow>,
}

fn kind_of(s: &str) -> rusqlite::Result<Kind> {
    match s {
        "match" => Ok(Kind::Match),
        "timeline" => Ok(Kind::Timeline),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            format!("kind '{other}'"),
            rusqlite::types::Type::Text,
        )),
    }
}

/// Visit every `body_packs` row, then every `body_locations` row, in
/// primary-key order.
fn each_row(
    c: &rusqlite::Connection,
    mut on_pack: impl FnMut(PackRow) -> Result<(), ArchiveError>,
    mut on_location: impl FnMut(LocationRow) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    let mut stmt = c.prepare(
        "SELECT pack_id, kind, bytes, bodies, live_bytes, sha256, created_at FROM body_packs ORDER BY pack_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        on_pack(PackRow {
            pack_id: r.get(0)?,
            kind: kind_of(&r.get::<_, String>(1)?)?,
            bytes: r.get(2)?,
            bodies: r.get(3)?,
            live_bytes: r.get(4)?,
            sha256: hex::encode(r.get::<_, Vec<u8>>(5)?),
            created_at: r.get(6)?,
        })?;
    }
    let mut stmt = c.prepare(
        "SELECT match_id, kind, pack_id, offset, len, sha256 FROM body_locations ORDER BY match_id, kind",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        on_location(LocationRow {
            match_id: r.get(0)?,
            kind: kind_of(&r.get::<_, String>(1)?)?,
            pack_id: r.get(2)?,
            offset: r.get(3)?,
            len: r.get(4)?,
            sha256: hex::encode(r.get::<_, Vec<u8>>(5)?),
        })?;
    }
    Ok(())
}

/// Both tables as they are now (the restore drill and the tests compare them).
pub async fn tables(db: &Db) -> Result<Tables, ArchiveError> {
    db.read(|c| {
        let mut t = Tables::default();
        each_row(
            c,
            |p| {
                t.packs.push(p);
                Ok(())
            },
            |l| {
                t.locations.push(l);
                Ok(())
            },
        )?;
        Ok(t)
    })
    .await
}

/// What [`write_snapshot`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// UTC, `yyyy-mm-dd`.
    pub date: String,
    pub packs: usize,
    pub locations: usize,
    /// The newest pack it covers; `None` with no packs yet.
    pub last_pack: Option<String>,
}

pub fn packs_key(date: &str) -> String {
    format!("{INDEX_PREFIX}packs-{date}.jsonl.zst")
}

pub fn locations_key(date: &str) -> String {
    format!("{INDEX_PREFIX}locations-{date}.jsonl.zst")
}

/// The UTC date of `ms`, `yyyy-mm-dd`.
fn date_of(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
        .unwrap_or_else(|_| "1970-01-01".into())
}

fn encoder() -> std::io::Result<zstd::Encoder<'static, Vec<u8>>> {
    let mut enc = zstd::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    // A frame checksum: a flipped byte fails the decode, not just the parse.
    enc.include_checksum(true)?;
    Ok(enc)
}

fn write_line<T: Serialize>(out: &mut impl std::io::Write, row: &T) -> Result<(), ArchiveError> {
    serde_json::to_writer(&mut *out, row).map_err(std::io::Error::other)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Today's snapshot (`archive:index-snapshot`, daily): both tables read in
/// one transaction, written as two objects, the locations first. A second
/// run on the same day replaces them. Metadata: `x-amz-meta-last-pack` and
/// `x-amz-meta-rows`.
pub async fn write_snapshot(db: &Db, objects: &Objects, now_ms: i64) -> Result<Snapshot, RecoveryError> {
    struct Encoded {
        packs: Vec<u8>,
        locations: Vec<u8>,
        pack_rows: usize,
        location_rows: usize,
        last_pack: Option<String>,
    }
    let e = db
        .read(|c| {
            let tx = c.unchecked_transaction()?;
            let (mut packs, mut locations) = (encoder()?, encoder()?);
            let (mut pack_rows, mut location_rows, mut last_pack) = (0, 0, None);
            each_row(
                &tx,
                |p| {
                    pack_rows += 1;
                    write_line(&mut packs, &p)?;
                    last_pack = Some(p.pack_id);
                    Ok(())
                },
                |l| {
                    location_rows += 1;
                    write_line(&mut locations, &l)
                },
            )?;
            drop(tx);
            Ok::<_, ArchiveError>(Encoded {
                packs: packs.finish()?,
                locations: locations.finish()?,
                pack_rows,
                location_rows,
                last_pack,
            })
        })
        .await?;
    let date = date_of(now_ms);
    let last = e.last_pack.clone().unwrap_or_default();
    let location_count = e.location_rows.to_string();
    let pack_count = e.pack_rows.to_string();
    objects
        .put(
            &locations_key(&date),
            e.locations.into(),
            &[("last-pack", &last), ("rows", &location_count)],
        )
        .await?;
    objects
        .put(
            &packs_key(&date),
            e.packs.into(),
            &[("last-pack", &last), ("rows", &pack_count)],
        )
        .await?;
    Ok(Snapshot {
        date,
        packs: e.pack_rows,
        locations: e.location_rows,
        last_pack: e.last_pack,
    })
}

/// Dates with both snapshot objects, newest first.
async fn snapshot_dates(objects: &Objects) -> Result<Vec<(String, ObjectInfo, ObjectInfo)>, ObjectsError> {
    let mut found: BTreeMap<String, (Option<ObjectInfo>, Option<ObjectInfo>)> = BTreeMap::new();
    for info in objects.list(INDEX_PREFIX).await? {
        let Some(name) = info
            .key
            .strip_prefix(INDEX_PREFIX)
            .and_then(|n| n.strip_suffix(".jsonl.zst"))
        else {
            continue;
        };
        let (which, date) = if let Some(d) = name.strip_prefix("packs-") {
            (0, d)
        } else if let Some(d) = name.strip_prefix("locations-") {
            (1, d)
        } else {
            continue;
        };
        if date.parse::<jiff::civil::Date>().is_err() {
            continue;
        }
        let slot = found.entry(date.to_string()).or_default();
        if which == 0 {
            slot.0 = Some(info);
        } else {
            slot.1 = Some(info);
        }
    }
    Ok(found
        .into_iter()
        .rev()
        .filter_map(|(date, pair)| match pair {
            (Some(p), Some(l)) => Some((date, p, l)),
            _ => None,
        })
        .collect())
}

/// Every line of a snapshot object, parsed.
async fn read_lines<T: for<'de> Deserialize<'de>>(
    objects: &Objects,
    info: &ObjectInfo,
) -> Result<Vec<T>, RecoveryError> {
    let raw = objects.get_range(&info.key, 0..info.size).await?;
    let text = zstd::decode_all(raw.as_ref()).map_err(|e| bad(&info.key, format!("zstd: {e}")))?;
    text.split(|b| *b == b'\n')
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(n, line)| {
            serde_json::from_slice(line).map_err(|e| bad(&info.key, format!("line {}: {e}", n + 1)))
        })
        .collect()
}

fn digest(key: &str, hex_digest: &str) -> Result<Digest32, RecoveryError> {
    let mut d = [0u8; 32];
    hex::decode_to_slice(hex_digest, &mut d).map_err(|e| bad(key, format!("sha256 '{hex_digest}': {e}")))?;
    Ok(d)
}

/// A snapshot's two objects, read and checked: every pack id a ULID and
/// unique, every digest 32 bytes, every location in a listed pack of its
/// kind, and each pack's live bytes the sum of its locations.
async fn load_snapshot(
    objects: &Objects,
    packs: &ObjectInfo,
    locations: &ObjectInfo,
) -> Result<Tables, RecoveryError> {
    let packs_rows: Vec<PackRow> = read_lines(objects, packs).await?;
    let location_rows: Vec<LocationRow> = read_lines(objects, locations).await?;
    let mut kinds: HashMap<&str, Kind> = HashMap::with_capacity(packs_rows.len());
    for p in &packs_rows {
        Ulid::from_string(&p.pack_id)
            .map_err(|e| bad(&packs.key, format!("pack id '{}': {e}", p.pack_id)))?;
        digest(&packs.key, &p.sha256)?;
        if kinds.insert(&p.pack_id, p.kind).is_some() {
            return Err(bad(&packs.key, format!("pack {} is listed twice", p.pack_id)));
        }
    }
    let mut located: HashMap<&str, i64> = HashMap::with_capacity(packs_rows.len());
    for l in &location_rows {
        digest(&locations.key, &l.sha256)?;
        if kinds.get(l.pack_id.as_str()) != Some(&l.kind) {
            return Err(bad(
                &locations.key,
                format!(
                    "{} {} is in pack {}, which the packs snapshot does not list as a {0}",
                    l.kind, l.match_id, l.pack_id
                ),
            ));
        }
        if l.len <= 0 || l.offset < 0 {
            return Err(bad(
                &locations.key,
                format!("{} {} has no body range", l.kind, l.match_id),
            ));
        }
        *located.entry(&l.pack_id).or_default() += l.len;
    }
    // A pack's live bytes are the bodies located in it; a locations object
    // cut short (even to a valid empty frame) shows here.
    for p in &packs_rows {
        let got = located.get(p.pack_id.as_str()).copied().unwrap_or(0);
        if got != p.live_bytes {
            return Err(bad(
                &locations.key,
                format!(
                    "pack {} has {got} bytes located, {} live",
                    p.pack_id, p.live_bytes
                ),
            ));
        }
    }
    Ok(Tables {
        packs: packs_rows,
        locations: location_rows,
    })
}

/// Replace both tables with `t`, in one transaction, and mark every located
/// body packed (as of its pack's creation) if its row says it is not.
async fn replace_tables(db: &Db, t: Tables) -> Result<(), RecoveryError> {
    db.write(move |c| {
        let tx = c.transaction()?;
        tx.execute("DELETE FROM body_locations", [])?;
        tx.execute("DELETE FROM body_packs", [])?;
        {
            let mut pack = tx.prepare(
                "INSERT INTO body_packs (pack_id, kind, bytes, bodies, live_bytes, sha256, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for p in &t.packs {
                pack.execute(rusqlite::params![
                    p.pack_id,
                    p.kind.as_str(),
                    p.bytes,
                    p.bodies,
                    p.live_bytes,
                    digest("", &p.sha256).map(|d| d.to_vec()).unwrap_or_default(),
                    p.created_at
                ])?;
            }
            let mut location = tx.prepare(
                "INSERT INTO body_locations (match_id, kind, pack_id, offset, len, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for l in &t.locations {
                location.execute(rusqlite::params![
                    l.match_id,
                    l.kind.as_str(),
                    l.pack_id,
                    l.offset,
                    l.len,
                    digest("", &l.sha256).map(|d| d.to_vec()).unwrap_or_default()
                ])?;
            }
            for kind in [Kind::Match, Kind::Timeline] {
                tx.execute(
                    &format!(
                        "UPDATE {table} SET packed_at = (
                             SELECT p.created_at FROM body_locations l JOIN body_packs p ON p.pack_id = l.pack_id
                              WHERE l.match_id = {table}.match_id AND l.kind = ?1)
                          WHERE packed_at IS NULL
                            AND match_id IN (SELECT match_id FROM body_locations WHERE kind = ?1)",
                        table = kind.table()
                    ),
                    [kind.as_str()],
                )?;
            }
        }
        tx.commit()?;
        Ok::<_, RecoveryError>(())
    })
    .await
}

/// What [`reindex`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reindexed {
    /// Packs whose index was read and recorded.
    pub packs: usize,
    /// Bodies located.
    pub bodies: usize,
    /// Packs that could not be read, with why; the rest were still recorded.
    pub failed: Vec<(String, String)>,
}

/// A pack found by listing the bucket.
struct Listed {
    kind: Kind,
    id: Ulid,
    key: String,
    size: u64,
}

/// The digest and entries of one pack: its HEAD for the whole object's
/// digest, then the trailer and the index (two ranged GETs). A store that
/// keeps no metadata (a local directory) is read whole and hashed instead.
async fn read_pack(objects: &Objects, p: &Listed) -> Result<(Digest32, Vec<Entry>), RecoveryError> {
    let meta = objects
        .head(&p.key)
        .await?
        .and_then(|h| h.metadata.get("sha256").cloned());
    if let Some(hex_digest) = meta {
        if p.size < TRAILER_LEN {
            return Err(bad(
                &p.key,
                PackError::NotAPack("shorter than its trailer".into()),
            ));
        }
        let trailer = objects.get_range(&p.key, p.size - TRAILER_LEN..p.size).await?;
        let range = pack::index_range(p.size, &trailer).map_err(|e| bad(&p.key, e))?;
        let index = objects.get_range(&p.key, range.clone()).await?;
        let entries = pack::parse_index(p.kind, &index, range.start).map_err(|e| bad(&p.key, e))?;
        Ok((digest(&p.key, &hex_digest)?, entries))
    } else {
        let whole = objects.get_range(&p.key, 0..p.size).await?;
        let entries = pack::read(p.kind, &whole).map_err(|e| bad(&p.key, e))?;
        Ok((pack::sha256(&whole), entries))
    }
}

/// Rebuild the map from the packs' own indexes: every pack in the bucket, or
/// only those with an id after `since`, oldest first. Each pack is recorded
/// in its own transaction: its `body_packs` row if missing (created at its
/// ULID's time), and a location per body, which moves a body located in an
/// earlier-recorded pack to this one; live bytes follow the moves. A pack that cannot be read is
/// reported in [`Reindexed::failed`] and the rest go on. Run it again to
/// finish; it is idempotent.
pub async fn reindex(db: &Db, objects: &Objects, since: Option<Ulid>) -> Result<Reindexed, RecoveryError> {
    let mut listed: Vec<Listed> = objects
        .list(PACKS_PREFIX)
        .await?
        .into_iter()
        .filter_map(|info| {
            let parsed = pack::parse_key(&info.key);
            if parsed.is_none() {
                tracing::warn!(key = %info.key, "not a pack key; skipped");
            }
            let (kind, id) = parsed?;
            Some(Listed {
                kind,
                id,
                key: info.key,
                size: info.size,
            })
        })
        .filter(|p| since.is_none_or(|s| p.id > s))
        .collect();
    listed.sort_by_key(|p| p.id);

    let mut done = Reindexed::default();
    let mut reads = stream::iter(listed)
        .map(|p| async move {
            let read = read_pack(objects, &p).await;
            (p, read)
        })
        .buffered(objects.concurrency());
    while let Some((p, read)) = reads.next().await {
        let (sha, entries) = match read {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(key = %p.key, error = %e, "pack not reindexed");
                done.failed.push((p.key, e.to_string()));
                continue;
            }
        };
        let bodies = entries.len();
        let (id, kind, size) = (p.id, p.kind, p.size);
        db.write(move |c| {
            let tx = c.transaction()?;
            let created = i64::try_from(id.timestamp_ms()).unwrap_or(i64::MAX);
            let live: u64 = entries.iter().map(|e| e.len).sum();
            tx.execute(
                "INSERT INTO body_packs (pack_id, kind, bytes, bodies, live_bytes, sha256, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (pack_id) DO NOTHING",
                rusqlite::params![
                    id.to_string(),
                    kind.as_str(),
                    size as i64,
                    entries.len() as i64,
                    live as i64,
                    sha.as_slice(),
                    created
                ],
            )?;
            pack::locate_on(&tx, id, kind, &entries, created)?;
            // `locate_on` lowers the packs bodies moved out of; a row that
            // already existed gets back what moved into it.
            tx.execute(
                "UPDATE body_packs SET live_bytes =
                   (SELECT coalesce(sum(len), 0) FROM body_locations WHERE pack_id = ?1)
                  WHERE pack_id = ?1",
                [id.to_string()],
            )?;
            tx.commit()?;
            Ok::<_, RecoveryError>(())
        })
        .await?;
        done.packs += 1;
        done.bodies += bodies;
    }
    Ok(done)
}

/// What [`restore_index`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Restored {
    /// The date of the snapshot loaded; `None` when none read cleanly and
    /// every pack was reindexed.
    pub snapshot: Option<String>,
    pub packs: usize,
    pub locations: usize,
    /// Newer snapshots passed over, with why.
    pub skipped: Vec<(String, String)>,
    /// The packs written after the snapshot.
    pub reindexed: Reindexed,
}

/// Replace the map with the newest snapshot that reads cleanly, then
/// [`reindex`] the packs written after the newest pack it lists. A corrupt
/// or incomplete snapshot is passed over for the day before. With none, every
/// pack is reindexed. Run it with the service stopped: an offload recording a
/// pack meanwhile could be lost from the map until the next reindex.
pub async fn restore_index(db: &Db, objects: &Objects) -> Result<Restored, RecoveryError> {
    let mut out = Restored::default();
    for (date, packs, locations) in snapshot_dates(objects).await? {
        match load_snapshot(objects, &packs, &locations).await {
            Ok(t) => {
                let since = match t.packs.iter().map(|p| p.pack_id.as_str()).max() {
                    Some(id) => Some(Ulid::from_string(id).map_err(|e| bad(&packs.key, e))?),
                    None => None,
                };
                out.packs = t.packs.len();
                out.locations = t.locations.len();
                replace_tables(db, t).await?;
                out.reindexed = reindex(db, objects, since).await?;
                out.snapshot = Some(date);
                return Ok(out);
            }
            Err(e) => {
                tracing::warn!(%date, error = %e, "index snapshot unreadable; trying the one before");
                out.skipped.push((date, e.to_string()));
            }
        }
    }
    tracing::warn!("no readable index snapshot; reindexing every pack");
    out.reindexed = reindex(db, objects, None).await?;
    Ok(out)
}

#[cfg(test)]
mod tests;

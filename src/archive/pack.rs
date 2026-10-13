//! Packs: many bodies of one kind in one object (design/12 §Pack format,
//! ADR-146).
//!
//! ```text
//! [body 0][body 1] … [body n-1][index][index length: u64 LE]["RPXPACK1"]
//! ```
//!
//! Each body is the zstd frame SQLite holds, byte for byte, in match-id order.
//! The index is zstd-compressed JSON lines, one per body
//! (`{"kind","matchId","offset","len","sha256"}`), so a pack describes itself:
//! its index alone rebuilds every location. Nothing here talks to a store.

use std::fmt;

use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// The last eight bytes of every pack.
pub const MAGIC: &[u8; 8] = b"RPXPACK1";
/// The index length (u64 LE) and [`MAGIC`].
pub const TRAILER_LEN: u64 = 16;
/// The index is small next to the bodies; level 3 as for bodies (design/04).
const INDEX_ZSTD_LEVEL: i32 = 3;

pub type Digest32 = [u8; 32];

/// Which archive table a body comes from. A pack holds one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Match,
    Timeline,
}

impl Kind {
    /// As stored in `body_locations.kind` and `body_packs.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Timeline => "timeline",
        }
    }

    /// The SQLite table its bodies live in.
    pub fn table(self) -> &'static str {
        match self {
            Self::Match => "matches",
            Self::Timeline => "timelines",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("a pack needs at least one body")]
    Empty,
    #[error("match {0} is in the pack twice")]
    Duplicate(String),
    #[error("match {0} has an empty body")]
    EmptyBody(String),
    #[error("not a pack: {0}")]
    NotAPack(String),
    #[error("pack index: {0}")]
    Index(String),
    #[error("pack index zstd: {0}")]
    Zstd(#[from] std::io::Error),
}

/// One body's place in a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub match_id: String,
    pub offset: u64,
    pub len: u64,
    /// Of the body's bytes as stored (the zstd frame).
    pub sha256: Digest32,
}

/// An index line as written.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Line {
    kind: Kind,
    match_id: String,
    offset: u64,
    len: u64,
    sha256: String,
}

/// A pack built in memory, ready to upload.
#[derive(Debug, Clone)]
pub struct Pack {
    pub id: Ulid,
    pub kind: Kind,
    pub bytes: Bytes,
    pub entries: Vec<Entry>,
    /// Of the whole object.
    pub sha256: Digest32,
}

impl Pack {
    pub fn key(&self) -> String {
        key(self.kind, self.id)
    }

    /// Object metadata: `x-amz-meta-sha256` and `x-amz-meta-bodies`.
    pub fn metadata(&self) -> [(&'static str, String); 2] {
        [
            ("sha256", hex::encode(self.sha256)),
            ("bodies", self.entries.len().to_string()),
        ]
    }
}

/// `packs/<kind>s/<yyyy>/<mm>/<ulid>.pack`, year and month (UTC) from the ULID.
pub fn key(kind: Kind, id: Ulid) -> String {
    let ms = i64::try_from(id.timestamp_ms()).unwrap_or(i64::MAX);
    let (year, month) = jiff::Timestamp::from_millisecond(ms)
        .map(|t| {
            let d = t.to_zoned(jiff::tz::TimeZone::UTC).date();
            (d.year(), d.month())
        })
        .unwrap_or((1970, 1));
    format!("packs/{}/{year:04}/{month:02}/{id}.pack", kind.table())
}

pub fn sha256(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

/// Bodies sorted by match id, back to back, then the index and trailer.
/// Refuses no bodies, a repeated id and an empty body (an offloaded row's
/// `X''` must never be packed as if it were one).
pub fn build(id: Ulid, kind: Kind, mut bodies: Vec<(String, Bytes)>) -> Result<Pack, PackError> {
    if bodies.is_empty() {
        return Err(PackError::Empty);
    }
    bodies.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(w) = bodies.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(PackError::Duplicate(w[0].0.clone()));
    }
    let total: usize = bodies.iter().map(|(_, b)| b.len()).sum();
    let mut out = BytesMut::with_capacity(total + 64 * bodies.len() + 64);
    let mut entries = Vec::with_capacity(bodies.len());
    let mut lines = Vec::with_capacity(bodies.len() * 160);
    for (match_id, body) in bodies {
        if body.is_empty() {
            return Err(PackError::EmptyBody(match_id));
        }
        let entry = Entry {
            kind,
            offset: out.len() as u64,
            len: body.len() as u64,
            sha256: sha256(&body),
            match_id,
        };
        serde_json::to_writer(
            &mut lines,
            &Line {
                kind,
                match_id: entry.match_id.clone(),
                offset: entry.offset,
                len: entry.len,
                sha256: hex::encode(entry.sha256),
            },
        )
        .map_err(|e| PackError::Index(e.to_string()))?;
        lines.push(b'\n');
        out.put_slice(&body);
        entries.push(entry);
    }
    let index = zstd::encode_all(lines.as_slice(), INDEX_ZSTD_LEVEL)?;
    out.put_slice(&index);
    out.put_u64_le(index.len() as u64);
    out.put_slice(MAGIC);
    let bytes = out.freeze();
    Ok(Pack {
        id,
        kind,
        sha256: sha256(&bytes),
        bytes,
        entries,
    })
}

/// The index's byte range in a pack of `size` bytes, from its last
/// [`TRAILER_LEN`] bytes.
pub fn index_range(size: u64, trailer: &[u8]) -> Result<std::ops::Range<u64>, PackError> {
    let not = |why: &str| PackError::NotAPack(why.to_string());
    if size < TRAILER_LEN || trailer.len() as u64 != TRAILER_LEN {
        return Err(not("shorter than its trailer"));
    }
    if &trailer[8..] != MAGIC {
        return Err(not("no RPXPACK1 trailer"));
    }
    let mut len = [0u8; 8];
    len.copy_from_slice(&trailer[..8]);
    let len = u64::from_le_bytes(len);
    let end = size - TRAILER_LEN;
    let start = end
        .checked_sub(len)
        .ok_or_else(|| not("index longer than the pack"))?;
    Ok(start..end)
}

/// Parse a compressed index whose bodies end at `bodies_end`. Every entry
/// must be of `kind`, inside the body region, in match-id order and not
/// overlapping the one before.
pub fn parse_index(kind: Kind, compressed: &[u8], bodies_end: u64) -> Result<Vec<Entry>, PackError> {
    let bad = |why: String| PackError::Index(why);
    let text = zstd::decode_all(compressed)?;
    let mut entries: Vec<Entry> = Vec::new();
    for (n, raw) in text.split(|b| *b == b'\n').enumerate() {
        if raw.is_empty() {
            continue;
        }
        let line: Line = serde_json::from_slice(raw).map_err(|e| bad(format!("line {}: {e}", n + 1)))?;
        let mut digest = [0u8; 32];
        hex::decode_to_slice(&line.sha256, &mut digest)
            .map_err(|e| bad(format!("line {}: sha256: {e}", n + 1)))?;
        let entry = Entry {
            kind: line.kind,
            match_id: line.match_id,
            offset: line.offset,
            len: line.len,
            sha256: digest,
        };
        if entry.kind != kind {
            return Err(bad(format!(
                "{} is a {}, not a {kind}",
                entry.match_id, entry.kind
            )));
        }
        if entry.len == 0
            || entry
                .offset
                .checked_add(entry.len)
                .is_none_or(|end| end > bodies_end)
        {
            return Err(bad(format!("{} lies outside the bodies", entry.match_id)));
        }
        if let Some(prev) = entries.last()
            && (prev.match_id >= entry.match_id || prev.offset + prev.len > entry.offset)
        {
            return Err(bad(format!("{} is out of order", entry.match_id)));
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(bad("no entries".into()));
    }
    Ok(entries)
}

/// Read a whole pack held in memory: trailer, then index. Bodies are not
/// checked; see [`verify`].
pub fn read(kind: Kind, pack: &[u8]) -> Result<Vec<Entry>, PackError> {
    let size = pack.len() as u64;
    let tail = pack
        .get(pack.len().saturating_sub(TRAILER_LEN as usize)..)
        .unwrap_or_default();
    let index = index_range(size, tail)?;
    let compressed = &pack[index.start as usize..index.end as usize];
    parse_index(kind, compressed, index.start)
}

/// `true` when every entry's bytes in `pack` match its digest.
pub fn verify(pack: &[u8], entries: &[Entry]) -> bool {
    entries.iter().all(|e| {
        pack.get(e.offset as usize..(e.offset + e.len) as usize)
            .is_some_and(|b| sha256(b) == e.sha256)
    })
}

/// Record a verified pack (OBJ-03 calls this after the upload and HEAD; a
/// reindex from a pack's own index uses it too): its `body_packs` row, a
/// location per body, and `packed_at` on each body's row. A body already
/// located elsewhere moves here, and its old pack's live bytes drop. The
/// local copies stay. Run inside the caller's transaction.
pub fn record_on(
    tx: &rusqlite::Connection,
    id: Ulid,
    kind: Kind,
    bytes: u64,
    sha256: &Digest32,
    entries: &[Entry],
    now_ms: i64,
) -> rusqlite::Result<()> {
    let pack_id = id.to_string();
    let live: u64 = entries.iter().map(|e| e.len).sum();
    tx.execute(
        "INSERT INTO body_packs (pack_id, kind, bytes, bodies, live_bytes, sha256, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            pack_id,
            kind.as_str(),
            bytes as i64,
            entries.len() as i64,
            live as i64,
            sha256.as_slice(),
            now_ms
        ],
    )?;
    let mut moved = tx.prepare_cached(
        "UPDATE body_packs SET live_bytes = live_bytes - l.len
           FROM (SELECT pack_id, len FROM body_locations WHERE match_id = ?1 AND kind = ?2) AS l
          WHERE body_packs.pack_id = l.pack_id AND l.pack_id <> ?3",
    )?;
    let mut locate = tx.prepare_cached(
        "INSERT INTO body_locations (match_id, kind, pack_id, offset, len, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (match_id, kind) DO UPDATE SET pack_id = excluded.pack_id,
           offset = excluded.offset, len = excluded.len, sha256 = excluded.sha256",
    )?;
    let mut packed = tx.prepare_cached(&format!(
        "UPDATE {} SET packed_at = ?2 WHERE match_id = ?1 AND packed_at IS NULL",
        kind.table()
    ))?;
    for e in entries {
        moved.execute(rusqlite::params![e.match_id, kind.as_str(), pack_id])?;
        locate.execute(rusqlite::params![
            e.match_id,
            kind.as_str(),
            pack_id,
            e.offset as i64,
            e.len as i64,
            e.sha256.as_slice()
        ])?;
        packed.execute(rusqlite::params![e.match_id, now_ms])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

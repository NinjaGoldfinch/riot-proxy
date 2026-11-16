//! L2: the SQLite `cache` table (docs/design/04 §Cache tiers), holding the
//! expensive-to-rebuild endpoints (`Endpoint::persist_l2`).
//!
//! Write-behind: puts are queued on a bounded channel and a task writes them in
//! one transaction per batch, every 2 s or 500 entries, whichever comes first. A
//! crash loses at most one batch window of L2 writes; L1 and Riot cover it. The
//! request path never waits: with the queue full, an entry is dropped and logged.
//!
//! Boot: [`warm`] streams the rows L1 would still hold into L1, the
//! latest-expiring first, until L1's budget is reached; then [`sweep`] deletes
//! the rows that are of no more use. A 200 of an endpoint a player's page
//! reads ([`Kept`]) is kept [`EXPIRED_KEEP`] past its hard TTL, as L1 keeps
//! it, so a player's stored parts survive a restart (AVL-03, ADR-212). Every
//! other row is loaded only inside its hard TTL and swept at it (AVL-08).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cache::keys::{KeyScope, endpoint_prefix};
use crate::cache::l1::{CacheEntry, EXPIRED_KEEP, L1, entry_weight};
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::riot::endpoints::ENDPOINTS;

/// design/04: flush every 2 s …
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// … or at 500 entries.
pub const FLUSH_BATCH: usize = 500;
/// Queued writes before new ones are dropped.
const QUEUE: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub body: Bytes,
    pub status: u16,
    pub content_at: i64,
    pub fetched_at: i64,
    pub soft_expires: i64,
    pub hard_expires: i64,
}

impl Row {
    pub fn from_entry(key: &str, entry: &CacheEntry, clock: Clock) -> Self {
        Self {
            key: key.to_string(),
            body: entry.body.clone(),
            status: entry.status,
            content_at: clock.to_unix_ms(entry.content_at),
            fetched_at: clock.to_unix_ms(entry.fetched_at),
            soft_expires: clock.to_unix_ms(entry.soft_expires),
            hard_expires: clock.to_unix_ms(entry.hard_expires),
        }
    }

    /// The L1 entry this row was. `kept` says whether it is one of [`Kept`]'s
    /// rows: a 200 that is read as `Expired` for [`EXPIRED_KEEP`] past its
    /// hard TTL.
    pub fn to_entry(&self, clock: Clock, kept: bool) -> CacheEntry {
        let hard_expires = clock.to_instant(self.hard_expires);
        CacheEntry {
            status: self.status,
            body: self.body.clone(),
            content_at: clock.to_instant(self.content_at),
            fetched_at: clock.to_instant(self.fetched_at),
            soft_expires: clock.to_instant(self.soft_expires),
            hard_expires,
            kept_until: if kept && self.status == 200 {
                hard_expires + EXPIRED_KEEP
            } else {
                hard_expires
            },
        }
    }
}

/// Which L2 rows outlive their hard TTL (AVL-03, AVL-08): the 200s of the
/// endpoints with [`keep_expired`](crate::riot::endpoints::Endpoint::keep_expired),
/// under the process's key scope. A cache key starts with its scope and
/// endpoint, so each endpoint's rows are one range of the table's primary
/// key: [`warm`] reads the kept rows through those ranges and never visits
/// another endpoint's expired rows. Rows of another key scope hold another
/// key's ids and are never kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    /// `[from, to)` of each kept endpoint's keys.
    ranges: Vec<(String, String)>,
}

impl Kept {
    /// Nothing is kept past its hard TTL.
    pub fn none() -> Self {
        Self::default()
    }

    /// The kept endpoints that are persisted at all, under `scope`.
    pub fn for_scope(scope: &KeyScope) -> Self {
        let ranges = ENDPOINTS
            .iter()
            .filter(|e| e.keep_expired && e.persist_l2)
            .map(|e| {
                let from = endpoint_prefix(scope, e.id);
                // The prefix ends in ':'; ';' is the next byte, so this is
                // the first key past every key that starts with it.
                let mut to = from.clone();
                to.pop();
                to.push(';');
                (from, to)
            })
            .collect();
        Self { ranges }
    }

    /// Whether `key` is one of a kept endpoint's.
    pub fn holds(&self, key: &str) -> bool {
        self.ranges
            .iter()
            .any(|(from, to)| key >= from.as_str() && key < to.as_str())
    }
}

/// Handle to the write-behind task.
#[derive(Debug)]
pub struct L2Writer {
    tx: Mutex<Option<mpsc::Sender<Row>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Entries dropped on a full queue since the start (`cache_l2_dropped_total`).
    dropped: AtomicU64,
}

impl L2Writer {
    /// Start the writer with design/04's batching.
    pub fn spawn(db: Db) -> Self {
        Self::spawn_with(db, FLUSH_BATCH, FLUSH_EVERY)
    }

    pub fn spawn_with(db: Db, batch: usize, every: Duration) -> Self {
        Self::spawn_queued(db, batch, every, QUEUE)
    }

    /// As [`L2Writer::spawn_with`], holding at most `queue` writes before it drops.
    pub fn spawn_queued(db: Db, batch: usize, every: Duration, queue: usize) -> Self {
        let (tx, rx) = mpsc::channel(queue.max(1));
        let task = tokio::spawn(write_loop(db, rx, batch.max(1), every));
        Self {
            tx: Mutex::new(Some(tx)),
            task: Mutex::new(Some(task)),
            dropped: AtomicU64::new(0),
        }
    }

    /// Entries dropped because the queue was full, since the start.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Queue an entry. Never waits. After shutdown, does nothing.
    pub fn enqueue(&self, key: &str, entry: &CacheEntry) {
        let row = Row::from_entry(key, entry, Clock::now());
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(tx) = tx
            && let Err(mpsc::error::TrySendError::Full(row)) = tx.try_send(row)
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!(crate::metrics::CACHE_L2_DROPPED_TOTAL).increment(1);
            tracing::warn!(key = %row.key, "L2 write queue full; entry not persisted");
        }
    }

    /// Flush what is queued and stop (shutdown). Idempotent.
    pub async fn shutdown(&self) {
        drop(self.tx.lock().unwrap_or_else(PoisonError::into_inner).take());
        let task = self.task.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(task) = task
            && let Err(e) = task.await
        {
            tracing::warn!(error = %e, "L2 writer task failed");
        }
    }
}

async fn write_loop(db: Db, mut rx: mpsc::Receiver<Row>, batch: usize, every: Duration) {
    let mut pending: Vec<Row> = Vec::with_capacity(batch);
    loop {
        // Wait for the first row of a batch, then give the batch `every` to fill.
        let Some(first) = rx.recv().await else { break };
        pending.push(first);
        let deadline = tokio::time::Instant::now() + every;
        let mut closed = false;
        while pending.len() < batch {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(row)) => pending.push(row),
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        flush(&db, std::mem::take(&mut pending)).await;
        if closed {
            break;
        }
    }
}

async fn flush(db: &Db, rows: Vec<Row>) {
    let n = rows.len();
    if let Err(e) = put_rows(db, rows).await {
        tracing::warn!(error = %e, rows = n, "L2 flush failed; entries stay in L1 only");
    }
}

/// Upsert rows in one transaction.
pub async fn put_rows(db: &Db, rows: Vec<Row>) -> Result<(), DbError> {
    db.write(move |c| {
        let tx = c.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO cache (key, body, status, content_at, fetched_at, soft_expires, hard_expires)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for r in &rows {
                stmt.execute(rusqlite::params![
                    r.key,
                    r.body.as_ref(),
                    r.status,
                    r.content_at,
                    r.fetched_at,
                    r.soft_expires,
                    r.hard_expires
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// [`EXPIRED_KEEP`] in unix ms.
fn keep_ms() -> i64 {
    i64::try_from(EXPIRED_KEEP.as_millis()).unwrap_or(i64::MAX)
}

/// The rows inside their hard TTL (`?1` is now), the latest-expiring first:
/// `cache_hard` read backwards, so rows come in order with no sort and the
/// read can stop at any row. A row from before V0009 has no fetch time: its
/// content's is the best there is.
pub const WARM_LIVE_SQL: &str =
    "SELECT key, body, status, content_at, COALESCE(fetched_at, content_at), soft_expires, hard_expires
   FROM cache WHERE hard_expires > ?1 ORDER BY hard_expires DESC";

/// One kept endpoint's 200s past their hard TTL (`?4` is now) by no more than
/// `EXPIRED_KEEP` (`?3`): the endpoint's range of the primary key (`?1` to
/// `?2`). The `+` keeps the planner off `cache_hard`, whose range holds every
/// endpoint's expired rows.
pub const WARM_KEPT_SQL: &str = "SELECT hard_expires, rowid FROM cache
   WHERE key >= ?1 AND key < ?2 AND status = 200 AND +hard_expires > ?3 AND +hard_expires <= ?4";

/// One row by its rowid.
pub const WARM_ROW_SQL: &str =
    "SELECT key, body, status, content_at, COALESCE(fetched_at, content_at), soft_expires, hard_expires
   FROM cache WHERE rowid = ?1";

/// Rows the reader may hold ahead of L1 while [`warm`] streams.
const WARM_AHEAD: usize = 64;

/// What [`warm`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Warmed {
    /// Rows loaded into L1.
    pub loaded: usize,
    /// Their weight against L1's budget.
    pub bytes: u64,
    /// The next row did not fit L1's budget, so the rest were left in L2.
    pub full: bool,
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        key: r.get(0)?,
        body: Bytes::from(r.get::<_, Vec<u8>>(1)?),
        status: r.get(2)?,
        content_at: r.get(3)?,
        fetched_at: r.get(4)?,
        soft_expires: r.get(5)?,
        hard_expires: r.get(6)?,
    })
}

/// Hand `send` the rows [`warm`] loads, the latest-expiring first, until it
/// returns `false`: every row inside its hard TTL, then the kept endpoints'
/// 200s that passed it within [`EXPIRED_KEEP`]. Each row is read when it is
/// handed over; of the kept rows only their ids are listed first, to order them.
fn stream_rows(
    c: &rusqlite::Connection,
    now_ms: i64,
    kept: &Kept,
    mut send: impl FnMut(Row, bool) -> bool,
) -> Result<(), DbError> {
    let mut live = c.prepare(WARM_LIVE_SQL)?;
    let mut rows = live.query([now_ms])?;
    while let Some(r) = rows.next()? {
        let row = read_row(r)?;
        let is_kept = kept.holds(&row.key);
        if !send(row, is_kept) {
            return Ok(());
        }
    }
    let kept_ms = now_ms.saturating_sub(keep_ms());
    let mut ids: Vec<(i64, i64)> = Vec::new();
    let mut of_range = c.prepare(WARM_KEPT_SQL)?;
    for (from, to) in &kept.ranges {
        let found = of_range.query_map(rusqlite::params![from, to, kept_ms, now_ms], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        for id in found {
            ids.push(id?);
        }
    }
    // Most recently expired first; the rowid settles a tie the same way each time.
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let mut by_id = c.prepare(WARM_ROW_SQL)?;
    for (_, rowid) in ids {
        let mut rows = by_id.query([rowid])?;
        if let Some(r) = rows.next()? {
            let row = read_row(r)?;
            if !send(row, true) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Load the rows L1 would still hold into `l1`: every row inside its hard
/// TTL, and `kept`'s 200s up to [`EXPIRED_KEEP`] past theirs. They are
/// streamed from a reader, the latest-expiring first, and the load stops at
/// the first row that does not fit `l1`'s budget (`CACHE_L1_MAX_MB`), so what
/// boot reads and holds is bounded by that budget, not by what a day of
/// traffic left in L2 (AVL-08).
pub async fn warm(db: &Db, l1: &L1, kept: &Kept) -> Result<Warmed, DbError> {
    let clock = Clock::now();
    let now_ms = clock.unix_ms;
    let (tx, mut rx) = mpsc::channel::<(Row, bool)>(WARM_AHEAD);
    let kept = kept.clone();
    let read = db.read(move |c| {
        // A send fails once `load` has stopped listening: the budget is full.
        stream_rows(c, now_ms, &kept, |row, is_kept| {
            tx.blocking_send((row, is_kept)).is_ok()
        })
    });
    let load = async move {
        let budget = l1.max_bytes();
        let mut out = Warmed::default();
        while let Some((row, is_kept)) = rx.recv().await {
            let weight = entry_weight(&row.key, row.body.len());
            if out.bytes.saturating_add(weight) > budget {
                out.full = true;
                break;
            }
            l1.insert_entry(&row.key, row.to_entry(clock, is_kept)).await;
            out.loaded += 1;
            out.bytes += weight;
        }
        // Dropping the receiver here is what stops the reader.
        out
    };
    let (read, out) = tokio::join!(read, load);
    read?;
    Ok(out)
}

/// `DELETE` for [`sweep`] with `kept` key ranges: every row past its hard
/// TTL (`?1` is now) but the kept endpoints' 200s that passed it within
/// `EXPIRED_KEEP` (`?2`). The ranges are `?3` to `?4`, `?5` to `?6` and so on.
/// The rows come from `cache_hard`'s range below now.
pub fn sweep_sql(kept: usize) -> String {
    if kept == 0 {
        return "DELETE FROM cache WHERE hard_expires <= ?1".to_string();
    }
    let ranges = (0..kept)
        .map(|i| format!("(key >= ?{} AND key < ?{})", 3 + 2 * i, 4 + 2 * i))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "DELETE FROM cache WHERE hard_expires <= ?1
            AND NOT (status = 200 AND hard_expires > ?2 AND ({ranges}))"
    )
}

/// Delete the rows [`warm`] would never load again: every row past its hard
/// TTL except `kept`'s 200s inside [`EXPIRED_KEEP`]. Returns how many.
pub async fn sweep(db: &Db, kept: &Kept) -> Result<usize, DbError> {
    let now_ms = Clock::now().unix_ms;
    let kept_ms = now_ms.saturating_sub(keep_ms());
    let kept = kept.clone();
    db.write(move |c| {
        let mut params: Vec<rusqlite::types::Value> = vec![now_ms.into()];
        if !kept.ranges.is_empty() {
            params.push(kept_ms.into());
        }
        for (from, to) in &kept.ranges {
            params.push(from.clone().into());
            params.push(to.clone().into());
        }
        Ok(c.execute(&sweep_sql(kept.ranges.len()), rusqlite::params_from_iter(params))?)
    })
    .await
}

/// Keys matching `pred`, deleting nothing (a purge's dry run, ADM-10, and the
/// selection before [`delete_keys`]).
pub async fn keys_where(
    db: &Db,
    pred: impl Fn(&str) -> bool + Send + 'static,
) -> Result<Vec<String>, DbError> {
    db.read(move |c| {
        let mut stmt = c.prepare("SELECT key FROM cache")?;
        let keys = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys.into_iter().filter(|k| pred(k)).collect())
    })
    .await
}

/// Delete these rows in one transaction (admin purge, P5-05). Returns the keys
/// that were there. The caller selects them with [`keys_where`] on a reader, so
/// the writer runs no matching (REV-03).
pub async fn delete_keys(db: &Db, keys: Vec<String>) -> Result<Vec<String>, DbError> {
    if keys.is_empty() {
        return Ok(keys);
    }
    db.write(move |c| {
        let tx = c.transaction()?;
        let mut deleted = Vec::new();
        {
            let mut stmt = tx.prepare("DELETE FROM cache WHERE key = ?1")?;
            for k in keys {
                if stmt.execute([&k])? > 0 {
                    deleted.push(k);
                }
            }
        }
        tx.commit()?;
        Ok(deleted)
    })
    .await
}

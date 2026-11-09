//! L2: the SQLite `cache` table (docs/design/04 §Cache tiers), holding the
//! expensive-to-rebuild endpoints (`Endpoint::persist_l2`).
//!
//! Write-behind: puts are queued on a bounded channel and a task writes them in
//! one transaction per batch, every 2 s or 500 entries, whichever comes first. A
//! crash loses at most one batch window of L2 writes; L1 and Riot cover it. The
//! request path never waits: with the queue full, an entry is dropped and logged.
//!
//! Boot: [`warm`] loads every row L1 would still hold into L1, then [`sweep`]
//! deletes the rest. A positive row is kept [`EXPIRED_KEEP`] past its hard TTL,
//! as L1 keeps it, so a player's stored parts survive a restart (AVL-03, ADR-212).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cache::l1::{CacheEntry, EXPIRED_KEEP, L1};
use crate::clock::Clock;
use crate::db::{Db, DbError};

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

    pub fn to_entry(&self, clock: Clock) -> CacheEntry {
        CacheEntry {
            status: self.status,
            body: self.body.clone(),
            content_at: clock.to_instant(self.content_at),
            fetched_at: clock.to_instant(self.fetched_at),
            soft_expires: clock.to_instant(self.soft_expires),
            hard_expires: clock.to_instant(self.hard_expires),
        }
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

/// Load every row L1 would still hold into `l1`: a negative one inside its
/// TTL, a positive one up to [`EXPIRED_KEEP`] past its hard TTL. Returns how many.
pub async fn warm(db: &Db, l1: &L1) -> Result<usize, DbError> {
    let clock = Clock::now();
    let now_ms = clock.unix_ms;
    let kept_ms = now_ms.saturating_sub(keep_ms());
    let rows: Vec<Row> = db
        .read(move |c| {
            // A row from before V0009 has no fetch time: its content's is the best there is.
            let mut stmt = c.prepare(
                "SELECT key, body, status, content_at, COALESCE(fetched_at, content_at), soft_expires, hard_expires
                 FROM cache WHERE hard_expires > ?2 AND (status = 200 OR hard_expires > ?1)",
            )?;
            let rows = stmt
                .query_map([now_ms, kept_ms], |r| {
                    Ok(Row {
                        key: r.get(0)?,
                        body: Bytes::from(r.get::<_, Vec<u8>>(1)?),
                        status: r.get(2)?,
                        content_at: r.get(3)?,
                        fetched_at: r.get(4)?,
                        soft_expires: r.get(5)?,
                        hard_expires: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await?;
    for row in &rows {
        l1.insert_entry(&row.key, row.to_entry(clock)).await;
    }
    Ok(rows.len())
}

/// Delete the rows [`warm`] would skip. Returns how many.
pub async fn sweep(db: &Db) -> Result<usize, DbError> {
    let now_ms = Clock::now().unix_ms;
    let kept_ms = now_ms.saturating_sub(keep_ms());
    db.write(move |c| {
        Ok(c.execute(
            "DELETE FROM cache WHERE hard_expires <= ?1 AND (status <> 200 OR hard_expires <= ?2)",
            [now_ms, kept_ms],
        )?)
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

//! L2: the SQLite `cache` table (docs/design/04 §Cache tiers), holding the
//! expensive-to-rebuild endpoints (`Endpoint::persist_l2`).
//!
//! Write-behind: puts are queued on a bounded channel and a task writes them in
//! one transaction per batch, every 2 s or 500 entries, whichever comes first. A
//! crash loses at most one batch window of L2 writes; L1 and Riot cover it. The
//! request path never waits: with the queue full, an entry is dropped and logged.
//!
//! Boot: [`warm`] loads every unexpired row into L1, then [`sweep`] deletes the rest.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cache::l1::{CacheEntry, L1};
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
            soft_expires: clock.to_unix_ms(entry.soft_expires),
            hard_expires: clock.to_unix_ms(entry.hard_expires),
        }
    }

    pub fn to_entry(&self, clock: Clock) -> CacheEntry {
        CacheEntry {
            status: self.status,
            body: self.body.clone(),
            content_at: clock.to_instant(self.content_at),
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
}

impl L2Writer {
    /// Start the writer with design/04's batching.
    pub fn spawn(db: Db) -> Self {
        Self::spawn_with(db, FLUSH_BATCH, FLUSH_EVERY)
    }

    pub fn spawn_with(db: Db, batch: usize, every: Duration) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        let task = tokio::spawn(write_loop(db, rx, batch.max(1), every));
        Self {
            tx: Mutex::new(Some(tx)),
            task: Mutex::new(Some(task)),
        }
    }

    /// Queue an entry. Never waits. After shutdown, does nothing.
    pub fn enqueue(&self, key: &str, entry: &CacheEntry) {
        let row = Row::from_entry(key, entry, Clock::now());
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(tx) = tx
            && let Err(mpsc::error::TrySendError::Full(row)) = tx.try_send(row)
        {
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
                "INSERT OR REPLACE INTO cache (key, body, status, content_at, soft_expires, hard_expires)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for r in &rows {
                stmt.execute(rusqlite::params![
                    r.key,
                    r.body.as_ref(),
                    r.status,
                    r.content_at,
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

/// Load every unexpired row into `l1`. Returns how many.
pub async fn warm(db: &Db, l1: &L1) -> Result<usize, DbError> {
    let clock = Clock::now();
    let now_ms = clock.unix_ms;
    let rows: Vec<Row> = db
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT key, body, status, content_at, soft_expires, hard_expires FROM cache WHERE hard_expires > ?1",
            )?;
            let rows = stmt
                .query_map([now_ms], |r| {
                    Ok(Row {
                        key: r.get(0)?,
                        body: Bytes::from(r.get::<_, Vec<u8>>(1)?),
                        status: r.get(2)?,
                        content_at: r.get(3)?,
                        soft_expires: r.get(4)?,
                        hard_expires: r.get(5)?,
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

/// Delete expired rows. Returns how many.
pub async fn sweep(db: &Db) -> Result<usize, DbError> {
    let now_ms = Clock::now().unix_ms;
    db.write(move |c| Ok(c.execute("DELETE FROM cache WHERE hard_expires <= ?1", [now_ms])?))
        .await
}

/// Delete rows whose key matches `pred` (admin purge, P5-05).
pub async fn delete_where(db: &Db, pred: impl Fn(&str) -> bool + Send + 'static) -> Result<usize, DbError> {
    db.write(move |c| {
        let keys: Vec<String> = {
            let mut stmt = c.prepare("SELECT key FROM cache")?;
            stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?
        };
        let tx = c.transaction()?;
        let mut n = 0;
        for k in keys.iter().filter(|k| pred(k)) {
            n += tx.execute("DELETE FROM cache WHERE key = ?1", [k])?;
        }
        tx.commit()?;
        Ok(n)
    })
    .await
}

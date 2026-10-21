//! The WAL checkpointer (WR-04, ADR-185): a thread with its own connection
//! that copies committed WAL frames back into the database file, so commits
//! on the writer don't.
//!
//! With `wal_autocheckpoint` at SQLite's 1000 pages, the commit that crossed
//! the threshold ran a checkpoint before returning, on the writer thread:
//! production's `aggregate:delta` commits took 0.25–1.4 s that way while
//! their statements took a tenth of it (THR-03, ADR-182). A `PASSIVE`
//! checkpoint takes no write lock, so it runs here beside the writer and
//! never blocks it; frames a reader still needs are left for the next pass.
//! The writer keeps an automatic checkpoint as a backstop at
//! [`super::WAL_BACKSTOP_PAGES`], for a checkpointer that falls behind.

use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::{CONNECTION_PRAGMAS, DbError, apply_pragmas};

/// How often the checkpointer runs. A pass with nothing to copy is a read
/// of the WAL index.
pub const EVERY: Duration = Duration::from_secs(1);

/// What the last pass found, for tests and the dashboard.
#[derive(Debug, Default)]
pub struct Progress {
    /// Passes run since open.
    pub runs: AtomicU64,
    /// Frames in the WAL, and how many of them are in the database file,
    /// after the last pass (SQLite's `log` and `checkpointed`); -1 before.
    pub log: AtomicI64,
    pub checkpointed: AtomicI64,
}

/// The running checkpointer. Dropping it stops the thread and waits for it.
pub(crate) struct Checkpointer {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
    pub(crate) progress: Arc<Progress>,
}

impl Checkpointer {
    /// Open a connection to `path` and start checkpointing every `every`.
    pub(crate) fn start(path: &Path, every: Duration) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        apply_pragmas(&conn, CONNECTION_PRAGMAS)?;
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let progress = Arc::new(Progress {
            log: AtomicI64::new(-1),
            checkpointed: AtomicI64::new(-1),
            ..Progress::default()
        });
        let (thread_stop, thread_progress) = (Arc::clone(&stop), Arc::clone(&progress));
        let handle = std::thread::Builder::new()
            .name("sqlite-checkpoint".into())
            .spawn(move || run(&conn, &thread_stop, &thread_progress, every))
            .map_err(DbError::Spawn)?;
        Ok(Self {
            stop,
            handle: Some(handle),
            progress,
        })
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stop;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(conn: &Connection, stop: &(Mutex<bool>, Condvar), progress: &Progress, every: Duration) {
    let (lock, wake) = stop;
    loop {
        {
            let stopped = lock.lock().unwrap_or_else(PoisonError::into_inner);
            let (stopped, _) = wake
                .wait_timeout_while(stopped, every, |s| !*s)
                .unwrap_or_else(PoisonError::into_inner);
            if *stopped {
                return;
            }
        }
        let started = Instant::now();
        match pass(conn) {
            Ok((_busy, log, checkpointed)) => {
                progress.log.store(log, Ordering::Relaxed);
                progress.checkpointed.store(checkpointed, Ordering::Relaxed);
                metrics::histogram!(crate::metrics::SQLITE_CHECKPOINT_SECONDS).record(started.elapsed().as_secs_f64());
                #[allow(clippy::cast_precision_loss)]
                metrics::gauge!(crate::metrics::SQLITE_WAL_FRAMES_PENDING).set((log - checkpointed).max(0) as f64);
            }
            Err(e) => tracing::warn!(error = %e, "WAL checkpoint failed"),
        }
        progress.runs.fetch_add(1, Ordering::Relaxed);
    }
}

/// One `PASSIVE` checkpoint: `(busy, log, checkpointed)` as SQLite reports
/// them. `busy` is 1 when another checkpoint held the lock (the writer's
/// backstop, or maintenance's `TRUNCATE`); this pass then did nothing.
pub(crate) fn pass(conn: &Connection) -> rusqlite::Result<(i64, i64, i64)> {
    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
}

//! SQLite access: one writer thread fed by a channel of closures, plus a pool of
//! read-only connections used through `spawn_blocking` (docs/design/04 §SQLite
//! configuration). Async code never touches a `Connection` directly.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

use crate::archive::objects::Objects;

mod checkpointer;
pub mod disk;
pub mod store;
mod writer;

pub use checkpointer::Progress as CheckpointProgress;

pub use writer::WriterStatus;
use writer::{Stamps, WriteJob};

mod embedded {
    refinery::embed_migrations!("src/db/migrations");
}

/// Pragmas every connection gets (design 04). `journal_mode` and
/// `wal_autocheckpoint` are database-wide and set by the writer only.
const CONNECTION_PRAGMAS: &[(&str, &str)] = &[
    ("busy_timeout", "5000"),
    ("foreign_keys", "ON"),
    ("cache_size", "-65536"),
    ("mmap_size", "268435456"),
    ("temp_store", "MEMORY"),
];
const WRITER_PRAGMAS: &[(&str, &str)] = &[
    ("journal_mode", "WAL"),
    ("synchronous", "NORMAL"),
    ("wal_autocheckpoint", WAL_BACKSTOP_PAGES),
];

/// The writer's own automatic checkpoint, a backstop only (WR-04, ADR-185):
/// the checkpointer thread copies frames back every second, so a commit
/// reaches this many WAL pages (about 40 MiB) only if it falls behind. At
/// SQLite's default of 1000 the commit that crossed it ran the checkpoint.
const WAL_BACKSTOP_PAGES: &str = "10000";

/// Queued writes before `write()` callers start waiting for the writer.
const WRITE_QUEUE: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration failed: {0}")]
    Migration(#[from] refinery::Error),
    #[error("could not create {path}: {source}")]
    CreateDir { path: PathBuf, source: std::io::Error },
    #[error("could not stat {path}: {source}")]
    Stat { path: PathBuf, source: std::io::Error },
    #[error("the SQLite writer thread has stopped or the write panicked")]
    WriterGone,
    #[error("a SQLite read closure panicked")]
    ReaderPanicked,
    #[error("no SQLite reader connection available")]
    PoolEmpty,
    #[error("a SQLite reader task failed: {0}")]
    ReaderJoin(#[from] tokio::task::JoinError),
    #[error("could not start the SQLite writer thread: {0}")]
    Spawn(std::io::Error),
    /// The `postgres` feature's store is a stub (ADR-062).
    #[cfg(feature = "postgres")]
    #[error("the Postgres store is not implemented yet")]
    PostgresUnimplemented,
}

/// Cheap to clone; every clone shares the writer and the reader pool. The writer
/// thread exits once the last clone is dropped and its queue has drained.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: mpsc::Sender<WriteJob>,
    /// Idle read connections. A permit from `permits` entitles its holder to one.
    readers: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
    reader_count: usize,
    /// The bucket this database's offloaded bodies are in (OBJ-02). Set once,
    /// at boot; unset, an offloaded body reads as missing.
    objects: OnceLock<Objects>,
    /// Calls to [`Db::read`] since open: what a cached analytics hit must not
    /// add to (PERF-03).
    reads: AtomicU64,
    /// What the writer thread is doing (WR-02).
    stamps: Arc<Stamps>,
    /// Copies the WAL back into the database file off the writer (WR-04).
    checkpointer: checkpointer::Checkpointer,
}

impl Inner {
    fn idle_readers(&self) -> std::sync::MutexGuard<'_, Vec<Connection>> {
        self.readers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A reader connection on loan with the permit that pays for it. Dropping the loan
/// puts the connection back and only then releases the permit, wherever that
/// happens: at the end of the blocking closure, or with the closure if it never
/// runs. So a cancelled [`Db::read`] cannot leave a permit without a connection
/// (INC-01, ADR-113).
struct ReaderLoan {
    inner: Arc<Inner>,
    conn: Option<Connection>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ReaderLoan {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut idle = self.inner.idle_readers();
            idle.push(conn);
            set_readers_free(idle.len());
        }
        // `_permit` is released after this, once the connection is back.
    }
}

fn set_readers_free(free: usize) {
    metrics::gauge!(crate::metrics::SQLITE_READERS_FREE).set(free as f64);
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Open (creating if needed) the database at `path`, apply pragmas, run the
    /// embedded migrations, start the writer thread and open `readers` read-only
    /// connections. Blocking — call from `spawn_blocking` or before the runtime starts,
    /// or use [`Db::open_async`].
    pub fn open(path: &Path, readers: usize) -> Result<Self, DbError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| DbError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut writer = Connection::open(path)?;
        apply_pragmas(&writer, WRITER_PRAGMAS)?;
        apply_pragmas(&writer, CONNECTION_PRAGMAS)?;
        migrate(&mut writer)?;
        optimize(&writer)?;
        let stamps = Arc::new(Stamps::new());
        writer::install_progress(&writer, &stamps)?;

        let readers = readers.max(1);
        let pool = (0..readers)
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>, _>>()?;

        let (tx, rx) = mpsc::channel(WRITE_QUEUE);
        let writer_stamps = Arc::clone(&stamps);
        // The thread records its CPU clock first, so it is there before any job.
        let (registered, wait_registered) = std::sync::mpsc::sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("sqlite-writer".into())
            .spawn(move || {
                writer::register_thread(&writer_stamps);
                let _ = registered.send(());
                writer_loop(writer, rx, &writer_stamps);
            })
            .map_err(DbError::Spawn)?;
        let _ = wait_registered.recv();
        let checkpointer = checkpointer::Checkpointer::start(path, checkpointer::EVERY)?;

        set_readers_free(readers);
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: tx,
                readers: Mutex::new(pool),
                permits: Arc::new(Semaphore::new(readers)),
                reader_count: readers,
                objects: OnceLock::new(),
                reads: AtomicU64::new(0),
                stamps,
                checkpointer,
            }),
        })
    }

    /// [`Db::open`] on the blocking pool.
    pub async fn open_async(path: PathBuf, readers: usize) -> Result<Self, DbError> {
        tokio::task::spawn_blocking(move || Self::open(&path, readers)).await?
    }

    /// Give this database the bucket its offloaded bodies live in (design/12).
    /// Only the first call takes effect; later ones return `false`.
    pub fn attach_objects(&self, objects: Objects) -> bool {
        self.inner.objects.set(objects).is_ok()
    }

    /// What the WAL checkpointer's last pass found (WR-04).
    pub fn checkpoints(&self) -> &CheckpointProgress {
        &self.inner.checkpointer.progress
    }

    /// The bucket set by [`Db::attach_objects`], if any.
    pub fn objects(&self) -> Option<&Objects> {
        self.inner.objects.get()
    }

    /// Reader count used by `serve`: one per core (design 04).
    pub fn default_readers() -> usize {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Read connections in the pool, idle or not.
    pub fn readers(&self) -> usize {
        self.inner.reader_count
    }

    /// Calls to [`Db::read`] since the database was opened.
    pub fn reads(&self) -> u64 {
        self.inner.reads.load(Ordering::Relaxed)
    }

    /// Read connections idle right now (`/readyz`, `sqlite_readers_free`).
    pub fn readers_free(&self) -> usize {
        self.inner.idle_readers().len()
    }

    /// What the writer thread is doing now: read from atomics, never queued behind
    /// it (`/readyz`, WR-02).
    pub fn writer_status(&self) -> WriterStatus {
        self.inner.stamps.status(self.write_queued())
    }

    /// Writes waiting behind the running one.
    fn write_queued(&self) -> usize {
        WRITE_QUEUE.saturating_sub(self.inner.writer.capacity())
    }

    /// Test hook: how long a busy writer may go without progress before `/readyz`
    /// counts it as stalled. `None` until set; `/readyz` then uses its default.
    #[doc(hidden)]
    pub fn set_writer_stall(&self, stall: std::time::Duration) {
        self.inner.stamps.set_stall(stall);
    }

    #[doc(hidden)]
    pub fn writer_stall(&self) -> Option<std::time::Duration> {
        self.inner.stamps.stall()
    }

    /// CPU time the writer thread has used (Linux; `None` elsewhere). Two readings
    /// apart tell a write that computes without SQLite stamping progress, such as a
    /// large sort, from one that waits (WR-02).
    pub fn writer_cpu_time(&self) -> Option<std::time::Duration> {
        self.inner.stamps.cpu_time()
    }

    /// Run `f` on the writer thread. Writes are serialised by construction, so
    /// they never see `SQLITE_BUSY` from each other. A panic inside `f` is
    /// contained: this call returns [`DbError::WriterGone`] and the writer lives on.
    pub async fn write<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job = WriteJob {
            queued_at: std::time::Instant::now(),
            run: Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }),
        };
        self.inner
            .writer
            .send(job)
            .await
            .map_err(|_| DbError::WriterGone)?;
        writer::set_queue_gauge(self.write_queued());
        rx.await.map_err(|_| DbError::WriterGone)?
    }

    /// Run `f` on a read-only pooled connection via `spawn_blocking`. Writes
    /// attempted here fail with `SQLITE_READONLY`; a panic inside `f` returns
    /// [`DbError::ReaderPanicked`] and the connection goes back to the pool.
    /// Dropping the returned future (a sibling failing in `try_join!`, a client
    /// going away) lets `f` finish, and the connection still goes back.
    pub async fn read<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        self.inner.reads.fetch_add(1, Ordering::Relaxed);
        // The semaphore is never closed, so acquire cannot fail in practice.
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|_| DbError::PoolEmpty)?;
        let conn = {
            let mut idle = self.inner.idle_readers();
            let conn = idle.pop();
            set_readers_free(idle.len());
            conn
        };
        // Every permit holder finds a connection, so this cannot fail in practice.
        let conn = conn.ok_or(DbError::PoolEmpty)?;
        let loan = ReaderLoan {
            inner: Arc::clone(&self.inner),
            conn: Some(conn),
            _permit: permit,
        };
        tokio::task::spawn_blocking(move || {
            let conn = loan.conn.as_ref().ok_or(DbError::PoolEmpty)?;
            // Catch here so a panic is reported as an error, not a JoinError.
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| f(conn)));
            drop(loan);
            result.unwrap_or_else(|_| Err(DbError::ReaderPanicked.into()))
        })
        .await
        .map_err(DbError::from)?
    }

    /// [`optimize`] on the writer.
    pub async fn optimize(&self) -> Result<(), DbError> {
        self.write(|c| optimize(c)).await
    }
}

/// Bring the query planner's statistics up to date (ADR-102): `ANALYZE` each
/// table that was never analysed or has grown about tenfold since, within the
/// time limit SQLite sets itself. Run on the writer at open, before each
/// analytics rebuild and by the daily `maintenance`. Readers load the new
/// statistics on their next read.
pub fn optimize(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("PRAGMA optimize = 0x10002")?;
    Ok(())
}

fn open_reader(path: &Path) -> Result<Connection, DbError> {
    let flags =
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(path, flags)?;
    apply_pragmas(&conn, CONNECTION_PRAGMAS)?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection, pragmas: &[(&str, &str)]) -> Result<(), DbError> {
    for (name, value) in pragmas {
        // journal_mode returns a row; pragma_update_and_check tolerates both kinds.
        conn.pragma_update_and_check(None, name, value, |_| Ok(()))
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(()),
                e => Err(e),
            })?;
    }
    Ok(())
}

/// Apply embedded migrations in one transaction. Already-applied versions are
/// skipped, so this is safe on every boot.
pub fn migrate(conn: &mut Connection) -> Result<refinery::Report, DbError> {
    Ok(embedded::migrations::runner().set_grouped(true).run(conn)?)
}

fn writer_loop(mut conn: Connection, mut rx: mpsc::Receiver<WriteJob>, stamps: &Stamps) {
    let mut after_idle = true;
    while let Some(job) = rx.blocking_recv() {
        after_idle = writer::run_job(&mut conn, job, stamps, &rx, after_idle);
    }
    tracing::debug!("SQLite writer stopped");
}

#[cfg(test)]
mod tests;

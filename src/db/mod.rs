//! SQLite access: one writer thread fed by a channel of closures, plus a pool of
//! read-only connections used through `spawn_blocking` (docs/design/04 §SQLite
//! configuration). Async code never touches a `Connection` directly.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

pub mod store;

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
    ("wal_autocheckpoint", "1000"),
];

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

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send>;

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

        let readers = readers.max(1);
        let pool = (0..readers)
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>, _>>()?;

        let (tx, rx) = mpsc::channel(WRITE_QUEUE);
        std::thread::Builder::new()
            .name("sqlite-writer".into())
            .spawn(move || writer_loop(writer, rx))
            .map_err(DbError::Spawn)?;

        set_readers_free(readers);
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: tx,
                readers: Mutex::new(pool),
                permits: Arc::new(Semaphore::new(readers)),
                reader_count: readers,
            }),
        })
    }

    /// [`Db::open`] on the blocking pool.
    pub async fn open_async(path: PathBuf, readers: usize) -> Result<Self, DbError> {
        tokio::task::spawn_blocking(move || Self::open(&path, readers)).await?
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

    /// Read connections idle right now (`/readyz`, `sqlite_readers_free`).
    pub fn readers_free(&self) -> usize {
        self.inner.idle_readers().len()
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
        let job: WriteJob = Box::new(move |conn| {
            let _ = tx.send(f(conn));
        });
        self.inner
            .writer
            .send(job)
            .await
            .map_err(|_| DbError::WriterGone)?;
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

fn writer_loop(mut conn: Connection, mut rx: mpsc::Receiver<WriteJob>) {
    while let Some(job) = rx.blocking_recv() {
        // The job's oneshot sender is dropped on panic, which the caller sees as
        // WriterGone; the connection stays usable for the next job.
        if std::panic::catch_unwind(AssertUnwindSafe(|| job(&mut conn))).is_err() {
            tracing::error!("a SQLite write closure panicked");
        }
    }
    tracing::debug!("SQLite writer stopped");
}

#[cfg(test)]
mod tests;

//! SQLite access: one writer thread fed by a channel of closures, plus a pool of
//! read-only connections used through `spawn_blocking` (docs/design/04 §SQLite
//! configuration). Async code never touches a `Connection` directly.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{Semaphore, mpsc, oneshot};

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
    readers: Mutex<Vec<Connection>>,
    permits: Semaphore,
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

        let readers = readers.max(1);
        let pool = (0..readers)
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>, _>>()?;

        let (tx, rx) = mpsc::channel(WRITE_QUEUE);
        std::thread::Builder::new()
            .name("sqlite-writer".into())
            .spawn(move || writer_loop(writer, rx))
            .map_err(DbError::Spawn)?;

        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: tx,
                readers: Mutex::new(pool),
                permits: Semaphore::new(readers),
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
    pub async fn read<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        // The semaphore is never closed, so acquire cannot fail in practice.
        let _permit = self
            .inner
            .permits
            .acquire()
            .await
            .map_err(|_| DbError::PoolEmpty)?;
        let conn = self
            .inner
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let conn = conn.ok_or(DbError::PoolEmpty)?;
        let (conn, result) = tokio::task::spawn_blocking(move || {
            // Catch here so the connection always goes back to the pool.
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| f(&conn)));
            (conn, result)
        })
        .await
        .map_err(DbError::from)?;
        self.inner
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(conn);
        result.unwrap_or_else(|_| Err(DbError::ReaderPanicked.into()))
    }
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

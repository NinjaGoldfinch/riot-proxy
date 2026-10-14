//! `maintenance` (design/06 §Job catalogue, design/07 §Backups & restore):
//! the daily housekeeping, in this order.
//!
//! 1. **Backup** first, so a bad day's other steps can be undone from it:
//!    `VACUUM INTO "$DATA_DIR/backups/riot-proxy-<date>.db"`, a consistent
//!    snapshot taken while the service runs. One per UTC day (a restart the
//!    same day does not take another), the newest `BACKUP_KEEP` kept (14;
//!    0 takes none and prunes none). Skipped, with a warning, when the disk
//!    has less than [`BACKUP_FREE_FACTOR`] times the database's size free,
//!    and a copy that fails is logged and cleaned up: neither stops the
//!    steps below (DSK-02, ADR-136).
//! 2. **Trim** `done` jobs older than 7 days (design/06) and `metrics_history`
//!    older than 24 hours (1440 points at 60 s, design/04).
//! 3. **Sweep** expired L2 cache rows.
//! 4. **Optimize:** `PRAGMA optimize` refreshes the planner's statistics
//!    where they are missing or stale (ADR-102).
//! 5. **Checkpoint** the WAL with `PRAGMA wal_checkpoint(TRUNCATE)`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use futures_util::future::BoxFuture;
use serde::Serialize;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{Handler, Job, JobError};

/// Backups kept by default (`BACKUP_KEEP`, design/07).
pub const KEEP_BACKUPS: usize = 14;
/// A backup needs this many times the database's size (file plus WAL) free
/// on the backup directory's file system, else it is skipped. The copy is
/// about the file's live size; the rest is room for the day's writes.
pub const BACKUP_FREE_FACTOR: f64 = 1.2;
/// `done` jobs kept this long (design/06).
pub const JOB_RETENTION_MS: i64 = 7 * 24 * 3_600_000;
/// `metrics_history` kept this long (design/04: 1440 rows at 60 s).
pub const HISTORY_RETENTION_MS: i64 = 24 * 3_600_000;

const PREFIX: &str = "riot-proxy-";
const SUFFIX: &str = ".db";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{0} already exists")]
    Exists(PathBuf),
    #[error("could not read the free disk space: {0}")]
    Disk(DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

/// `VACUUM INTO out` from its own read-only connection, so the writer is not
/// held for the length of the copy. `out` must not exist (SQLite's rule too).
pub async fn vacuum_into(db_path: &Path, out: &Path) -> Result<(), BackupError> {
    if out.exists() {
        return Err(BackupError::Exists(out.to_path_buf()));
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir).await?;
    }
    let (from, to) = (db_path.to_path_buf(), out.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &from,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute("VACUUM INTO ?1", [to.to_string_lossy()])?;
        Ok::<_, BackupError>(())
    })
    .await?
}

/// `riot-proxy-<YYYY-MM-DD>.db` for the UTC day of `now_ms`.
pub fn backup_name(now_ms: i64) -> String {
    let day = jiff::Timestamp::from_millisecond(now_ms)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "unknown".into());
    format!("{PREFIX}{day}{SUFFIX}")
}

/// The daily backups in `dir`, newest first (the date sorts as text).
pub async fn backups(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut found = vec![];
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let date = name.strip_prefix(PREFIX).and_then(|n| n.strip_suffix(SUFFIX));
        // Only our own names: a file someone put beside them is never deleted.
        if date.is_some_and(|d| d.len() == 10 && d.bytes().all(|b| b.is_ascii_digit() || b == b'-')) {
            found.push(entry.path());
        }
    }
    found.sort();
    found.reverse();
    Ok(found)
}

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The backup written, if one was (none when today's already exists, when
    /// `BACKUP_KEEP` is 0, or when it was skipped or failed).
    #[schema(value_type = Option<String>)]
    pub backup: Option<PathBuf>,
    /// Why no backup was attempted today: `BACKUP_KEEP=0`, or the disk's
    /// free space against what the copy needs.
    pub backup_skipped: Option<String>,
    /// The error of a copy that was attempted and failed; its partial file is
    /// removed and the run goes on.
    pub backup_failed: Option<String>,
    pub backups_deleted: usize,
    pub jobs_deleted: usize,
    pub history_deleted: usize,
    pub cache_swept: usize,
}

/// The last run this process made (ADM-06, `GET /v1/admin/backups`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LastRun {
    /// Unix ms when it started.
    pub at: i64,
    /// How long it took.
    pub ms: u64,
    /// What it did; null when it failed.
    pub report: Option<Report>,
    /// Why it failed; the job is retried like any other.
    pub error: Option<String>,
}

/// Free bytes on a directory's file system; [`crate::db::disk::free_bytes`]
/// unless a test says otherwise.
pub type DiskFree = Arc<dyn Fn(&Path) -> Result<u64, DbError> + Send + Sync>;

pub struct Maintenance {
    pub db: Db,
    /// `$DATA_DIR/backups`.
    pub backup_dir: PathBuf,
    /// `BACKUP_KEEP`: backups kept, newest first; 0 takes none.
    pub keep: usize,
    pub disk_free: DiskFree,
    /// The last run since the process started (ADM-06).
    last: RwLock<Option<LastRun>>,
    /// Where `archive:offload` and `archive:index-snapshot` are queued after
    /// each run, with a bucket: the daily pass that frees bodies past the
    /// window (OBJ-03), and the day's index snapshot (OBJ-04).
    bucket_jobs: Option<crate::jobs::Queue>,
}

impl Maintenance {
    pub fn new(db: Db, backup_dir: PathBuf, keep: usize) -> Self {
        Self {
            db,
            backup_dir,
            keep,
            disk_free: Arc::new(crate::db::disk::free_bytes),
            last: RwLock::new(None),
            bucket_jobs: None,
        }
    }

    /// Queue `archive:offload` (OBJ-03) and `archive:index-snapshot`
    /// (OBJ-04) on `queue` after every run.
    #[must_use]
    pub fn then_bucket_jobs(mut self, queue: crate::jobs::Queue) -> Self {
        self.bucket_jobs = Some(queue);
        self
    }

    /// The last run since the process started; `None` before the first.
    pub fn last(&self) -> Option<LastRun> {
        self.last.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Replace how free disk space is read (tests).
    pub fn with_disk_free(mut self, f: DiskFree) -> Self {
        self.disk_free = f;
        self
    }

    /// One run, remembered for [`Maintenance::last`].
    pub async fn run_once(&self, now_ms: i64) -> Result<Report, JobError> {
        let started = std::time::Instant::now();
        let result = self.run_steps(now_ms).await;
        let run = LastRun {
            at: now_ms,
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            report: result.as_ref().ok().cloned(),
            error: result.as_ref().err().map(ToString::to_string),
        };
        *self.last.write().unwrap_or_else(PoisonError::into_inner) = Some(run);
        // Whatever the day's steps did: freeing bodies and the snapshot do
        // not depend on them.
        if let Some(queue) = &self.bucket_jobs {
            if let Err(e) = crate::jobs::offload::enqueue(queue).await {
                tracing::warn!(error = %e, "could not queue the offload after maintenance");
            }
            if let Err(e) = crate::jobs::index_snapshot::enqueue(queue).await {
                tracing::warn!(error = %e, "could not queue the index snapshot after maintenance");
            }
        }
        result
    }

    async fn run_steps(&self, now_ms: i64) -> Result<Report, JobError> {
        let fail = |e: &dyn std::fmt::Display| JobError::Retry(e.to_string());
        let mut report = Report::default();

        if self.keep == 0 {
            report.backup_skipped = Some("BACKUP_KEEP=0".into());
        } else {
            self.backup(now_ms, &mut report).await;
            for old in backups(&self.backup_dir)
                .await
                .map_err(|e| fail(&e))?
                .into_iter()
                .skip(self.keep)
            {
                tokio::fs::remove_file(&old).await.map_err(|e| fail(&e))?;
                report.backups_deleted += 1;
            }
        }

        let (jobs, history) = self
            .db
            .write(move |c| {
                let jobs = c.execute(
                    "DELETE FROM jobs WHERE state = 'done' AND finished_at < ?1",
                    [now_ms - JOB_RETENTION_MS],
                )?;
                let history = c.execute(
                    "DELETE FROM metrics_history WHERE at < ?1",
                    [now_ms - HISTORY_RETENTION_MS],
                )?;
                Ok::<_, DbError>((jobs, history))
            })
            .await
            .map_err(|e| fail(&e))?;
        report.jobs_deleted = jobs;
        report.history_deleted = history;
        report.cache_swept = crate::cache::l2::sweep(&self.db).await.map_err(|e| fail(&e))?;
        self.db.optimize().await.map_err(|e| fail(&e))?;

        let (busy, log, moved): (i64, i64, i64) = self
            .db
            .write(|c| {
                Ok::<_, DbError>(c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?)
            })
            .await
            .map_err(|e| fail(&e))?;
        tracing::info!(
            ?report,
            checkpoint_busy = busy,
            wal_frames = log,
            checkpointed = moved,
            "maintenance done"
        );
        Ok(report)
    }
}

impl Maintenance {
    /// Today's backup, unless one exists, the disk is short, or the copy
    /// fails: the last two are logged and reported, never returned, so the
    /// trims, the sweep and the checkpoint still run (ADR-136).
    async fn backup(&self, now_ms: i64, report: &mut Report) {
        let name = backup_name(now_ms);
        let today = self.backup_dir.join(&name);
        if today.exists() {
            return;
        }
        let started = std::time::Instant::now();
        // Written aside and renamed: a copy cut short by a crash is never
        // mistaken for today's backup.
        let partial = self.backup_dir.join(format!(".{name}.partial"));
        let outcome: Result<(), BackupError> = async {
            if partial.exists() {
                tokio::fs::remove_file(&partial).await?;
            }
            tokio::fs::create_dir_all(&self.backup_dir).await?;
            let db_path = self.db.path().to_path_buf();
            let need = {
                let size = crate::db::disk::file_bytes(&db_path)
                    + crate::db::disk::file_bytes(&crate::db::disk::wal_path(&db_path));
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let need = (size as f64 * BACKUP_FREE_FACTOR) as u64;
                need
            };
            let free = (self.disk_free)(&self.backup_dir).map_err(BackupError::Disk)?;
            if free < need {
                report.backup_skipped = Some(format!("{free} bytes free, the copy needs {need}"));
                tracing::warn!(free, need, dir = %self.backup_dir.display(), "backup skipped: not enough disk");
                return Ok(());
            }
            vacuum_into(&db_path, &partial).await?;
            tokio::fs::rename(&partial, &today).await?;
            tracing::info!(path = %today.display(), ms = started.elapsed().as_millis(), "backup written");
            report.backup = Some(today.clone());
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            tracing::warn!(error = %e, path = %today.display(), "backup failed; maintenance goes on");
            report.backup_failed = Some(e.to_string());
            if partial.exists()
                && let Err(e) = tokio::fs::remove_file(&partial).await
            {
                tracing::warn!(error = %e, path = %partial.display(), "partial backup not removed");
            }
        }
    }
}

pub struct MaintenanceHandler(pub Arc<Maintenance>);

impl Handler for MaintenanceHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move { self.0.run_once(Clock::now().unix_ms).await.map(|_| ()) })
    }
}

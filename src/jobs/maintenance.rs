//! `maintenance` (design/06 §Job catalogue, design/07 §Backups & restore):
//! the daily housekeeping, in this order.
//!
//! 1. **Backup** first, so a bad day's other steps can be undone from it:
//!    `VACUUM INTO "$DATA_DIR/backups/riot-proxy-<date>.db"`, a consistent
//!    snapshot taken while the service runs. One per UTC day (a restart the
//!    same day does not take another), the newest 14 kept.
//! 2. **Trim** `done` jobs older than 7 days (design/06) and `metrics_history`
//!    older than 24 hours (1440 points at 60 s, design/04).
//! 3. **Sweep** expired L2 cache rows.
//! 4. **Optimize:** `PRAGMA optimize` refreshes the planner's statistics
//!    where they are missing or stale (ADR-102).
//! 5. **Checkpoint** the WAL with `PRAGMA wal_checkpoint(TRUNCATE)`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Serialize;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{Handler, Job, JobError};

/// Backups kept (design/07).
pub const KEEP_BACKUPS: usize = 14;
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The backup written, if one was (none when today's already exists).
    pub backup: Option<PathBuf>,
    pub backups_deleted: usize,
    pub jobs_deleted: usize,
    pub history_deleted: usize,
    pub cache_swept: usize,
}

pub struct Maintenance {
    pub db: Db,
    /// `$DATA_DIR/backups`.
    pub backup_dir: PathBuf,
}

impl Maintenance {
    pub async fn run_once(&self, now_ms: i64) -> Result<Report, JobError> {
        let fail = |e: &dyn std::fmt::Display| JobError::Retry(e.to_string());
        let mut report = Report::default();

        let name = backup_name(now_ms);
        let today = self.backup_dir.join(&name);
        if !today.exists() {
            let started = std::time::Instant::now();
            // Written aside and renamed: a copy cut short by a crash is never
            // mistaken for today's backup.
            let partial = self.backup_dir.join(format!(".{name}.partial"));
            if partial.exists() {
                tokio::fs::remove_file(&partial).await.map_err(|e| fail(&e))?;
            }
            vacuum_into(self.db.path(), &partial)
                .await
                .map_err(|e| fail(&e))?;
            tokio::fs::rename(&partial, &today).await.map_err(|e| fail(&e))?;
            tracing::info!(path = %today.display(), ms = started.elapsed().as_millis(), "backup written");
            report.backup = Some(today);
        }
        for old in backups(&self.backup_dir)
            .await
            .map_err(|e| fail(&e))?
            .into_iter()
            .skip(KEEP_BACKUPS)
        {
            tokio::fs::remove_file(&old).await.map_err(|e| fail(&e))?;
            report.backups_deleted += 1;
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

pub struct MaintenanceHandler(pub Arc<Maintenance>);

impl Handler for MaintenanceHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move { self.0.run_once(Clock::now().unix_ms).await.map(|_| ()) })
    }
}

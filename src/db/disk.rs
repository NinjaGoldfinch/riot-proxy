//! What the database takes on disk and what the disk has left (DSK-01,
//! ADR-135): the file and WAL sizes, SQLite's page geometry, the free list,
//! and the free space of the file system the data directory is on. Sampled
//! with the metrics history and shown on `GET /v1/admin/stats`, so growth and
//! the space a maintenance step frees are both visible.

use std::path::Path;

use serde::Serialize;

use super::{Db, DbError};

/// One reading. Byte counts are from the file system; pages from SQLite.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// The main database file.
    pub db_bytes: u64,
    /// `riot-proxy.db-wal`; 0 when there is none (checkpointed and truncated).
    pub wal_bytes: u64,
    pub page_size: u64,
    pub page_count: u64,
    /// Pages freed by deletes and not yet reused. They stay in the file until
    /// a `VACUUM` (design/07 §Reclaiming space).
    pub freelist_pages: u64,
    /// Bytes an unprivileged writer can still put on the data directory's file system.
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
}

impl Usage {
    /// What the free list holds, in bytes.
    pub fn freelist_bytes(&self) -> u64 {
        self.freelist_pages.saturating_mul(self.page_size)
    }
}

/// Take a reading: the pragmas on a reader connection, then the files and the
/// file system. A missing WAL reads 0.
pub async fn usage(db: &Db) -> Result<Usage, DbError> {
    let (page_size, page_count, freelist_pages) = db
        .read(|c| {
            let read = |pragma: &str| c.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get::<_, i64>(0));
            Ok::<_, DbError>((
                read("page_size")?.max(0) as u64,
                read("page_count")?.max(0) as u64,
                read("freelist_count")?.max(0) as u64,
            ))
        })
        .await?;
    let path = db.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let (disk_free_bytes, disk_total_bytes) = disk_space(&dir_of(&path))?;
        Ok(Usage {
            db_bytes: file_bytes(&path),
            wal_bytes: file_bytes(&wal_path(&path)),
            page_size,
            page_count,
            freelist_pages,
            disk_free_bytes,
            disk_total_bytes,
        })
    })
    .await?
}

/// Set the gauges from a reading (`docs/design/metrics.md`).
pub fn record(u: &Usage) {
    use crate::metrics::{DATA_DIR_FREE_BYTES, SQLITE_DB_BYTES, SQLITE_FREELIST_BYTES, SQLITE_WAL_BYTES};
    metrics::gauge!(SQLITE_DB_BYTES).set(u.db_bytes as f64);
    metrics::gauge!(SQLITE_WAL_BYTES).set(u.wal_bytes as f64);
    metrics::gauge!(SQLITE_FREELIST_BYTES).set(u.freelist_bytes() as f64);
    metrics::gauge!(DATA_DIR_FREE_BYTES).set(u.disk_free_bytes as f64);
}

/// `riot-proxy.db` → `riot-proxy.db-wal`, SQLite's own naming.
pub fn wal_path(db: &Path) -> std::path::PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push("-wal");
    s.into()
}

fn dir_of(db: &Path) -> std::path::PathBuf {
    match db.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => Path::new(".").to_path_buf(),
    }
}

/// A file's size, or 0 when it does not exist.
pub fn file_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// `(free, total)` bytes of the file system holding `dir`, as an unprivileged
/// writer sees them (`statvfs`'s `f_bavail`, what `df` calls "Avail").
pub fn disk_space(dir: &Path) -> Result<(u64, u64), DbError> {
    use std::os::unix::ffi::OsStrExt;
    let c_dir = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(|_| DbError::Stat {
        path: dir.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path holds a NUL byte"),
    })?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_dir` is a valid NUL-terminated string and `st` is a writable
    // `statvfs` the call fills; nothing is aliased.
    let rc = unsafe { libc::statvfs(c_dir.as_ptr(), &raw mut st) };
    if rc != 0 {
        return Err(DbError::Stat {
            path: dir.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ by platform
    let frsize = st.f_frsize as u64;
    #[allow(clippy::unnecessary_cast)]
    let (avail, blocks) = (st.f_bavail as u64, st.f_blocks as u64);
    Ok((avail.saturating_mul(frsize), blocks.saturating_mul(frsize)))
}

/// Free bytes on `dir`'s file system; what the backup guard asks (DSK-02).
pub fn free_bytes(dir: &Path) -> Result<u64, DbError> {
    disk_space(dir).map(|(free, _)| free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(&dir.path().join("riot-proxy.db"), 1).expect("open");
        (dir, db)
    }

    #[tokio::test]
    async fn a_fresh_database_has_design_04s_page_size_and_no_free_pages() {
        let (dir, db) = open();
        // Until a checkpoint the migrations' pages sit in the WAL, not the file.
        db.write(|c| {
            c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("checkpoint");
        let u = usage(&db).await.expect("usage");
        assert_eq!(u.page_size, 4096);
        assert_eq!(u.freelist_pages, 0);
        assert!(u.page_count > 0, "{u:?}");
        assert_eq!(u.db_bytes, u.page_count * u.page_size, "{u:?}");
        assert_eq!(u.wal_bytes, 0, "{u:?}");
        assert!(u.disk_total_bytes >= u.disk_free_bytes, "{u:?}");
        assert!(u.disk_free_bytes > 0, "{u:?}");
        assert_eq!(wal_path(db.path()), dir.path().join("riot-proxy.db-wal"));
    }

    #[tokio::test]
    async fn deleted_rows_show_up_as_free_pages_and_the_wal_empties_on_checkpoint() {
        let (_dir, db) = open();
        db.write(|c| {
            c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, body BLOB)")?;
            let mut stmt = c.prepare("INSERT INTO t (body) VALUES (zeroblob(4000))")?;
            for _ in 0..200 {
                stmt.execute([])?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("fill");
        let filled = usage(&db).await.expect("usage");
        assert!(filled.wal_bytes > 0, "{filled:?}");
        assert_eq!(filled.freelist_pages, 0, "{filled:?}");

        db.write(|c| {
            c.execute("DELETE FROM t", [])?;
            c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("delete");
        let emptied = usage(&db).await.expect("usage");
        assert!(emptied.freelist_pages >= 200, "{emptied:?}");
        assert_eq!(emptied.freelist_bytes(), emptied.freelist_pages * 4096);
        assert_eq!(emptied.wal_bytes, 0, "{emptied:?}");
        // The file does not shrink: the pages wait on the free list.
        assert_eq!(emptied.page_count, filled.page_count, "{emptied:?} vs {filled:?}");
    }

    #[test]
    fn a_missing_file_reads_zero_and_a_missing_directory_is_an_error() {
        assert_eq!(file_bytes(Path::new("/nonexistent/riot-proxy.db")), 0);
        assert!(matches!(
            disk_space(Path::new("/nonexistent/dir")),
            Err(DbError::Stat { .. })
        ));
        assert!(free_bytes(Path::new(".")).expect("cwd") > 0);
    }
}

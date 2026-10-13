//! `maintenance` and `riot-proxy backup` (plan P7-05): the daily backup is a
//! database that opens and has the rows, one per day, `BACKUP_KEEP` kept;
//! done jobs and old metrics history are trimmed, expired L2 rows swept, and
//! the WAL truncated. A backup the disk has no room for, or one that fails,
//! never stops the rest (DSK-02).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use riot_proxy::db::{Db, DbError};
use std::sync::Arc;

use riot_proxy::jobs::maintenance::{KEEP_BACKUPS, Maintenance, backup_name, backups, vacuum_into};

const DAY_MS: i64 = 86_400_000;
/// 2026-10-03T12:00:00Z.
const NOW: i64 = 1_791_028_800_000;

fn open(dir: &Path) -> Db {
    Db::open(&dir.join("riot-proxy.db"), 2).unwrap()
}

async fn seed(db: &Db) {
    db.write(|c| {
        c.execute_batch(&format!(
            "INSERT INTO consumers (id, name, key_sha256, scopes, created_at) VALUES ('c1', 'keep me', x'01', '[]', 0);
             INSERT INTO jobs (id, kind, priority, payload, state, run_after, finished_at) VALUES
               ('old-done', 'poll:live', 1, '{{}}', 'done', 0, {old}),
               ('new-done', 'poll:live', 1, '{{}}', 'done', 0, {recent}),
               ('old-failed', 'poll:live', 1, '{{}}', 'failed', 0, {old}),
               ('pending', 'poll:live', 1, '{{}}', 'pending', 0, NULL);
             INSERT INTO metrics_history (at, point) VALUES ({old}, '{{}}'), ({recent}, '{{}}');
             INSERT INTO cache (key, status, body, content_at, soft_expires, hard_expires)
               VALUES ('gone', 200, x'00', 0, 0, 1), ('kept', 200, x'00', 0, 0, {far});",
            old = NOW - 8 * DAY_MS,
            recent = NOW - 3_600_000,
            far = i64::MAX / 2,
        ))?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

fn ids(db_path: &Path, sql: &str) -> Vec<String> {
    let c = rusqlite::Connection::open(db_path).unwrap();
    let mut stmt = c.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
}

#[tokio::test]
async fn the_daily_run_backs_up_first_then_trims_sweeps_and_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let backup_dir = dir.path().join("backups");
    // Sixteen older backups, and a file that only looks like one.
    std::fs::create_dir_all(&backup_dir).unwrap();
    for d in 1..=16 {
        std::fs::write(backup_dir.join(backup_name(NOW - d * DAY_MS)), b"old").unwrap();
    }
    std::fs::write(backup_dir.join("riot-proxy-notes.db"), b"mine").unwrap();
    let m = Maintenance::new(db.clone(), backup_dir.clone(), KEEP_BACKUPS);

    let report = m.run_once(NOW).await.unwrap();
    let today = backup_dir.join("riot-proxy-2026-10-03.db");
    assert_eq!(report.backup.as_deref(), Some(today.as_path()));
    assert_eq!(
        (report.backup_skipped.clone(), report.backup_failed.clone()),
        (None, None)
    );
    // The backup opens and has the rows, as they were before the trims.
    assert_eq!(ids(&today, "SELECT name FROM consumers"), ["keep me"]);
    assert_eq!(ids(&today, "SELECT id FROM jobs ORDER BY id").len(), 4);
    // Fourteen kept, newest first; the stranger is untouched.
    let kept = backups(&backup_dir).await.unwrap();
    assert_eq!((kept.len(), report.backups_deleted), (KEEP_BACKUPS, 3));
    assert_eq!(kept[0], today);
    assert!(backup_dir.join("riot-proxy-notes.db").exists());

    // Trimmed: done jobs older than a week, history older than a day, expired cache.
    let live = dir.path().join("riot-proxy.db");
    assert_eq!(
        ids(&live, "SELECT id FROM jobs ORDER BY id"),
        ["new-done", "old-failed", "pending"]
    );
    assert_eq!(
        (report.jobs_deleted, report.history_deleted, report.cache_swept),
        (1, 1, 1)
    );
    assert_eq!(ids(&live, "SELECT key FROM cache"), ["kept"]);
    // The WAL is truncated.
    let wal = dir.path().join("riot-proxy.db-wal");
    assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);

    // Again the same day: no second backup, nothing else to do.
    let again = m.run_once(NOW + 3_600_000).await.unwrap();
    assert_eq!((again.backup, again.jobs_deleted), (None, 0));
    // The next day: a new one, and the oldest goes.
    let next = m.run_once(NOW + DAY_MS).await.unwrap();
    assert!(next.backup.is_some());
    assert_eq!(
        (backups(&backup_dir).await.unwrap().len(), next.backups_deleted),
        (KEEP_BACKUPS, 1)
    );
}

#[tokio::test]
async fn a_backup_cut_short_is_not_taken_for_todays() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    // A crash mid-copy leaves the partial file only.
    std::fs::write(backup_dir.join(".riot-proxy-2026-10-03.db.partial"), b"half").unwrap();
    let m = Maintenance::new(db, backup_dir.clone(), KEEP_BACKUPS);
    let report = m.run_once(NOW).await.unwrap();
    let today = report.backup.unwrap();
    assert_eq!(ids(&today, "SELECT name FROM consumers"), ["keep me"]);
    assert!(!backup_dir.join(".riot-proxy-2026-10-03.db.partial").exists());
}

/// What a run did besides the backup: the trims, the sweep, the checkpoint.
async fn trimmed(dir: &Path, report: &riot_proxy::jobs::maintenance::Report) {
    let live = dir.join("riot-proxy.db");
    assert_eq!(
        ids(&live, "SELECT id FROM jobs ORDER BY id"),
        ["new-done", "old-failed", "pending"]
    );
    assert_eq!(
        (report.jobs_deleted, report.history_deleted, report.cache_swept),
        (1, 1, 1)
    );
    let wal = dir.join("riot-proxy.db-wal");
    assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);
}

#[tokio::test]
async fn keep_zero_takes_no_backup_prunes_none_and_still_trims() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    for d in 1..=16 {
        std::fs::write(backup_dir.join(backup_name(NOW - d * DAY_MS)), b"old").unwrap();
    }
    let m = Maintenance::new(db, backup_dir.clone(), 0);
    assert_eq!(m.last(), None, "no run yet");
    let report = m.run_once(NOW).await.unwrap();
    assert_eq!(report.backup, None);
    assert_eq!(report.backup_skipped.as_deref(), Some("BACKUP_KEEP=0"));
    // ADM-06: the run is remembered for GET /v1/admin/backups.
    let last = m.last().unwrap();
    assert_eq!(
        (last.at, last.report.as_ref(), last.error.as_ref()),
        (NOW, Some(&report), None)
    );
    assert_eq!(report.backups_deleted, 0);
    assert_eq!(
        backups(&backup_dir).await.unwrap().len(),
        16,
        "nothing pruned either"
    );
    assert!(!backup_dir.join("riot-proxy-2026-10-03.db").exists());
    trimmed(dir.path(), &report).await;
}

#[tokio::test]
async fn no_room_on_the_disk_skips_the_backup_and_the_rest_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let backup_dir = dir.path().join("backups");
    // 1.2 x (file + WAL) is what it takes; one byte less is not enough.
    let size = std::fs::metadata(dir.path().join("riot-proxy.db")).unwrap().len()
        + std::fs::metadata(dir.path().join("riot-proxy.db-wal")).map_or(0, |m| m.len());
    let short = (size as f64 * 1.2) as u64 - 1;
    let m = Maintenance::new(db.clone(), backup_dir.clone(), KEEP_BACKUPS)
        .with_disk_free(Arc::new(move |_| Ok(short)));
    let report = m.run_once(NOW).await.unwrap();
    assert_eq!(report.backup, None);
    assert!(
        report
            .backup_skipped
            .as_deref()
            .is_some_and(|s| s.contains("bytes free")),
        "{report:?}"
    );
    assert_eq!(
        m.last().unwrap().report.as_ref(),
        Some(&report),
        "the skip is remembered"
    );
    assert_eq!(report.backup_failed, None);
    assert!(!backup_dir.join("riot-proxy-2026-10-03.db").exists());
    assert!(!backup_dir.join(".riot-proxy-2026-10-03.db.partial").exists());
    trimmed(dir.path(), &report).await;

    // With room, the same run writes it.
    let m = Maintenance::new(db, backup_dir.clone(), KEEP_BACKUPS).with_disk_free(Arc::new(|_| Ok(u64::MAX)));
    let report = m.run_once(NOW).await.unwrap();
    assert!(report.backup.is_some(), "{report:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_copy_that_fails_leaves_no_partial_and_the_rest_runs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    // VACUUM INTO cannot create its file in a directory it may not write to.
    std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let m = Maintenance::new(db, backup_dir.clone(), KEEP_BACKUPS).with_disk_free(Arc::new(|_| Ok(u64::MAX)));
    let report = m.run_once(NOW).await.unwrap();
    std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.backup, None);
    assert!(report.backup_failed.is_some(), "{report:?}");
    let last = m.last().unwrap();
    assert_eq!(
        (last.report.as_ref(), last.error.as_ref()),
        (Some(&report), None),
        "a failed copy is not a failed run"
    );
    assert_eq!(report.backup_skipped, None);
    assert!(
        std::fs::read_dir(&backup_dir).unwrap().next().is_none(),
        "no partial left"
    );
    trimmed(dir.path(), &report).await;
}

#[tokio::test]
async fn vacuum_into_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let out = dir.path().join("out.db");
    vacuum_into(db.path(), &out).await.unwrap();
    assert!(
        vacuum_into(db.path(), &out)
            .await
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
}

#[tokio::test]
async fn the_daily_run_refreshes_planner_statistics() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(&db).await;
    let m = Maintenance::new(db.clone(), dir.path().join("backups"), KEEP_BACKUPS);
    m.run_once(NOW).await.unwrap();
    // ADR-102: `jobs` gained rows since the open, so it has statistics now.
    let analysed = ids(
        &dir.path().join("riot-proxy.db"),
        "SELECT DISTINCT tbl FROM sqlite_stat1 WHERE tbl = 'jobs'",
    );
    assert_eq!(analysed, ["jobs"]);
}

/// ADM-06: a run that fails is remembered with its error and no report.
#[tokio::test]
async fn a_failed_run_is_remembered_with_its_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    // The backup directory is a file: the copy fails (logged), then listing
    // the backups to prune fails the run.
    let backup_dir = dir.path().join("backups");
    std::fs::write(&backup_dir, b"not a directory").unwrap();
    let m = Maintenance::new(db, backup_dir, KEEP_BACKUPS).with_disk_free(Arc::new(|_| Ok(u64::MAX)));
    let err = m.run_once(NOW).await.unwrap_err();
    let last = m.last().unwrap();
    assert_eq!(last.at, NOW);
    assert_eq!(last.report, None);
    assert_eq!(last.error, Some(err.to_string()));
}

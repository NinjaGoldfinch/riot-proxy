//! `maintenance` and `riot-proxy backup` (plan P7-05): the daily backup is a
//! database that opens and has the rows, one per day, 14 kept; done jobs and
//! old metrics history are trimmed, expired L2 rows swept, and the WAL
//! truncated.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use riot_proxy::db::{Db, DbError};
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
    let m = Maintenance {
        db: db.clone(),
        backup_dir: backup_dir.clone(),
    };

    let report = m.run_once(NOW).await.unwrap();
    let today = backup_dir.join("riot-proxy-2026-10-03.db");
    assert_eq!(report.backup.as_deref(), Some(today.as_path()));
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
    let m = Maintenance {
        db,
        backup_dir: backup_dir.clone(),
    };
    let report = m.run_once(NOW).await.unwrap();
    let today = report.backup.unwrap();
    assert_eq!(ids(&today, "SELECT name FROM consumers"), ["keep me"]);
    assert!(!backup_dir.join(".riot-proxy-2026-10-03.db.partial").exists());
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
    let m = Maintenance {
        db: db.clone(),
        backup_dir: dir.path().join("backups"),
    };
    m.run_once(NOW).await.unwrap();
    // ADR-102: `jobs` gained rows since the open, so it has statistics now.
    let analysed = ids(
        &dir.path().join("riot-proxy.db"),
        "SELECT DISTINCT tbl FROM sqlite_stat1 WHERE tbl = 'jobs'",
    );
    assert_eq!(analysed, ["jobs"]);
}

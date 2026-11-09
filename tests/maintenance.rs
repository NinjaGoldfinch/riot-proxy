//! `maintenance` and `riot-proxy backup` (plan P7-05): the daily backup is a
//! database that opens and has the rows, one per day, `BACKUP_KEEP` kept;
//! done jobs and old metrics history are trimmed, expired L2 rows swept, and
//! the WAL truncated. A backup the disk has no room for, or one that fails,
//! never stops the rest (DSK-02).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

/// AVL-05 (ADR-215): `done` jobs are trimmed a kind at a time and at most
/// 5 000 rows a write, as `DELETE /v1/admin/jobs` deletes, and only those
/// past the retention: newer `done` rows and rows in any other state stay.
#[tokio::test]
async fn the_trim_deletes_old_done_jobs_in_chunks_of_five_thousand() {
    use riot_proxy::jobs::maintenance::{JOB_RETENTION_MS, Trimmed, trim_done_jobs};
    use riot_proxy::jobs::scheduler::PURGE_CHUNK;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let cutoff = NOW - JOB_RETENTION_MS;
    db.write(move |c| {
        let tx = c.transaction()?;
        {
            let mut add = tx.prepare(
                "INSERT INTO jobs (id, kind, priority, payload, state, run_after, finished_at)
                 VALUES (?1, ?2, 1, '{}', ?3, 0, ?4)",
            )?;
            // 12 001 old `archive:match` rows: three writes of it.
            for i in 0..12_001 {
                add.execute(rusqlite::params![
                    format!("a{i}"),
                    "archive:match",
                    "done",
                    cutoff - 1 - i
                ])?;
            }
            // Three old `poll:live` rows: one write.
            for i in 0..3 {
                add.execute(rusqlite::params![
                    format!("p{i}"),
                    "poll:live",
                    "done",
                    cutoff - 1
                ])?;
            }
            // Kept: done inside the retention, and old rows not `done`.
            add.execute(rusqlite::params!["recent", "archive:match", "done", cutoff + 1])?;
            add.execute(rusqlite::params!["edge", "poll:live", "done", cutoff])?;
            add.execute(rusqlite::params!["failed", "archive:match", "failed", cutoff - 1])?;
            add.execute(rusqlite::params!["pending", "poll:live", "pending", None::<i64>])?;
        }
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();

    // No write deletes more than a chunk.
    let biggest = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&biggest);
    db.write(move |c| {
        let rows = Arc::new(AtomicUsize::new(0));
        let (count, reset) = (Arc::clone(&rows), Arc::clone(&rows));
        c.update_hook(Some(
            move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
                if table == "jobs" {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            },
        ))?;
        c.commit_hook(Some(move || {
            seen.fetch_max(reset.swap(0, Ordering::SeqCst), Ordering::SeqCst);
            false
        }))?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();

    let done = trim_done_jobs(&db, cutoff).await.unwrap();
    db.write(|c| {
        c.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)?;
        c.commit_hook(None::<fn() -> bool>)?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    assert_eq!(
        done,
        Trimmed {
            deleted: 12_004,
            writes: 4
        }
    );
    assert_eq!(biggest.load(Ordering::SeqCst), PURGE_CHUNK);
    let left = ids(
        &dir.path().join("riot-proxy.db"),
        "SELECT id FROM jobs ORDER BY id",
    );
    assert_eq!(left, ["edge", "failed", "pending", "recent"]);

    // The daily run trims through it.
    db.write(move |c| {
        c.execute(
            "INSERT INTO jobs (id, kind, priority, payload, state, run_after, finished_at)
             VALUES ('old', 'ddragon:sync', 1, '{}', 'done', 0, ?1)",
            [cutoff - 1],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let m = Maintenance::new(db.clone(), dir.path().join("backups"), 0);
    assert_eq!(m.run_once(NOW).await.unwrap().jobs_deleted, 1);
    assert_eq!(trim_done_jobs(&db, cutoff).await.unwrap().deleted, 0);
}

/// The trim's reads and writes seek into `jobs_state_kind`; none scans
/// `jobs`.
#[tokio::test]
async fn the_trim_seeks_its_index() {
    use riot_proxy::jobs::maintenance::{NEXT_DONE_KIND_SQL, TRIM_CHUNK_SQL};
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    for sql in [NEXT_DONE_KIND_SQL, TRIM_CHUNK_SQL] {
        let plan: Vec<String> = db
            .read(move |c| {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap();
        assert!(
            plan.iter()
                .any(|l| l.contains("jobs_state_kind (state=? AND kind")),
            "{sql}: {plan:?}"
        );
        assert!(
            !plan.iter().any(|l| l.starts_with("SCAN jobs")),
            "{sql}: {plan:?}"
        );
    }
}

/// Rows of `kind` the boot queued: every one but the seeded `seed` row.
fn queued(db_path: &Path, kind: &str) -> usize {
    ids(
        db_path,
        &format!("SELECT id FROM jobs WHERE kind = '{kind}' AND id <> 'seed'"),
    )
    .len()
}

/// Boot `serve` on `dir` until its ticks have run (the `aggregate:delta`
/// tick fires at once on every boot), then return how many `maintenance`
/// rows the boot queued.
async fn maintenance_queued_by_a_boot(dir: &Path) -> usize {
    // `serve_with` listens for this process's signals; another test's SIGTERM
    // must not stop this server (AVL-04).
    let _signals = crate::common::SIGNALS.lock().await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = crate::common::config(&[
        ("DATA_DIR", dir.to_str().unwrap()),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("LOG_LEVEL", "warn"),
    ]);
    let options = riot_proxy::cli::serve::ServeOptions {
        // Nothing listens there: whatever a job asks for fails and retries.
        riot_base_url: Some("http://127.0.0.1:9".into()),
        skip_tracing_init: true,
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock("http://127.0.0.1:9")),
        ..riot_proxy::cli::serve::ServeOptions::default()
    };
    let task = tokio::spawn(riot_proxy::cli::serve::serve_with(config, options));
    let path = dir.join("riot-proxy.db");
    let mut ticked = false;
    for _ in 0..400 {
        if queued(&path, "aggregate:delta") > 0 {
            ticked = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(ticked, "the ticks never ran");
    // Each loop reads its last run in a moment, so a maintenance tick due at
    // boot has fired by now or does within this wait.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let n = queued(&path, "maintenance");
    task.abort();
    let _ = task.await;
    n
}

/// AVL-07 (ADR-214): a first boot queues `maintenance` at once, as before;
/// a restart a few hours after the last run queues none, so the backup, the
/// trim and the checkpoint wait for their day.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_boot_queues_maintenance_only_when_it_is_due() {
    let fresh = tempfile::tempdir().unwrap();
    drop(open(fresh.path()));
    assert_eq!(
        maintenance_queued_by_a_boot(fresh.path()).await,
        1,
        "a fresh database: at once"
    );

    let restarted = tempfile::tempdir().unwrap();
    let db = open(restarted.path());
    let finished = riot_proxy::clock::Clock::now().unix_ms - 3 * 3_600_000;
    db.write(move |c| {
        c.execute(
            "INSERT INTO jobs (id, kind, dedupe_key, priority, payload, state, run_after, finished_at)
             VALUES ('seed', 'maintenance', 'maintenance', 30000, '{}', 'done', 0, ?1)",
            [finished],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    drop(db);
    assert_eq!(
        maintenance_queued_by_a_boot(restarted.path()).await,
        0,
        "ran 3 h ago: not before 21 h from now"
    );
    assert!(
        !restarted.path().join("backups").exists(),
        "and no backup was written"
    );
}

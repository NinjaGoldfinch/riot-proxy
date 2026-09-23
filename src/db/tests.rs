use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::{OptionalExtension, params};

use super::*;

fn open_temp(readers: usize) -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("nested/riot-proxy.db"), readers).expect("open");
    (dir, db)
}

fn pragma(conn: &Connection, name: &str) -> rusqlite::Result<String> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| {
        r.get::<_, rusqlite::types::Value>(0).map(|v| match v {
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Text(t) => t,
            other => format!("{other:?}"),
        })
    })
}

#[tokio::test]
async fn writer_has_every_design_04_pragma() {
    let (_dir, db) = open_temp(1);
    let got: Vec<(String, String)> = db
        .write(|c| {
            [
                "journal_mode",
                "synchronous",
                "busy_timeout",
                "foreign_keys",
                "cache_size",
                "mmap_size",
                "temp_store",
                "wal_autocheckpoint",
            ]
            .iter()
            .map(|p| Ok((p.to_string(), pragma(c, p)?)))
            .collect::<Result<_, DbError>>()
        })
        .await
        .expect("pragmas");
    let expected = [
        ("journal_mode", "wal"),
        ("synchronous", "1"), // NORMAL
        ("busy_timeout", "5000"),
        ("foreign_keys", "1"),
        ("cache_size", "-65536"),
        ("mmap_size", "268435456"),
        ("temp_store", "2"), // MEMORY
        ("wal_autocheckpoint", "1000"),
    ];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn readers_have_connection_pragmas_and_see_wal() {
    let (_dir, db) = open_temp(2);
    let got = db
        .read(|c| {
            Ok::<_, DbError>((
                pragma(c, "journal_mode")?,
                pragma(c, "busy_timeout")?,
                pragma(c, "cache_size")?,
                pragma(c, "mmap_size")?,
                pragma(c, "temp_store")?,
            ))
        })
        .await
        .expect("pragmas");
    assert_eq!(
        got,
        (
            "wal".into(),
            "5000".into(),
            "-65536".into(),
            "268435456".into(),
            "2".into()
        )
    );
}

#[tokio::test]
async fn init_creates_exactly_the_p0_tables() {
    let (_dir, db) = open_temp(1);
    let tables: Vec<String> = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .expect("tables");
    assert_eq!(
        tables,
        [
            "cache",
            "consumers",
            "jobs",
            "limiter_state",
            "metrics_history",
            "refinery_schema_history"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hundred_concurrent_writes_all_succeed() {
    let (_dir, db) = open_temp(2);
    let tasks: Vec<_> = (0..100)
        .map(|i| {
            let db = db.clone();
            tokio::spawn(async move {
                db.write(move |c| {
                    c.execute(
                        "INSERT INTO metrics_history (at, point) VALUES (?1, ?2)",
                        params![i, format!("{{\"i\":{i}}}")],
                    )
                    .map_err(DbError::from)
                })
                .await
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.expect("join").expect("no SQLITE_BUSY"), 1);
    }
    let n: i64 = db
        .read(|c| Ok::<_, DbError>(c.query_row("SELECT COUNT(*) FROM metrics_history", [], |r| r.get(0))?))
        .await
        .expect("count");
    assert_eq!(n, 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_never_overlap() {
    let (_dir, db) = open_temp(1);
    let in_flight = Arc::new(AtomicBool::new(false));
    let overlaps = Arc::new(AtomicUsize::new(0));
    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let (db, in_flight, overlaps) = (db.clone(), in_flight.clone(), overlaps.clone());
            tokio::spawn(async move {
                db.write(move |_| {
                    if in_flight.swap(true, Ordering::SeqCst) {
                        overlaps.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    in_flight.store(false, Ordering::SeqCst);
                    Ok::<_, DbError>(())
                })
                .await
            })
        })
        .collect();
    for t in tasks {
        t.await.expect("join").expect("write");
    }
    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn migrations_apply_once_across_reopens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    let history = |db: Db| async move {
        db.read(|c| {
            Ok::<i64, DbError>(c.query_row("SELECT COUNT(*) FROM refinery_schema_history", [], |r| r.get(0))?)
        })
        .await
    };

    let first = Db::open(&path, 1).expect("first open");
    first
        .write(|c| {
            c.execute(
                "INSERT INTO consumers (id, name, key_sha256, scopes, created_at) VALUES ('c1', 'n', x'00', '[]', 0)",
                [],
            )
            .map_err(DbError::from)
        })
        .await
        .expect("insert");
    assert_eq!(history(first.clone()).await.expect("history"), 1);
    drop(first);

    let second = Db::open(&path, 1).expect("second open");
    assert_eq!(history(second.clone()).await.expect("history"), 1, "no re-run");
    let name: Option<String> = second
        .read(|c| {
            Ok::<_, DbError>(
                c.query_row("SELECT name FROM consumers WHERE id = 'c1'", [], |r| r.get(0))
                    .optional()?,
            )
        })
        .await
        .expect("row survives");
    assert_eq!(name.as_deref(), Some("n"));

    // And directly: running the embedded runner again is a no-op.
    let mut conn = Connection::open(&path).expect("raw open");
    let report = migrate(&mut conn).expect("migrate again");
    assert!(report.applied_migrations().is_empty());
}

#[tokio::test]
async fn readers_are_read_only() {
    let (_dir, db) = open_temp(1);
    let err = db
        .read(|c| {
            c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
                .map_err(DbError::from)
        })
        .await
        .expect_err("read-only");
    assert!(err.to_string().contains("readonly"), "{err}");
}

#[tokio::test]
async fn readers_see_committed_writes() {
    let (_dir, db) = open_temp(3);
    db.write(|c| {
        c.execute("INSERT INTO metrics_history (at, point) VALUES (42, '{}')", [])
            .map_err(DbError::from)
    })
    .await
    .expect("write");
    for _ in 0..3 {
        let at: i64 = db
            .read(|c| Ok::<_, DbError>(c.query_row("SELECT at FROM metrics_history", [], |r| r.get(0))?))
            .await
            .expect("read");
        assert_eq!(at, 42);
    }
}

#[tokio::test]
async fn a_panicking_write_does_not_kill_the_writer() {
    let (_dir, db) = open_temp(1);
    let err = db
        .write(|_| -> Result<(), DbError> { panic!("boom") })
        .await
        .expect_err("panic surfaces as an error");
    assert!(matches!(err, DbError::WriterGone), "{err:?}");

    let n = db
        .write(|c| {
            c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
                .map_err(DbError::from)
        })
        .await
        .expect("writer still alive");
    assert_eq!(n, 1);
}

#[tokio::test]
async fn a_panicking_transaction_is_rolled_back() {
    let (_dir, db) = open_temp(1);
    let _ = db
        .write(|c| -> Result<(), DbError> {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO metrics_history (at, point) VALUES (7, '{}')", [])?;
            panic!("mid-transaction");
        })
        .await;
    let n: i64 = db
        .read(|c| Ok::<_, DbError>(c.query_row("SELECT COUNT(*) FROM metrics_history", [], |r| r.get(0))?))
        .await
        .expect("count");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_panicking_read_returns_its_connection() {
    let (_dir, db) = open_temp(1);
    let err = db
        .read(|_| -> Result<(), DbError> { panic!("boom") })
        .await
        .expect_err("panic");
    assert!(matches!(err, DbError::ReaderPanicked), "{err:?}");
    // With a pool of one, this would hang or fail if the connection had been lost.
    db.read(|c| Ok::<_, DbError>(pragma(c, "journal_mode")?))
        .await
        .expect("pool intact");
}

#[tokio::test]
async fn open_async_and_custom_error_types() {
    #[derive(Debug)]
    enum AppError {
        Db,
        NotFound,
    }
    impl From<DbError> for AppError {
        fn from(_: DbError) -> Self {
            Self::Db
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open_async(dir.path().join("a.db"), 1).await.expect("open");
    let res: Result<(), AppError> = db.read(|_| Err(AppError::NotFound)).await;
    assert!(matches!(res, Err(AppError::NotFound)));
    assert!(Db::default_readers() >= 1);
    assert_eq!(db.path(), dir.path().join("a.db"));
}

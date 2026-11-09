//! A real SIGTERM lets an in-flight request finish before the server exits,
//! and `serve` stops inside its budget while a job holds the writer (AVL-04).
//! The tests that send this process SIGTERM take [`common::SIGNALS`] first,
//! as does every test that runs `serve_with`: each one's server would
//! otherwise also stop on the other's signal under `cargo test`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use futures_util::future::BoxFuture;
use riot_proxy::app;
use riot_proxy::cli::serve::{ExtraHandler, ServeOptions, serve_with};
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::{Handler, Job, JobError, NewJob, Queue, Registry, Scheduler};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::MockServer;

fn sigterm_self() {
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

async fn raw_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_drains_in_flight_requests() {
    let _signals = common::SIGNALS.lock().await;
    let router = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            "done"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = app::shutdown_signal().expect("signal handlers");
    let server = tokio::spawn(app::serve(listener, router, shutdown));

    let in_flight = tokio::spawn(raw_get(addr, "/slow"));
    tokio::time::sleep(Duration::from_millis(100)).await; // request is now inside the handler

    let started = Instant::now();
    sigterm_self();

    let response = in_flight.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server exits after draining")
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < app::SHUTDOWN_GRACE, "drained, not timed out");

    // No longer accepting.
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_is_bounded_by_the_grace_period() {
    let router = Router::new().route(
        "/stuck",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            "never"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(app::serve_with_grace(
        listener,
        router,
        async {
            let _ = rx.await;
        },
        Duration::from_millis(300),
    ));
    let _stuck = tokio::spawn(raw_get(addr, "/stuck"));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("exits")
        .unwrap()
        .unwrap();
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(2),
        "{took:?}"
    );
}

// ── AVL-04: shutdown inside 90 s (ADR-211) ──────────────────────────────────

const HOLD: &str = "test:hold-writer";

/// A job whose one write would hold the writer for 60 s: a transaction that
/// creates a table, then counts in chunks until a minute has passed.
struct HoldTheWriter {
    db: Db,
    writing: Arc<AtomicBool>,
}

impl Handler for HoldTheWriter {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        let writing = Arc::clone(&self.writing);
        Box::pin(async move {
            self.db
                .write(move |c| {
                    let tx = c.transaction()?;
                    tx.execute_batch("CREATE TABLE avl04_marker (x); INSERT INTO avl04_marker VALUES (1);")?;
                    writing.store(true, Ordering::SeqCst);
                    let began = Instant::now();
                    while began.elapsed() < Duration::from_secs(60) {
                        tx.query_row(
                            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 200000)
                             SELECT count(*) FROM r",
                            [],
                            |r| r.get::<_, i64>(0),
                        )?;
                    }
                    tx.commit()?;
                    Ok::<_, DbError>(())
                })
                .await
                .map_err(|e| JobError::Retry(e.to_string()))
        })
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn hold_state(db: &Db) -> String {
    db.read(|c| {
        c.query_row("SELECT state FROM jobs WHERE kind = ?1", [HOLD], |r| r.get(0))
            .map_err(DbError::from)
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_while_a_job_holds_the_writer_exits_inside_the_budget() {
    let _signals = common::SIGNALS.lock().await;
    let riot = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("riot-proxy.db");
    // The job, first in line, and a limiter checkpoint from long ago.
    {
        let db = Db::open(&path, 1).unwrap();
        Queue::new(db.clone())
            .enqueue(NewJob::new(HOLD, 0, json!({})))
            .await
            .unwrap();
        db.write(|c| {
            c.execute(
                r#"INSERT INTO limiter_state (scope, windows, frozen_until, updated_at)
                   VALUES ('app:test', '{"windows":[{"limit":20,"seconds":1,"stamps":[]}]}', NULL, 1)"#,
                [],
            )
            .map_err(DbError::from)
        })
        .await
        .unwrap();
    }

    let port = free_port();
    let config = common::config(&[
        ("DATA_DIR", data.path().to_str().unwrap()),
        ("DATABASE_URL", &format!("sqlite://{}", path.display())),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("AUTH_DISABLED", "true"),
        ("LOG_LEVEL", "warn"),
    ]);
    let writing = Arc::new(AtomicBool::new(false));
    let make = {
        let writing = Arc::clone(&writing);
        move |db| -> Arc<dyn Handler> {
            Arc::new(HoldTheWriter {
                db,
                writing: Arc::clone(&writing),
            })
        }
    };
    let options = ServeOptions {
        riot_base_url: Some(riot.uri()),
        skip_tracing_init: true,
        // Unmocked: the sync fails and retries, but never leaves the machine.
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock(&riot.uri())),
        extra_handler: Some(ExtraHandler {
            kind: HOLD,
            make: Arc::new(make),
        }),
        ..ServeOptions::default()
    };
    let serve = tokio::spawn(serve_with(config, options));
    let waiting = Instant::now();
    while !writing.load(Ordering::SeqCst) {
        assert!(
            waiting.elapsed() < Duration::from_secs(30),
            "the job never started its write"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let signalled = riot_proxy::clock::Clock::now().unix_ms;
    let started = Instant::now();
    sigterm_self();
    tokio::time::timeout(Duration::from_secs(90), serve)
        .await
        .expect("serve exits")
        .unwrap()
        .unwrap();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(25), "stopped in {took:?}");

    let db = Db::open(&path, 1).unwrap();
    assert_eq!(
        hold_state(&db).await,
        "running",
        "the interrupted job's row stays running"
    );
    let (marker, checkpointed) = db
        .read(|c| {
            let marker: i64 = c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'avl04_marker'",
                [],
                |r| r.get(0),
            )?;
            let checkpointed: i64 =
                c.query_row("SELECT min(updated_at) FROM limiter_state", [], |r| r.get(0))?;
            Ok::<_, DbError>((marker, checkpointed))
        })
        .await
        .unwrap();
    assert_eq!(marker, 0, "its transaction rolled back");
    assert!(
        checkpointed >= signalled,
        "the final limiter checkpoint ran after the signal (ADR-026): {checkpointed} < {signalled}"
    );

    // The next boot re-queues it, with the boot's other jobs still running
    // when the workers were aborted.
    let running: i64 = db
        .read(|c| {
            c.query_row("SELECT count(*) FROM jobs WHERE state = 'running'", [], |r| {
                r.get(0)
            })
            .map_err(DbError::from)
        })
        .await
        .unwrap();
    let recovered = Scheduler::new(db.clone(), Registry::new())
        .recover()
        .await
        .unwrap();
    assert_eq!(recovered.requeued, usize::try_from(running).unwrap());
    assert_eq!(hold_state(&db).await, "pending");
}

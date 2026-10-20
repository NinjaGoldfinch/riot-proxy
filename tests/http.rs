//! The HTTP skeleton: health routes, the error envelope on unrouted requests, layers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{app, get, send};

fn key_scope() -> String {
    riot_proxy::cache::keys::KeyScope::from_key(&riot_proxy::config::Secret::new(common::TEST_KEY))
        .to_string()
}

#[tokio::test]
async fn healthz_is_200_ok_true() {
    let (_dir, _state, router) = app();
    let res = get(router, "/healthz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json(), serde_json::json!({"ok": true}));
    assert!(res.headers.contains_key("x-request-id"));
}

#[tokio::test]
async fn readyz_is_200_when_sqlite_is_writable() {
    let (_dir, _state, router) = app();
    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json(),
        serde_json::json!({"ok": true, "sqlite": true, "limiter": true, "keyScope": key_scope(), "sqliteReaders": 2, "sqliteReadersFree": 2, "writerBusy": false, "writerBusyMs": 0, "writerQueued": 0})
    );
}

#[tokio::test]
async fn readyz_is_503_with_the_same_body_when_sqlite_is_not_writable() {
    let (_dir, state, router) = app();
    // Hold the write lock from a second connection so the idle writer's probe
    // cannot get it within its 1.5 s lock wait.
    let blocker = rusqlite::Connection::open(state.db.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res.json(),
        serde_json::json!({"ok": false, "sqlite": false, "limiter": true, "keyScope": key_scope(), "sqliteReaders": 2, "sqliteReadersFree": 2, "writerBusy": false, "writerBusyMs": 0, "writerQueued": 0})
    );
    blocker.execute_batch("ROLLBACK;").unwrap();
}

/// Start `job` on the writer and return once it is running (WR-02).
async fn start_write<F>(state: &riot_proxy::app::AppState, job: F) -> tokio::task::JoinHandle<()>
where
    F: FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<()> + Send + 'static,
{
    let (started, running) = tokio::sync::oneshot::channel();
    let db = state.db.clone();
    let task = tokio::spawn(async move {
        let _ = db
            .write(move |c| {
                let _ = started.send(());
                job(c).map_err(riot_proxy::db::DbError::from)
            })
            .await;
    });
    running.await.unwrap();
    task
}

const STALL: Duration = Duration::from_millis(200);

fn assert_busy_ready(body: &serde_json::Value, ok: bool) {
    assert_eq!(body["ok"], ok, "{body}");
    assert_eq!(body["sqlite"], ok, "{body}");
    assert_eq!(body["writerBusy"], true, "{body}");
    assert!(
        body["writerBusyMs"].as_u64().unwrap() >= STALL.as_millis() as u64,
        "{body}"
    );
    assert_eq!(body["writerQueued"], 0, "{body}");
}

#[tokio::test]
async fn readyz_is_200_while_a_long_write_keeps_sqlite_executing() {
    let (_dir, state, router) = app();
    state.db.set_writer_stall(STALL);
    let job = start_write(&state, |c| {
        c.execute_batch("CREATE TABLE busy (a INTEGER, b TEXT)")?;
        let tx = c.transaction()?;
        let until = Instant::now() + STALL * 3;
        let mut i = 0_i64;
        while Instant::now() < until {
            tx.prepare_cached("INSERT INTO busy (a, b) VALUES (?1, ?2)")?
                .execute(rusqlite::params![i, format!("row-{i}")])?;
            i += 1;
        }
        tx.commit()
    })
    .await;
    tokio::time::sleep(STALL * 2).await;

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_busy_ready(&res.json(), true);
    job.await.unwrap();
}

/// A sort runs as one SQLite step and stamps nothing for seconds; the writer
/// thread's CPU clock shows it is working. A loop with no SQLite in it stands in.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn readyz_is_200_while_the_running_write_computes_without_stamping() {
    let (_dir, state, router) = app();
    state.db.set_writer_stall(STALL);
    let job = start_write(&state, |_| {
        let until = Instant::now() + STALL * 4;
        let mut x = 0_u64;
        while Instant::now() < until {
            x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
        }
        Ok(())
    })
    .await;
    tokio::time::sleep(STALL * 2).await;

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_busy_ready(&res.json(), true);
    job.await.unwrap();
}

#[tokio::test]
async fn readyz_is_503_when_the_running_write_is_stuck_outside_sqlite() {
    let (_dir, state, router) = app();
    state.db.set_writer_stall(STALL);
    let job = start_write(&state, |_| {
        std::thread::sleep(STALL * 3);
        Ok(())
    })
    .await;
    tokio::time::sleep(STALL * 2).await;

    let started = Instant::now();
    let res = get(router, "/readyz").await;
    assert!(
        started.elapsed() < STALL,
        "answered without queueing behind the write"
    );
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_busy_ready(&res.json(), false);
    job.await.unwrap();
}

#[tokio::test]
async fn readyz_is_503_when_the_running_write_waits_on_another_process_lock() {
    let (_dir, state, router) = app();
    state.db.set_writer_stall(STALL);
    let blocker = rusqlite::Connection::open(state.db.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    // Waits in busy_timeout (5 s) for the blocker's lock: no progress.
    let job = start_write(&state, |c| {
        c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
            .map(|_| ())
    })
    .await;
    tokio::time::sleep(STALL * 2).await;

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_busy_ready(&res.json(), false);
    blocker.execute_batch("ROLLBACK;").unwrap();
    job.await.unwrap();
}

/// A job's start counts as progress only briefly (1.5 s) until SQLite stamps, so
/// a lone write blocked on another process's lock is a stall well before the 5 s
/// threshold that applies once SQLite has stamped.
#[tokio::test]
async fn readyz_gives_a_blocked_write_only_the_start_grace() {
    let (_dir, state, router) = app();
    state.db.set_writer_stall(Duration::from_secs(3));
    let blocker = rusqlite::Connection::open(state.db.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let job = start_write(&state, |c| {
        c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
            .map(|_| ())
    })
    .await;
    let early = get(router.clone(), "/readyz").await;
    assert_eq!(early.status, StatusCode::OK, "{}", early.json());
    tokio::time::sleep(Duration::from_millis(1800)).await;

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.json()["writerBusy"], true);
    blocker.execute_batch("ROLLBACK;").unwrap();
    job.await.unwrap();
}

#[tokio::test]
async fn unknown_route_is_a_404_envelope() {
    let (_dir, _state, router) = app();
    let res = get(router, "/nope?x=1").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.headers[header::CONTENT_TYPE], "application/json");

    let mut body = res.json();
    let request_id = body["error"]["requestId"]
        .as_str()
        .expect("requestId")
        .to_string();
    assert_eq!(
        res.headers["x-request-id"],
        request_id.as_str(),
        "body and header agree"
    );
    body["error"]["requestId"] = "[request-id]".into();
    insta::assert_json_snapshot!("unknown_route_404", body);
}

#[tokio::test]
async fn wrong_method_on_a_known_path_is_also_a_404_envelope() {
    let (_dir, _state, router) = app();
    let res = send(router, Request::post("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["error"]["message"], "No route for POST /healthz");
}

#[tokio::test]
async fn responses_are_compressed_when_asked() {
    let (_dir, _state, router) = app();
    let req = Request::get("/metrics")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let res = send(router, req).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.headers[header::CONTENT_ENCODING], "gzip");
}

#[tokio::test]
async fn metrics_is_mounted() {
    let (_dir, _state, router) = app();
    let res = get(router, "/metrics").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        String::from_utf8(res.body)
            .unwrap()
            .contains("proxy_archived_matches_total")
    );
}

#[tokio::test]
async fn readyz_counts_a_busy_reader_and_stays_ready() {
    let (_dir, state, router) = app();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let db = state.db.clone();
    let busy = tokio::spawn(async move {
        db.read(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, riot_proxy::db::DbError>(())
        })
        .await
    });
    tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
        .await
        .unwrap();

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["sqliteReaders"], 2);
    assert_eq!(res.json()["sqliteReadersFree"], 1);

    release_tx.send(()).unwrap();
    busy.await.unwrap().unwrap();
    assert_eq!(state.db.readers_free(), 2);
}

#[tokio::test]
async fn readyz_is_503_until_the_limiter_is_restored() {
    let (_dir, state, router) = app();
    state
        .limiter_restored
        .store(false, std::sync::atomic::Ordering::Release);
    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res.json(),
        serde_json::json!({"ok": false, "sqlite": true, "limiter": false, "keyScope": key_scope(), "sqliteReaders": 2, "sqliteReadersFree": 2, "writerBusy": false, "writerBusyMs": 0, "writerQueued": 0})
    );
}

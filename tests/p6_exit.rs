//! The P6 exit check (docs/IMPLEMENTATION.md), end to end through `serve`:
//!
//! 1. Track a player, start `serve`, and see `game.started` on `/v1/ws` when a
//!    wiremock spectator flips from "not in game" to a game.
//! 2. Kill `serve` mid-backfill, restart it on the same database, and see the
//!    backfill resume from the `jobs` table with no duplicate `archive:match` rows.
//!
//! `serve` runs in-process against wiremock ([`ServeOptions::riot_base_url`]);
//! "kill" is aborting its task, so nothing shuts down gracefully. Ignored by
//! default (about 10 s):
//!     cargo test --test p6_exit -- --ignored --nocapture
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use riot_proxy::cli::serve::{ServeOptions, serve_with};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::archive::BackfillState;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const IDS: &str = "/lol/match/v5/matches/by-puuid/NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw/ids";
const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");
const HISTORY: u32 = 150;

/// Generous limits, so the limiter never waits on a mock.
fn riot(status: u16) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("x-app-rate-limit", "30000:10,1800000:600")
        .insert_header("x-app-rate-limit-count", "1:10,1:600")
        .insert_header("x-method-rate-limit", "30000:10")
        .insert_header("x-method-rate-limit-count", "1:10")
}

fn ids(range: std::ops::Range<u32>) -> Value {
    json!(range.map(|n| format!("KR_{}", 20_000 - n)).collect::<Vec<_>>())
}

/// Spectator: 404 until flipped, then one game.
struct Spectator(Arc<AtomicBool>);

impl Respond for Spectator {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.0.load(Ordering::SeqCst) {
            riot(200).set_body_json(json!({"gameId": 77, "gameQueueConfigId": 420, "participants": [{"puuid": P, "championId": 134}]}))
        } else {
            riot(404).set_body_json(json!({"status": {"status_code": 404}}))
        }
    }
}

/// The backfill's second page, delayed by a settable amount (to kill mid-walk).
struct SlowPage(Arc<AtomicU64>);

impl Respond for SlowPage {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        riot(200)
            .set_body_json(ids(100..HISTORY))
            .set_delay(Duration::from_millis(self.0.load(Ordering::SeqCst)))
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn start(data_dir: &std::path::Path, riot_url: &str) -> (tokio::task::JoinHandle<()>, u16) {
    let port = free_port();
    let config = common::config(&[
        ("DATA_DIR", data_dir.to_str().unwrap()),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("TRACK_POLL_LIVE_S", "10"),
        ("LOOKUP_BACKFILL_LIMIT", &HISTORY.to_string()),
        ("LOG_LEVEL", "warn"),
    ]);
    let options = ServeOptions {
        riot_base_url: Some(riot_url.to_string()),
        skip_tracing_init: true,
        // Unmocked: the sync fails and retries, but never leaves the machine.
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock(riot_url)),
    };
    let task = tokio::spawn(async move {
        serve_with(config, options).await.unwrap();
    });
    let client = reqwest::Client::new();
    for _ in 0..200 {
        if client
            .get(format!("http://127.0.0.1:{port}/readyz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return (task, port);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("serve did not become ready");
}

async fn walk(db: &Db) -> Option<BackfillState> {
    let raw: Option<String> = db
        .read(|c| {
            c.query_row("SELECT backfill_state FROM players WHERE puuid = ?1", [P], |r| {
                r.get(0)
            })
            .map_err(DbError::from)
        })
        .await
        .ok()?;
    BackfillState::parse(raw.as_deref())
}

async fn archive_rows(db: &Db) -> (i64, i64, i64, i64) {
    db.read(|c| {
        c.query_row(
            "SELECT count(*), count(DISTINCT dedupe_key), sum(state = 'done'),
                    (SELECT count(*) FROM matches)
               FROM jobs WHERE kind = 'archive:match'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get(3)?,
                ))
            },
        )
        .map_err(DbError::from)
    })
    .await
    .unwrap()
}

async fn until<T>(what: &str, secs: u64, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "P6 exit check, ~10 s; run for the phase gate"]
async fn p6_exit_check() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let in_game = Arc::new(AtomicBool::new(false));
    let page_two_delay = Arc::new(AtomicU64::new(60_000));
    Mock::given(method("GET"))
        .and(path_regex("^/lol/spectator/v5/active-games/by-summoner/"))
        .respond_with(Spectator(Arc::clone(&in_game)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/lol/league/v4/entries/by-puuid/"))
        .respond_with(riot(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    for (start, count, body) in [(0, 5, ids(0..5)), (0, 100, ids(0..100))] {
        Mock::given(method("GET"))
            .and(path(IDS))
            .and(query_param("start", start.to_string()))
            .and(query_param("count", count.to_string()))
            .respond_with(riot(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(IDS))
        .and(query_param("start", "100"))
        .respond_with(SlowPage(Arc::clone(&page_two_delay)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/lol/match/v5/matches/KR_[0-9]+$"))
        .respond_with(riot(200).set_body_raw(MATCH, "application/json"))
        .mount(&server)
        .await;

    // An admin key, made before serve so no bootstrap key is printed.
    let db_path = dir.path().join("riot-proxy.db");
    let key = {
        let db = Db::open(&db_path, 1).unwrap();
        let made = consumers::create(
            &db,
            NewConsumer {
                name: "exit-check".into(),
                scopes: vec![Scope::Read, Scope::Admin],
                quota_per_min: 10_000,
                key: None,
            },
        )
        .await
        .unwrap();
        made.key.expose().to_string()
    };
    let db = Db::open(&db_path, 2).unwrap();

    // ── 1. track, start, see game.started on /v1/ws ────────────────────────
    let (first, port) = start(dir.path(), &server.uri()).await;
    let http = reqwest::Client::new();
    let tracked: Value = http
        .post(format!("http://127.0.0.1:{port}/v1/admin/tracked-players"))
        .bearer_auth(&key)
        .json(&json!({"platform": "kr", "puuid": P}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tracked["tracked"], true);
    assert_eq!(
        tracked["backfill"]["status"], "queued",
        "tracking queues the history walk"
    );

    let mut req = format!("ws://127.0.0.1:{port}/v1/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let subscribe = json!({"op": "subscribe", "topics": [format!("player:{P}")]});
    ws.send(Message::Text(subscribe.to_string().into()))
        .await
        .unwrap();

    in_game.store(true, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    let game = loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("game.started within one live tick");
        if let Some(Ok(Message::Text(t))) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["event"] == "game.started" {
                break v;
            }
        }
    };
    println!(
        "1. game.started on /v1/ws {:.1} s after the spectator flip: {}",
        started.elapsed().as_secs_f64(),
        game["data"]
    );
    assert_eq!(
        (game["data"]["gameId"].clone(), game["data"]["championId"].clone()),
        (json!(77), json!(134))
    );

    // ── 2. kill mid-backfill ───────────────────────────────────────────────
    let before = until("the backfill to finish page one", 20, async || {
        walk(&db).await.filter(|w| w.cursor == Some(100))
    })
    .await;
    assert_eq!((before.depth, before.done_at), (100, None));
    first.abort();
    let _ = first.await;
    let (rows, distinct, _, _) = archive_rows(&db).await;
    let running: i64 = db
        .read(|c| {
            c.query_row(
                "SELECT count(*) FROM jobs WHERE kind = 'backfill:player' AND state = 'running'",
                [],
                |r| r.get(0),
            )
            .map_err(DbError::from)
        })
        .await
        .unwrap();
    println!(
        "2. killed serve: backfill cursor 100 of {HISTORY}, backfill row still running ({running}), {rows} archive:match rows"
    );
    assert_eq!(running, 1, "a killed process leaves its job running");
    assert_eq!((rows, distinct), (100, 100));

    // ── 3. restart: resume from the jobs table ─────────────────────────────
    page_two_delay.store(0, Ordering::SeqCst);
    let (second, _) = start(dir.path(), &server.uri()).await;
    let after = until("the backfill to complete", 30, async || {
        walk(&db).await.filter(|w| w.done_at.is_some())
    })
    .await;
    let (rows, distinct, done, archived) = until("every archive job to finish", 30, async || {
        let r = archive_rows(&db).await;
        (r.2 == i64::from(HISTORY)).then_some(r)
    })
    .await;
    second.abort();

    let first_pages = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == IDS && r.url.query() == Some("start=0&count=100"))
        .count();
    println!(
        "3. restarted: backfill resumed at 100 and completed (depth {}); page one read {first_pages}×; \
         archive:match rows {rows}, distinct {distinct}, done {done}; matches archived {archived}",
        after.depth
    );
    assert_eq!(after.depth, i64::from(HISTORY));
    assert_eq!(first_pages, 1, "resumed from the cursor, not from the top");
    assert_eq!(
        (rows, distinct),
        (i64::from(HISTORY), i64::from(HISTORY)),
        "no duplicate archive:match rows"
    );
    assert_eq!(archived, i64::from(HISTORY));
}

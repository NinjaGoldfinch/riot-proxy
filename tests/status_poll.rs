//! The status poller (SITE-13, design/14 §Status poller) against a wiremock
//! Riot: a round fills L1, so `GET /v1/lol/status/{platform}` answers `HIT`
//! without calling Riot; a platform with no limiter room is not called; a
//! failure is a row; the rows survive a restart.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::Db;
use riot_proxy::riot::limiter::headers::RateLimitType;
use riot_proxy::riot::routing::Platform;
use riot_proxy::status::poll::{self, Polled, Poller};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PLATFORM_DATA: &str = "/lol/status/v4/platform-data";

/// Test data with the portal's `PlatformDataDto` fields (ADR-217).
fn body(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{id}","locales":["en_US"],"maintenances":[],"incidents":[]}}"#)
}

struct Env {
    dir: tempfile::TempDir,
    state: AppState,
    router: axum::Router,
    server: MockServer,
    key: String,
}

async fn env(platforms: &str) -> Env {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[("STATUS_POLL_PLATFORMS", platforms)], &server.uri());
    let key = consumers::create(
        &state.db,
        NewConsumer {
            name: "site".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 10_000,
            key: None,
        },
    )
    .await
    .unwrap()
    .key
    .expose()
    .to_string();
    Env {
        dir,
        state,
        router,
        server,
        key,
    }
}

impl Env {
    async fn answer(&self, platform: &str, status: u16, body: String) {
        Mock::given(method("GET"))
            .and(path(PLATFORM_DATA))
            .and(header(
                "x-riot-host",
                format!("{platform}.api.riotgames.com").as_str(),
            ))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&self.server)
            .await;
    }

    async fn answer_after(&self, platform: &str, status: u16, body: String, delay: Duration) {
        Mock::given(method("GET"))
            .and(path(PLATFORM_DATA))
            .and(header(
                "x-riot-host",
                format!("{platform}.api.riotgames.com").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_string(body)
                    .set_delay(delay),
            )
            .mount(&self.server)
            .await;
    }

    async fn calls(&self, platform: &str) -> usize {
        let host = format!("{platform}.api.riotgames.com");
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| {
                r.url.path() == PLATFORM_DATA
                    && r.headers
                        .get("x-riot-host")
                        .is_some_and(|h| h.to_str().unwrap() == host)
            })
            .count()
    }

    async fn get(&self, uri: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {}", self.key))
            .body(Body::empty())
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    fn poller(&self) -> Poller {
        Poller::new(&self.state)
            .unwrap()
            .expect("STATUS_POLL_PLATFORMS lists some")
    }
}

/// After a round the per-platform route is answered from L1: `HIT`, a fetch
/// age well under the interval, and no call to Riot.
#[tokio::test]
async fn after_a_round_the_platform_route_is_a_hit_without_calling_riot() {
    let e = env("oc1,kr").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.answer("kr", 200, body("KR")).await;

    let round = e.poller().round().await;
    assert_eq!(round, [(Platform::Oc1, Polled::Ok), (Platform::Kr, Polled::Ok)]);
    assert_eq!((e.calls("oc1").await, e.calls("kr").await), (1, 1));

    for (platform, id) in [("oc1", "OC1"), ("kr", "KR")] {
        let r = e.get(&format!("/v1/lol/status/{platform}")).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.headers["x-cache"], "HIT", "{platform}");
        let age: u64 = r.headers["x-cache-fetched-age"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(age < 300, "fetched {age} s ago");
        assert_eq!(String::from_utf8(r.body).unwrap(), body(id));
    }
    assert_eq!(
        (e.calls("oc1").await, e.calls("kr").await),
        (1, 1),
        "the route called Riot"
    );
}

/// A platform the limiter has no room for within `poll::ROOM` (30 s) is not called and
/// gets no row; the others are read. A failing platform is a failure row,
/// not an error of the round.
#[tokio::test]
async fn a_full_limiter_skips_a_platform_and_a_failure_is_a_row() {
    let e = env("oc1,kr,tw2").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.answer("kr", 200, body("KR")).await;
    e.answer("tw2", 404, r#"{"status":{"status_code":404}}"#.into())
        .await;
    e.state
        .limiter
        .freeze("kr", Duration::from_secs(120), RateLimitType::Service);

    let started = std::time::Instant::now();
    let round = e.poller().round().await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        round,
        [
            (Platform::Oc1, Polled::Ok),
            (Platform::Kr, Polled::Skipped),
            (Platform::Tw2, Polled::Failed)
        ]
    );
    assert_eq!(e.calls("kr").await, 0, "no room, no call");

    let rows = poll::stored(&e.state.db).await.unwrap();
    let platforms: Vec<Platform> = rows.iter().map(|r| r.platform).collect();
    assert_eq!(platforms, [Platform::Oc1, Platform::Tw2], "a skip writes nothing");
    let tw2 = &rows[1];
    assert_eq!((tw2.body.as_deref(), tw2.fetched_at_ms), (None, None));
    assert!(tw2.failed_at_ms.is_some());
    assert_eq!(
        tw2.error.as_deref(),
        Some("NOT_FOUND: Resource not found upstream")
    );
}

/// What a round stored is in SQLite: a fresh process (a new `Db` on the same
/// file) reads the same rows.
#[tokio::test]
async fn the_rows_survive_a_restart() {
    let e = env("oc1").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.poller().round().await;
    let before = poll::stored(&e.state.db).await.unwrap();
    assert_eq!(before.len(), 1);

    let reopened = Db::open(&e.dir.path().join("riot-proxy.db"), 1).unwrap();
    let after = poll::stored(&reopened).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(after[0].body.as_deref(), Some(body("OC1").as_str()));
    assert!(after[0].fetched_at_ms.is_some());
}

/// `STATUS_POLL_PLATFORMS=none` builds no poller; the default polls every
/// platform but `ph2`, `th2` and `tw2` (owner decision of 2026-11-17), every 240 s
/// (owner decision of 2026-11-17), and that interval is the poller's.
#[tokio::test]
async fn none_turns_the_poller_off() {
    let e = env("none").await;
    assert!(Poller::new(&e.state).unwrap().is_none());
    let (_dir, state, _) = common::app_with(&[], "http://127.0.0.1:9");
    assert_eq!(
        state.config.status_poll_platforms,
        riot_proxy::config::STATUS_POLL_DEFAULT.to_vec()
    );
    let p = Poller::new(&state).unwrap().unwrap();
    assert_eq!(p.interval(), Duration::from_secs(240));
    let (_dir, state, _) = common::app_with(&[("STATUS_POLL_INTERVAL_S", "90")], "http://127.0.0.1:9");
    assert_eq!(
        Poller::new(&state).unwrap().unwrap().interval(),
        Duration::from_secs(90),
        "STATUS_POLL_INTERVAL_S reaches the poller"
    );
}

// ── Review of #282 (SITE-17) ─────────────────────────────────────────────────

/// Riot's typed 429 (`X-Rate-Limit-Type`, `Retry-After` past the poller's
/// limiter wait) is our limiter frozen: one call, then a skip, no row.
#[tokio::test]
async fn a_typed_429_is_a_skip_after_one_call() {
    let e = env("oc1").await;
    Mock::given(method("GET"))
        .and(path(PLATFORM_DATA))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("X-Rate-Limit-Type", "method")
                .insert_header("Retry-After", "90"),
        )
        .mount(&e.server)
        .await;
    assert_eq!(e.poller().round().await, [(Platform::Oc1, Polled::Skipped)]);
    assert_eq!(e.calls("oc1").await, 1);
    assert!(poll::stored(&e.state.db).await.unwrap().is_empty());
}

/// A service 429 (no `X-Rate-Limit-Type`) is Riot's own trouble: backed off,
/// retried, and a failure row when it persists.
#[tokio::test]
async fn a_service_429_is_a_failure_after_the_back_offs() {
    let e = env("oc1").await;
    e.answer("oc1", 429, String::new()).await;
    assert_eq!(e.poller().round().await, [(Platform::Oc1, Polled::Failed)]);
    assert!(e.calls("oc1").await > 1, "retried");
    let rows = poll::stored(&e.state.db).await.unwrap();
    assert!(
        rows[0]
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("RATE_LIMITED")),
        "{rows:?}"
    );
}

/// The poller's calls are status polls, not traffic (owner decision of
/// 2026-11-17): one platform refusing the key (403) while the others answer
/// does not turn `riot_key` to rejected or log `RIOT KEY REJECTED`, and no
/// scope is stamped with traffic, so `/status`'s probe keeps its 30 s
/// cadence. The 403 is delayed so it lands after the 200s: were it judged,
/// the verdict would end rejected.
#[tokio::test]
async fn a_403_on_one_platform_does_not_reject_the_key_and_polls_are_not_traffic() {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let e = env("oc1,kr,tw2").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.answer("kr", 200, body("KR")).await;
    e.answer_after(
        "tw2",
        403,
        r#"{"status":{"status_code":403}}"#.into(),
        Duration::from_millis(300),
    )
    .await;
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
    );
    let round = {
        let _guard = tracing::subscriber::set_default(subscriber);
        e.poller().round().await
    };
    assert_eq!(
        round,
        [
            (Platform::Oc1, Polled::Ok),
            (Platform::Kr, Polled::Ok),
            (Platform::Tw2, Polled::Failed)
        ]
    );
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("RIOT KEY REJECTED").count(), 0, "{log}");
    assert!(log.contains("status poll refused"), "{log}");
    let watch = e.state.fetcher.client().watch();
    let key = watch.key().unwrap();
    assert!(key.accepted, "riot_key is not down: {key:?}");
    for scope in ["oc1", "kr", "tw2"] {
        assert_eq!(
            watch.last_traffic_ms(scope),
            None,
            "{scope}: a poll is not traffic"
        );
    }
    assert_eq!(riot_key(&e).await, "ok");
}

/// `/status`'s `riot_key` check's status, as a loopback peer reads it.
async fn riot_key(e: &Env) -> String {
    let router = riot_proxy::app::router(e.state.clone(), riot_proxy::telemetry::metrics_handle().unwrap())
        .layer(axum::extract::connect_info::MockConnectInfo(
            "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
        ));
    let req = Request::get("/status").body(Body::empty()).unwrap();
    let json = common::send(router, req).await.json();
    json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "riot_key")
        .unwrap_or_else(|| panic!("no riot_key in {json}"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Owner decision of 2026-11-17: a round in which every platform the poller
/// reads refuses the key judges it, on a deployment with no probe platform:
/// `riot_key` is `down` after the round, and `RIOT KEY REJECTED` is logged
/// once across two such rounds.
#[tokio::test]
async fn a_round_every_platform_refuses_rejects_the_key_once() {
    let e = env("oc1,kr").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.answer("kr", 200, body("KR")).await;
    e.poller().round().await;
    assert_eq!(riot_key(&e).await, "ok", "the polls' 200s accepted it");

    e.server.reset().await;
    e.answer("oc1", 403, String::new()).await;
    e.answer("kr", 401, String::new()).await;
    let poller = e.poller();
    let (rounds, log) = logged(async {
        let first = poller.round().await;
        let second = poller.round().await;
        (first, second)
    })
    .await;
    let failed = [(Platform::Oc1, Polled::Failed), (Platform::Kr, Polled::Failed)];
    assert_eq!((rounds.0, rounds.1), (failed.to_vec(), failed.to_vec()));
    assert_eq!(log.matches("RIOT KEY REJECTED").count(), 1, "{log}");
    assert_eq!(riot_key(&e).await, "down");
    assert!(!e.state.fetcher.client().watch().key().unwrap().accepted);
}

/// Lines at `debug` and above logged while `f` runs on this thread.
async fn logged<F: std::future::Future>(f: F) -> (F::Output, String) {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
    );
    let out = {
        let _guard = tracing::subscriber::set_default(subscriber);
        f.await
    };
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, text)
}

/// A poll reads no cache, so it counts as no cache read: an idle proxy's
/// hit ratio is not driven to 0 by the poller (`CacheHitRatioLow`).
#[tokio::test]
async fn a_round_counts_no_cache_read() {
    let metrics = riot_proxy::telemetry::metrics_handle().unwrap();
    let e = env("oc1,kr").await;
    e.answer("oc1", 200, body("OC1")).await;
    e.answer("kr", 200, body("KR")).await;
    let misses = || {
        metrics
            .render()
            .lines()
            .find(|l| l.starts_with("proxy_cache_reads_total{state=\"miss\"}"))
            .map(str::to_string)
    };
    let before = misses();
    assert_eq!(e.poller().round().await.len(), 2);
    assert_eq!(misses(), before);
}

// ── Review of #288 ───────────────────────────────────────────────────────────

/// A typed 429 whose `Retry-After` is inside the poller's room is waited
/// out and the read retried: a 200 after it is `Ok`, with two calls.
#[tokio::test]
async fn a_typed_429_with_a_short_retry_after_then_a_200_is_ok_after_two_calls() {
    let e = env("oc1").await;
    Mock::given(method("GET"))
        .and(path(PLATFORM_DATA))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("X-Rate-Limit-Type", "method")
                .insert_header("Retry-After", "2"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&e.server)
        .await;
    e.answer("oc1", 200, body("OC1")).await;
    let started = std::time::Instant::now();
    assert_eq!(e.poller().round().await, [(Platform::Oc1, Polled::Ok)]);
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(e.calls("oc1").await, 2);
}

/// The deadline with the real fetcher: a platform that answers after it is
/// stored failed at the deadline, and its call goes on and writes L1 when it
/// lands, so the route then answers `HIT` without another call.
#[tokio::test]
async fn a_read_past_the_deadline_is_stored_failed_and_its_call_fills_l1() {
    let e = env("oc1").await;
    e.answer_after("oc1", 200, body("OC1"), Duration::from_millis(1_500))
        .await;
    let poller = e.poller().with_deadline(Duration::from_millis(500));
    let started = std::time::Instant::now();
    assert_eq!(poller.round().await, [(Platform::Oc1, Polled::Failed)]);
    assert!(
        started.elapsed() < Duration::from_millis(1_400),
        "{:?}",
        started.elapsed()
    );
    let rows = poll::stored(&e.state.db).await.unwrap();
    assert_eq!(rows[0].error.as_deref(), Some("no answer within 0.5 s"));
    assert_eq!(rows[0].body, None);

    for _ in 0..200 {
        if e.state.fetcher.inflight_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let r = e.get("/v1/lol/status/oc1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["x-cache"], "HIT");
    assert_eq!(String::from_utf8(r.body).unwrap(), body("OC1"));
    assert_eq!(e.calls("oc1").await, 1, "the poll's call filled L1");
}

/// A poll has its own call (SITE-17): a consumer's miss for the same
/// platform while it is in flight does not join it, makes its own call and
/// is a `MISS`.
#[tokio::test]
async fn an_interactive_miss_during_a_poll_makes_its_own_call() {
    let e = env("oc1").await;
    e.answer_after("oc1", 200, body("OC1"), Duration::from_secs(1))
        .await;
    let poller = e.poller();
    let poll = tokio::spawn(async move { poller.round().await });
    for _ in 0..200 {
        if e.calls("oc1").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(e.calls("oc1").await, 1, "the poll is in flight");
    let r = e.get("/v1/lol/status/oc1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["x-cache"], "MISS");
    assert_eq!(e.calls("oc1").await, 2, "its own call");
    assert_eq!(poll.await.unwrap(), [(Platform::Oc1, Polled::Ok)]);
}

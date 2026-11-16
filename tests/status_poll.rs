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

/// A platform the limiter has no room for within a second is not called and
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
/// platform but `ph2`, `th2` and `tw2` (owner decision D3), every 240 s
/// (D5), and that interval is the poller's.
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

/// The poller's calls are status polls, not traffic (owner decision D7):
/// one platform refusing the key (403) while the others answer does not
/// turn `riot_key` to rejected or log `RIOT KEY REJECTED`, and no scope is
/// stamped with traffic, so `/status`'s probe keeps its 30 s cadence.
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
    e.answer("tw2", 403, r#"{"status":{"status_code":403}}"#.into())
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
    assert!(log.matches("RIOT KEY REJECTED").count() <= 1, "{log}");
    let watch = e.state.fetcher.client().watch();
    assert!(watch.key().unwrap().accepted, "riot_key is not down");
    for scope in ["oc1", "kr", "tw2"] {
        assert_eq!(
            watch.last_traffic_ms(scope),
            None,
            "{scope}: a poll is not traffic"
        );
    }
    let r = e.get("/status").await;
    let json = r.json();
    assert_ne!(
        json.pointer("/checks/riot_key/status").and_then(|v| v.as_str()),
        Some("down"),
        "{json}"
    );
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

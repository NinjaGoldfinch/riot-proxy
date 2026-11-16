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
        .freeze("kr", Duration::from_secs(30), RateLimitType::Service);

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
/// platform.
#[tokio::test]
async fn none_turns_the_poller_off() {
    let e = env("none").await;
    assert!(Poller::new(&e.state).unwrap().is_none());
    let (_dir, state, _) = common::app_with(&[], "http://127.0.0.1:9");
    assert_eq!(state.config.status_poll_platforms, Platform::ALL.to_vec());
}

//! `GET /v1/lol/status` (SITE-14, ADR-220) against a wiremock Riot read by
//! the real status poller: every polled platform from `platform_status`, a
//! failing platform as `available: false` in a 200, no call to Riot on read,
//! the headers, and a 503 when the poller is off.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::riot::routing::Platform;
use riot_proxy::status::poll::{Polled, Poller};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PLATFORM_DATA: &str = "/lol/status/v4/platform-data";

/// Test data with the portal's `PlatformDataDto` and `StatusDto` fields
/// (ADR-217), not a recording.
fn body(id: &str, incidents: serde_json::Value, maintenances: serde_json::Value) -> String {
    json!({"id": id, "name": id, "locales": ["en_US"], "incidents": incidents, "maintenances": maintenances})
        .to_string()
}

fn quiet(id: &str) -> String {
    body(id, json!([]), json!([]))
}

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    router: axum::Router,
    server: MockServer,
    key: String,
}

async fn env(platforms: &str, quota: u32) -> Env {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[("STATUS_POLL_PLATFORMS", platforms)], &server.uri());
    let key = consumers::create(
        &state.db,
        NewConsumer {
            name: "site".into(),
            scopes: vec![Scope::Read],
            quota_per_min: quota,
            key: None,
        },
    )
    .await
    .unwrap()
    .key
    .expose()
    .to_string();
    Env {
        _dir: dir,
        state,
        router,
        server,
        key,
    }
}

impl Env {
    fn mock(platform: &str, status: u16, body: String) -> Mock {
        Mock::given(method("GET"))
            .and(path(PLATFORM_DATA))
            .and(header(
                "x-riot-host",
                format!("{platform}.api.riotgames.com").as_str(),
            ))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
    }

    async fn answer(&self, platform: &str, status: u16, body: String) {
        Self::mock(platform, status, body).mount(&self.server).await;
    }

    async fn upstream_calls(&self) -> usize {
        self.server.received_requests().await.unwrap().len()
    }

    async fn get(&self, uri: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {}", self.key))
            .body(Body::empty())
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn round(&self) -> Vec<(Platform, Polled)> {
        Poller::new(&self.state)
            .unwrap()
            .expect("STATUS_POLL_PLATFORMS lists some")
            .round()
            .await
    }
}

fn header_u64(r: &common::Reply, name: &str) -> u64 {
    r.headers[name].to_str().unwrap().parse().unwrap()
}

/// After a round every polled platform is listed in platform order with its
/// counts; a platform Riot fails for is `available: false` and the call is
/// still a 200; reading it calls Riot for nothing.
#[tokio::test]
async fn every_polled_platform_in_one_call_without_calling_riot() {
    // Configured out of platform order, to see the route put them back.
    let e = env("tw2,oc1,euw1,kr", 10_000).await;
    e.answer(
        "euw1",
        200,
        body(
            "EUW1",
            json!([
                {"id": 1, "incident_severity": "warning", "titles": [], "updates": []},
                {"id": 2, "incident_severity": "critical", "titles": [], "updates": []},
            ]),
            json!([
                {"id": 3, "maintenance_status": "in_progress", "titles": [], "updates": []},
                {"id": 4, "maintenance_status": "complete", "titles": [], "updates": []},
            ]),
        ),
    )
    .await;
    e.answer("oc1", 200, quiet("OC1")).await;
    e.answer("kr", 200, quiet("KR")).await;
    e.answer("tw2", 404, r#"{"status":{"status_code":404}}"#.into())
        .await;
    let round = e.round().await;
    assert!(round.contains(&(Platform::Tw2, Polled::Failed)), "{round:?}");
    let polled = e.upstream_calls().await;
    assert_eq!(polled, 4);

    let r = e.get("/v1/lol/status").await;
    assert_eq!(r.status, StatusCode::OK);
    // Read just after the round: each age is a second or two at most.
    let mut got = r.json();
    for entry in got["platforms"].as_array_mut().unwrap() {
        if let Some(age) = entry["fetchedAgeSeconds"].as_u64() {
            assert!(age < 5, "{entry}");
            entry["fetchedAgeSeconds"] = json!(0);
        }
    }
    assert_eq!(
        got,
        json!({"platforms": [
            {"platform": "euw1", "available": true, "error": null, "incidents": 2, "maintenances": 1,
             "worstSeverity": "critical", "maintenanceInProgress": true, "fetchedAgeSeconds": 0},
            {"platform": "kr", "available": true, "error": null, "incidents": 0, "maintenances": 0,
             "worstSeverity": null, "maintenanceInProgress": false, "fetchedAgeSeconds": 0},
            {"platform": "oc1", "available": true, "error": null, "incidents": 0, "maintenances": 0,
             "worstSeverity": null, "maintenanceInProgress": false, "fetchedAgeSeconds": 0},
            {"platform": "tw2", "available": false, "error": "NOT_FOUND: Resource not found upstream",
             "incidents": null, "maintenances": null, "worstSeverity": null, "maintenanceInProgress": null,
             "fetchedAgeSeconds": null},
        ]})
    );
    assert_eq!(r.headers["x-cache"], "HIT");
    assert!(header_u64(&r, "x-cache-fetched-age") < 5);
    assert!(!r.headers.contains_key("x-cache-age"), "no content age");
    assert!(r.headers.contains_key("x-request-id"));
    assert_eq!(e.upstream_calls().await, polled, "the summary called Riot");

    // The per-platform route is still the passthrough, answered from L1.
    let one = e.get("/v1/lol/status/oc1").await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.headers["x-cache"], "HIT");
    assert_eq!(String::from_utf8(one.body).unwrap(), quiet("OC1"));
    assert_eq!(e.upstream_calls().await, polled);
}

/// Before the first round nothing is stored: every entry is `not read yet`,
/// no fetch age is sent, and Riot is still not called.
#[tokio::test]
async fn before_the_first_round_every_entry_is_not_read_yet() {
    let e = env("oc1,kr", 10_000).await;
    let r = e.get("/v1/lol/status").await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    let entries = body["platforms"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for (entry, platform) in entries.iter().zip(["kr", "oc1"]) {
        assert_eq!(entry["platform"], platform);
        assert_eq!(entry["available"], false);
        assert_eq!(entry["error"], "not read yet");
        assert_eq!(entry["incidents"], serde_json::Value::Null);
        assert_eq!(entry["fetchedAgeSeconds"], serde_json::Value::Null);
    }
    assert_eq!(r.headers["x-cache"], "HIT");
    assert!(!r.headers.contains_key("x-cache-fetched-age"));
    assert_eq!(e.upstream_calls().await, 0);
}

/// A read that fails after a 200 makes the platform unavailable, with the
/// error and the age of the 200 it still has; the counts are not reported
/// from that older body.
#[tokio::test]
async fn a_failure_after_a_200_is_unavailable_with_the_last_200s_age() {
    let e = env("oc1", 10_000).await;
    let incident = json!([{"id": 9, "incident_severity": "info", "titles": [], "updates": []}]);
    Env::mock("oc1", 200, body("OC1", incident, json!([])))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    e.answer("oc1", 404, "{}".into()).await;
    assert_eq!(e.round().await, [(Platform::Oc1, Polled::Ok)]);
    let before = e.get("/v1/lol/status").await.json();
    assert_eq!(before["platforms"][0]["incidents"], 1);
    assert_eq!(before["platforms"][0]["worstSeverity"], "info");

    assert_eq!(e.round().await, [(Platform::Oc1, Polled::Failed)]);
    let r = e.get("/v1/lol/status").await;
    assert_eq!(r.status, StatusCode::OK);
    let oc1 = &r.json()["platforms"][0];
    assert_eq!(oc1["available"], false);
    assert_eq!(oc1["error"], "NOT_FOUND: Resource not found upstream");
    assert_eq!(oc1["incidents"], serde_json::Value::Null);
    assert_eq!(oc1["worstSeverity"], serde_json::Value::Null);
    assert!(oc1["fetchedAgeSeconds"].is_u64(), "{oc1}");
    assert!(
        !r.headers.contains_key("x-cache-fetched-age"),
        "no platform is available"
    );
}

/// `STATUS_POLL_PLATFORMS=none`: no poller, nothing stored, a 503 without
/// `Retry-After` (waiting does not change it). Still metered and keyed.
#[tokio::test]
async fn the_poller_off_is_a_503() {
    let e = env("none", 10_000).await;
    let r = e.get("/v1/lol/status").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let err = &r.json()["error"];
    assert_eq!(err["code"], "NOT_FOUND");
    assert_eq!(
        err["message"],
        "The status poller is off on this deployment (STATUS_POLL_PLATFORMS=none)"
    );
    assert!(!r.headers.contains_key("retry-after"));
    assert!(r.headers.contains_key("x-ratelimit-remaining"));
    assert!(r.headers.contains_key("x-request-id"));
    assert_eq!(e.upstream_calls().await, 0);
}

/// The per-platform route's key and scope: no key is a 401. One call counts
/// once against the consumer's quota, however many platforms it lists.
#[tokio::test]
async fn keyed_like_the_platform_route_and_metered_once() {
    let e = env("na1,euw1,kr,oc1", 5).await;
    let keyless = common::send(
        e.router.clone(),
        Request::get("/v1/lol/status").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(keyless.status, StatusCode::UNAUTHORIZED);

    let r = e.get("/v1/lol/status").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["platforms"].as_array().unwrap().len(), 4);
    assert_eq!(header_u64(&r, "x-ratelimit-limit"), 5);
    assert_eq!(header_u64(&r, "x-ratelimit-remaining"), 4);
    let r = e.get("/v1/lol/status").await;
    assert_eq!(header_u64(&r, "x-ratelimit-remaining"), 3);
}

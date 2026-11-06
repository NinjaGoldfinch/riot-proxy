//! `GET /status` through the full router (STS-02, design/14): the contract's
//! shape, who may read it, that it never calls Riot, and the request counts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    server: MockServer,
}

async fn env(extra: &[(&str, &str)]) -> Env {
    let server = MockServer::start().await;
    let (dir, state, _) = common::app_with(extra, &server.uri());
    Env {
        _dir: dir,
        state,
        server,
    }
}

impl Env {
    /// The app as seen from `peer`.
    fn router(&self, peer: &str) -> Router {
        riot_proxy::app::router(
            self.state.clone(),
            riot_proxy::telemetry::metrics_handle().unwrap(),
        )
        .layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()))
    }

    async fn key(&self, name: &str, scopes: Vec<Scope>) -> String {
        consumers::create(
            &self.state.db,
            NewConsumer {
                name: name.into(),
                scopes,
                quota_per_min: 600,
                key: None,
            },
        )
        .await
        .unwrap()
        .key
        .expose()
        .to_string()
    }

    async fn get(&self, peer: &str, uri: &str, key: Option<&str>, headers: &[(&str, &str)]) -> common::Reply {
        let mut req = Request::get(uri);
        if let Some(key) = key {
            req = req.header("authorization", format!("Bearer {key}"));
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        common::send(self.router(peer), req.body(Body::empty()).unwrap()).await
    }
}

const LOCAL: &str = "127.0.0.1:50000";
const OUTSIDE: &str = "203.0.113.9:50000";

/// design/14 §The shared contract: every top-level key, `status` derived,
/// `metrics` flat numbers, `metrics_meta` strings or null.
#[tokio::test]
async fn keyless_loopback_gets_the_contract() {
    let e = env(&[]).await;
    let r = e.get(LOCAL, "/status", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    let keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "schema",
        "service",
        "host",
        "version",
        "commit",
        "built_at",
        "started_at",
        "uptime_s",
        "status",
        "checks",
        "metrics",
        "metrics_meta",
    ];
    want.sort_unstable();
    let mut got = keys.clone();
    got.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(body["schema"], 1);
    assert_eq!(body["service"], "riot-proxy");
    assert!(!body["host"].as_str().unwrap().is_empty());
    assert_eq!(body["version"], riot_proxy::build_info::status_version());
    assert_eq!(body["commit"], riot_proxy::build_info::short_commit());
    for k in ["built_at", "started_at"] {
        let at = body[k].as_str().unwrap();
        assert!(
            at.ends_with('Z') && at.parse::<jiff::Timestamp>().is_ok(),
            "{k}: {at}"
        );
    }
    assert!(body["uptime_s"].is_u64());
    assert_eq!(body["status"], "ok");
    // Before the first round, without a platform to probe or a bucket.
    let checks = body["checks"].as_array().unwrap();
    let names: Vec<&str> = checks.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["riot_key", "cache"]);
    for c in checks {
        assert_eq!(
            (c["status"].as_str(), c["detail"].as_str()),
            (Some("ok"), Some("pending"))
        );
        assert_eq!(c["checked_at"], body["started_at"]);
        assert!(c["latency_ms"].is_null());
    }
    let metrics = body["metrics"].as_object().unwrap();
    for k in [
        "requests_total",
        "riot_requests_total",
        "cache_entries",
        "fetch_queue_depth",
    ] {
        assert!(metrics[k].is_u64(), "{k}");
    }
    assert!(metrics.values().all(serde_json::Value::is_u64));
    assert!(metrics.keys().all(|k| {
        k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }));
    assert_eq!(
        body["metrics_meta"]["riot_last_success_at"],
        serde_json::Value::Null
    );
    assert!(r.headers.get("x-request-id").is_some());
}

/// design/14 §A local request: the TCP peer inside `STATUS_ALLOW_CIDR`, and
/// no `Cf-Connecting-IP`; otherwise the auth layer's usual 401.
#[tokio::test]
async fn who_may_read_it_without_a_key() {
    let e = env(&[("TRUST_PROXY", "true")]).await;
    assert_eq!(
        e.get("[::1]:50000", "/status", None, &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(
        e.get("[::ffff:127.0.0.1]:50000", "/status", None, &[])
            .await
            .status,
        StatusCode::OK
    );
    // cloudflared connects from loopback and adds the header.
    let tunnelled = e
        .get(LOCAL, "/status", None, &[("cf-connecting-ip", "198.51.100.4")])
        .await;
    assert_eq!(tunnelled.status, StatusCode::UNAUTHORIZED);
    assert_eq!(tunnelled.json()["error"]["code"], "UNAUTHORIZED");
    assert_eq!(
        e.get(OUTSIDE, "/status", None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    // The peer, never X-Forwarded-For, even behind a trusted proxy.
    let forged = e
        .get(OUTSIDE, "/status", None, &[("x-forwarded-for", "127.0.0.1")])
        .await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
}

/// A reverse proxy on the same host (nginx, Caddy, Traefik) reaches the
/// proxy from loopback without `Cf-Connecting-IP`, but adds one of these:
/// with one present, a keyless request is not local, whatever `TRUST_PROXY`
/// says and whatever the value (ADR-205).
#[tokio::test]
async fn a_forwarded_loopback_request_needs_a_key() {
    let e = env(&[("TRUST_PROXY", "true")]).await;
    assert_eq!(e.get(LOCAL, "/status", None, &[]).await.status, StatusCode::OK);
    for (header, value) in [
        ("x-forwarded-for", "127.0.0.1"),
        ("forwarded", "for=127.0.0.1"),
        ("x-real-ip", "127.0.0.1"),
    ] {
        let r = e.get(LOCAL, "/status", None, &[(header, value)]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{header}");
        assert_eq!(r.json()["error"]["code"], "UNAUTHORIZED", "{header}");
    }
    let admin = e.key("status-admin-fwd", vec![Scope::Read, Scope::Admin]).await;
    assert_eq!(
        e.get(LOCAL, "/status", Some(&admin), &[("x-real-ip", "198.51.100.4")])
            .await
            .status,
        StatusCode::OK
    );
}

/// `AUTH_DISABLED` lets every request in as `dev-local`, as on every other
/// guarded route, so the peer does not matter.
#[tokio::test]
async fn auth_disabled_lets_a_keyless_request_in_from_anywhere() {
    let e = env(&[("AUTH_DISABLED", "true")]).await;
    let r = e.get(OUTSIDE, "/status", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["schema"], 1);
}

#[tokio::test]
async fn the_allowed_peers_follow_status_allow_cidr() {
    let e = env(&[("STATUS_ALLOW_CIDR", "10.0.0.0/8")]).await;
    assert_eq!(
        e.get("10.1.2.3:4000", "/status", None, &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(
        e.get(LOCAL, "/status", None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let none = env(&[("STATUS_ALLOW_CIDR", "none")]).await;
    assert_eq!(
        none.get(LOCAL, "/status", None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
}

/// The admin key from anywhere the admin allowlist allows; a read key is 403,
/// an unknown one 401. No consumer quota headers: it is not metered.
#[tokio::test]
async fn keys() {
    let e = env(&[]).await;
    let admin = e.key("status-admin", vec![Scope::Read, Scope::Admin]).await;
    let read = e.key("status-reader", vec![Scope::Read]).await;
    let ok = e.get(OUTSIDE, "/status", Some(&admin), &[]).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(ok.headers.get("x-ratelimit-remaining").is_none());
    // A key through the tunnel is fine: the key is the credential.
    let tunnelled = e
        .get(
            LOCAL,
            "/status",
            Some(&admin),
            &[("cf-connecting-ip", "198.51.100.4")],
        )
        .await;
    assert_eq!(tunnelled.status, StatusCode::OK);
    let refused = e.get(OUTSIDE, "/status", Some(&read), &[]).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.json()["error"]["code"], "FORBIDDEN");
    // A key is checked even from loopback: a bad one is not waved through.
    assert_eq!(
        e.get(LOCAL, "/status", Some("rpx_not_a_key"), &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        e.get(LOCAL, "/status", Some(&read), &[]).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_admin_allowlist_still_applies_to_the_admin_key() {
    let e = env(&[("ADMIN_IP_ALLOWLIST", "10.0.0.0/8")]).await;
    let admin = e.key("status-admin-ip", vec![Scope::Read, Scope::Admin]).await;
    assert_eq!(
        e.get(OUTSIDE, "/status", Some(&admin), &[]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.get("10.0.0.5:1", "/status", Some(&admin), &[]).await.status,
        StatusCode::OK
    );
}

/// design/14: a status request never calls Riot and answers in under 50 ms,
/// whatever Riot is doing.
#[tokio::test]
async fn a_slow_riot_gets_no_call_and_status_stays_fast() {
    let e = env(&[]).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&e.server)
        .await;
    // Warm the router's first build, which is not what is measured.
    e.get(LOCAL, "/status", None, &[]).await;
    let router = e.router(LOCAL);
    let started = Instant::now();
    let r = common::send(router, Request::get("/status").body(Body::empty()).unwrap()).await;
    let took = started.elapsed();
    assert_eq!(r.status, StatusCode::OK);
    assert!(took < Duration::from_millis(50), "took {took:?}");
    assert!(e.server.received_requests().await.unwrap().is_empty());
}

/// `consumer_requests_total` moves `requests_total_<consumer>` and its group's
/// count; a Riot 2xx sets `riot_last_success_at`. The status poll itself is
/// kept out of the recent-requests ring.
#[tokio::test]
async fn a_consumers_requests_show_in_its_counters() {
    let e = env(&[]).await;
    Mock::given(path("/lol/status/v4/platform-data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "EUW1"})))
        .mount(&e.server)
        .await;
    let web = e.key("Status-Web.Prod", vec![Scope::Read]).await;
    let before = e.get(LOCAL, "/status", None, &[]).await.json();
    let read = e.get(OUTSIDE, "/v1/lol/status/euw1", Some(&web), &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    let missing = e.get(OUTSIDE, "/v1/lol/status/nope1", Some(&web), &[]).await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    let after = e.get(LOCAL, "/status", None, &[]).await.json();
    let m = &after["metrics"];
    let delta = |k: &str| m[k].as_u64().unwrap() - before["metrics"][k].as_u64().unwrap_or(0);
    assert_eq!(m["requests_total_status_web_prod"], 2);
    assert_eq!(m["requests_errors_total_status_web_prod"], 0);
    assert!(delta("requests_total_lol") >= 2);
    assert!(
        delta("requests_total_ops") >= 1,
        "the first status request counts"
    );
    assert!(delta("requests_total") >= 3);
    assert!(delta("riot_requests_2xx_total") >= 1);
    let at = after["metrics_meta"]["riot_last_success_at"].as_str().unwrap();
    assert!(at.parse::<jiff::Timestamp>().is_ok(), "{at}");
    let ring = riot_proxy::http::recent::requests(riot_proxy::http::recent::REQUESTS_CAP);
    assert!(
        ring.iter().all(|r| r.path != "/status"),
        "status polls stay out of the ring"
    );
    assert!(ring.iter().any(|r| r.path == "/v1/lol/status/euw1"));
}

/// A refused key's 429 still names the consumer it refused.
#[tokio::test]
async fn a_quota_refusal_counts_under_the_consumer() {
    let e = env(&[]).await;
    let key = consumers::create(
        &e.state.db,
        NewConsumer {
            name: "status-quota-1".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 1,
            key: None,
        },
    )
    .await
    .unwrap()
    .key
    .expose()
    .to_string();
    e.get(OUTSIDE, "/v1/lol/status/nope1", Some(&key), &[]).await;
    let refused = e.get(OUTSIDE, "/v1/lol/status/nope1", Some(&key), &[]).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    let m = e.get(LOCAL, "/status", None, &[]).await.json()["metrics"].clone();
    assert_eq!(m["requests_total_status_quota_1"], 2);
}

/// A consumer's name is free text: one holding `,`, `}`, `"` and `=` keeps
/// its counts in `/status` (it is read back from the Prometheus rendering).
#[tokio::test]
async fn a_consumer_named_with_the_format_s_characters_is_counted() {
    let e = env(&[]).await;
    Mock::given(path("/lol/status/v4/platform-data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "EUW1"})))
        .mount(&e.server)
        .await;
    let key = e.key("Status \"Acme, Inc\"}=x", vec![Scope::Read]).await;
    let before = e.get(LOCAL, "/status", None, &[]).await.json();
    let read = e.get(OUTSIDE, "/v1/lol/status/euw1", Some(&key), &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    let after = e.get(LOCAL, "/status", None, &[]).await.json();
    let m = &after["metrics"];
    assert_eq!(m["requests_total_status__acme__inc___x"], 1, "{m}");
    assert_eq!(m["requests_errors_total_status__acme__inc___x"], 0);
    assert!(m.get("requests_total_status__acme").is_none(), "{m}");
    let delta = |k: &str| m[k].as_u64().unwrap() - before["metrics"][k].as_u64().unwrap_or(0);
    assert!(delta("requests_total") >= 2, "the read and the first status poll");
    assert!(delta("requests_total_lol") >= 1);
}

// ---- the checks (STS-02b, design/14 §Checks) ----

const PROBE: &str = "/lol/status/v4/platform-data";

async fn mock_probe(e: &Env, status: u16) {
    Mock::given(method("GET"))
        .and(path(PROBE))
        .respond_with(ResponseTemplate::new(status).set_body_string(r#"{"id":"OC1"}"#))
        .mount(&e.server)
        .await;
}

async fn round(e: &Env) {
    riot_proxy::status::checks::Checker::new(&e.state)
        .round(tokio::time::Instant::now())
        .await;
}

fn check<'a>(body: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {body}"))
}

async fn probes(e: &Env) -> usize {
    let got = e.server.received_requests().await.unwrap();
    got.iter().filter(|r| r.url.path() == PROBE).count()
}

#[tokio::test]
async fn a_round_fills_every_check() {
    let e = env(&[("STATUS_CHECK_PLATFORMS", "oc1,kr")]).await;
    mock_probe(&e, 200).await;
    round(&e).await;
    assert_eq!(probes(&e).await, 2, "the first platform and the other, at boot");
    let body = e.get(LOCAL, "/status", None, &[]).await.json();
    let names: Vec<&str> = body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["riot_api", "riot_key", "cache"], "no bucket, no storage");
    assert_eq!(body["status"], "ok", "{body}");
    for name in names {
        let c = check(&body, name);
        assert_eq!(
            (c["status"].as_str(), c["detail"].as_str()),
            (Some("ok"), Some("")),
            "{c}"
        );
        assert!(
            c["checked_at"]
                .as_str()
                .unwrap()
                .parse::<jiff::Timestamp>()
                .is_ok()
        );
    }
    assert!(check(&body, "riot_api")["latency_ms"].is_u64());
    assert!(check(&body, "riot_key")["latency_ms"].is_null());
    assert!(body["metrics_meta"]["riot_last_success_at"].is_string());
}

/// design/14: the answer is 200 even when `status` is `down`.
#[tokio::test]
async fn a_check_that_is_down_is_still_a_200() {
    let e = env(&[("STATUS_CHECK_PLATFORMS", "oc1")]).await;
    mock_probe(&e, 503).await;
    round(&e).await;
    let r = e.get(LOCAL, "/status", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(body["status"], "down");
    let api = check(&body, "riot_api");
    assert_eq!(
        (api["status"].as_str(), api["detail"].as_str()),
        (Some("down"), Some("oc1: Riot answered 503"))
    );
    assert_eq!(probes(&e).await, 1, "a probe is never retried");
}

/// A 403 is Riot answering: `riot_api` stays ok and `riot_key` goes down.
#[tokio::test]
async fn a_403_from_riot_makes_riot_key_down() {
    let e = env(&[("STATUS_CHECK_PLATFORMS", "oc1")]).await;
    mock_probe(&e, 403).await;
    round(&e).await;
    let body = e.get(LOCAL, "/status", None, &[]).await.json();
    assert_eq!(body["status"], "down");
    let key = check(&body, "riot_key");
    assert_eq!(
        (key["status"].as_str(), key["detail"].as_str()),
        (
            Some("down"),
            Some("Riot answered 403 to status.platformData on oc1")
        )
    );
    assert_eq!(check(&body, "riot_api")["status"], "ok");
}

/// The probe neither reads nor writes the response cache, so `/v1/lol/status`
/// still misses after one and still hits after another.
#[tokio::test]
async fn the_probe_skips_the_response_cache() {
    let e = env(&[("STATUS_CHECK_PLATFORMS", "oc1")]).await;
    mock_probe(&e, 200).await;
    let key = e.key("reader", vec![Scope::Read]).await;
    round(&e).await;
    let first = e.get(OUTSIDE, "/v1/lol/status/oc1", Some(&key), &[]).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.headers["x-cache"], "MISS", "the probe wrote nothing");
    round(&e).await;
    assert_eq!(probes(&e).await, 3, "the second probe read nothing either");
    assert_eq!(
        e.get(OUTSIDE, "/v1/lol/status/oc1", Some(&key), &[])
            .await
            .headers["x-cache"],
        "HIT"
    );
}

/// No bulk room within a second skips the probe; the check says so.
#[tokio::test]
async fn a_full_limiter_skips_the_probe() {
    let e = env(&[("STATUS_CHECK_PLATFORMS", "oc1")]).await;
    mock_probe(&e, 200).await;
    e.state.limiter.freeze(
        "oc1",
        Duration::from_secs(30),
        riot_proxy::riot::limiter::headers::RateLimitType::Service,
    );
    let started = Instant::now();
    round(&e).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a freeze past the budget fails at once"
    );
    assert_eq!(probes(&e).await, 0);
    let body = e.get(LOCAL, "/status", None, &[]).await.json();
    let api = check(&body, "riot_api");
    assert_eq!(api["status"], "degraded", "the freeze itself degrades it: {api}");
    let detail = api["detail"].as_str().unwrap();
    assert!(
        detail.contains("oc1: probe skipped, no room in the rate limiter"),
        "{detail}"
    );
    assert!(detail.contains("rate limited by Riot for "), "{detail}");
}

#[tokio::test]
async fn a_bucket_adds_the_storage_check() {
    let e = env(&[]).await;
    e.state
        .db
        .attach_objects(riot_proxy::archive::objects::Objects::in_memory());
    round(&e).await;
    let body = e.get(LOCAL, "/status", None, &[]).await.json();
    let storage = check(&body, "storage");
    assert_eq!(
        (storage["status"].as_str(), storage["detail"].as_str()),
        (Some("ok"), Some(""))
    );
    assert!(
        body["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"] != "riot_api")
    );
}

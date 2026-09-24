//! `/v1/riot/*` through the full app against a wiremock Riot: auth, passthrough,
//! cache headers, validation, admin-only refresh, the request metric, and v1
//! contract coverage.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    read: String,
    admin: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let make = |n: &str, scopes| NewConsumer {
        name: n.into(),
        scopes,
        quota_per_min: 600,
        key: None,
    };
    let read = consumers::create(&state.db, make("web", vec![Scope::Read]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    let admin = consumers::create(&state.db, make("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    Env {
        _dir: dir,
        server,
        state,
        router,
        read,
        admin,
    }
}

async fn get(e: &Env, uri: &str, key: Option<&str>) -> common::Reply {
    let mut req = Request::get(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    common::send(e.router.clone(), req.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn requires_a_key() {
    let e = env().await;
    let r = get(&e, "/v1/riot/accounts/by-riot-id/europe/Name/TAG", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn by_riot_id_passes_bytes_through_with_cache_headers() {
    let e = env().await;
    let body = br#"{"puuid":"P","gameName":"Hide on bush","tagLine":"KR1"}"#;
    Mock::given(path("/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.to_vec(), "application/json;charset=utf-8"),
        )
        .expect(1)
        .mount(&e.server)
        .await;

    let first = get(
        &e,
        "/v1/riot/accounts/by-riot-id/asia/Hide%20on%20bush/KR1",
        Some(&e.read),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body, body, "byte-identical");
    assert_eq!(first.headers["content-type"], "application/json; charset=utf-8");
    assert_eq!(first.headers["x-cache"], "MISS");
    assert_eq!(first.headers["x-cache-age"], "0");
    assert!(first.headers.contains_key("x-request-id"));
    assert_eq!(first.headers["x-ratelimit-limit"], "600");

    let second = get(
        &e,
        "/v1/riot/accounts/by-riot-id/asia/Hide%20on%20bush/KR1",
        Some(&e.read),
    )
    .await;
    assert_eq!(
        (
            second.headers["x-cache"].to_str().unwrap(),
            second.body.as_slice()
        ),
        ("HIT", body.as_slice())
    );
}

#[tokio::test]
async fn sea_is_accepted_and_routed_to_asia() {
    let e = env().await;
    Mock::given(path(format!("/riot/account/v1/accounts/by-puuid/{PUUID}")))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&e.server)
        .await;
    let r = get(
        &e,
        &format!("/v1/riot/accounts/by-puuid/sea/{PUUID}"),
        Some(&e.read),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        e.state.limiter.usage("asia")[0].used,
        1,
        "the token came from asia's bucket"
    );
    assert_eq!(e.state.limiter.usage("sea")[0].used, 0);
}

#[tokio::test]
async fn a_404_is_negative_cached_and_the_second_carries_hit_neg() {
    let e = env().await;
    Mock::given(path(format!("/riot/account/v1/accounts/by-puuid/{PUUID}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&e.server)
        .await;
    let uri = format!("/v1/riot/accounts/by-puuid/europe/{PUUID}");
    let first = get(&e, &uri, Some(&e.read)).await;
    assert_eq!(first.status, StatusCode::NOT_FOUND);
    assert!(!first.headers.contains_key("x-cache"));
    let second = get(&e, &uri, Some(&e.read)).await;
    assert_eq!(second.status, StatusCode::NOT_FOUND);
    assert_eq!(second.headers["x-cache"], "HIT-NEG");
    assert_eq!(second.json()["error"]["code"], "NOT_FOUND");

    let metrics = riot_proxy::telemetry::metrics_handle().unwrap().render();
    assert!(
        metrics.contains(r#"proxy_requests_total{route="/v1/riot/accounts/by-puuid/:region/:puuid",status="404",cache="neg"}"#),
        "{metrics}"
    );
}

#[tokio::test]
async fn validation_errors_use_v1_codes_and_never_reach_upstream() {
    let e = env().await;
    let cases = [
        (
            "/v1/riot/accounts/by-riot-id/EUROPE/Name/TAG",
            "BAD_REGION",
            "params/region must be equal to one of the allowed values",
        ),
        (
            "/v1/riot/accounts/by-riot-id/eu/Name/TAG",
            "BAD_REGION",
            "params/region must be equal to one of the allowed values",
        ),
        (
            "/v1/riot/accounts/by-riot-id/europe/ABCDEFGHIJKLMNOPQ/TAG",
            "VALIDATION",
            "params/gameName must NOT have more than 16 characters",
        ),
        (
            "/v1/riot/accounts/by-riot-id/europe/Name/TOOLONG",
            "VALIDATION",
            "params/tagLine must NOT have more than 5 characters",
        ),
        (
            "/v1/riot/accounts/by-puuid/europe/short",
            "VALIDATION",
            "params/puuid must NOT have fewer than 60 characters",
        ),
    ];
    for (uri, code, message) in cases {
        let r = get(&e, uri, Some(&e.read)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            (
                r.json()["error"]["code"].as_str().unwrap(),
                r.json()["error"]["message"].as_str().unwrap()
            ),
            (code, message),
            "{uri}"
        );
    }
    assert!(e.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn refresh_bypasses_only_for_admin_keys() {
    let e = env().await;
    Mock::given(path(format!("/riot/account/v1/accounts/by-puuid/{PUUID}")))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&e.server)
        .await;
    let uri = format!("/v1/riot/accounts/by-puuid/europe/{PUUID}");
    get(&e, &uri, Some(&e.read)).await;
    let ignored = get(&e, &format!("{uri}?refresh=true"), Some(&e.read)).await;
    assert_eq!(
        ignored.headers["x-cache"], "HIT",
        "a read key's refresh is ignored"
    );
    let bypass = get(&e, &format!("{uri}?refresh=true"), Some(&e.admin)).await;
    assert_eq!(bypass.headers["x-cache"], "BYPASS");
    assert_eq!(e.server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn upstream_auth_failure_is_sanitised() {
    let e = env().await;
    Mock::given(path(format!("/riot/account/v1/accounts/by-puuid/{PUUID}")))
        .respond_with(ResponseTemplate::new(403))
        .mount(&e.server)
        .await;
    let r = get(
        &e,
        &format!("/v1/riot/accounts/by-puuid/europe/{PUUID}"),
        Some(&e.read),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "UPSTREAM_ERROR");
}

/// Plan P4-04: OpenAPI compare, `/v1/riot/*` complete.
#[test]
fn every_v1_riot_operation_is_documented() {
    let v1: serde_json::Value =
        serde_json::from_str(include_str!("../docs/contract/v1-openapi.json")).unwrap();
    let ours: serde_json::Value =
        serde_json::from_str(&riot_proxy::routes::docs::spec().to_json().unwrap()).unwrap();
    let ops = |d: &serde_json::Value| -> Vec<String> {
        let mut v: Vec<String> = d["paths"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with("/v1/riot/"))
            .flat_map(|(p, item)| {
                item.as_object()
                    .unwrap()
                    .keys()
                    .filter(|m| m.as_str() == "get")
                    .map(move |m| format!("{m} {p}"))
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(ops(&ours), ops(&v1));
}

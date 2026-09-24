//! The HTTP skeleton: health routes, the error envelope on unrouted requests, layers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

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
        serde_json::json!({"ok": true, "sqlite": true, "limiter": true, "keyScope": key_scope()})
    );
}

#[tokio::test]
async fn readyz_is_503_with_the_same_body_when_sqlite_is_not_writable() {
    let (_dir, state, router) = app();
    // Hold the write lock from a second connection so BEGIN IMMEDIATE cannot get it
    // within busy_timeout… which is 5 s, longer than the 2 s probe timeout.
    let blocker = rusqlite::Connection::open(state.db.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res.json(),
        serde_json::json!({"ok": false, "sqlite": false, "limiter": true, "keyScope": key_scope()})
    );
    blocker.execute_batch("ROLLBACK;").unwrap();
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
async fn readyz_is_503_until_the_limiter_is_restored() {
    let (_dir, state, router) = app();
    state
        .limiter_restored
        .store(false, std::sync::atomic::Ordering::Release);
    let res = get(router, "/readyz").await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res.json(),
        serde_json::json!({"ok": false, "sqlite": true, "limiter": false, "keyScope": key_scope()})
    );
}

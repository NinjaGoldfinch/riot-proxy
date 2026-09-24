//! Auth through a real router: keys, revocation, scopes, the admin allowlist,
//! AUTH_DISABLED, and v1's 401/403 bodies (snapshots).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::http::auth::{Auth, Consumer, require_admin, require_read};
use riot_proxy::http::request_id::request_id;

async fn whoami(Extension(c): Extension<Arc<Consumer>>) -> String {
    c.name.clone()
}

/// `/read` needs `read`, `/admin` needs `admin`, `/public` needs nothing.
fn router(state: AppState, peer: &str) -> Router {
    let read = Router::new()
        .route("/read", get(whoami))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), require_read));
    let admin = Router::new()
        .route("/admin", get(whoami))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), require_admin));
    Router::new()
        .route("/public", get(|| async { "ok" }))
        .merge(read)
        .merge(admin)
        .with_state(state)
        .layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()))
        .layer(axum::middleware::from_fn(request_id))
}

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    read_key: String,
    admin_key: String,
}

async fn env(extra: &[(&str, &str)]) -> Env {
    let (dir, mut state, _) = common::app();
    let config = common::config(extra);
    state.auth = Arc::new(Auth::new(&config, state.db.clone()));
    state.config = config.into();
    let make = |name: &str, scopes: Vec<Scope>| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 600,
        key: None,
    };
    let read_key = consumers::create(&state.db, make("web", vec![Scope::Read]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    let admin_key = consumers::create(&state.db, make("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    Env {
        _dir: dir,
        state,
        read_key,
        admin_key,
    }
}

async fn call(router: Router, path: &str, key: Option<&str>, extra: &[(&str, &str)]) -> common::Reply {
    let mut req = Request::get(path);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    common::send(router, req.body(Body::empty()).unwrap()).await
}

fn redacted(reply: &common::Reply) -> serde_json::Value {
    let mut v = reply.json();
    v["error"]["requestId"] = "[request-id]".into();
    v
}

#[tokio::test]
async fn public_routes_need_no_key() {
    let e = env(&[]).await;
    assert_eq!(
        call(router(e.state, "127.0.0.1:1"), "/public", None, &[])
            .await
            .status,
        StatusCode::OK
    );
}

/// v1: "401s an unauthenticated request in the error envelope (§6.1)".
#[tokio::test]
async fn missing_key_is_401() {
    let e = env(&[]).await;
    let r = call(router(e.state, "127.0.0.1:1"), "/read", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    insta::assert_json_snapshot!("auth_401_missing_key", redacted(&r));
}

/// v1: "401s a syntactically valid but unknown key".
#[tokio::test]
async fn unknown_key_is_401() {
    let e = env(&[]).await;
    let r = call(
        router(e.state, "127.0.0.1:1"),
        "/read",
        Some("rpx_000000000000000000000000000000000000"),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn a_valid_key_passes_and_the_handler_sees_its_consumer() {
    let e = env(&[]).await;
    let r = call(
        router(e.state.clone(), "127.0.0.1:1"),
        "/read",
        Some(&e.read_key),
        &[],
    )
    .await;
    assert_eq!(
        (r.status, String::from_utf8(r.body).unwrap()),
        (StatusCode::OK, "web".into())
    );
    // ?token= works too (v1: WS handshakes cannot set headers).
    let r = call(
        router(e.state, "127.0.0.1:1"),
        &format!("/read?token={}", e.read_key),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn a_revoked_key_is_401_once_its_cache_entry_is_gone() {
    let e = env(&[]).await;
    let app = router(e.state.clone(), "127.0.0.1:1");
    assert_eq!(
        call(app.clone(), "/read", Some(&e.read_key), &[]).await.status,
        StatusCode::OK
    );
    consumers::revoke(&e.state.db, "web".into()).await.unwrap();
    // The admin API invalidates on revoke (P5-05); a CLI revoke waits out the 60 s TTL.
    e.state.auth.invalidate(consumers::hash_key(&e.read_key)).await;
    assert_eq!(
        call(app, "/read", Some(&e.read_key), &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
}

/// v1: "403s a read key on an admin route (§12.1)".
#[tokio::test]
async fn a_read_key_on_an_admin_route_is_403() {
    let e = env(&[]).await;
    let r = call(router(e.state, "127.0.0.1:1"), "/admin", Some(&e.read_key), &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    insta::assert_json_snapshot!("auth_403_missing_scope", redacted(&r));
}

#[tokio::test]
async fn an_admin_key_passes_read_and_admin_routes() {
    let e = env(&[]).await;
    for path in ["/read", "/admin"] {
        let r = call(
            router(e.state.clone(), "127.0.0.1:1"),
            path,
            Some(&e.admin_key),
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn the_admin_allowlist_applies_to_admin_routes_only() {
    let e = env(&[("ADMIN_IP_ALLOWLIST", "127.0.0.1,10.0.0.0/8")]).await;
    let ok = call(
        router(e.state.clone(), "10.2.3.4:9"),
        "/admin",
        Some(&e.admin_key),
        &[],
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);

    let denied = call(
        router(e.state.clone(), "203.0.113.9:9"),
        "/admin",
        Some(&e.admin_key),
        &[],
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    insta::assert_json_snapshot!("auth_403_admin_ip", redacted(&denied));

    // X-Forwarded-For is trusted (v1 trustProxy): a proxy on 10.x forwarding an outsider is refused.
    let via_proxy = call(
        router(e.state.clone(), "10.0.0.2:9"),
        "/admin",
        Some(&e.admin_key),
        &[("x-forwarded-for", "203.0.113.9")],
    )
    .await;
    assert_eq!(via_proxy.status, StatusCode::FORBIDDEN);

    // Read routes are not allowlisted.
    let read = call(router(e.state, "203.0.113.9:9"), "/read", Some(&e.read_key), &[]).await;
    assert_eq!(read.status, StatusCode::OK);
}

#[tokio::test]
async fn auth_disabled_runs_everything_as_dev_local() {
    let e = env(&[("AUTH_DISABLED", "true"), ("ADMIN_IP_ALLOWLIST", "127.0.0.1")]).await;
    for path in ["/read", "/admin"] {
        let r = call(router(e.state.clone(), "203.0.113.9:9"), path, None, &[]).await;
        assert_eq!(
            (r.status, String::from_utf8(r.body).unwrap()),
            (StatusCode::OK, "dev-local".into()),
            "{path}"
        );
    }
}

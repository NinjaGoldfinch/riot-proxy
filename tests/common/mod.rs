//! Shared fixtures for integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use riot_proxy::app::{self, AppState};
use riot_proxy::config::{Config, Sources};
use riot_proxy::db::Db;
use riot_proxy::telemetry;
use tower::ServiceExt;

pub const TEST_KEY: &str = "RGAPI-test-key-not-real";

pub fn config(extra: &[(&str, &str)]) -> Config {
    let mut env = vec![("RIOT_API_KEY".to_string(), TEST_KEY.to_string())];
    env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Config::from_sources(Sources {
        env,
        ..Sources::default()
    })
    .expect("test config")
}

/// A full app over a fresh SQLite file. Keep the TempDir alive for the test.
pub fn app() -> (tempfile::TempDir, AppState, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).expect("db");
    let state = AppState {
        config: config(&[]).into(),
        db,
        limiter: std::sync::Arc::new(riot_proxy::riot::limiter::Limiter::new(0.8)),
        limiter_restored: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let router = app::router(state.clone(), telemetry::metrics_handle().expect("metrics"));
    (dir, state, router)
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("json body")
    }
}

pub async fn send(router: Router, req: Request<Body>) -> Reply {
    let res = router.oneshot(req).await.expect("response");
    let status = res.status();
    let headers = res.headers().clone();
    let body = to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

pub async fn get(router: Router, path: &str) -> Reply {
    send(router, Request::get(path).body(Body::empty()).unwrap()).await
}

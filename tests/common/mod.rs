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
    let config = config(&[]);
    let limiter = std::sync::Arc::new(riot_proxy::riot::limiter::Limiter::new(0.8));
    let fetcher = fetcher(
        &config,
        "http://127.0.0.1:9",
        std::sync::Arc::clone(&limiter),
        None,
    );
    let auth = std::sync::Arc::new(riot_proxy::http::auth::Auth::new(&config, db.clone()));
    let state = AppState {
        config: config.into(),
        db,
        auth,
        quotas: std::sync::Arc::new(riot_proxy::http::quota::Quotas::new()),
        limiter,
        fetcher,
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

/// A fetcher pointed at `upstream` (a wiremock URI), with an optional test archive.
pub fn fetcher(
    config: &Config,
    upstream: &str,
    limiter: std::sync::Arc<riot_proxy::riot::limiter::Limiter>,
    archive: Option<std::sync::Arc<dyn riot_proxy::fetcher::Archive>>,
) -> riot_proxy::fetcher::Fetcher {
    use riot_proxy::fetcher::{Fetcher, FetcherParts, NoArchive};
    Fetcher::new(FetcherParts {
        client: riot_proxy::riot::client::RiotClient::with_base_url(config, upstream).expect("client"),
        limiter,
        cache: std::sync::Arc::new(riot_proxy::cache::ResponseCache::new(
            riot_proxy::cache::l1::L1::new(16 * 1024 * 1024),
            None,
        )),
        archive: archive.unwrap_or_else(|| std::sync::Arc::new(NoArchive)),
        scope: riot_proxy::cache::keys::KeyScope::from_key(&config.riot_api_key),
        policy: riot_proxy::riot::endpoints::TtlPolicy::from_config(config),
        interactive_budget: std::time::Duration::from_millis(config.client_wait_budget_ms),
        swr: config.stale_while_revalidate,
    })
}

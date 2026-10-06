//! `/dev` and `/dashboard`: two browser pages embedded in the binary (design/07
//! §The artefact) and served without a key. `/dev` is the dev explorer (design/10,
//! ADR-071), never served in production; `DASHBOARD_UI` defaults on (the page is
//! inert, and everything behind it needs an admin key).
//! The dashboard is v1's `public/dashboard.html` with its data wired (P7-06); its
//! only change is following v2's `crawl.phase` event (ADR-058).

use std::sync::Arc;

use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::config::{Config, Environment};
use crate::riot::routing::{Platform, Region};

pub const DEV_UI_HTML: &str = include_str!("../ui/dev-ui.html");
pub const DASHBOARD_HTML: &str = include_str!("../ui/dashboard.html");

const NO_STORE: (header::HeaderName, &str) = (header::CACHE_CONTROL, "no-store");

fn page(html: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), NO_STORE],
        html,
    )
        .into_response()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevConfig {
    auth_disabled: bool,
    version: &'static str,
    env: &'static str,
    docs_ui: bool,
    dashboard_ui: bool,
    regions: Vec<Region>,
    platforms: Vec<PlatformOption>,
}

#[derive(Debug, Serialize)]
struct PlatformOption {
    value: Platform,
    label: &'static str,
    region: Region,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardConfig {
    auth_disabled: bool,
}

/// A `no-store` JSON response of a body computed once at startup.
fn json_route(body: serde_json::Value) -> axum::routing::MethodRouter {
    get(move || {
        let body = body.clone();
        async move { ([NO_STORE], Json(body)).into_response() }
    })
}

/// The UI routes the config enables (possibly none). `openapi_json` is the
/// finished document; the explorer builds its forms from it even when `DOCS_UI`
/// is off.
pub fn router(config: &Config, openapi_json: Arc<str>) -> Router {
    let mut router = Router::new();
    if config.dev_ui {
        let dev = DevConfig {
            auth_disabled: config.auth_disabled,
            version: env!("CARGO_PKG_VERSION"),
            env: match config.env {
                Environment::Development => "development",
                Environment::Test => "test",
                Environment::Production => "production",
            },
            docs_ui: config.docs_ui,
            dashboard_ui: config.dashboard_ui,
            regions: Region::ALL.to_vec(),
            platforms: Platform::ALL
                .iter()
                .map(|&p| PlatformOption {
                    value: p,
                    label: p.label(),
                    region: p.region(),
                })
                .collect(),
        };
        router = router
            .route("/dev", get(|| async { page(DEV_UI_HTML) }))
            .route(
                "/dev/config.json",
                json_route(serde_json::to_value(dev).unwrap_or_default()),
            )
            .route(
                "/dev/openapi.json",
                get(move || {
                    let json = Arc::clone(&openapi_json);
                    async move {
                        (
                            [(header::CONTENT_TYPE, "application/json"), NO_STORE],
                            json.to_string(),
                        )
                            .into_response()
                    }
                }),
            );
    }
    if config.dashboard_ui {
        let dash = DashboardConfig {
            auth_disabled: config.auth_disabled,
        };
        router = router
            .route("/dashboard", get(|| async { page(DASHBOARD_HTML) }))
            .route(
                "/dashboard/config.json",
                json_route(serde_json::to_value(dash).unwrap_or_default()),
            );
    }
    router
}

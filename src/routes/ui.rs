//! `/dev` and `/dashboard`: v1's two browser pages, embedded in the binary
//! (design/07 §The artefact) and served without a key, as in v1. Each is gated by
//! its flag: `DEV_UI` (default off in production) and `DASHBOARD_UI` (default on;
//! the page is inert, and everything behind it needs an admin key).
//! The pages are v1's `public/*.html`, verbatim; the dashboard's data is wired in P7-06.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::config::Config;
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
    default_platform: Platform,
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

/// The UI routes the config enables (possibly none).
pub fn router(config: &Config) -> Router {
    let mut router = Router::new();
    if config.dev_ui {
        let dev = DevConfig {
            auth_disabled: config.auth_disabled,
            default_platform: config.default_platform,
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
            // Client-side routes (e.g. /dev/Name-TAG) get the same document (v1).
            .route("/dev/{*rest}", get(|| async { page(DEV_UI_HTML) }))
            .route(
                "/dev/config.json",
                json_route(serde_json::to_value(dev).unwrap_or_default()),
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

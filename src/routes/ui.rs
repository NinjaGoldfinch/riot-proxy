//! `/dev`, `/dev/showcase` and `/dashboard`: browser pages embedded in the binary
//! (design/07 §The artefact) and served without a key. `/dev` is the dev explorer
//! (design/10, ADR-071) and `/dev/showcase` an example frontend (design/11,
//! ADR-080); neither is ever served in production. `DASHBOARD_UI` defaults on (the
//! page is inert, and everything behind it needs an admin key).
//! The dashboard is v1's `public/dashboard.html` with its data wired (P7-06); its
//! only change is following v2's `crawl.phase` event (ADR-058).
//!
//! Both pages carry a page bar at the top (DEV-10, ADR-078), rendered here once at
//! startup from the config, so it links only to pages that exist: the dashboard
//! in production never offers `/dev`. It is a `div`, not a `nav`, so neither
//! page's own `nav` rules reach it.

use std::sync::Arc;

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::config::{Config, Environment};
use crate::riot::routing::{Platform, Region};

pub const DEV_UI_HTML: &str = include_str!("../ui/dev-ui.html");
pub const DASHBOARD_HTML: &str = include_str!("../ui/dashboard.html");
pub const SHOWCASE_HTML: &str = include_str!("../ui/showcase.html");

const NO_STORE: (header::HeaderName, &str) = (header::CACHE_CONTROL, "no-store");

/// Where each page's HTML wants the page bar.
pub const PAGEBAR_MARK: &str = "<!-- pagebar -->";

/// The browser pages this config serves, in bar order, as `(href, label)`.
pub fn pages(config: &Config) -> Vec<(&'static str, &'static str)> {
    [
        (config.dashboard_ui, "/dashboard", "Dashboard"),
        (config.dev_ui, "/dev", "Dev explorer"),
        (config.dev_ui, "/dev/showcase", "Showcase"),
        (config.docs_ui, "/docs", "API docs"),
        (true, "/metrics", "Metrics"),
    ]
    .into_iter()
    .filter(|(on, ..)| *on)
    .map(|(_, href, label)| (href, label))
    .collect()
}

/// The bar across the top of the page at `current`: every page from [`pages`],
/// the current one marked, and the version and environment on the right.
pub fn pagebar(config: &Config, current: &str) -> String {
    let links: String = pages(config)
        .into_iter()
        .map(|(href, label)| {
            let here = if href == current {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(r#"<a href="{href}"{here}>{label}</a>"#)
        })
        .collect();
    format!(
        r#"<style>
.rp-bar {{ position: sticky; top: 0; margin: 0; box-sizing: border-box; z-index: 100; display: flex; align-items: center; gap: 2px; height: 34px; padding: 0 12px; background: #0b0e13; border-bottom: 1px solid #262d38; font: 13px/1 system-ui, sans-serif; }}
.rp-bar b {{ font: 600 12px ui-monospace, Menlo, Consolas, monospace; color: #7d8794; margin-right: 10px; letter-spacing: .04em; }}
.rp-bar a {{ color: #7d8794; text-decoration: none; padding: 9px 10px; border-bottom: 2px solid transparent; }}
.rp-bar a:hover {{ color: #d7dee8; }}
.rp-bar a[aria-current] {{ color: #d7dee8; border-color: #58a6ff; }}
.rp-bar span {{ margin-left: auto; font: 11px ui-monospace, Menlo, Consolas, monospace; color: #7d8794; }}
</style>
<div class="rp-bar" role="navigation" aria-label="riot-proxy pages"><b>riot-proxy</b>{links}<span>v{version} · {env}</span></div>"#,
        version = env!("CARGO_PKG_VERSION"),
        env = env_name(config.env),
    )
}

fn env_name(env: Environment) -> &'static str {
    match env {
        Environment::Development => "development",
        Environment::Test => "test",
        Environment::Production => "production",
    }
}

/// `html` with the page bar for `current` in place of [`PAGEBAR_MARK`].
pub fn render(html: &str, config: &Config, current: &str) -> String {
    html.replacen(PAGEBAR_MARK, &pagebar(config, current), 1)
}

/// A `no-store` HTML response of a page rendered once at startup.
fn page(html: String) -> axum::routing::MethodRouter {
    let html: Arc<str> = html.into();
    get(move || {
        let html = Arc::clone(&html);
        async move {
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8"), NO_STORE],
                html.to_string(),
            )
                .into_response()
        }
    })
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
            env: env_name(config.env),
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
            .route("/dev", page(render(DEV_UI_HTML, config, "/dev")))
            .route(
                "/dev/showcase",
                page(render(SHOWCASE_HTML, config, "/dev/showcase")),
            )
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
            .route("/dashboard", page(render(DASHBOARD_HTML, config, "/dashboard")))
            .route(
                "/dashboard/config.json",
                json_route(serde_json::to_value(dash).unwrap_or_default()),
            );
    }
    router
}

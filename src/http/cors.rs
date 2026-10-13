//! Opt-in CORS for the admin API (ADM-07, design/13 §Reach): only when
//! `ADMIN_CORS_ORIGINS` lists origins, and only on the admin routes, `/v1/ws`
//! and the health routes, so the dashboard can run from another origin. The
//! layer sits outside the admin key check and the router's 404 fallbacks
//! (axum answers an `OPTIONS` no route handles there), so a preflight needs no
//! key; the request that follows still does. `/v1/riot/*`, `/v1/lol/*` and the other
//! read routes never send CORS headers.

use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower::{Layer, ServiceExt};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// How long a browser may cache a preflight.
pub const MAX_AGE: Duration = Duration::from_secs(600);

/// The layer for these origins; `None` when there are none (the default).
pub fn admin(origins: &[String]) -> Option<CorsLayer> {
    let list: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    if list.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(list))
            .allow_methods([Method::GET, Method::POST, Method::DELETE])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .expose_headers([HeaderName::from_static("x-request-id")])
            .max_age(MAX_AGE),
    )
}

/// Whether CORS applies to `path`: the admin API, `/v1/ws` and the health routes.
pub fn covers(path: &str) -> bool {
    path.starts_with("/v1/admin/") || matches!(path, "/v1/ws" | "/healthz" | "/readyz")
}

/// `from_fn_with_state(layer, cors::scoped)` on the whole router: CORS for
/// the paths it [`covers`], nothing at all for the rest.
pub async fn scoped(State(cors): State<CorsLayer>, req: Request, next: Next) -> Response {
    if !covers(req.uri().path()) {
        return next.run(req).await;
    }
    match cors.layer(next).oneshot(req).await {
        Ok(res) => res,
        Err(never) => match never {},
    }
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_admin_api_the_socket_and_the_probes_only() {
        for p in [
            "/v1/admin/info",
            "/v1/admin/jobs/x/retry",
            "/v1/ws",
            "/healthz",
            "/readyz",
        ] {
            assert!(covers(p), "{p}");
        }
        for p in [
            "/v1/admin",
            "/v1/lol/x",
            "/v1/riot/x",
            "/v1/players/x",
            "/metrics",
            "/dashboard",
            "/v1/ws/x",
        ] {
            assert!(!covers(p), "{p}");
        }
    }

    #[test]
    fn no_origins_no_layer() {
        assert!(admin(&[]).is_none());
        assert!(admin(&["https://a.example".into()]).is_some());
    }
}

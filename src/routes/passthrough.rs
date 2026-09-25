//! What every Riot-backed route shares: Riot's bytes out untouched, `X-Cache` and
//! `X-Cache-Age` (v1 `routes/helpers.ts`), errors in the envelope, and the
//! admin-only `?refresh=true`.

use std::collections::HashMap;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::consumers::Scope;
use crate::fetcher::{FetchError, FetchOptions, FetchResult};
use crate::http::auth::Consumer;
use crate::http::error::ErrorResponse;

/// The responses every Riot-backed route documents (v1 `upstreamErrors`).
#[derive(utoipa::IntoResponses)]
pub enum PassthroughResponses {
    /// Riot's payload, unmodified. `X-Cache` and `X-Cache-Age` describe where it came from.
    #[response(status = 200, content_type = "application/json")]
    Ok,
    /// A parameter failed validation (`VALIDATION`, or `BAD_REGION` for a platform or region).
    #[response(status = 400)]
    BadRequest(ErrorResponse),
    /// Missing or invalid key.
    #[response(status = 401)]
    Unauthorized(ErrorResponse),
    /// The key lacks a scope.
    #[response(status = 403)]
    Forbidden(ErrorResponse),
    /// Not found upstream; a cached 404 carries `X-Cache: HIT-NEG`.
    #[response(status = 404)]
    NotFound(ErrorResponse),
    /// Your consumer quota is spent (`QUOTA_EXCEEDED`).
    #[response(status = 429)]
    Quota(ErrorResponse),
    /// Riot failed or rejected the proxy's key (`UPSTREAM_ERROR`).
    #[response(status = 502)]
    Upstream(ErrorResponse),
    /// Riot's rate limit budget is exhausted (`RATE_LIMITED`, with `Retry-After`).
    #[response(status = 503)]
    RateLimited(ErrorResponse),
}

/// Riot answers JSON; so does the proxy (v1 served `application/json; charset=utf-8`).
pub const JSON: &str = "application/json; charset=utf-8";

/// Build the request for `endpoint_id` and fetch it; validation errors never
/// reach upstream.
pub async fn fetch(
    state: &crate::app::AppState,
    consumer: &Consumer,
    query: &HashMap<String, String>,
    build: impl FnOnce() -> Result<crate::riot::client::RiotRequest, crate::http::ApiError>,
) -> Response {
    match build() {
        Ok(req) => respond(state.fetcher.fetch(req, options(consumer, query)).await),
        Err(e) => e.into_response(),
    }
}

/// A request for `endpoint_id` on `target` with path `params` and `query`.
pub fn request(
    endpoint_id: &str,
    target: Option<crate::riot::endpoints::Target>,
    params: &[&str],
    query: &[(&str, Option<String>)],
) -> Result<crate::riot::client::RiotRequest, crate::http::ApiError> {
    use crate::http::ApiError;
    let ep = crate::riot::endpoints::Endpoint::by_id(endpoint_id).ok_or_else(ApiError::internal)?;
    let target = target.ok_or_else(ApiError::internal)?;
    let mut req =
        crate::riot::client::RiotRequest::new(ep, target, params).map_err(|_| ApiError::internal())?;
    for (k, v) in query {
        req = req.query(k, v.clone()).map_err(|_| ApiError::internal())?;
    }
    Ok(req)
}

pub fn respond(outcome: Result<FetchResult, FetchError>) -> Response {
    match outcome {
        Ok(r) => {
            let mut res = (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], r.body).into_response();
            let h = res.headers_mut();
            h.insert("x-cache", HeaderValue::from_static(r.x_cache.as_str()));
            // v1 rounded the age to whole seconds.
            let age = (r.cache_age.as_millis() + 500) / 1000;
            h.insert(
                "x-cache-age",
                HeaderValue::from(u64::try_from(age).unwrap_or(u64::MAX)),
            );
            res
        }
        Err(e) => {
            let mut res = e.api.into_response();
            if let Some(x) = e.x_cache {
                res.headers_mut()
                    .insert("x-cache", HeaderValue::from_static(x.as_str()));
            }
            res
        }
    }
}

/// `?refresh=true` skips the cache (`X-Cache: BYPASS`), for admin keys only
/// (design/03). For anyone else it is ignored, as v1 dropped unknown query params.
pub fn options(consumer: &Consumer, query: &HashMap<String, String>) -> FetchOptions {
    let refresh = query.get("refresh").is_some_and(|v| v == "true" || v == "1");
    FetchOptions {
        bypass: refresh && consumer.has(Scope::Admin),
        ..FetchOptions::default()
    }
}

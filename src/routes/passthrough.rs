//! What every Riot-backed route shares: Riot's bytes out untouched, `X-Cache` and
//! `X-Cache-Age` (v1 `routes/helpers.ts`), errors in the envelope, and the
//! admin-only `?refresh=true`.

use std::collections::HashMap;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::consumers::Scope;
use crate::fetcher::{FetchError, FetchOptions, FetchResult};
use crate::http::auth::Consumer;

/// Riot answers JSON; so does the proxy (v1 served `application/json; charset=utf-8`).
pub const JSON: &str = "application/json; charset=utf-8";

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

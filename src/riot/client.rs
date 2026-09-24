//! One HTTP call to Riot and a classification of what came back. No retries and no
//! limiter here: the limiter (P2) and fetcher (P3-05) own those. v1 did both in
//! its client (`src/riot/client.ts`); the policy moves, it doesn't disappear.
//!
//! Status policy is v1 spec §5.5 / §9.4: 404 → not found; 401/403 → the key is bad,
//! logged at error; 429 → rate limited, typed or not; 5xx and network → upstream
//! unavailable.

use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, header};
use bytes::Bytes;

use crate::config::{Config, Secret};
use crate::http::{ApiError, ErrorCode};
use crate::metrics::{RL_429_TOTAL, UPSTREAM_LATENCY_SECONDS, UPSTREAM_REQUESTS_TOTAL};
use crate::riot::endpoints::{Endpoint, PathError, Target, encode_component};

/// v1 undici pool: 32 connections per host, 60 s keep-alive.
const POOL_MAX_IDLE_PER_HOST: usize = 32;
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// v1: 10 s for headers + 15 s for the body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

pub const X_RIOT_TOKEN: &str = "x-riot-token";
pub const X_RATE_LIMIT_TYPE: &str = "x-rate-limit-type";

/// A request resolved to a host, path and query, ready to send.
#[derive(Debug, Clone)]
pub struct RiotRequest {
    pub endpoint: &'static Endpoint,
    pub target: Target,
    pub path: String,
    /// Path parameter values, percent-encoded as they appear in `path` (cache keys).
    pub params: Vec<String>,
    pub query: Vec<(&'static str, String)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("{id} does not take the query parameter '{key}'")]
    UnknownQuery { id: &'static str, key: String },
}

impl RiotRequest {
    pub fn new(endpoint: &'static Endpoint, target: Target, params: &[&str]) -> Result<Self, RequestError> {
        Ok(Self {
            endpoint,
            target,
            path: endpoint.path(params)?,
            params: params.iter().map(|p| encode_component(p)).collect(),
            query: Vec::new(),
        })
    }

    /// Add a query parameter the endpoint declares. `None` or empty values are
    /// omitted, as v1 `buildPath` did.
    pub fn query(mut self, key: &str, value: Option<impl ToString>) -> Result<Self, RequestError> {
        let Some(&key) = self.endpoint.query.iter().find(|k| **k == key) else {
            return Err(RequestError::UnknownQuery {
                id: self.endpoint.id,
                key: key.to_string(),
            });
        };
        if let Some(value) = value.map(|v| v.to_string()).filter(|v| !v.is_empty()) {
            self.query.push((key, value));
        }
        Ok(self)
    }

    /// Path plus query string, `URLSearchParams` encoding (v1 `buildPath`).
    pub fn path_and_query(&self) -> String {
        if self.query.is_empty() {
            return self.path.clone();
        }
        let qs: Vec<String> = self
            .query
            .iter()
            .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
            .collect();
        format!("{}?{}", self.path, qs.join("&"))
    }
}

/// `application/x-www-form-urlencoded`, as `URLSearchParams.toString()` produces it.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_') => {
                out.push(char::from(b))
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A 2xx response. `body` is Riot's bytes, untouched (after transfer decoding).
#[derive(Debug, Clone)]
pub struct RiotResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Round trip, excluding any limiter wait.
    pub upstream_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiotErrorKind {
    /// Status 404.
    NotFound,
    /// 401 or 403: the key is expired, revoked or blacklisted. Never retry.
    UpstreamAuth,
    /// Status 429. `limit_type` is `X-Rate-Limit-Type`, absent for service-level 429s
    /// that must not touch our buckets. `retry_after` is `Retry-After` in seconds.
    RateLimited {
        limit_type: Option<String>,
        retry_after: Option<u64>,
    },
    /// 5xx, or the request never completed (`status` is `None`).
    UpstreamUnavailable,
    /// Any other non-2xx status.
    UnexpectedStatus,
}

/// A non-2xx outcome. Carries the response headers because the limiter must
/// observe every response, errors included (v1 §9.1).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{method} on {host}: {kind:?} (status {status:?})")]
pub struct RiotError {
    pub kind: RiotErrorKind,
    pub method: &'static str,
    pub host: String,
    pub status: Option<u16>,
    /// Boxed to keep `Result<_, RiotError>` small.
    pub headers: Box<HeaderMap>,
}

impl RiotError {
    /// v1 `RiotError.toProxyError`: what the client of the proxy is told. Never
    /// says the Riot key was rejected; that is an operator problem.
    pub fn to_api_error(&self) -> ApiError {
        match &self.kind {
            RiotErrorKind::NotFound => ApiError::not_found("Resource not found upstream"),
            RiotErrorKind::UpstreamAuth => {
                ApiError::new(ErrorCode::UpstreamError, "Upstream authentication failed")
            }
            RiotErrorKind::RateLimited { retry_after, .. } => {
                ApiError::new(ErrorCode::RateLimited, "Upstream rate limited")
                    .with_retry_after(retry_after.unwrap_or(1))
            }
            RiotErrorKind::UpstreamUnavailable | RiotErrorKind::UnexpectedStatus => ApiError::upstream(),
        }
    }
}

impl From<RiotError> for ApiError {
    fn from(e: RiotError) -> Self {
        e.to_api_error()
    }
}

/// Where requests go. Tests point every host at one mock server.
#[derive(Debug, Clone)]
enum Base {
    Https,
    Fixed(String),
}

#[derive(Debug, Clone)]
pub struct RiotClient {
    http: reqwest::Client,
    token: HeaderValue,
    base: Base,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientBuildError {
    #[error("RIOT_API_KEY is not a valid header value")]
    Token,
    #[error("could not build the HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

impl RiotClient {
    pub fn new(config: &Config) -> Result<Self, ClientBuildError> {
        Self::build(&config.riot_api_key, &config.riot_user_agent, Base::Https)
    }

    /// Send every request to `base_url` (e.g. a wiremock server) instead of Riot.
    pub fn with_base_url(config: &Config, base_url: &str) -> Result<Self, ClientBuildError> {
        Self::build(
            &config.riot_api_key,
            &config.riot_user_agent,
            Base::Fixed(base_url.trim_end_matches('/').into()),
        )
    }

    fn build(key: &Secret, user_agent: &str, base: Base) -> Result<Self, ClientBuildError> {
        let mut token = HeaderValue::from_str(key.expose()).map_err(|_| ClientBuildError::Token)?;
        // Redacted from HeaderMap's Debug output, so it never reaches a log line.
        token.set_sensitive(true);
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // Embedded roots only: a FROM scratch image has no system CA store (ADR-007).
            .tls_certs_only(webpki_roots())
            .build()?;
        Ok(Self { http, token, base })
    }

    fn url(&self, req: &RiotRequest) -> String {
        match &self.base {
            Base::Https => format!("https://{}{}", req.target.host(), req.path_and_query()),
            Base::Fixed(base) => format!("{base}{}", req.path_and_query()),
        }
    }

    /// Perform one GET and classify it. Records `proxy_upstream_requests_total`,
    /// `proxy_upstream_latency_seconds` and, on 429, `proxy_rl_429_total`.
    pub async fn send(&self, req: &RiotRequest) -> Result<RiotResponse, RiotError> {
        let scope = req.target.scope();
        let method = req.endpoint.id;
        let host = req.target.host();
        let started = Instant::now();

        let result = self
            .http
            .get(self.url(req))
            // v1 §5.2: the key goes in a header, never the query string.
            .header(X_RIOT_TOKEN, self.token.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await;
        let (res, body) = match result {
            Ok(res) => {
                let status = res.status();
                let headers = res.headers().clone();
                match res.bytes().await {
                    Ok(body) => ((status.as_u16(), headers), body),
                    Err(e) => return Err(self.network_failure(scope, method, host, started, &e)),
                }
            }
            Err(e) => return Err(self.network_failure(scope, method, host, started, &e)),
        };
        let ((status, headers), elapsed) = (res, started.elapsed());
        record(scope, method, &status.to_string(), elapsed);

        if (200..300).contains(&status) {
            #[allow(clippy::cast_possible_truncation)]
            let upstream_ms = elapsed.as_millis() as u64;
            return Ok(RiotResponse {
                status,
                headers,
                body,
                upstream_ms,
            });
        }

        let kind = match status {
            404 => RiotErrorKind::NotFound,
            401 | 403 => {
                tracing::error!(status, method, host = %host, "RIOT KEY REJECTED — check RIOT_API_KEY (dev keys expire every 24h)");
                RiotErrorKind::UpstreamAuth
            }
            429 => {
                let limit_type = header_str(&headers, X_RATE_LIMIT_TYPE).map(str::to_string);
                let retry_after =
                    header_str(&headers, header::RETRY_AFTER.as_str()).and_then(|v| v.trim().parse().ok());
                metrics::counter!(RL_429_TOTAL, "region" => scope, "type" => limit_type.clone().unwrap_or_else(|| "service".into()))
                    .increment(1);
                RiotErrorKind::RateLimited {
                    limit_type,
                    retry_after,
                }
            }
            500..=599 => {
                tracing::warn!(status, method, "upstream 5xx");
                RiotErrorKind::UpstreamUnavailable
            }
            _ => {
                let snippet = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
                tracing::warn!(status, method, body = %snippet, "unexpected upstream status");
                RiotErrorKind::UnexpectedStatus
            }
        };
        Err(RiotError {
            kind,
            method,
            host,
            status: Some(status),
            headers: Box::new(headers),
        })
    }

    fn network_failure(
        &self,
        scope: &'static str,
        method: &'static str,
        host: String,
        started: Instant,
        err: &reqwest::Error,
    ) -> RiotError {
        record(scope, method, "network", started.elapsed());
        // reqwest errors carry the URL, which never contains the key (header-only).
        tracing::warn!(method, host = %host, error = %err, "network failure contacting Riot API");
        RiotError {
            kind: RiotErrorKind::UpstreamUnavailable,
            method,
            host,
            status: None,
            headers: Box::default(),
        }
    }
}

fn record(scope: &'static str, method: &'static str, status: &str, elapsed: Duration) {
    metrics::counter!(UPSTREAM_REQUESTS_TOTAL, "region" => scope, "method" => method, "status" => status.to_string())
        .increment(1);
    metrics::histogram!(UPSTREAM_LATENCY_SECONDS, "region" => scope, "method" => method)
        .record(elapsed.as_secs_f64());
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn webpki_roots() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riot::endpoints::Endpoint;
    use crate::riot::routing::{Platform, Region};

    fn ep(id: &str) -> &'static Endpoint {
        Endpoint::by_id(id).unwrap()
    }

    /// v1 "serialises query params and omits empty ones".
    #[test]
    fn query_serialises_and_omits_empty_values() {
        let e = ep("match.idsByPuuid");
        let req = RiotRequest::new(e, e.target_for_region(Region::Europe).unwrap(), &["P"])
            .unwrap()
            .query("start", Some(0))
            .unwrap()
            .query("count", Some(20))
            .unwrap()
            .query("queue", None::<u32>)
            .unwrap()
            .query("type", Some(""))
            .unwrap();
        assert_eq!(
            req.path_and_query(),
            "/lol/match/v5/matches/by-puuid/P/ids?start=0&count=20"
        );
    }

    /// v1 "omits the query string entirely when there are no params" and
    /// "pages the tier walk from 1".
    #[test]
    fn path_and_query_shapes() {
        let e = ep("platform.championRotations");
        let req = RiotRequest::new(e, e.target_for_platform(Platform::Euw1), &[]).unwrap();
        assert_eq!(req.path_and_query(), "/lol/platform/v3/champion-rotations");

        let e = ep("league.entriesByTier");
        let req = RiotRequest::new(
            e,
            e.target_for_platform(Platform::Euw1),
            &["RANKED_SOLO_5x5", "IRON", "IV"],
        )
        .unwrap()
        .query("page", Some(42))
        .unwrap();
        assert_eq!(
            req.path_and_query(),
            "/lol/league/v4/entries/RANKED_SOLO_5x5/IRON/IV?page=42"
        );
    }

    #[test]
    fn undeclared_query_keys_are_rejected() {
        let e = ep("summoner.byPuuid");
        let err = RiotRequest::new(e, e.target_for_platform(Platform::Kr), &["P"])
            .unwrap()
            .query("count", Some(1));
        assert_eq!(
            err.unwrap_err(),
            RequestError::UnknownQuery {
                id: "summoner.byPuuid",
                key: "count".into()
            }
        );
    }

    #[test]
    fn form_encoding_matches_url_search_params() {
        assert_eq!(form_encode("a b&c=d/é"), "a+b%26c%3Dd%2F%C3%A9");
        assert_eq!(form_encode("A-z_0.9*"), "A-z_0.9*");
    }

    #[test]
    fn embeds_the_webpki_roots() {
        assert!(webpki_roots().len() > 100, "embedded root store looks empty");
    }

    fn err(kind: RiotErrorKind) -> RiotError {
        RiotError {
            kind,
            method: "summoner.byPuuid",
            host: "euw1.api.riotgames.com".into(),
            status: None,
            headers: Box::default(),
        }
    }

    /// v1 test/errors.test.ts "upstream error policy (§5.5)".
    #[test]
    fn upstream_error_policy() {
        assert_eq!(
            err(RiotErrorKind::NotFound).to_api_error().code,
            ErrorCode::NotFound
        );

        let auth = err(RiotErrorKind::UpstreamAuth).to_api_error();
        assert_eq!(auth.code, ErrorCode::UpstreamError);
        assert_eq!(auth.status.as_u16(), 502);
        assert!(
            !auth.message.to_lowercase().contains("key"),
            "never tells the client the key was rejected"
        );

        let limited = err(RiotErrorKind::RateLimited {
            limit_type: Some("method".into()),
            retry_after: Some(7),
        })
        .to_api_error();
        assert_eq!(limited.code, ErrorCode::RateLimited);
        assert_eq!(limited.retry_after, Some(7));
        let untyped = err(RiotErrorKind::RateLimited {
            limit_type: None,
            retry_after: None,
        })
        .to_api_error();
        assert_eq!(untyped.retry_after, Some(1), "v1 defaults Retry-After to 1");

        assert_eq!(
            err(RiotErrorKind::UpstreamUnavailable).to_api_error().code,
            ErrorCode::UpstreamError
        );
        assert_eq!(
            err(RiotErrorKind::UnexpectedStatus).to_api_error().code,
            ErrorCode::UpstreamError
        );
    }
}

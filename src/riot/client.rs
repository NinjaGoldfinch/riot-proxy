//! One HTTP call to Riot and a classification of what came back. No retries and no
//! limiter here: the limiter (P2) and fetcher (P3-05) own those. v1 did both in
//! its client (`src/riot/client.ts`); the policy moves, it doesn't disappear.
//!
//! Status policy is v1 spec §5.5 / §9.4: 404 → not found; 401/403 → the key is bad,
//! logged at error (a status probe's only as the verdict turns); 429 → rate
//! limited, typed or not; 5xx and network → upstream unavailable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, header};
use bytes::Bytes;

use crate::config::{Config, Secret};
use crate::http::{ApiError, ErrorCode};
use crate::metrics::{RL_429_TOTAL, UPSTREAM_LATENCY_SECONDS, UPSTREAM_REQUESTS_TOTAL};
use crate::riot::endpoints::{Endpoint, PathError, Target, encode_component};
use crate::status::outbound::Events;

/// v1 undici pool: 32 connections per host, 60 s keep-alive.
const POOL_MAX_IDLE_PER_HOST: usize = 32;
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// v1: 10 s for headers + 15 s for the body.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

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
    /// account-v1 only: the fetcher picks the cluster from
    /// [`Region::ACCOUNT_PICK`](crate::riot::routing::Region::ACCOUNT_PICK), and
    /// `target` is just the first choice (ADR-066).
    pub pick_cluster: bool,
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
            pick_cluster: false,
        })
    }

    /// An account-v1 request on whichever cluster has room: Riot serves every
    /// account from americas, asia and europe alike (ADR-066).
    pub fn account(endpoint: &'static Endpoint, params: &[&str]) -> Result<Self, RequestError> {
        let first = crate::riot::routing::Region::ACCOUNT_PICK[0];
        let mut req = Self::new(endpoint, Target::Region(first), params)?;
        req.pick_cluster = endpoint.host == crate::riot::endpoints::HostKind::Account;
        Ok(req)
    }

    /// The hosts to try, in order: the picker's clusters, or the one target.
    pub fn targets(&self) -> Vec<Target> {
        if self.pick_cluster {
            crate::riot::routing::Region::ACCOUNT_PICK
                .iter()
                .map(|r| Target::Region(*r))
                .collect()
        } else {
            vec![self.target]
        }
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

/// The message of the `UPSTREAM_ERROR` a 401 or 403 from Riot becomes; the
/// status poller tells a refused read by it (SITE-17).
pub const UPSTREAM_AUTH_FAILED: &str = "Upstream authentication failed";

impl RiotError {
    /// v1 `RiotError.toProxyError`: what the client of the proxy is told. Never
    /// says the Riot key was rejected; that is an operator problem.
    pub fn to_api_error(&self) -> ApiError {
        match &self.kind {
            RiotErrorKind::NotFound => ApiError::not_found("Resource not found upstream"),
            RiotErrorKind::UpstreamAuth => ApiError::new(ErrorCode::UpstreamError, UPSTREAM_AUTH_FAILED),
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
/// Sent only to a fixed base URL (a mock): the Riot host the request was for.
pub const MOCK_HOST_HEADER: &str = "x-riot-host";

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
    watch: Arc<UpstreamWatch>,
}

/// What `/status` reads about Riot's answers (STS-02, design/14), shared by
/// every clone of a client. Written on the response path; the locks are
/// held for one assignment and never across an await.
#[derive(Debug, Default)]
pub struct UpstreamWatch {
    /// Unix ms of the last 2xx; 0 before the first.
    last_success_ms: AtomicI64,
    /// What the last 2xx, 401 or 403 said about the key (`riot_key`, STS-02b).
    key: Mutex<Option<KeyVerdict>>,
    /// Unix ms of the last answer on each scope, a status probe's excepted:
    /// what makes a platform *active* for the probe's cadence (design/14).
    traffic: Mutex<HashMap<&'static str, i64>>,
    /// What Riot's rate-limit headers last said (STS-03, ADR-207).
    rate_limits: crate::status::ratelimits::RateLimitTable,
    /// The status poller's last 401 or 403, which judges nothing on its own
    /// ([`UpstreamWatch::polls_refused`]).
    poll_refusal: Mutex<Option<KeyVerdict>>,
    /// Where `riot_key_rejected` and a 429's freeze go (STS-04); unset, the
    /// no-op handle.
    events: OnceLock<Events>,
}

/// The key as Riot last judged it: a 2xx accepts it, a 401 or 403 rejects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyVerdict {
    pub accepted: bool,
    /// The response's status, method id and scope.
    pub status: u16,
    pub method: &'static str,
    pub scope: &'static str,
    /// Unix ms.
    pub at_ms: i64,
}

impl UpstreamWatch {
    /// When Riot last answered 2xx, in unix ms; `None` before the first.
    pub fn last_success_ms(&self) -> Option<i64> {
        Some(self.last_success_ms.load(Ordering::Relaxed)).filter(|&ms| ms > 0)
    }

    /// The last 2xx, 401 or 403; `None` before the first.
    pub fn key(&self) -> Option<KeyVerdict> {
        lock(&self.key).clone()
    }

    /// When `scope` last had an answer from Riot that was not a status probe.
    pub fn last_traffic_ms(&self, scope: &str) -> Option<i64> {
        lock(&self.traffic).get(scope).copied()
    }

    /// The rate-limit table, from every answer with a status, probes included.
    pub fn rate_limits(&self) -> &crate::status::ratelimits::RateLimitTable {
        &self.rate_limits
    }

    /// Send events from here on to `events` (STS-04, `status::outbound::start`).
    /// Set once: a second call is ignored.
    pub fn set_events(&self, events: Events) {
        let _ = self.events.set(events);
    }

    /// The events handle: a no-op until [`set_events`](Self::set_events).
    pub fn events(&self) -> &Events {
        self.events.get().unwrap_or(&crate::status::outbound::NONE)
    }

    /// One answer from Riot with `status`, to a call made as `caller`: a
    /// status check's probe or the status poller's read says nothing about
    /// real traffic, and the poller's 401 or 403 says nothing about the key
    /// (one platform refusing a status read is not the key rejected; the
    /// probe and real traffic still judge it; SITE-17). Returns whether this
    /// answer turned the key's verdict to rejected: a 401 or 403 when the key
    /// was not already rejected (unknown or accepted before), which is also
    /// when `riot_key_rejected` is emitted (design/14).
    fn answered(
        &self,
        scope: &'static str,
        method: &'static str,
        status: u16,
        caller: Caller,
        at_ms: i64,
    ) -> bool {
        let accepted = (200..300).contains(&status);
        if accepted {
            self.last_success_ms.fetch_max(at_ms, Ordering::Relaxed);
        }
        let mut newly_rejected = false;
        let judges_key = accepted || (matches!(status, 401 | 403) && caller != Caller::Poll);
        if judges_key {
            let mut key = lock(&self.key);
            newly_rejected = !accepted && key.as_ref().is_none_or(|k| k.accepted);
            let verdict = KeyVerdict {
                accepted,
                status,
                method,
                scope,
                at_ms,
            };
            if newly_rejected {
                self.events().key_rejected(&verdict);
            }
            *key = Some(verdict);
        }
        if caller == Caller::Poll && matches!(status, 401 | 403) {
            *lock(&self.poll_refusal) = Some(KeyVerdict {
                accepted: false,
                status,
                method,
                scope,
                at_ms,
            });
        }
        if caller == Caller::Traffic {
            lock(&self.traffic).insert(scope, at_ms);
        }
        newly_rejected
    }

    /// The status poller ended a round in which every read that reached
    /// Riot was refused (401 or 403), and at least one was: that judges the
    /// key, where one platform's refusal does not (owner decision of
    /// 2026-11-17, SITE-17). The verdict turns rejected, from the poller's
    /// last refusal, and `RIOT KEY REJECTED` is logged once, when it turns;
    /// a verdict already rejected stays so and logs nothing. Returns whether
    /// it turned, as [`UpstreamWatch::answered`] does for one answer (what
    /// fires `riot_key_rejected`, design/14, emitted here when it turns). The
    /// next 2xx, a poll's included, accepts the key again.
    pub fn polls_refused(&self) -> bool {
        let Some(refusal) = lock(&self.poll_refusal).clone() else {
            return false;
        };
        let mut key = lock(&self.key);
        let newly_rejected = key.as_ref().is_none_or(|k| k.accepted);
        if newly_rejected {
            tracing::error!(
                status = refusal.status,
                method = refusal.method,
                scope = refusal.scope,
                "RIOT KEY REJECTED — check RIOT_API_KEY (dev keys expire every 24h); every status poll this round was refused"
            );
            self.events().key_rejected(&refusal);
            *key = Some(refusal);
        }
        newly_rejected
    }
}

/// Who a call to Riot is for, which decides what its answer says about the
/// key and the scope's traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caller {
    /// A request's or a job's call: traffic.
    Traffic,
    /// `/status`'s probe (STS-02b): judges the key, is not traffic.
    Probe,
    /// The status poller (SITE-17): not traffic, and its 401 or 403 does
    /// not judge the key on its own; a round wholly refused does
    /// ([`UpstreamWatch::polls_refused`]).
    Poll,
}

/// A poisoned lock still holds a whole value: each write is one assignment.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
        Ok(Self {
            http,
            token,
            base,
            watch: Arc::default(),
        })
    }

    /// What `/status` reads about this client's responses.
    pub fn watch(&self) -> &Arc<UpstreamWatch> {
        &self.watch
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
        self.send_as(req, Caller::Traffic).await
    }

    /// [`send`](Self::send) for `/status`'s own probe (STS-02b): counted and
    /// judged like any call, but not as traffic on its scope, so a probe
    /// never makes its platform look busy.
    pub async fn send_probe(&self, req: &RiotRequest) -> Result<RiotResponse, RiotError> {
        self.send_as(req, Caller::Probe).await
    }

    /// [`send`](Self::send) for the status poller (SITE-17, owner decision
    /// of 2026-11-17): counted, and in the rate-limit table, but not traffic on its
    /// scope, and its 401 or 403 neither turns the key's verdict to rejected
    /// nor logs `RIOT KEY REJECTED` (the poller logs its own failures).
    pub async fn send_poll(&self, req: &RiotRequest) -> Result<RiotResponse, RiotError> {
        self.send_as(req, Caller::Poll).await
    }

    async fn send_as(&self, req: &RiotRequest, caller: Caller) -> Result<RiotResponse, RiotError> {
        let scope = req.target.scope();
        let method = req.endpoint.id;
        let host = req.target.host();
        let started = Instant::now();

        let mut builder = self
            .http
            .get(self.url(req))
            // v1 §5.2: the key goes in a header, never the query string.
            .header(X_RIOT_TOKEN, self.token.clone())
            .header(header::ACCEPT, "application/json");
        if let Base::Fixed(_) = self.base {
            // A mock stands in for every Riot host at one address; this says
            // which host the request was for, so it can keep Riot's per-host
            // rate-limit buckets and check the routing (ADR-059).
            builder = builder.header(MOCK_HOST_HEADER, host.as_str());
        }
        let result = builder.send().await;
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
        let at_ms = crate::clock::Clock::now().unix_ms;
        let newly_rejected = self.watch.answered(scope, method, status, caller, at_ms);
        // Every answer with a status, a probe's included (design/14, ADR-206);
        // the fetcher or the probe already read and warned on these headers.
        let limits = crate::riot::limiter::headers::RateLimitHeaders::from_headers_quiet(&headers);
        self.watch.rate_limits.record(scope, method, &limits, at_ms);
        // The headers the limiter freezes a scope on (`Limiter::observe`):
        // a freeze over 10 s fires `riot_rate_limited` now, even one that
        // ends before the next check round (STS-04).
        if let (Some(_), Some(retry_after_s)) = (limits.limit_type, limits.retry_after) {
            self.watch.events().frozen(retry_after_s, at_ms);
        }

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
            // `/status`'s probe asks every round, so its 401 or 403 logs only
            // when it turns the verdict to rejected (STS-02b, ADR-206).
            401 | 403 if caller == Caller::Probe && !newly_rejected => {
                tracing::debug!(status, method, host = %host, "status probe: the key is still rejected");
                RiotErrorKind::UpstreamAuth
            }
            401 | 403 if caller == Caller::Poll => {
                tracing::debug!(status, method, host = %host, "status poll refused");
                RiotErrorKind::UpstreamAuth
            }
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

pub(crate) fn webpki_roots() -> Vec<reqwest::Certificate> {
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

    /// `riot_key` follows the last 2xx, 401 or 403 only; a probe's answer
    /// judges the key but is not traffic on its scope (STS-02b).
    #[test]
    fn the_watch_judges_the_key_and_counts_traffic() {
        let w = UpstreamWatch::default();
        assert_eq!(w.key(), None);
        assert!(!w.answered("oc1", "status.platformData", 200, Caller::Probe, 10));
        assert_eq!(w.last_traffic_ms("oc1"), None, "a probe is not traffic");
        assert!(w.key().unwrap().accepted);
        assert_eq!(w.last_success_ms(), Some(10));

        assert!(
            w.answered("oc1", "summoner.byPuuid", 403, Caller::Traffic, 20),
            "accepted to rejected"
        );
        let k = w.key().unwrap();
        assert_eq!(
            (k.accepted, k.status, k.method, k.scope, k.at_ms),
            (false, 403, "summoner.byPuuid", "oc1", 20)
        );
        assert_eq!(w.last_traffic_ms("oc1"), Some(20));
        assert!(
            !w.answered("oc1", "status.platformData", 401, Caller::Probe, 21),
            "still rejected"
        );

        // Neither a 404, a 429 nor a 5xx says anything about the key.
        for status in [404, 429, 503] {
            w.answered("kr", "match.byId", status, Caller::Traffic, 30);
        }
        assert!(!w.key().unwrap().accepted);
        assert_eq!(w.last_traffic_ms("kr"), Some(30));
        assert_eq!(w.last_success_ms(), Some(10));

        w.answered("kr", "match.byId", 200, Caller::Traffic, 40);
        assert!(w.key().unwrap().accepted, "a 2xx accepts it again");
        assert!(
            UpstreamWatch::default().answered("kr", "match.byId", 401, Caller::Probe, 50),
            "unknown to rejected"
        );

        // The status poller (SITE-17): not traffic, and its 401 or 403 does
        // not judge the key; its 2xx is a success like any.
        let p = UpstreamWatch::default();
        assert!(!p.answered("tw2", "status.platformData", 403, Caller::Poll, 60));
        assert_eq!((p.key(), p.last_traffic_ms("tw2")), (None, None));
        assert!(!p.answered("kr", "status.platformData", 200, Caller::Poll, 61));
        assert!(p.key().unwrap().accepted);
        assert_eq!((p.last_success_ms(), p.last_traffic_ms("kr")), (Some(61), None));
        assert!(!p.answered("tw2", "status.platformData", 401, Caller::Poll, 62));
        assert!(p.key().unwrap().accepted, "a poll's 401 leaves the verdict");
    }

    /// Lines at `level` or above logged while `f` runs on this thread.
    async fn logged<F: std::future::Future>(level: tracing::Level, f: F) -> (F::Output, String) {
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                lock(&self.0).extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::from_level(level)),
        );
        let out = {
            let _guard = tracing::subscriber::set_default(subscriber);
            f.await
        };
        let text = String::from_utf8(lock(&buf.0).clone()).unwrap();
        (out, text)
    }

    /// STS-02b (owner, ADR-206): a rejected key logs `RIOT KEY REJECTED` at
    /// error for every real call, but `/status`'s probe, which asks every
    /// round, logs it only when its answer turns the verdict to rejected.
    #[tokio::test]
    async fn a_probe_logs_a_rejected_key_once_per_change() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        const REJECTED: &str = "RIOT KEY REJECTED";

        let server = MockServer::start().await;
        let probe_path = "/lol/status/v4/platform-data";
        let client = RiotClient::build(
            &Secret::new("RGAPI-test"),
            "riot-proxy-test",
            Base::Fixed(server.uri()),
        )
        .unwrap();
        let e = ep("status.platformData");
        let probe = RiotRequest::new(e, e.target_for_platform(Platform::Oc1), &[]).unwrap();
        let answer = |status: u16| {
            Mock::given(path(probe_path))
                .respond_with(ResponseTemplate::new(status))
                .mount_as_scoped(&server)
        };

        let rejected = answer(403).await;
        let (out, text) = logged(tracing::Level::WARN, async {
            for _ in 0..3 {
                let err = client.send_probe(&probe).await.unwrap_err();
                assert!(matches!(err.kind, RiotErrorKind::UpstreamAuth), "{err:?}");
            }
            client.send(&probe).await
        })
        .await;
        assert!(out.is_err());
        assert_eq!(
            text.matches(REJECTED).count(),
            2,
            "the first probe, then the real call; the repeat probes are quiet:\n{text}"
        );
        assert!(
            text.lines().all(|l| !l.contains(REJECTED) || l.contains("ERROR")),
            "{text}"
        );

        drop(rejected);
        let accepted = answer(200).await;
        client.send_probe(&probe).await.unwrap();
        drop(accepted);
        let _rejected = answer(401).await;
        let (_, text) = logged(tracing::Level::WARN, async {
            client.send_probe(&probe).await.unwrap_err();
            client.send_probe(&probe).await.unwrap_err();
        })
        .await;
        assert_eq!(
            text.matches(REJECTED).count(),
            1,
            "accepted, then rejected again: one line\n{text}"
        );

        let (_, text) = logged(tracing::Level::DEBUG, client.send_probe(&probe)).await;
        assert!(text.contains("status probe: the key is still rejected"), "{text}");
        assert!(!text.contains(REJECTED), "{text}");
    }

    /// The events the watch emits (STS-04, design/14): `riot_key_rejected`
    /// once per turn of the verdict, whatever turned it.
    fn rejections(
        rx: &mut tokio::sync::mpsc::Receiver<crate::status::outbound::Event>,
    ) -> Vec<serde_json::Value> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| {
                assert_eq!(e.event.as_str(), "riot_key_rejected");
                e.detail
            })
            .collect()
    }

    /// A bad key at boot fires once; again only after an accepted answer.
    #[test]
    fn riot_key_rejected_fires_once_per_rejection() {
        let w = UpstreamWatch::default();
        let (events, mut rx) = Events::channel("h");
        w.set_events(events);
        assert!(w.events().is_on());

        w.answered("oc1", "summoner.byPuuid", 401, Caller::Traffic, 10);
        assert_eq!(
            rejections(&mut rx),
            [serde_json::json!({ "status": 401, "method": "summoner.byPuuid", "scope": "oc1" })],
            "unknown to rejected"
        );
        w.answered("oc1", "summoner.byPuuid", 401, Caller::Traffic, 11);
        w.answered("oc1", "status.platformData", 403, Caller::Probe, 12);
        assert_eq!(rejections(&mut rx).len(), 0, "still rejected");

        w.answered("kr", "match.byId", 200, Caller::Traffic, 20);
        w.answered("kr", "status.platformData", 403, Caller::Probe, 21);
        assert_eq!(
            rejections(&mut rx),
            [serde_json::json!({ "status": 403, "method": "status.platformData", "scope": "kr" })],
            "accepted, then rejected again"
        );
    }

    /// A lone poll refusal fires nothing; a wholly refused round fires once.
    #[test]
    fn a_polls_refusal_fires_only_as_a_wholly_refused_round() {
        let w = UpstreamWatch::default();
        let (events, mut rx) = Events::channel("h");
        w.set_events(events);
        w.answered("tw2", "status.platformData", 403, Caller::Poll, 10);
        assert_eq!(rejections(&mut rx).len(), 0);
        assert!(w.polls_refused());
        assert_eq!(
            rejections(&mut rx),
            [serde_json::json!({ "status": 403, "method": "status.platformData", "scope": "tw2" })]
        );
        assert!(!w.polls_refused());
        assert_eq!(rejections(&mut rx).len(), 0);
    }

    /// Without `EVENTS_URL` the watch has the no-op handle; setting one
    /// twice keeps the first.
    #[test]
    fn the_watchs_events_are_set_once() {
        let w = UpstreamWatch::default();
        assert!(!w.events().is_on());
        w.answered("oc1", "summoner.byPuuid", 401, Caller::Traffic, 10);
        let (first, mut rx) = Events::channel("h");
        let (second, mut other) = Events::channel("h");
        w.set_events(first);
        w.set_events(second);
        w.answered("oc1", "summoner.byPuuid", 200, Caller::Traffic, 20);
        w.answered("oc1", "summoner.byPuuid", 401, Caller::Traffic, 21);
        assert_eq!(rejections(&mut rx).len(), 1);
        assert!(other.try_recv().is_err());
    }

    /// A 429 that freezes a scope for more than 10 s fires `riot_rate_limited`
    /// on the response path; a shorter one, or one the limiter would not
    /// freeze on (no `X-Rate-Limit-Type`), does not.
    #[tokio::test]
    async fn a_429_freezing_over_10_s_fires_riot_rate_limited() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let client = RiotClient::build(&Secret::new("RGAPI-test"), "t", Base::Fixed(server.uri())).unwrap();
        let (events, mut rx) = Events::channel("h");
        client.watch().set_events(events);
        let e = ep("summoner.byPuuid");
        let req = RiotRequest::new(e, e.target_for_platform(Platform::Oc1), &["P"]).unwrap();
        let answer = |retry_after: &'static str, limit_type: Option<&'static str>| {
            let mut t = ResponseTemplate::new(429).insert_header("retry-after", retry_after);
            if let Some(kind) = limit_type {
                t = t.insert_header(X_RATE_LIMIT_TYPE, kind);
            }
            let server = &server;
            async move {
                server.reset().await;
                Mock::given(path("/lol/summoner/v4/summoners/by-puuid/P"))
                    .respond_with(t)
                    .mount(server)
                    .await;
            }
        };
        let fired = |rx: &mut tokio::sync::mpsc::Receiver<crate::status::outbound::Event>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .map(|e| (e.event.as_str(), e.detail))
                .collect::<Vec<_>>()
        };

        answer("10", Some("method")).await;
        let _ = client.send(&req).await;
        answer("60", None).await;
        let _ = client.send(&req).await;
        assert_eq!(fired(&mut rx), [], "10 s is not above 10; no type, no freeze");
        answer("11", Some("application")).await;
        let _ = client.send(&req).await;
        assert_eq!(
            fired(&mut rx),
            [("riot_rate_limited", serde_json::json!({ "retry_after_s": 11 }))]
        );
        answer("30", Some("service")).await;
        let _ = client.send(&req).await;
        assert_eq!(fired(&mut rx), [], "already limited until a round sees 0");
    }

    /// SITE-17: a status poll's 401 or 403 logs `status poll refused` at
    /// debug, never `RIOT KEY REJECTED`, and leaves the key's verdict alone;
    /// the same answer to a real call logs it.
    #[tokio::test]
    async fn a_polls_refusal_is_debug_and_does_not_judge_the_key() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        const REJECTED: &str = "RIOT KEY REJECTED";

        let server = MockServer::start().await;
        let client = RiotClient::build(
            &Secret::new("RGAPI-test"),
            "riot-proxy-test",
            Base::Fixed(server.uri()),
        )
        .unwrap();
        let e = ep("status.platformData");
        let req = RiotRequest::new(e, e.target_for_platform(Platform::Tw2), &[]).unwrap();
        Mock::given(path("/lol/status/v4/platform-data"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let (out, text) = logged(tracing::Level::DEBUG, client.send_poll(&req)).await;
        let err = out.unwrap_err();
        assert!(matches!(err.kind, RiotErrorKind::UpstreamAuth), "{err:?}");
        assert!(text.contains("status poll refused"), "{text}");
        assert!(!text.contains(REJECTED), "{text}");
        assert_eq!(client.watch().key(), None, "the verdict is untouched");

        let (_, text) = logged(tracing::Level::WARN, client.send(&req)).await;
        assert_eq!(text.matches(REJECTED).count(), 1, "{text}");
    }

    /// Owner decision of 2026-11-17 (SITE-17): a round in which every poll
    /// that reached Riot was refused judges the key, once; one already
    /// rejected stays so without another line; a 2xx accepts it again.
    #[tokio::test]
    async fn a_wholly_refused_round_rejects_the_key_once() {
        const REJECTED: &str = "RIOT KEY REJECTED";
        let w = UpstreamWatch::default();
        assert!(!w.polls_refused(), "no refusal recorded: nothing to judge");
        assert_eq!(w.key(), None);

        w.answered("kr", "status.platformData", 200, Caller::Poll, 10);
        w.answered("tw2", "status.platformData", 403, Caller::Poll, 11);
        assert!(w.key().unwrap().accepted, "one refusal alone judges nothing");
        let (turned, text) = logged(tracing::Level::WARN, async { w.polls_refused() }).await;
        assert!(turned);
        assert_eq!(text.matches(REJECTED).count(), 1, "{text}");
        let k = w.key().unwrap();
        assert_eq!(
            (k.accepted, k.status, k.method, k.scope, k.at_ms),
            (false, 403, "status.platformData", "tw2", 11)
        );

        w.answered("kr", "status.platformData", 401, Caller::Poll, 20);
        let (turned, text) = logged(tracing::Level::WARN, async { w.polls_refused() }).await;
        assert!(!turned, "already rejected");
        assert!(!text.contains(REJECTED), "{text}");

        w.answered("kr", "status.platformData", 200, Caller::Poll, 30);
        assert!(w.key().unwrap().accepted, "a poll's 2xx accepts it again");
    }

    /// STS-03 (design/14 §Rate-limit table): a real answer and a probe's
    /// both feed the rate-limit table, a 429's `Retry-After` with them; a
    /// network failure, which carries no headers, records nothing.
    #[tokio::test]
    async fn every_answer_feeds_the_rate_limit_table() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let client = RiotClient::build(
            &Secret::new("RGAPI-test"),
            "riot-proxy-test",
            Base::Fixed(server.uri()),
        )
        .unwrap();
        let e = ep("status.platformData");
        let req = RiotRequest::new(e, e.target_for_platform(Platform::Oc1), &[]).unwrap();
        let now = crate::clock::Clock::now().unix_ms;

        // Header values from the recorded kr exchange in
        // tests/fixtures/replay/cold-lookup/02-summoner.byPuuid.json.
        let ok = Mock::given(path("/lol/status/v4/platform-data"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-app-rate-limit", "100:120,20:1")
                    .insert_header("x-app-rate-limit-count", "1:120,1:1")
                    .insert_header("x-method-rate-limit", "2000:60")
                    .insert_header("x-method-rate-limit-count", "1:60"),
            )
            .mount_as_scoped(&server)
            .await;
        client.send(&req).await.unwrap();
        drop(ok);
        let report = client.watch().rate_limits().report(0, now);
        let oc1 = &report.scopes["oc1"];
        assert_eq!(oc1.remaining_pct, Some(95));
        assert_eq!(oc1.methods["status.platformData"].windows.len(), 1);
        assert_eq!(oc1.retry_after.s, 0);

        let _limited = Mock::given(path("/lol/status/v4/platform-data"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .mount_as_scoped(&server)
            .await;
        client.send_probe(&req).await.unwrap_err();
        let report = client.watch().rate_limits().report(0, now);
        let oc1 = &report.scopes["oc1"];
        assert_eq!(oc1.retry_after.s, 7, "the probe's 429 is recorded");
        assert!(oc1.retry_after.at.is_some());
        assert_eq!(
            oc1.remaining_pct,
            Some(95),
            "a 429 without app headers keeps them"
        );

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let unreachable = RiotClient::build(
            &Secret::new("RGAPI-test"),
            "riot-proxy-test",
            Base::Fixed(upstream),
        )
        .unwrap();
        unreachable.send(&req).await.unwrap_err();
        assert!(
            unreachable.watch().rate_limits().report(0, now).scopes.is_empty(),
            "a network failure records nothing"
        );
    }
}

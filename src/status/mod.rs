//! `GET /status` (STS-02, design/14, ADR-203): the shared ninjagoldfinch
//! status contract, answered from memory only. A status request never calls
//! Riot, the bucket or the event URL: the build identity, the recorder's
//! counters, L1's size and the stored check results are all it reads.
//!
//! Who may read it is `http::auth::require_status`'s: the admin key, or a
//! keyless request from `STATUS_ALLOW_CIDR` that no reverse proxy forwarded
//! (none of `Cf-Connecting-IP`, `X-Forwarded-For`, `Forwarded`, `X-Real-IP`).
//! Neither the consumer quota nor the recent-requests ring sees it.

pub mod snapshot;

use std::collections::BTreeMap;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::build_info;

/// The contract's version.
pub const SCHEMA: u32 = 1;
/// The contract's `service`.
pub const SERVICE: &str = "riot-proxy";

/// What `/status` knows about this process from its start.
#[derive(Debug, Clone)]
pub struct Status {
    started: Instant,
    started_at_ms: i64,
    host: String,
}

impl Status {
    /// A process that started at `started`, on this machine.
    pub fn new(started: Instant) -> Self {
        let ago = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        Self {
            started,
            started_at_ms: crate::clock::Clock::now().unix_ms.saturating_sub(ago),
            host: hostname(),
        }
    }

    /// The machine's hostname, as `/status` and events name it.
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// `gethostname(2)`; `unknown` if it fails.
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the pointer and length describe `buf`, which outlives the call;
    // gethostname writes at most `len` bytes into it.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    match String::from_utf8_lossy(&buf[..end]).trim() {
        "" => "unknown".into(),
        name => name.to_string(),
    }
}

/// A check's or the whole service's state, worst last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum StatusLevel {
    Ok,
    Degraded,
    Down,
}

/// The service's `status`: the worst of its checks, `ok` with none.
pub fn overall(checks: &[Check]) -> StatusLevel {
    checks.iter().map(|c| c.status).max().unwrap_or(StatusLevel::Ok)
}

/// One background check's last result (design/14 §Checks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Check {
    /// `riot_api`, `riot_key`, `cache` or `storage`.
    pub name: String,
    pub status: StatusLevel,
    /// How long the check took; null for one that makes no call of its own.
    pub latency_ms: Option<u64>,
    /// RFC 3339, UTC.
    pub checked_at: String,
    /// Why it is not `ok`, or what it saw; empty when there is nothing to say.
    pub detail: String,
}

/// The shared ninjagoldfinch status contract, `schema: 1`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatusReport {
    /// Always 1.
    pub schema: u32,
    /// Always `riot-proxy`.
    pub service: &'static str,
    /// The machine's hostname.
    pub host: String,
    /// The release tag without `v`, or `dev` for a build that is not a release.
    pub version: &'static str,
    /// The commit, 7 characters.
    pub commit: &'static str,
    /// RFC 3339, UTC.
    pub built_at: String,
    /// When this process started; RFC 3339, UTC.
    pub started_at: String,
    pub uptime_s: u64,
    /// The worst of `checks`: any `down` is `down`, else any `degraded` is
    /// `degraded`, else `ok`.
    pub status: StatusLevel,
    pub checks: Vec<Check>,
    /// Flat snake_case keys to numbers. Counters count since `started_at`;
    /// gauges are current values. design/14 §Metrics lists every key.
    pub metrics: BTreeMap<String, u64>,
    /// Strings: `riot_last_success_at`, the last 2xx from Riot (RFC 3339), or
    /// null before the first.
    pub metrics_meta: BTreeMap<String, Option<String>>,
}

impl StatusReport {
    /// The contract from what `state` holds in memory.
    pub fn from_state(state: &AppState) -> Self {
        let status = &state.status;
        let cache = &state.fetcher.cache().l1;
        let metrics = snapshot::metrics(
            &state.stats.metrics.render(),
            snapshot::Cache {
                entries: cache.entry_count(),
                bytes: cache.weighted_size(),
            },
        );
        let last_success = state.fetcher.client().watch().last_success_ms();
        let checks = Vec::new();
        Self {
            schema: SCHEMA,
            service: SERVICE,
            host: status.host.clone(),
            version: build_info::status_version(),
            commit: build_info::short_commit(),
            built_at: build_info::built_at_rfc3339(),
            started_at: build_info::rfc3339(status.started_at_ms),
            uptime_s: status.started.elapsed().as_secs(),
            status: overall(&checks),
            checks,
            metrics,
            metrics_meta: BTreeMap::from([(
                "riot_last_success_at".to_string(),
                last_success.map(build_info::rfc3339),
            )]),
        }
    }
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(status))
}

/// The service's health in the shared ninjagoldfinch status contract.
#[utoipa::path(
    get,
    path = "/status",
    tag = "ops",
    summary = "Status",
    description = "The shared ninjagoldfinch status contract (`schema: 1`): build identity, uptime, the \
                   background checks, and flat counters and gauges. Answered from memory, never by calling \
                   Riot, so it is fast and costs no Riot request. **200 even when `status` is `down`**: \
                   a non-200 means this endpoint itself failed.\n\n\
                   Takes the admin key (admin scope and `ADMIN_IP_ALLOWLIST`), or no key from a TCP peer \
                   inside `STATUS_ALLOW_CIDR` (default loopback) on a request carrying none of \
                   `Cf-Connecting-IP`, `X-Forwarded-For`, `Forwarded` or `X-Real-IP`: a request a reverse \
                   proxy forwarded needs the key, whatever `TRUST_PROXY` says. Not metered by the consumer \
                   quota.",
    security(("bearerAuth" = []), ()),
    responses(
        (status = 200, description = "The contract", body = StatusReport),
        (status = 401, description = "No key from outside `STATUS_ALLOW_CIDR`, or through a reverse proxy; or an unknown key", body = crate::http::error::ErrorResponse),
        (status = 403, description = "A key without the admin scope, or from outside `ADMIN_IP_ALLOWLIST`", body = crate::http::error::ErrorResponse),
    ),
)]
async fn status(State(state): State<AppState>) -> Json<StatusReport> {
    Json(StatusReport::from_state(&state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: StatusLevel) -> Check {
        Check {
            name: "x".into(),
            status,
            latency_ms: None,
            checked_at: String::new(),
            detail: String::new(),
        }
    }

    /// design/14: any `down` → `down`; else any `degraded` → `degraded`; else `ok`.
    #[test]
    fn status_is_the_worst_check() {
        use StatusLevel::{Degraded, Down, Ok};
        assert_eq!(overall(&[]), Ok);
        let all = [Ok, Degraded, Down];
        for a in all {
            for b in all {
                for c in all {
                    let checks = [check(a), check(b), check(c)];
                    let want = if [a, b, c].contains(&Down) {
                        Down
                    } else if [a, b, c].contains(&Degraded) {
                        Degraded
                    } else {
                        Ok
                    };
                    assert_eq!(overall(&checks), want, "{a:?} {b:?} {c:?}");
                }
            }
        }
    }

    #[test]
    fn health_serialises_lowercase() {
        assert_eq!(serde_json::to_value(StatusLevel::Degraded).unwrap(), "degraded");
    }

    #[test]
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }
}

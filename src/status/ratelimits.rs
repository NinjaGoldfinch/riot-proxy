//! The rate-limit table (STS-03, design/14 §Rate-limit table, ADR-207): what
//! Riot last *said* about our limits, for people. The limiter paces itself on
//! its own sliding logs; this keeps the headers as they arrived, per scope
//! (`X-App-Rate-Limit` and its count), per `(scope, method)` (`X-Method-*`),
//! and the last `Retry-After` with its time.
//!
//! Written on the client's response path for every answer from Riot, status
//! probes included; read by `/status` (`riot_app_limit_remaining_pct_<scope>`)
//! and `GET /status/ratelimits`. The lock is held for one update or one read
//! and never across an await. Memory only: a restart starts it empty.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use utoipa::ToSchema;

use crate::build_info::rfc3339;
use crate::riot::limiter::headers::{CountWindow, LimitWindow, RateLimitHeaders};
use crate::status::snapshot::key_name;

/// Every scope's and method's latest headers.
#[derive(Debug, Default)]
pub struct RateLimitTable(Mutex<BTreeMap<&'static str, ScopeEntry>>);

#[derive(Debug, Default, Clone)]
struct ScopeEntry {
    app: Option<Observed>,
    methods: BTreeMap<&'static str, Observed>,
    retry_after: Option<RetryAfterSeen>,
}

#[derive(Debug, Clone)]
struct Observed {
    at_ms: i64,
    windows: Vec<RateLimitWindow>,
}

#[derive(Debug, Clone, Copy)]
struct RetryAfterSeen {
    s: u64,
    at_ms: i64,
}

/// One window as Riot last reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct RateLimitWindow {
    /// The window's length.
    pub window_s: u32,
    /// Requests allowed per window.
    pub limit: u32,
    /// Requests counted in the current window.
    pub count: u32,
    /// `(limit − count) / limit × 100`, rounded down, 0 when the count is
    /// at or over the limit.
    pub remaining_pct: u32,
}

/// `(limit − count) / limit × 100`, rounded down and never below 0: what is
/// left is never overstated. `limit` is never 0 (the parser drops such a
/// window); 0 is answered for one anyway.
pub fn remaining_pct(limit: u32, count: u32) -> u32 {
    if limit == 0 {
        return 0;
    }
    let left = u64::from(limit.saturating_sub(count));
    // left ≤ limit, so the quotient is at most 100.
    u32::try_from(left * 100 / u64::from(limit)).unwrap_or(100)
}

/// A limit header's windows with their counts, paired by window length. A
/// window the count header does not mention is left out, so headroom is
/// only ever what Riot said.
pub fn windows(limits: &[LimitWindow], counts: &[CountWindow]) -> Vec<RateLimitWindow> {
    limits
        .iter()
        .filter_map(|l| {
            let c = counts.iter().find(|c| c.seconds == l.seconds)?;
            Some(RateLimitWindow {
                window_s: l.seconds,
                limit: l.limit,
                count: c.count,
                remaining_pct: remaining_pct(l.limit, c.count),
            })
        })
        .collect()
}

/// The smallest headroom across `windows`; `None` for none.
fn least(windows: &[RateLimitWindow]) -> Option<u32> {
    windows.iter().map(|w| w.remaining_pct).min()
}

fn observed(
    limits: Option<&Vec<LimitWindow>>,
    counts: Option<&Vec<CountWindow>>,
    at_ms: i64,
) -> Option<Observed> {
    let windows = windows(limits?, counts?);
    (!windows.is_empty()).then_some(Observed { at_ms, windows })
}

impl RateLimitTable {
    /// One answer from Riot on `scope` for `method`, at `at_ms` (unix ms).
    /// A header pair that is absent, or pairs no window, leaves what was
    /// there: a service 429 or a bare 5xx says nothing about our limits.
    pub fn record(&self, scope: &'static str, method: &'static str, headers: &RateLimitHeaders, at_ms: i64) {
        let app = observed(headers.app_limits.as_ref(), headers.app_counts.as_ref(), at_ms);
        let by_method = observed(
            headers.method_limits.as_ref(),
            headers.method_counts.as_ref(),
            at_ms,
        );
        let retry_after = headers.retry_after.map(|s| RetryAfterSeen { s, at_ms });
        if app.is_none() && by_method.is_none() && retry_after.is_none() {
            return;
        }
        let mut table = self.lock();
        let entry = table.entry(scope).or_default();
        if app.is_some() {
            entry.app = app;
        }
        if let Some(m) = by_method {
            entry.methods.insert(method, m);
        }
        if retry_after.is_some() {
            entry.retry_after = retry_after;
        }
    }

    /// `riot_app_limit_remaining_pct_<scope>` for every scope whose app
    /// headers have been seen, the scope flattened as `/status` keys are.
    pub fn metrics(&self) -> BTreeMap<String, u64> {
        self.lock()
            .iter()
            .filter_map(|(scope, e)| {
                let pct = least(&e.app.as_ref()?.windows)?;
                Some((
                    format!("riot_app_limit_remaining_pct_{}", key_name(scope)),
                    u64::from(pct),
                ))
            })
            .collect()
    }

    /// `GET /status/ratelimits`'s body at `now_ms`, with `retry_after_s`
    /// the longest freeze left on any scope.
    pub fn report(&self, retry_after_s: u64, now_ms: i64) -> RateLimitReport {
        let table = self.lock().clone();
        RateLimitReport {
            schema: super::SCHEMA,
            at: rfc3339(now_ms),
            retry_after_s,
            scopes: table
                .into_iter()
                .map(|(scope, e)| (scope.to_string(), ScopeRateLimits::from(e)))
                .collect(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<&'static str, ScopeEntry>> {
        // A poisoned lock still holds a whole map: each write is one update.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `GET /status/ratelimits`: what Riot last said about every scope's limits.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RateLimitReport {
    /// Always 1.
    pub schema: u32,
    /// When this answer was made; RFC 3339, UTC.
    pub at: String,
    /// The longest any scope is still frozen after a 429, rounded up; 0 when
    /// nothing is throttled. The same as `/status`'s `riot_retry_after_s`.
    pub retry_after_s: u64,
    /// By scope: a platform (`oc1`) or a region (`sea`).
    pub scopes: BTreeMap<String, ScopeRateLimits>,
}

/// One scope's latest headers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScopeRateLimits {
    /// When the app headers were last seen; null before the first.
    pub observed_at: Option<String>,
    /// `X-App-Rate-Limit` with `X-App-Rate-Limit-Count`, shortest window first.
    pub app: Vec<RateLimitWindow>,
    /// The smallest `remaining_pct` in `app`; null when `app` is empty.
    pub remaining_pct: Option<u32>,
    /// The last `Retry-After` Riot sent on this scope.
    pub retry_after: LastRetryAfter,
    /// By method id (`summoner.byPuuid`).
    pub methods: BTreeMap<String, MethodRateLimits>,
}

/// The last `Retry-After` on a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LastRetryAfter {
    /// The header's value in seconds, as sent; 0 before the first.
    pub s: u64,
    /// When it was sent; RFC 3339, null before the first.
    pub at: Option<String>,
}

/// One method's latest `X-Method-Rate-Limit` and its count.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MethodRateLimits {
    /// RFC 3339, UTC.
    pub observed_at: String,
    /// Shortest window first.
    pub windows: Vec<RateLimitWindow>,
}

impl From<ScopeEntry> for ScopeRateLimits {
    fn from(e: ScopeEntry) -> Self {
        let (observed_at, app) = match e.app {
            Some(o) => (Some(rfc3339(o.at_ms)), o.windows),
            None => (None, Vec::new()),
        };
        Self {
            observed_at,
            remaining_pct: least(&app),
            app,
            retry_after: match e.retry_after {
                Some(r) => LastRetryAfter {
                    s: r.s,
                    at: Some(rfc3339(r.at_ms)),
                },
                None => LastRetryAfter { s: 0, at: None },
            },
            methods: e
                .methods
                .into_iter()
                .map(|(method, o)| {
                    (
                        method.to_string(),
                        MethodRateLimits {
                            observed_at: rfc3339(o.at_ms),
                            windows: o.windows,
                        },
                    )
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;

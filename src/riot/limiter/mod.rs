//! The header-driven rate limiter (docs/design/05, with the sliding-log windows of
//! v1 — ADR-023). Runs in process memory: one mutex around every scope's windows,
//! held only for check-and-take.
//!
//! P2-01: the public API with `todo!()` bodies, so the ported v1 suite in
//! `tests.rs` compiles. P2-02..P2-06 fill it in and un-ignore the tests.

pub mod headers;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

use self::headers::{LimitWindow, RateLimitHeaders, RateLimitType};

/// Who is asking. Interactive requests are a user waiting; bulk is background work
/// that yields at `BULK_USAGE_CEILING` and whenever interactive requests queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    Interactive,
    Bulk,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Bulk => "bulk",
        }
    }
}

/// A granted request. Plain data with no `Drop` side effects: once taken, a token is
/// spent whether or not the request succeeds (design/05, v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permit {
    /// Time spent waiting for the token (v1 `waitedMs`).
    pub waited: Duration,
}

/// No token within the caller's budget (v1 `RateLimitBudgetExceeded`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rate limit budget exceeded; next token at {retry_at:?}")]
pub struct RateLimited {
    /// Earliest instant a token could be available.
    pub retry_at: Instant,
}

/// One window's current use, as the admin surface and dashboard report it (v1 `usage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowUsage {
    /// `limit:seconds`, e.g. `20:1`.
    pub window: String,
    pub used: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodUsage {
    pub method: String,
    pub windows: Vec<WindowUsage>,
}

/// The in-process limiter. `scope` is the routing value on the host (`euw1`,
/// `europe`); `method` is the endpoint's method id.
#[derive(Debug)]
pub struct Limiter {
    _bulk_ceiling: f64,
}

impl Limiter {
    /// `bulk_ceiling` is `BULK_USAGE_CEILING` (0.80 by default).
    pub fn new(bulk_ceiling: f64) -> Self {
        Self {
            _bulk_ceiling: bulk_ceiling,
        }
    }

    /// Take one token from every app and method window for `scope`/`method`, all or
    /// nothing, waiting up to `budget` for windows to roll over.
    pub async fn acquire(
        &self,
        _scope: &str,
        _method: &str,
        _priority: Priority,
        _budget: Duration,
    ) -> Result<Permit, RateLimited> {
        todo!("P2-03")
    }

    /// Learn limits and absorb Riot's counts from one response's headers.
    pub fn observe(&self, _scope: &str, _method: &str, _headers: &RateLimitHeaders) {
        todo!("P2-04")
    }

    /// Block every acquire on `scope` for `retry_after` (typed 429).
    pub fn freeze(&self, _scope: &str, _retry_after: Duration, _kind: RateLimitType) {
        todo!("P2-04")
    }

    /// Time left on a freeze, if any (v1 `isFrozen`).
    pub fn frozen_for(&self, _scope: &str) -> Option<Duration> {
        todo!("P2-04")
    }

    /// Set a scope's app windows directly (what v1 tests did with `redis.set(cfg)`).
    pub fn configure_app(&self, _scope: &str, _windows: &[LimitWindow]) {
        todo!("P2-02")
    }

    /// Set a method's windows directly.
    pub fn configure_method(&self, _scope: &str, _method: &str, _windows: &[LimitWindow]) {
        todo!("P2-02")
    }

    /// Current app-window use for a scope.
    pub fn usage(&self, _scope: &str) -> Vec<WindowUsage> {
        todo!("P2-02")
    }

    /// Current use of each named method's windows.
    pub fn method_usage(&self, _scope: &str, _methods: &[&str]) -> Vec<MethodUsage> {
        todo!("P2-02")
    }

    /// Scopes with any configured app or method windows.
    pub fn known_scopes(&self) -> Vec<String> {
        todo!("P2-02")
    }

    /// Methods with configured windows, per scope, sorted.
    pub fn known_scope_methods(&self) -> BTreeMap<String, Vec<String>> {
        todo!("P2-02")
    }

    /// Interactive callers currently queued on `scope` (bulk yields while > 0).
    pub fn interactive_waiters(&self, _scope: &str) -> usize {
        todo!("P2-05")
    }
}

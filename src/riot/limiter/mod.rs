//! The header-driven rate limiter (docs/design/05, with the sliding-log windows of
//! v1 — ADR-023). Runs in process memory: one mutex around every scope's windows,
//! held only for check-and-take.
//!
//! P2-01: the public API with `todo!()` bodies, so the ported v1 suite in
//! `tests.rs` compiles. P2-02..P2-06 fill it in and un-ignore the tests.

pub mod bucket;
pub mod headers;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use self::bucket::{ScopeEntry, ScopeState, Window};
use self::headers::{BOOTSTRAP_APP_LIMITS, LimitWindow, RateLimitHeaders, RateLimitType};

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
    scopes: Mutex<HashMap<String, ScopeEntry>>,
}

impl ScopeEntry {
    fn bootstrap() -> Self {
        Self {
            app: ScopeState::new(&BOOTSTRAP_APP_LIMITS),
            ..Self::default()
        }
    }
}

fn usage_of(window: &mut Window, now: Instant) -> WindowUsage {
    WindowUsage {
        window: format!("{}:{}", window.limit, window.seconds),
        used: window.used(now),
        limit: window.limit,
    }
}

impl Limiter {
    /// `bulk_ceiling` is `BULK_USAGE_CEILING` (0.80 by default).
    pub fn new(bulk_ceiling: f64) -> Self {
        Self {
            _bulk_ceiling: bulk_ceiling,
            scopes: Mutex::new(HashMap::new()),
        }
    }

    /// The state lock. Held only for check-and-take or bookkeeping, never across
    /// an await. A panic while holding it leaves counts as they were, which is
    /// safe, so poisoning is ignored.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, ScopeEntry>> {
        self.scopes.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// Admissions in windows of matching length are kept.
    pub fn configure_app(&self, scope: &str, windows: &[LimitWindow]) {
        let mut scopes = self.lock();
        let entry = scopes
            .entry(scope.to_string())
            .or_insert_with(ScopeEntry::bootstrap);
        entry.app.reconfigure(windows);
        entry.app_known = true;
    }

    /// Set a method's windows directly.
    pub fn configure_method(&self, scope: &str, method: &str, windows: &[LimitWindow]) {
        let mut scopes = self.lock();
        let entry = scopes
            .entry(scope.to_string())
            .or_insert_with(ScopeEntry::bootstrap);
        entry
            .methods
            .entry(method.to_string())
            .or_default()
            .reconfigure(windows);
    }

    /// Current app-window use for a scope, shortest window first. An untouched
    /// scope reports the bootstrap windows, empty.
    pub fn usage(&self, scope: &str) -> Vec<WindowUsage> {
        let now = Instant::now();
        let mut scopes = self.lock();
        match scopes.get_mut(scope) {
            Some(entry) => entry.app.windows.iter_mut().map(|w| usage_of(w, now)).collect(),
            None => ScopeState::new(&BOOTSTRAP_APP_LIMITS)
                .windows
                .iter_mut()
                .map(|w| usage_of(w, now))
                .collect(),
        }
    }

    /// Current use of each named method's windows. Methods with no known limits
    /// report no windows.
    pub fn method_usage(&self, scope: &str, methods: &[&str]) -> Vec<MethodUsage> {
        let now = Instant::now();
        let mut scopes = self.lock();
        let mut entry = scopes.get_mut(scope);
        methods
            .iter()
            .map(|&method| MethodUsage {
                method: method.to_string(),
                windows: entry
                    .as_deref_mut()
                    .and_then(|e| e.methods.get_mut(method))
                    .map(|s| s.windows.iter_mut().map(|w| usage_of(w, now)).collect())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Scopes whose app or method limits are known (from Riot or configuration),
    /// sorted. A scope only ever seen with the bootstrap limits is not listed.
    pub fn known_scopes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .lock()
            .iter()
            .filter(|(_, e)| e.app_known || !e.methods.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        out.sort();
        out
    }

    /// Methods with known windows, per scope, sorted.
    pub fn known_scope_methods(&self) -> BTreeMap<String, Vec<String>> {
        self.lock()
            .iter()
            .filter(|(_, e)| !e.methods.is_empty())
            .map(|(k, e)| (k.clone(), e.methods.keys().cloned().collect()))
            .collect()
    }

    /// Interactive callers currently queued on `scope` (bulk yields while > 0).
    pub fn interactive_waiters(&self, _scope: &str) -> usize {
        todo!("P2-05")
    }
}

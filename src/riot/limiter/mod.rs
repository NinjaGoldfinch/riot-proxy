//! The header-driven rate limiter (docs/design/05, with the sliding-log windows of
//! v1 — ADR-023). Runs in process memory: one mutex around every scope's windows,
//! held only for check-and-take.
//!
//! Windows and scopes (P2-02), acquire (P2-03), observe and freeze (P2-04),
//! priorities (P2-05), checkpoint and restore in `persist` (P2-06).

pub mod bucket;
pub mod headers;
pub mod persist;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::http::{ApiError, ErrorCode};
use crate::metrics::{LIMITER_BULK_WAITERS, LIMITER_INTERACTIVE_WAITERS, RL_WAIT_SECONDS};

use self::bucket::{ScopeEntry, ScopeState, Window};
use self::headers::{BOOTSTRAP_APP_LIMITS, CountWindow, LimitWindow, RateLimitHeaders, RateLimitType};

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

impl RateLimited {
    /// Seconds until `retry_at`, rounded up, at least 1.
    pub fn retry_after_secs(&self) -> u64 {
        let remaining = self.retry_at.saturating_duration_since(Instant::now());
        remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
    }
}

/// v1 `ProxyError.rateLimited`: `RATE_LIMITED` (503) with `Retry-After`.
impl From<RateLimited> for ApiError {
    fn from(e: RateLimited) -> Self {
        ApiError::new(ErrorCode::RateLimited, "Upstream rate limit budget exceeded")
            .with_retry_after(e.retry_after_secs().max(1))
    }
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
    bulk_ceiling: f64,
    scopes: Mutex<HashMap<String, ScopeEntry>>,
    /// Wakes waiters when state changes in a way a sleep would not notice: an
    /// interactive waiter leaving, a freeze lifting early, new limits.
    changed: Notify,
    bulk_waiting: AtomicUsize,
    interactive_waiting: AtomicUsize,
}

enum Attempt {
    Taken,
    WaitUntil(Instant),
}

/// The largest count a window can hold while bulk may still take from it: bulk
/// needs `used < ceiling × limit` (v1 test: 8 of 10 at 0.80 holds bulk back).
fn max_under_ceiling(limit: u32, ceiling: f64) -> u32 {
    let threshold = f64::from(limit) * ceiling;
    // Largest integer strictly below the threshold, and never negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let below = (threshold.ceil() as u32).saturating_sub(1);
    below
}

/// Registered while an acquire waits; unregisters on drop, so a cancelled acquire
/// can never leave a phantom waiter behind (the in-process form of v1's leaked
/// Redis waiter).
struct WaiterGuard<'a> {
    limiter: &'a Limiter,
    scope: String,
    priority: Priority,
}

impl<'a> WaiterGuard<'a> {
    fn new(limiter: &'a Limiter, scope: &str, priority: Priority) -> Self {
        match priority {
            Priority::Interactive => {
                if let Some(e) = limiter.lock().get_mut(scope) {
                    e.interactive_waiters += 1;
                }
                limiter.interactive_waiting.fetch_add(1, Ordering::Relaxed);
            }
            Priority::Bulk => {
                limiter.bulk_waiting.fetch_add(1, Ordering::Relaxed);
            }
        }
        limiter.publish_waiters();
        Self {
            limiter,
            scope: scope.to_string(),
            priority,
        }
    }
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        match self.priority {
            Priority::Interactive => {
                if let Some(e) = self.limiter.lock().get_mut(&self.scope) {
                    e.interactive_waiters = e.interactive_waiters.saturating_sub(1);
                }
                self.limiter.interactive_waiting.fetch_sub(1, Ordering::Relaxed);
                // Bulk may be standing aside for this caller.
                self.limiter.changed.notify_waiters();
            }
            Priority::Bulk => {
                self.limiter.bulk_waiting.fetch_sub(1, Ordering::Relaxed);
            }
        }
        self.limiter.publish_waiters();
    }
}

impl ScopeEntry {
    fn bootstrap() -> Self {
        Self {
            app: ScopeState::new(&BOOTSTRAP_APP_LIMITS),
            ..Self::default()
        }
    }
}

/// Sync each window to Riot's count for the window of the same length.
fn sync_counts(state: &mut ScopeState, counts: &[CountWindow], now: Instant) {
    for c in counts {
        if let Some(w) = state.windows.iter_mut().find(|w| w.seconds == c.seconds) {
            w.sync(c.count, now);
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
            bulk_ceiling,
            scopes: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            bulk_waiting: AtomicUsize::new(0),
            interactive_waiting: AtomicUsize::new(0),
        }
    }

    fn publish_waiters(&self) {
        #[allow(clippy::cast_precision_loss)]
        {
            metrics::gauge!(LIMITER_INTERACTIVE_WAITERS)
                .set(self.interactive_waiting.load(Ordering::Relaxed) as f64);
            metrics::gauge!(LIMITER_BULK_WAITERS).set(self.bulk_waiting.load(Ordering::Relaxed) as f64);
        }
    }

    /// The state lock. Held only for check-and-take or bookkeeping, never across
    /// an await. A panic while holding it leaves counts as they were, which is
    /// safe, so poisoning is ignored.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, ScopeEntry>> {
        self.scopes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take one token from every app and method window for `scope`/`method`, all or
    /// nothing, waiting up to `budget` for windows to roll over (design/05 §Acquire).
    /// The lock is held only for each check-and-take, never across a wait.
    ///
    /// Priorities (design/05 §Priorities, v1 §9.3): an interactive caller that has
    /// to wait registers as a waiter, and while any are registered on the scope,
    /// bulk callers stand aside. Bulk also stands aside while any window is at or
    /// above `BULK_USAGE_CEILING`, keeping the rest for interactive traffic.
    /// A wait that cannot finish inside `budget` fails at once with `RateLimited`.
    pub async fn acquire(
        &self,
        scope: &str,
        method: &str,
        priority: Priority,
        budget: Duration,
    ) -> Result<Permit, RateLimited> {
        let started = Instant::now();
        let deadline = started + budget;
        let mut waiter: Option<WaiterGuard<'_>> = None;
        loop {
            // Enable the wakeup before checking, so a change between the check and
            // the wait is not missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            let now = Instant::now();
            let until = match self.try_take_all(scope, method, priority, now) {
                Attempt::Taken => {
                    let waited = now - started;
                    metrics::histogram!(RL_WAIT_SECONDS, "region" => scope.to_string(), "priority" => priority.as_str())
                        .record(waited.as_secs_f64());
                    return Ok(Permit { waited });
                }
                Attempt::WaitUntil(until) => until,
            };
            if until > deadline {
                return Err(RateLimited { retry_at: until });
            }
            if waiter.is_none() {
                waiter = Some(WaiterGuard::new(self, scope, priority));
            }
            tokio::select! {
                () = tokio::time::sleep_until(until) => {}
                () = &mut changed => {}
            }
        }
    }

    /// One critical section: take from every window or from none. Otherwise, when
    /// to try again: when every window has room, a freeze ends, or (bulk) usage
    /// drops below the ceiling. Bulk blocked only by queued interactive callers
    /// retries when every window has room *and* is woken early when they leave.
    fn try_take_all(&self, scope: &str, method: &str, priority: Priority, now: Instant) -> Attempt {
        let mut scopes = self.lock();
        let entry = scopes
            .entry(scope.to_string())
            .or_insert_with(ScopeEntry::bootstrap);
        if let Some(until) = entry.frozen_until {
            if until > now {
                return Attempt::WaitUntil(until);
            }
            entry.frozen_until = None;
        }
        let interactive_waiting = entry.interactive_waiters > 0;
        let ScopeEntry { app, methods, .. } = entry;
        let mut windows: Vec<&mut Window> = app.windows.iter_mut().collect();
        if let Some(m) = methods.get_mut(method) {
            windows.extend(m.windows.iter_mut());
        }

        if priority == Priority::Bulk {
            let ceiling_clear = windows
                .iter_mut()
                .map(|w| w.until_at_most(max_under_ceiling(w.limit, self.bulk_ceiling), now))
                .max()
                .unwrap_or(now);
            if ceiling_clear > now {
                return Attempt::WaitUntil(ceiling_clear);
            }
            if interactive_waiting {
                let room = windows.iter_mut().map(|w| w.next_free(now)).max().unwrap_or(now);
                // Never "now": that would spin. The waiter leaving wakes us anyway.
                return Attempt::WaitUntil(room.max(now + Duration::from_millis(1)));
            }
        }

        let mut taken = 0;
        for w in windows.iter_mut() {
            if !w.try_take(now) {
                break;
            }
            taken += 1;
        }
        if taken == windows.len() {
            return Attempt::Taken;
        }
        for w in windows.iter_mut().take(taken) {
            w.rollback();
        }
        Attempt::WaitUntil(windows.iter_mut().map(|w| w.next_free(now)).max().unwrap_or(now))
    }

    /// Learn limits and absorb Riot's counts from one response's headers, errors
    /// included (design/05 §Observe, v1 `observeHeaders`):
    ///
    /// 1. New `X-App-Rate-Limit` / `X-Method-Rate-Limit` windows reconfigure the
    ///    buckets, keeping admissions in windows of the same length.
    /// 2. `-Count` headers sync each window up to Riot's count, never down.
    /// 3. A typed 429 (`X-Rate-Limit-Type` + `Retry-After`) freezes the scope.
    ///    An untyped 429 changes nothing; the caller backs off (ADR-021).
    pub fn observe(&self, scope: &str, method: &str, headers: &RateLimitHeaders) {
        let now = Instant::now();
        {
            let mut scopes = self.lock();
            let entry = scopes
                .entry(scope.to_string())
                .or_insert_with(ScopeEntry::bootstrap);
            if let Some(limits) = &headers.app_limits {
                entry.app.reconfigure(limits);
                // Even when equal to the bootstrap limits (v1 regression test).
                entry.app_known = true;
            }
            if let Some(limits) = &headers.method_limits {
                entry
                    .methods
                    .entry(method.to_string())
                    .or_default()
                    .reconfigure(limits);
            }
            if let Some(counts) = &headers.app_counts {
                sync_counts(&mut entry.app, counts, now);
            }
            if let (Some(counts), Some(state)) = (&headers.method_counts, entry.methods.get_mut(method)) {
                sync_counts(state, counts, now);
            }
        }
        if let (Some(kind), Some(seconds)) = (headers.limit_type, headers.retry_after) {
            self.freeze(scope, Duration::from_secs(seconds), kind);
        }
        // New limits can open room sooner than a waiter's computed wake time.
        self.changed.notify_waiters();
    }

    /// Block every acquire on `scope`, every method, for `retry_after`. A longer
    /// freeze already in place is kept. Application and method 429s mean our own
    /// accounting was wrong and are logged at error (v1 §9.4). Metrics are the
    /// client's job, so this never counts a 429.
    pub fn freeze(&self, scope: &str, retry_after: Duration, kind: RateLimitType) {
        let until = Instant::now() + retry_after;
        {
            let mut scopes = self.lock();
            let entry = scopes
                .entry(scope.to_string())
                .or_insert_with(ScopeEntry::bootstrap);
            entry.frozen_until = Some(entry.frozen_until.map_or(until, |current| current.max(until)));
        }
        match kind {
            RateLimitType::Application | RateLimitType::Method => tracing::error!(
                scope,
                kind = kind.as_str(),
                retry_after_s = retry_after.as_secs(),
                "accountable 429: limiter accounting is out of sync; scope frozen"
            ),
            RateLimitType::Service => {
                tracing::warn!(
                    scope,
                    retry_after_s = retry_after.as_secs(),
                    "service 429 with Retry-After; scope frozen"
                );
            }
        }
    }

    /// Time left on a freeze, if any (v1 `isFrozen`).
    pub fn frozen_for(&self, scope: &str) -> Option<Duration> {
        let now = Instant::now();
        let until = self.lock().get(scope)?.frozen_until?;
        (until > now).then(|| until - now)
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
    pub fn interactive_waiters(&self, scope: &str) -> usize {
        self.lock().get(scope).map_or(0, |e| e.interactive_waiters)
    }
}

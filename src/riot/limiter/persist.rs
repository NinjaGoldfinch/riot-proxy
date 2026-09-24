//! Limiter checkpoint and restore (design/05 §Persistence across restarts), for
//! sliding-log windows (ADR-023, ADR-026).
//!
//! A row per app scope (`app:euw1`) and per method (`method:euw1:match.byId`) in
//! `limiter_state`. Admission stamps are stored as unix-ms buckets rounded *up*
//! to a quantum of `max(100 ms, seconds ms)`, so a restored stamp expires no
//! earlier than the real one. At most ~1 000 buckets per window, whatever the limit.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::Limiter;
use super::bucket::{ScopeEntry, ScopeState, Window};
use super::headers::LimitWindow;
use crate::db::{Db, DbError};

/// Design/05: a checkpoint older than the longest dev-key window is not trusted;
/// every window is treated as full until one window length after restore.
pub const STALE_AFTER: Duration = Duration::from_secs(120);
/// Design/05: checkpoint every 10 s (and on shutdown).
pub const CHECKPOINT_EVERY: Duration = Duration::from_secs(10);

/// One `limiter_state` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub scope: String,
    /// JSON [`StoredScope`].
    pub windows: String,
    pub frozen_until: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct StoredScope {
    /// App rows only: limits came from Riot or config, not the bootstrap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    known: bool,
    windows: Vec<StoredWindow>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct StoredWindow {
    limit: u32,
    seconds: u32,
    /// `[unix_ms, count]`, ascending.
    stamps: Vec<(i64, u32)>,
}

/// Maps tokio instants to wall-clock unix ms around one fixed reading of both.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub instant: Instant,
    pub unix_ms: i64,
}

impl Clock {
    pub fn now() -> Self {
        Self {
            instant: Instant::now(),
            unix_ms: jiff::Timestamp::now().as_millisecond(),
        }
    }

    fn to_unix_ms(self, t: Instant) -> i64 {
        let ago = i64::try_from(self.instant.saturating_duration_since(t).as_millis()).unwrap_or(i64::MAX);
        let ahead = i64::try_from(t.saturating_duration_since(self.instant).as_millis()).unwrap_or(i64::MAX);
        self.unix_ms.saturating_sub(ago).saturating_add(ahead)
    }

    /// A stamp from before this process's monotonic clock can represent is
    /// clamped to now: it then expires later than it should, which is safe.
    fn to_instant(self, unix_ms: i64) -> Instant {
        let delta = unix_ms - self.unix_ms;
        let magnitude = Duration::from_millis(delta.unsigned_abs());
        if delta >= 0 {
            self.instant + magnitude
        } else {
            self.instant.checked_sub(magnitude).unwrap_or(self.instant)
        }
    }
}

fn quantum_ms(seconds: u32) -> i64 {
    i64::from(seconds).max(100)
}

fn store_window(w: &Window, clock: Clock) -> StoredWindow {
    let q = quantum_ms(w.seconds);
    let mut buckets: BTreeMap<i64, u32> = BTreeMap::new();
    for t in w.admitted() {
        let ms = clock.to_unix_ms(t);
        let up = ms.div_euclid(q) * q + if ms.rem_euclid(q) == 0 { 0 } else { q };
        *buckets.entry(up).or_default() += 1;
    }
    StoredWindow {
        limit: w.limit,
        seconds: w.seconds,
        stamps: buckets.into_iter().collect(),
    }
}

fn store_scope(state: &ScopeState, known: bool, clock: Clock) -> String {
    let stored = StoredScope {
        known,
        windows: state.windows.iter().map(|w| store_window(w, clock)).collect(),
    };
    serde_json::to_string(&stored).unwrap_or_else(|_| "{\"windows\":[]}".into())
}

fn restore_scope(json: &str, clock: Clock, stale: bool) -> Option<(ScopeState, bool)> {
    let stored: StoredScope = serde_json::from_str(json).ok()?;
    let specs: Vec<LimitWindow> = stored
        .windows
        .iter()
        .map(|w| LimitWindow {
            limit: w.limit,
            seconds: w.seconds,
        })
        .collect();
    let mut state = ScopeState::new(&specs);
    for w in &mut state.windows {
        if stale {
            // Design/05: assume full until one window length from now.
            w.set_admitted(std::iter::repeat_n(clock.instant, w.limit as usize));
        } else if let Some(s) = stored.windows.iter().find(|s| s.seconds == w.seconds) {
            w.set_admitted(
                s.stamps
                    .iter()
                    .flat_map(|&(ms, n)| std::iter::repeat_n(clock.to_instant(ms), n as usize)),
            );
            w.prune(clock.instant);
        }
    }
    Some((state, stored.known))
}

impl Limiter {
    /// Every scope's windows and freeze, as rows (design/05 `checkpoint()`).
    pub fn checkpoint(&self, clock: Clock) -> Vec<ScopeRow> {
        let mut rows = Vec::new();
        let mut scopes = self.lock();
        for (scope, entry) in scopes.iter_mut() {
            for w in &mut entry.app.windows {
                w.prune(clock.instant);
            }
            rows.push(ScopeRow {
                scope: format!("app:{scope}"),
                windows: store_scope(&entry.app, entry.app_known, clock),
                frozen_until: entry
                    .frozen_until
                    .filter(|&t| t > clock.instant)
                    .map(|t| clock.to_unix_ms(t)),
                updated_at: clock.unix_ms,
            });
            for (method, state) in &mut entry.methods {
                for w in &mut state.windows {
                    w.prune(clock.instant);
                }
                rows.push(ScopeRow {
                    scope: format!("method:{scope}:{method}"),
                    windows: store_scope(state, false, clock),
                    frozen_until: None,
                    updated_at: clock.unix_ms,
                });
            }
        }
        rows.sort_by(|a, b| a.scope.cmp(&b.scope));
        rows
    }

    /// Load rows written by [`Limiter::checkpoint`] (design/05 `restore()`).
    /// Expired stamps are dropped; rows older than [`STALE_AFTER`] restore every
    /// window as full. Unreadable rows are skipped with a warning.
    pub fn restore(&self, rows: &[ScopeRow], clock: Clock) {
        let mut scopes = self.lock();
        for row in rows {
            let stale =
                clock.unix_ms - row.updated_at > i64::try_from(STALE_AFTER.as_millis()).unwrap_or(i64::MAX);
            let Some((state, known)) = restore_scope(&row.windows, clock, stale) else {
                tracing::warn!(scope = %row.scope, "unreadable limiter checkpoint row skipped");
                continue;
            };
            if let Some(scope) = row.scope.strip_prefix("app:") {
                let entry = scopes
                    .entry(scope.to_string())
                    .or_insert_with(ScopeEntry::bootstrap);
                entry.app = state;
                entry.app_known = known;
                entry.frozen_until = row
                    .frozen_until
                    .map(|ms| clock.to_instant(ms))
                    .filter(|&t| t > clock.instant);
            } else if let Some((scope, method)) =
                row.scope.strip_prefix("method:").and_then(|r| r.split_once(':'))
            {
                let entry = scopes
                    .entry(scope.to_string())
                    .or_insert_with(ScopeEntry::bootstrap);
                entry.methods.insert(method.to_string(), state);
            } else {
                tracing::warn!(scope = %row.scope, "unknown limiter checkpoint row skipped");
            }
        }
    }
}

/// Replace the stored checkpoint with `rows` in one transaction.
pub async fn save(db: &Db, rows: Vec<ScopeRow>) -> Result<(), DbError> {
    db.write(move |c| {
        let tx = c.transaction()?;
        tx.execute("DELETE FROM limiter_state", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO limiter_state (scope, windows, frozen_until, updated_at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in &rows {
                stmt.execute(rusqlite::params![r.scope, r.windows, r.frozen_until, r.updated_at])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

pub async fn load(db: &Db) -> Result<Vec<ScopeRow>, DbError> {
    db.read(|c| {
        let mut stmt =
            c.prepare("SELECT scope, windows, frozen_until, updated_at FROM limiter_state ORDER BY scope")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ScopeRow {
                    scope: r.get(0)?,
                    windows: r.get(1)?,
                    frozen_until: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Checkpoint now. Errors are logged, not returned: a missed checkpoint only
/// makes the next restart more conservative.
pub async fn checkpoint_now(limiter: &Limiter, db: &Db) {
    let rows = limiter.checkpoint(Clock::now());
    if let Err(e) = save(db, rows).await {
        tracing::warn!(error = %e, "limiter checkpoint failed");
    }
}

/// Load the last checkpoint into `limiter` (boot).
pub async fn restore_from(limiter: &Limiter, db: &Db) -> Result<usize, DbError> {
    let rows = load(db).await?;
    limiter.restore(&rows, Clock::now());
    Ok(rows.len())
}

/// Checkpoint every [`CHECKPOINT_EVERY`] until the task is aborted.
pub fn spawn_checkpoints(limiter: Arc<Limiter>, db: Db) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(CHECKPOINT_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            checkpoint_now(&limiter, &db).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riot::limiter::Priority;
    use crate::riot::limiter::headers::RateLimitType;
    use crate::riot::limiter::headers::parse_limits;

    const SCOPE: &str = "euw1";

    fn w(spec: &str) -> Vec<LimitWindow> {
        parse_limits("t", Some(spec)).unwrap()
    }

    fn clock_at(instant: Instant, unix_ms: i64) -> Clock {
        Clock { instant, unix_ms }
    }

    async fn fill(l: &Limiter, method: &str, n: usize) {
        for _ in 0..n {
            l.acquire(SCOPE, method, Priority::Interactive, Duration::ZERO)
                .await
                .unwrap();
        }
    }

    fn used(l: &Limiter, window: &str) -> u32 {
        l.usage(SCOPE)
            .into_iter()
            .find(|u| u.window == window)
            .map_or(0, |u| u.used)
    }

    #[tokio::test(start_paused = true)]
    async fn round_trip_keeps_counts_limits_methods_and_freezes() {
        let a = Limiter::new(0.8);
        a.configure_app(SCOPE, &w("20:1,100:120"));
        a.configure_method(SCOPE, "match.byId", &w("50:10"));
        fill(&a, "match.byId", 7).await;
        a.freeze("kr", Duration::from_secs(30), RateLimitType::Application);
        let t = Instant::now();
        let rows = a.checkpoint(clock_at(t, 1_000_000));
        assert_eq!(
            rows.iter().map(|r| r.scope.as_str()).collect::<Vec<_>>(),
            ["app:euw1", "app:kr", "method:euw1:match.byId"]
        );

        let b = Limiter::new(0.8);
        b.restore(&rows, clock_at(t, 1_000_000));
        assert_eq!(used(&b, "20:1"), 7);
        assert_eq!(used(&b, "100:120"), 7);
        assert_eq!(b.method_usage(SCOPE, &["match.byId"])[0].windows[0].used, 7);
        assert_eq!(
            b.known_scopes(),
            vec!["euw1".to_string()],
            "kr only had bootstrap limits"
        );
        assert_eq!(b.frozen_for("kr"), Some(Duration::from_secs(30)));
    }

    #[tokio::test(start_paused = true)]
    async fn restored_stamps_expire_no_earlier_than_the_originals() {
        let a = Limiter::new(0.8);
        a.configure_app(SCOPE, &w("5:1"));
        let t0 = Instant::now();
        fill(&a, "m", 1).await;
        tokio::time::advance(Duration::from_millis(450)).await;
        fill(&a, "m", 4).await;
        let t = Instant::now();
        // An awkward wall clock, so rounding matters.
        let rows = a.checkpoint(clock_at(t, 1_000_037));

        let b = Limiter::new(0.8);
        b.restore(&rows, clock_at(t, 1_000_037));
        // Rounding up: every restored stamp is at or after the original, never before.
        tokio::time::advance(Duration::from_millis(550)).await; // t0 + 1000
        assert!(t0 + Duration::from_millis(1000) == Instant::now());
        assert!(
            used(&b, "5:1") >= used(&a, "5:1"),
            "restore is never less conservative"
        );
        assert_eq!(
            used(&b, "5:1"),
            5,
            "the first stamp was rounded up to the next 100 ms"
        );
        tokio::time::advance(Duration::from_millis(600)).await;
        assert_eq!(used(&b, "5:1"), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_stamps_are_dropped_on_restore() {
        let a = Limiter::new(0.8);
        a.configure_app(SCOPE, &w("20:1,100:120"));
        fill(&a, "m", 3).await;
        let t = Instant::now();
        let rows = a.checkpoint(clock_at(t, 5_000_000));

        // Restarted 60 s later: the 1 s window is empty, the 120 s one is not.
        tokio::time::advance(Duration::from_secs(60)).await;
        let b = Limiter::new(0.8);
        b.restore(&rows, clock_at(Instant::now(), 5_060_000));
        assert_eq!(used(&b, "20:1"), 0);
        assert_eq!(used(&b, "100:120"), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_checkpoint_restores_every_window_full() {
        let a = Limiter::new(0.8);
        a.configure_app(SCOPE, &w("20:1,100:120"));
        a.configure_method(SCOPE, "m", &w("50:10"));
        fill(&a, "m", 1).await;
        let rows = a.checkpoint(clock_at(Instant::now(), 1_000_000));

        let b = Limiter::new(0.8);
        let later = clock_at(Instant::now(), 1_000_000 + 121_000);
        b.restore(&rows, later);
        assert_eq!(used(&b, "20:1"), 20);
        assert_eq!(used(&b, "100:120"), 100);
        assert_eq!(b.method_usage(SCOPE, &["m"])[0].windows[0].used, 50);
        let err = b
            .acquire(SCOPE, "m", Priority::Interactive, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(
            err.retry_at,
            later.instant + Duration::from_secs(120),
            "full until one window length"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bad_rows_are_skipped() {
        let b = Limiter::new(0.8);
        let rows = vec![
            ScopeRow {
                scope: "app:euw1".into(),
                windows: "not json".into(),
                frozen_until: None,
                updated_at: 0,
            },
            ScopeRow {
                scope: "weird".into(),
                windows: "{\"windows\":[]}".into(),
                frozen_until: None,
                updated_at: 0,
            },
        ];
        b.restore(&rows, clock_at(Instant::now(), 0));
        assert!(b.known_scopes().is_empty());
    }

    #[test]
    fn stamps_are_bucketed() {
        let t = Instant::now();
        let clock = clock_at(t, 10_000);
        let mut win = Window::new(LimitWindow {
            limit: 1000,
            seconds: 600,
        });
        for ms in [0u64, 1, 2, 599, 600, 601] {
            win.try_take(t - Duration::from_millis(1000) + Duration::from_millis(ms));
        }
        let stored = store_window(&win, clock);
        // Unix ms 9000, 9001, 9002, 9599, 9600, 9601 with a 600 ms quantum (600 s
        // window): exact edges stay put, everything else rounds up.
        assert_eq!(stored.stamps, vec![(9_000, 1), (9_600, 4), (10_200, 1)]);
    }
}

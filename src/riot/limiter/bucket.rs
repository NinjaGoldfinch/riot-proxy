//! Sliding-log windows and per-scope state (ADR-023; v1 `limiter-scripts.ts`).
//!
//! A window remembers the instant of every admission in its last `seconds`, at
//! most `limit` of them. A take succeeds only if fewer than `limit` remain after
//! pruning, so no rolling interval of `seconds` ever holds more than `limit`.
//! That is the property Riot enforces, including across window boundaries.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use super::headers::LimitWindow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub limit: u32,
    pub seconds: u32,
    /// Admission instants, oldest first.
    admitted: VecDeque<Instant>,
}

impl Window {
    pub fn new(spec: LimitWindow) -> Self {
        Self {
            limit: spec.limit,
            seconds: spec.seconds,
            admitted: VecDeque::new(),
        }
    }

    pub fn spec(&self) -> LimitWindow {
        LimitWindow {
            limit: self.limit,
            seconds: self.seconds,
        }
    }

    fn span(&self) -> Duration {
        Duration::from_secs(self.seconds.into())
    }

    /// Forget admissions that have left the window.
    pub fn prune(&mut self, now: Instant) {
        let span = self.span();
        while self.admitted.front().is_some_and(|&t| t + span <= now) {
            self.admitted.pop_front();
        }
    }

    /// Admissions inside the window at `now`.
    pub fn used(&mut self, now: Instant) -> u32 {
        self.prune(now);
        u32::try_from(self.admitted.len()).unwrap_or(u32::MAX)
    }

    /// Admit one request at `now` if the window has room.
    pub fn try_take(&mut self, now: Instant) -> bool {
        if self.used(now) < self.limit {
            self.admitted.push_back(now);
            true
        } else {
            false
        }
    }

    /// Undo the most recent successful `try_take` (all-or-nothing acquire).
    pub fn rollback(&mut self) {
        self.admitted.pop_back();
    }

    /// When the next token frees up: now if there is room, else when the
    /// admission that would have to leave to make room ages out.
    pub fn next_free(&mut self, now: Instant) -> Instant {
        let used = self.used(now);
        if used < self.limit {
            return now;
        }
        // With `used` entries and room for `limit`, the (used - limit)th oldest
        // must leave. Normally used == limit and this is the front.
        let index = (used - self.limit) as usize;
        self.admitted.get(index).map_or(now, |&t| t + self.span())
    }

    /// When at most `max_used` admissions will remain in the window: now if
    /// already so, else when enough of the oldest have aged out.
    pub fn until_at_most(&mut self, max_used: u32, now: Instant) -> Instant {
        let used = self.used(now);
        if used <= max_used {
            return now;
        }
        let must_leave = (used - max_used) as usize;
        self.admitted
            .get(must_leave - 1)
            .map_or(now, |&t| t + self.span())
    }

    /// Absorb Riot's count: pad with entries stamped `now` until we hold at least
    /// `count`. Never lowers, and never moves our own admissions (v1 §9.1).
    pub fn sync(&mut self, count: u32, now: Instant) {
        let used = self.used(now);
        for _ in used..count {
            self.admitted.push_back(now);
        }
    }

    /// Replace every admission with `stamps` (checkpoint restore, P2-06).
    pub fn set_admitted(&mut self, stamps: impl IntoIterator<Item = Instant>) {
        self.admitted = stamps.into_iter().collect();
        self.admitted.make_contiguous().sort();
    }

    pub fn admitted(&self) -> impl Iterator<Item = Instant> + '_ {
        self.admitted.iter().copied()
    }

    /// Fraction of the limit in use at `now` (bulk ceiling, P2-05).
    pub fn usage_ratio(&mut self, now: Instant) -> f64 {
        f64::from(self.used(now)) / f64::from(self.limit.max(1))
    }
}

/// The windows of one app or method bucket, ordered by length.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeState {
    pub windows: Vec<Window>,
}

impl ScopeState {
    pub fn new(specs: &[LimitWindow]) -> Self {
        let mut windows: Vec<Window> = specs.iter().copied().map(Window::new).collect();
        windows.sort_by_key(|w| w.seconds);
        Self { windows }
    }

    pub fn specs(&self) -> Vec<LimitWindow> {
        self.windows.iter().map(Window::spec).collect()
    }

    /// Adopt new limits. Windows whose `seconds` match an existing window keep its
    /// admissions (design/05 §Observe 1); new lengths start empty; dropped lengths
    /// are forgotten. Returns whether anything changed.
    pub fn reconfigure(&mut self, specs: &[LimitWindow]) -> bool {
        let mut sorted = specs.to_vec();
        sorted.sort_by_key(|w| w.seconds);
        if self.specs() == sorted {
            return false;
        }
        let mut old = std::mem::take(&mut self.windows);
        self.windows = sorted
            .into_iter()
            .map(|spec| match old.iter().position(|w| w.seconds == spec.seconds) {
                Some(i) => {
                    let mut w = old.swap_remove(i);
                    w.limit = spec.limit;
                    w
                }
                None => Window::new(spec),
            })
            .collect();
        true
    }
}

/// Everything the limiter knows about one routing value (`euw1`, `europe`).
#[derive(Debug, Clone, Default)]
pub struct ScopeEntry {
    /// App windows. Until Riot tells us, the dev-key bootstrap limits apply.
    pub app: ScopeState,
    /// Whether `app` came from Riot (or configuration) rather than the bootstrap.
    pub app_known: bool,
    /// Method windows by method id. Unknown methods have none until Riot says.
    pub methods: std::collections::BTreeMap<String, ScopeState>,
    /// A typed 429 blocks the whole scope until then.
    pub frozen_until: Option<Instant>,
    /// Interactive acquires currently waiting on this scope. Bulk yields while > 0.
    pub interactive_waiters: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lw(limit: u32, seconds: u32) -> LimitWindow {
        LimitWindow { limit, seconds }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn take_until_full_then_refuse() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(3, 10));
        assert!(w.try_take(t0));
        assert!(w.try_take(t0));
        assert!(w.try_take(t0));
        assert!(!w.try_take(t0));
        assert_eq!(w.used(t0), 3);
    }

    #[test]
    fn rollback_returns_the_last_token() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(2, 10));
        assert!(w.try_take(t0));
        assert!(w.try_take(t0 + secs(1)));
        w.rollback();
        assert_eq!(w.used(t0 + secs(1)), 1);
        // The surviving admission is the older one: it expires at t0 + 10.
        assert_eq!(w.used(t0 + secs(10)), 0);
    }

    #[test]
    fn admissions_expire_individually() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(2, 10));
        w.try_take(t0);
        w.try_take(t0 + secs(4));
        assert!(!w.try_take(t0 + secs(9)));
        assert_eq!(w.used(t0 + secs(10)), 1, "the first left at exactly t0+10");
        assert!(w.try_take(t0 + secs(10)));
        assert!(!w.try_take(t0 + secs(13)), "the second is still inside");
        assert_eq!(w.used(t0 + secs(14)), 1);
    }

    #[test]
    fn next_free_is_when_the_oldest_ages_out() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(2, 10));
        assert_eq!(w.next_free(t0), t0);
        w.try_take(t0);
        w.try_take(t0 + secs(3));
        assert_eq!(w.next_free(t0 + secs(5)), t0 + secs(10));
    }

    #[test]
    fn next_free_accounts_for_overfull_windows() {
        // Synced above the limit (Riot counted more than we allow): the window only
        // has room once enough entries leave to bring it below the limit.
        let t0 = Instant::now();
        let mut w = Window::new(lw(2, 10));
        w.try_take(t0);
        w.sync(4, t0 + secs(2));
        assert_eq!(w.used(t0 + secs(2)), 4);
        assert_eq!(w.next_free(t0 + secs(2)), t0 + secs(12));
    }

    #[test]
    fn no_rolling_interval_exceeds_the_limit() {
        // The boundary burst a fixed counter allows: 1 at 0 ms, 4 at 820 ms, then
        // a refill at 1060 ms. A sliding log admits only what has aged out.
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut w = Window::new(lw(5, 1));
        let mut stamps = vec![];
        for t in [ms(0), ms(820), ms(820), ms(820), ms(820)] {
            assert!(w.try_take(t0 + t));
            stamps.push(t);
        }
        for _ in 0..5 {
            if w.try_take(t0 + ms(1060)) {
                stamps.push(ms(1060));
            }
        }
        assert_eq!(stamps.len(), 6, "only the 0 ms admission has aged out by 1060 ms");
        for &start in &stamps {
            assert!(
                stamps
                    .iter()
                    .filter(|&&t| t >= start && t < start + secs(1))
                    .count()
                    <= 5
            );
        }
    }

    #[test]
    fn sync_pads_but_never_lowers() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(100, 120));
        w.try_take(t0);
        w.sync(47, t0 + secs(60));
        assert_eq!(w.used(t0 + secs(60)), 47);
        w.sync(3, t0 + secs(60));
        assert_eq!(w.used(t0 + secs(60)), 47, "never lowers");
        assert_eq!(w.used(t0 + secs(120)), 46, "our own admission kept its stamp");
        assert_eq!(w.used(t0 + secs(180)), 0);
    }

    #[test]
    fn reconfigure_keeps_admissions_for_matching_lengths() {
        let t0 = Instant::now();
        let mut s = ScopeState::new(&[lw(20, 1), lw(100, 120)]);
        for w in &mut s.windows {
            for _ in 0..5 {
                w.try_take(t0);
            }
        }
        assert!(s.reconfigure(&[lw(30000, 600), lw(500, 10), lw(1000, 120)]));
        let summary: Vec<(u32, u32, u32)> = s
            .windows
            .iter_mut()
            .map(|w| (w.limit, w.seconds, w.used(t0)))
            .collect();
        assert_eq!(summary, vec![(500, 10, 0), (1000, 120, 5), (30000, 600, 0)]);
    }

    #[test]
    fn reconfigure_with_identical_limits_is_a_no_op() {
        let t0 = Instant::now();
        let mut s = ScopeState::new(&[lw(20, 1), lw(100, 120)]);
        s.windows[0].try_take(t0);
        assert!(
            !s.reconfigure(&[lw(100, 120), lw(20, 1)]),
            "order does not matter"
        );
        assert_eq!(s.windows[0].used(t0), 1);
    }

    #[test]
    fn a_smaller_limit_keeps_every_admission() {
        // Conservative: a shrunken limit makes the window over-full until entries age out.
        let t0 = Instant::now();
        let mut s = ScopeState::new(&[lw(10, 10)]);
        for _ in 0..8 {
            s.windows[0].try_take(t0);
        }
        s.reconfigure(&[lw(5, 10)]);
        assert_eq!(s.windows[0].used(t0), 8);
        assert!(!s.windows[0].try_take(t0));
    }

    #[test]
    fn until_at_most_counts_down_the_oldest_stamps() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(10, 10));
        for i in 0..8 {
            w.try_take(t0 + secs(i));
        }
        assert_eq!(w.until_at_most(8, t0 + secs(8)), t0 + secs(8), "already there");
        assert_eq!(
            w.until_at_most(7, t0 + secs(8)),
            t0 + secs(10),
            "the first leaves at t0+10"
        );
        assert_eq!(
            w.until_at_most(5, t0 + secs(8)),
            t0 + secs(12),
            "three must leave"
        );
    }

    #[test]
    fn usage_ratio() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(10, 10));
        for _ in 0..8 {
            w.try_take(t0);
        }
        assert!((w.usage_ratio(t0) - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn restore_sorts_stamps() {
        let t0 = Instant::now();
        let mut w = Window::new(lw(10, 10));
        w.set_admitted([t0 + secs(5), t0, t0 + secs(2)]);
        assert_eq!(
            w.admitted().collect::<Vec<_>>(),
            vec![t0, t0 + secs(2), t0 + secs(5)]
        );
    }
}

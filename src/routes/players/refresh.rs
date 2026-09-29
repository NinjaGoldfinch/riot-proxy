//! The per-player `?refresh=true` window (v1 `refreshWindow`): a refresh is the
//! one thing a consumer can ask for that always spends Riot quota, so it is
//! metered per player and part, at most once per [`COOLDOWN`].
//!
//! v1 kept the window in Redis (`SET NX EX 60`); v2 is one process, so it lives
//! in memory. A restart forgets the running windows, which at worst allows one
//! early refresh per player.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use crate::metrics::REFRESH_CLAIMS_TOTAL;

/// v1 `REFRESH_COOLDOWN_S`.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// Expired windows are swept once the map grows past this.
const SWEEP_AT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// This request won the window and goes upstream.
    pub refreshed: bool,
    /// Seconds until another refresh is allowed; 0 when allowed now.
    pub available_in: u64,
}

#[derive(Debug, Default)]
pub struct RefreshWindows {
    claimed: Mutex<HashMap<(&'static str, String), Instant>>,
}

impl RefreshWindows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the window for `part` of `puuid` if `claim`, or report how long is
    /// left on it. Losing the race is not an error: the winner fetched less than
    /// a minute ago, so the cached read is what was asked for.
    pub fn window(&self, part: &'static str, puuid: &str, claim: bool) -> Window {
        let now = Instant::now();
        let mut map = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (part, puuid.to_string());
        let open = map.get(&key).is_none_or(|t| now.duration_since(*t) >= COOLDOWN);
        if claim && open {
            if map.len() >= SWEEP_AT {
                map.retain(|_, t| now.duration_since(*t) < COOLDOWN);
            }
            map.insert(key, now);
            metrics::counter!(REFRESH_CLAIMS_TOTAL, "part" => part, "outcome" => "claimed").increment(1);
            return Window {
                refreshed: true,
                available_in: COOLDOWN.as_secs(),
            };
        }
        // Only a caller who asked is counted (v1 #81).
        if claim {
            metrics::counter!(REFRESH_CLAIMS_TOTAL, "part" => part, "outcome" => "coalesced").increment(1);
        }
        let left = map.get(&key).map_or(Duration::ZERO, |t| {
            COOLDOWN.saturating_sub(now.duration_since(*t))
        });
        Window {
            refreshed: false,
            // Rounded up, so a window still running never reads as 0.
            available_in: left.as_millis().div_ceil(1000).try_into().unwrap_or(u64::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn one_refresh_per_player_and_part_per_minute() {
        let w = RefreshWindows::new();
        assert_eq!(
            w.window("profile", "p1", false),
            Window {
                refreshed: false,
                available_in: 0
            }
        );
        assert_eq!(
            w.window("profile", "p1", true),
            Window {
                refreshed: true,
                available_in: 60
            }
        );
        assert_eq!(
            w.window("profile", "p1", true),
            Window {
                refreshed: false,
                available_in: 60
            }
        );
        assert!(
            w.window("matches", "p1", true).refreshed,
            "parts are metered independently"
        );
        assert!(w.window("profile", "p2", true).refreshed, "players too");

        tokio::time::advance(Duration::from_millis(59_500)).await;
        assert_eq!(w.window("profile", "p1", false).available_in, 1, "rounded up");
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(w.window("profile", "p1", false).available_in, 0);
        assert!(w.window("profile", "p1", true).refreshed, "the window reopens");
    }

    #[tokio::test(start_paused = true)]
    async fn expired_windows_are_swept() {
        let w = RefreshWindows::new();
        for i in 0..SWEEP_AT {
            w.window("profile", &i.to_string(), true);
        }
        tokio::time::advance(COOLDOWN).await;
        w.window("profile", "late", true);
        let len = w.claimed.lock().unwrap_or_else(PoisonError::into_inner).len();
        assert_eq!(len, 1);
    }
}

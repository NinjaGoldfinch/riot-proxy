//! Converting between tokio's monotonic `Instant` (which tests can pause) and
//! wall-clock unix milliseconds (what SQLite stores). One fixed reading of both
//! clocks anchors every conversion in a batch.

use std::time::Duration;

use tokio::time::Instant;

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

    pub fn to_unix_ms(self, t: Instant) -> i64 {
        let ago = i64::try_from(self.instant.saturating_duration_since(t).as_millis()).unwrap_or(i64::MAX);
        let ahead = i64::try_from(t.saturating_duration_since(self.instant).as_millis()).unwrap_or(i64::MAX);
        self.unix_ms.saturating_sub(ago).saturating_add(ahead)
    }

    /// A stamp from before this process's monotonic clock can represent is
    /// clamped to now: it then expires later than it should, which is safe.
    pub fn to_instant(self, unix_ms: i64) -> Instant {
        let delta = unix_ms - self.unix_ms;
        let magnitude = Duration::from_millis(delta.unsigned_abs());
        if delta >= 0 {
            self.instant + magnitude
        } else {
            self.instant.checked_sub(magnitude).unwrap_or(self.instant)
        }
    }
}

//! Per-consumer request quota (v1 §12.1 / FR-13): `quota_per_min` requests in any
//! rolling minute, independent of Riot's limits.
//!
//! A sliding log per consumer (design/03, plan P4-02; v1's `@fastify/rate-limit`
//! used a fixed window, ADR-034), reusing the limiter's `Window`. Every metered
//! response carries `X-RateLimit-Limit`, `-Remaining` and `-Reset` (header names as
//! v1). Over quota the answer is `QUOTA_EXCEEDED` (429) with `Retry-After`, never
//! Riot's `RATE_LIMITED`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue};
use dashmap::DashMap;
use tokio::time::Instant;

use crate::http::auth::Consumer;
use crate::http::{ApiError, ErrorCode};
use crate::riot::limiter::bucket::Window;
use crate::riot::limiter::headers::LimitWindow;

const WINDOW_SECONDS: u32 = 60;

/// What the headers report after a request was metered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaState {
    pub limit: u32,
    pub remaining: u32,
    /// Seconds until a slot frees up (`X-RateLimit-Reset`).
    pub reset_secs: u64,
}

impl QuotaState {
    pub fn apply(&self, headers: &mut HeaderMap) {
        headers.insert("x-ratelimit-limit", HeaderValue::from(self.limit));
        headers.insert("x-ratelimit-remaining", HeaderValue::from(self.remaining));
        headers.insert("x-ratelimit-reset", HeaderValue::from(self.reset_secs));
    }
}

#[derive(Debug, Default)]
pub struct Quotas {
    windows: DashMap<String, Arc<Mutex<Window>>>,
}

fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

impl Quotas {
    pub fn new() -> Self {
        Self::default()
    }

    /// Meter one request for `consumer`. `Err` carries the 429 and the state for
    /// its headers.
    pub fn check(&self, consumer: &Consumer) -> Result<QuotaState, (ApiError, QuotaState)> {
        let limit = consumer.quota_per_min.max(1);
        let window = self
            .windows
            .entry(consumer.id.clone())
            .or_insert_with(|| {
                Arc::new(Mutex::new(Window::new(LimitWindow {
                    limit,
                    seconds: WINDOW_SECONDS,
                })))
            })
            .clone();
        let mut w = window.lock().unwrap_or_else(PoisonError::into_inner);
        // A consumer's quota can change (admin API); follow it.
        w.limit = limit;
        let now = Instant::now();
        let taken = w.try_take(now);
        let used = w.used(now);
        let next_free = w.next_free(now);
        // With room left, the next slot to free is the oldest admission's.
        let oldest_leaves = w
            .admitted()
            .next()
            .map_or(now, |t| t + Duration::from_secs(WINDOW_SECONDS.into()));
        let state = QuotaState {
            limit,
            remaining: limit.saturating_sub(used),
            reset_secs: ceil_secs(
                if taken { oldest_leaves } else { next_free }.saturating_duration_since(now),
            ),
        };
        if taken {
            Ok(state)
        } else {
            let err = ApiError::new(ErrorCode::QuotaExceeded, format!("Quota of {limit}/min exceeded"))
                .with_retry_after(state.reset_secs.max(1));
            Err((err, state))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consumers::Scope;

    fn consumer(id: &str, quota: u32) -> Consumer {
        Consumer {
            id: id.into(),
            name: id.into(),
            scopes: vec![Scope::Read],
            quota_per_min: quota,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn window_math() {
        let q = Quotas::new();
        let c = consumer("a", 3);
        let first = q.check(&c).unwrap();
        assert_eq!(
            first,
            QuotaState {
                limit: 3,
                remaining: 2,
                reset_secs: 60
            }
        );
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(q.check(&c).unwrap().remaining, 1);
        assert_eq!(
            q.check(&c).unwrap(),
            QuotaState {
                limit: 3,
                remaining: 0,
                reset_secs: 50
            }
        );

        let (err, state) = q.check(&c).unwrap_err();
        assert_eq!(err.code, ErrorCode::QuotaExceeded);
        assert_eq!(err.status.as_u16(), 429);
        assert_eq!(err.message, "Quota of 3/min exceeded");
        assert_eq!(err.retry_after, Some(50), "until the first request ages out");
        assert_eq!(state.remaining, 0);

        // Sliding: at t=60 only the first request has left.
        tokio::time::advance(Duration::from_secs(50)).await;
        assert_eq!(q.check(&c).unwrap().remaining, 0);
        assert!(q.check(&c).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn consumers_are_metered_separately() {
        let q = Quotas::new();
        let (small, large) = (consumer("small", 2), consumer("large", 5));
        q.check(&small).unwrap();
        q.check(&small).unwrap();
        assert!(q.check(&small).is_err());
        assert_eq!(
            q.check(&large).unwrap(),
            QuotaState {
                limit: 5,
                remaining: 4,
                reset_secs: 60
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_changed_quota_applies_at_once() {
        let q = Quotas::new();
        q.check(&consumer("a", 1)).unwrap();
        assert!(q.check(&consumer("a", 1)).is_err());
        assert_eq!(q.check(&consumer("a", 10)).unwrap().remaining, 8);
    }

    #[test]
    fn headers_use_v1_names() {
        let mut h = HeaderMap::new();
        QuotaState {
            limit: 5,
            remaining: 4,
            reset_secs: 60,
        }
        .apply(&mut h);
        assert_eq!(h["x-ratelimit-limit"], "5");
        assert_eq!(h["x-ratelimit-remaining"], "4");
        assert_eq!(h["x-ratelimit-reset"], "60");
    }
}

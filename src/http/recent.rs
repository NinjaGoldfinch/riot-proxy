//! The last requests and errors this process answered (ADM-05), for the
//! dashboard's Traffic tab and its recent-errors panel: two bounded rings,
//! process-global like the metrics recorder, read by `GET /v1/admin/recent`.
//!
//! Requests are pushed by `app::record_request`, errors by
//! `ApiError::into_response`. Neither holds a body, a header or a key: only
//! the path (cut to [`PATH_MAX`] characters), the consumer's name and the
//! request id. The dashboard's own polling and the probes are skipped, so the
//! ring shows traffic rather than the page watching it.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::Serialize;
use utoipa::ToSchema;

/// Requests kept.
pub const REQUESTS_CAP: usize = 500;
/// Errors kept.
pub const ERRORS_CAP: usize = 200;
/// Characters of the path kept.
pub const PATH_MAX: usize = 200;

/// Paths never recorded as requests: probes, the scrape, and the dashboard's polling.
pub const SKIPPED: &[&str] = &[
    "/healthz",
    "/readyz",
    "/metrics",
    "/v1/admin/recent",
    "/v1/admin/metrics",
];

/// One answered request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecentRequest {
    /// Unix ms when the response was ready.
    pub at: i64,
    pub method: String,
    /// The matched route template (`/v1/lol/summoners/by-puuid/:platform/:puuid`),
    /// or `unmatched`: the `route` label of `proxy_requests_total`.
    pub route: String,
    /// The path as requested, at most 200 characters; no query string.
    pub path: String,
    pub status: u16,
    /// `hit`, `miss`, `neg`, `stale`, `archive`, `bypass` or `none`, from `X-Cache`.
    pub cache: String,
    pub latency_ms: u64,
    /// The consumer's name; null before authentication or on a public route.
    pub consumer: Option<String>,
    pub request_id: String,
}

/// One error answered with v1's envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecentError {
    /// Unix ms.
    pub at: i64,
    /// The envelope's `code`, e.g. `NOT_FOUND`.
    pub code: String,
    pub status: u16,
    pub message: String,
    /// Null when raised outside a request.
    pub request_id: Option<String>,
}

/// The consumer a response was served to, set by the auth layer so the
/// request ring can name it.
#[derive(Debug, Clone)]
pub struct ServedTo(pub String);

struct Rings {
    requests: Mutex<VecDeque<RecentRequest>>,
    errors: Mutex<VecDeque<RecentError>>,
}

fn rings() -> &'static Rings {
    static RINGS: OnceLock<Rings> = OnceLock::new();
    RINGS.get_or_init(|| Rings {
        requests: Mutex::new(VecDeque::with_capacity(REQUESTS_CAP)),
        errors: Mutex::new(VecDeque::with_capacity(ERRORS_CAP)),
    })
}

fn push<T>(ring: &Mutex<VecDeque<T>>, cap: usize, item: T) {
    let mut ring = ring.lock().unwrap_or_else(PoisonError::into_inner);
    if ring.len() == cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

fn newest<T: Clone>(ring: &Mutex<VecDeque<T>>, limit: usize) -> Vec<T> {
    let ring = ring.lock().unwrap_or_else(PoisonError::into_inner);
    ring.iter().rev().take(limit).cloned().collect()
}

/// Whether a request to `path` is recorded.
pub fn recorded(path: &str) -> bool {
    !SKIPPED.contains(&path)
}

/// `path` cut to [`PATH_MAX`] characters.
pub fn clip(path: &str) -> String {
    path.chars().take(PATH_MAX).collect()
}

pub fn push_request(r: RecentRequest) {
    push(&rings().requests, REQUESTS_CAP, r);
}

pub fn push_error(e: RecentError) {
    push(&rings().errors, ERRORS_CAP, e);
}

/// The newest `limit` requests, newest first.
pub fn requests(limit: usize) -> Vec<RecentRequest> {
    newest(&rings().requests, limit)
}

/// The newest `limit` errors, newest first.
pub fn errors(limit: usize) -> Vec<RecentError> {
    newest(&rings().errors, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_ring_drops_its_oldest() {
        let ring = Mutex::new(VecDeque::new());
        for i in 0..5 {
            push(&ring, 3, i);
        }
        assert_eq!(newest(&ring, 10), [4, 3, 2]);
        assert_eq!(newest(&ring, 2), [4, 3]);
    }

    #[test]
    fn probes_scrapes_and_the_dashboards_polling_are_skipped() {
        for p in SKIPPED {
            assert!(!recorded(p), "{p}");
        }
        for p in [
            "/v1/admin/metrics/history",
            "/v1/admin/stats",
            "/dashboard",
            "/v1/lol/x",
        ] {
            assert!(recorded(p), "{p}");
        }
    }

    #[test]
    fn paths_are_cut_to_200_characters() {
        let long = format!("/v1/{}", "é".repeat(300));
        assert_eq!(clip(&long).chars().count(), PATH_MAX);
        assert_eq!(clip("/short"), "/short");
    }

    #[test]
    fn the_global_rings_are_capped() {
        // Shared with every other test in this binary: assert only on what this one pushed.
        let tag = "cap-test";
        for i in 0..(ERRORS_CAP + 10) {
            push_error(RecentError {
                at: i64::try_from(i).unwrap_or_default(),
                code: "INTERNAL".into(),
                status: 500,
                message: tag.into(),
                request_id: None,
            });
        }
        let all = errors(usize::MAX);
        assert!(all.len() <= ERRORS_CAP);
        let mine: Vec<&RecentError> = all.iter().filter(|e| e.message == tag).collect();
        assert!(mine.len() <= ERRORS_CAP);
        assert_eq!(mine[0].at, i64::try_from(ERRORS_CAP + 9).unwrap_or_default());
    }
}

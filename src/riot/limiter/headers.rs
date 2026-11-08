//! Riot's rate-limit headers (v1 spec §5.4/§9.1, design/05 §Observe).
//!
//! `X-App-Rate-Limit: 20:1,100:120` is `limit:seconds` per window;
//! `X-App-Rate-Limit-Count: 3:1,40:120` is `count:seconds`. The `X-Method-*` pair is
//! the same for the method bucket. Parsing is tolerant: malformed windows are
//! skipped (v1), and a header with nothing usable is `None` and logged at warn.

use axum::http::HeaderMap;

pub const X_APP_RATE_LIMIT: &str = "x-app-rate-limit";
pub const X_APP_RATE_LIMIT_COUNT: &str = "x-app-rate-limit-count";
pub const X_METHOD_RATE_LIMIT: &str = "x-method-rate-limit";
pub const X_METHOD_RATE_LIMIT_COUNT: &str = "x-method-rate-limit-count";
pub const X_RATE_LIMIT_TYPE: &str = "x-rate-limit-type";
pub const RETRY_AFTER: &str = "retry-after";

/// One window of a limit header: at most `limit` requests per `seconds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitWindow {
    pub limit: u32,
    pub seconds: u32,
}

/// One window of a count header: `count` requests used in the current `seconds` window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountWindow {
    pub count: u32,
    pub seconds: u32,
}

/// Development-key app limits, assumed until Riot's headers say otherwise
/// (v1 `BOOTSTRAP_APP_LIMITS`, spec §2.3 and Appendix A).
pub const BOOTSTRAP_APP_LIMITS: [LimitWindow; 2] = [
    LimitWindow {
        limit: 20,
        seconds: 1,
    },
    LimitWindow {
        limit: 100,
        seconds: 120,
    },
];

/// `X-Rate-Limit-Type` on a 429.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitType {
    /// Our app bucket was exceeded: our accounting was wrong.
    Application,
    /// A method bucket was exceeded: our accounting was wrong.
    Method,
    /// An underlying service limited us; our buckets are fine.
    Service,
}

impl RateLimitType {
    pub fn parse(raw: &str) -> Option<Self> {
        let parsed = Self::parse_quiet(raw);
        if parsed.is_none() {
            tracing::warn!(
                value = raw.trim().to_ascii_lowercase(),
                "unknown X-Rate-Limit-Type"
            );
        }
        parsed
    }

    /// [`parse`](Self::parse) without the warning, for a second read of a
    /// response that was already read once (STS-03's rate-limit table).
    pub fn parse_quiet(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "application" => Some(Self::Application),
            "method" => Some(Self::Method),
            "service" => Some(Self::Service),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Method => "method",
            Self::Service => "service",
        }
    }
}

/// Split `a:b,c:d` into `(a, b)` pairs, skipping any part that isn't two
/// non-negative integers. Sorted by window length so order in the header
/// doesn't matter.
fn pairs(header: &str) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = header
        .split(',')
        .filter_map(|part| {
            let (a, b) = part.trim().split_once(':')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .collect();
    out.sort_by_key(|&(_, seconds)| seconds);
    out
}

fn warn_if_dropped(name: &str, header: &str, kept: usize) {
    let parts = header.split(',').filter(|p| !p.trim().is_empty()).count();
    if kept < parts {
        tracing::warn!(
            header = name,
            value = header,
            kept,
            parts,
            "malformed rate-limit windows skipped"
        );
    }
}

/// Parse a limit header. Windows need `limit > 0` and `seconds > 0` (v1).
/// `None` when the header is absent or has no valid window.
pub fn parse_limits(name: &str, header: Option<&str>) -> Option<Vec<LimitWindow>> {
    limits_of(name, header, true)
}

/// [`parse_limits`], warning on skipped windows only when `warn` is set.
fn limits_of(name: &str, header: Option<&str>, warn: bool) -> Option<Vec<LimitWindow>> {
    let header = header?;
    let windows: Vec<LimitWindow> = pairs(header)
        .into_iter()
        .filter(|&(limit, seconds)| limit > 0 && seconds > 0)
        .map(|(limit, seconds)| LimitWindow { limit, seconds })
        .collect();
    if warn {
        warn_if_dropped(name, header, windows.len());
    }
    (!windows.is_empty()).then_some(windows)
}

/// Parse a count header. Counts may be zero; windows need `seconds > 0`.
/// `None` when the header is absent or has no valid window.
pub fn parse_counts(name: &str, header: Option<&str>) -> Option<Vec<CountWindow>> {
    counts_of(name, header, true)
}

/// [`parse_counts`], warning on skipped windows only when `warn` is set.
fn counts_of(name: &str, header: Option<&str>, warn: bool) -> Option<Vec<CountWindow>> {
    let header = header?;
    let windows: Vec<CountWindow> = pairs(header)
        .into_iter()
        .filter(|&(_, seconds)| seconds > 0)
        .map(|(count, seconds)| CountWindow { count, seconds })
        .collect();
    if warn {
        warn_if_dropped(name, header, windows.len());
    }
    (!windows.is_empty()).then_some(windows)
}

/// Everything the limiter reads from one response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitHeaders {
    pub app_limits: Option<Vec<LimitWindow>>,
    pub app_counts: Option<Vec<CountWindow>>,
    pub method_limits: Option<Vec<LimitWindow>>,
    pub method_counts: Option<Vec<CountWindow>>,
    pub limit_type: Option<RateLimitType>,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
}

impl RateLimitHeaders {
    /// Read one response's headers, warning on malformed windows and an
    /// unknown `X-Rate-Limit-Type`.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self::read(headers, true)
    }

    /// [`from_headers`](Self::from_headers) without the warnings: the
    /// rate-limit table (STS-03) reads every response a second time, after
    /// the fetcher or the status probe has already read and warned on it.
    pub fn from_headers_quiet(headers: &HeaderMap) -> Self {
        Self::read(headers, false)
    }

    fn read(headers: &HeaderMap, warn: bool) -> Self {
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let limit_type = if warn {
            RateLimitType::parse
        } else {
            RateLimitType::parse_quiet
        };
        Self {
            app_limits: limits_of(X_APP_RATE_LIMIT, get(X_APP_RATE_LIMIT), warn),
            app_counts: counts_of(X_APP_RATE_LIMIT_COUNT, get(X_APP_RATE_LIMIT_COUNT), warn),
            method_limits: limits_of(X_METHOD_RATE_LIMIT, get(X_METHOD_RATE_LIMIT), warn),
            method_counts: counts_of(X_METHOD_RATE_LIMIT_COUNT, get(X_METHOD_RATE_LIMIT_COUNT), warn),
            limit_type: get(X_RATE_LIMIT_TYPE).and_then(limit_type),
            retry_after: get(RETRY_AFTER).and_then(|v| v.trim().parse().ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lw(limit: u32, seconds: u32) -> LimitWindow {
        LimitWindow { limit, seconds }
    }

    fn cw(count: u32, seconds: u32) -> CountWindow {
        CountWindow { count, seconds }
    }

    fn limits(h: &str) -> Option<Vec<LimitWindow>> {
        parse_limits("x", Some(h))
    }

    /// v1 test/limiter-parsing.test.ts "parses the documented header format".
    #[test]
    fn parses_the_documented_format() {
        assert_eq!(limits("20:1,100:120"), Some(vec![lw(20, 1), lw(100, 120)]));
    }

    /// v1 "tolerates whitespace and a single window".
    #[test]
    fn tolerates_whitespace_and_a_single_window() {
        assert_eq!(limits(" 500:10 "), Some(vec![lw(500, 10)]));
        assert_eq!(limits("20 : 1 , 100:120"), Some(vec![lw(20, 1), lw(100, 120)]));
    }

    /// v1 "returns an empty list for missing or malformed headers" (None in v2).
    #[test]
    fn missing_or_malformed_is_none() {
        assert_eq!(parse_limits("x", None), None);
        for bad in [
            "", "garbage", "0:1", "20:0", "-1:1", "20:1.5", "20", ":", "20:1:3",
        ] {
            assert_eq!(limits(bad), None, "{bad:?}");
        }
    }

    /// v1 "skips malformed windows but keeps valid ones".
    #[test]
    fn skips_malformed_windows_but_keeps_valid_ones() {
        assert_eq!(limits("20:1,bad,100:120"), Some(vec![lw(20, 1), lw(100, 120)]));
        assert_eq!(limits("20:1,,100:120,"), Some(vec![lw(20, 1), lw(100, 120)]));
    }

    #[test]
    fn out_of_order_windows_are_sorted_by_length() {
        assert_eq!(limits("100:120,20:1"), Some(vec![lw(20, 1), lw(100, 120)]));
        assert_eq!(
            limits("30000:600,500:10"),
            Some(vec![lw(500, 10), lw(30000, 600)])
        );
    }

    #[test]
    fn counts_allow_zero_and_share_the_rules() {
        assert_eq!(
            parse_counts("x", Some("0:1,40:120")),
            Some(vec![cw(0, 1), cw(40, 120)])
        );
        assert_eq!(
            parse_counts("x", Some("40:120,1:1")),
            Some(vec![cw(1, 1), cw(40, 120)])
        );
        assert_eq!(parse_counts("x", Some("3:0")), None);
        assert_eq!(parse_counts("x", Some("abc")), None);
        assert_eq!(parse_counts("x", None), None);
    }

    /// v1 "bootstraps with the documented development-key limits (§2.3)".
    #[test]
    fn bootstrap_app_limits_are_the_dev_key_limits() {
        assert_eq!(BOOTSTRAP_APP_LIMITS, [lw(20, 1), lw(100, 120)]);
    }

    #[test]
    fn rate_limit_type() {
        assert_eq!(
            RateLimitType::parse("application"),
            Some(RateLimitType::Application)
        );
        assert_eq!(RateLimitType::parse("METHOD"), Some(RateLimitType::Method));
        assert_eq!(RateLimitType::parse(" service "), Some(RateLimitType::Service));
        assert_eq!(RateLimitType::parse("nope"), None);
        assert_eq!(RateLimitType::Method.as_str(), "method");
    }

    #[test]
    fn reads_a_full_429_response() {
        let mut h = HeaderMap::new();
        h.insert(X_APP_RATE_LIMIT, "20:1,100:120".parse().unwrap());
        h.insert(X_APP_RATE_LIMIT_COUNT, "21:1,40:120".parse().unwrap());
        h.insert(X_METHOD_RATE_LIMIT, "2000:60".parse().unwrap());
        h.insert(X_METHOD_RATE_LIMIT_COUNT, "5:60".parse().unwrap());
        h.insert(X_RATE_LIMIT_TYPE, "application".parse().unwrap());
        h.insert(RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(
            RateLimitHeaders::from_headers(&h),
            RateLimitHeaders {
                app_limits: Some(vec![lw(20, 1), lw(100, 120)]),
                app_counts: Some(vec![cw(21, 1), cw(40, 120)]),
                method_limits: Some(vec![lw(2000, 60)]),
                method_counts: Some(vec![cw(5, 60)]),
                limit_type: Some(RateLimitType::Application),
                retry_after: Some(7),
            }
        );
        assert_eq!(
            RateLimitHeaders::from_headers(&HeaderMap::new()),
            RateLimitHeaders::default()
        );
    }

    /// The rate-limit table's second read (STS-03) gives the same headers
    /// as the limiter's, malformed windows and an unknown type included, and
    /// logs nothing.
    #[test]
    fn the_quiet_read_matches_the_loud_one() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let logged = |f: &dyn Fn() -> RateLimitHeaders| {
            let buf = Buf::default();
            let writer = buf.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .finish();
            let out = tracing::subscriber::with_default(subscriber, f);
            let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            (out, text)
        };

        let mut h = HeaderMap::new();
        h.insert(X_APP_RATE_LIMIT, "100:120,bad,20:1".parse().unwrap());
        h.insert(X_APP_RATE_LIMIT_COUNT, "41:120,21:1,x:y".parse().unwrap());
        h.insert(X_METHOD_RATE_LIMIT, "2000:60".parse().unwrap());
        h.insert(X_METHOD_RATE_LIMIT_COUNT, "garbage".parse().unwrap());
        h.insert(X_RATE_LIMIT_TYPE, "nope".parse().unwrap());
        h.insert(RETRY_AFTER, "1".parse().unwrap());
        let (loud, loud_log) = logged(&|| RateLimitHeaders::from_headers(&h));
        let (quiet, quiet_log) = logged(&|| RateLimitHeaders::from_headers_quiet(&h));
        assert_eq!(quiet, loud);
        assert_eq!(loud.app_limits, Some(vec![lw(20, 1), lw(100, 120)]));
        assert_eq!(loud.method_counts, None);
        assert_eq!(loud.limit_type, None);
        assert!(
            loud_log.contains("malformed rate-limit windows skipped"),
            "{loud_log}"
        );
        assert!(loud_log.contains("unknown X-Rate-Limit-Type"), "{loud_log}");
        assert_eq!(quiet_log, "", "the quiet read logs nothing");

        let mut ok = HeaderMap::new();
        ok.insert(X_RATE_LIMIT_TYPE, "Service".parse().unwrap());
        assert_eq!(
            RateLimitHeaders::from_headers_quiet(&ok).limit_type,
            Some(RateLimitType::Service)
        );
        assert_eq!(RateLimitType::parse_quiet("nope"), None);
    }

    #[test]
    fn a_non_numeric_retry_after_is_ignored() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(RateLimitHeaders::from_headers(&h).retry_after, None);
    }
}

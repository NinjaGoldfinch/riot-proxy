use axum::http::{HeaderMap, HeaderName, HeaderValue};

use super::*;

/// The headers of a recorded exchange in `tests/fixtures/replay`.
fn fixture(json: &str) -> RateLimitHeaders {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    let mut h = HeaderMap::new();
    for (name, value) in v["headers"].as_object().unwrap() {
        h.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value.as_str().unwrap()).unwrap(),
        );
    }
    RateLimitHeaders::from_headers_quiet(&h)
}

/// Real: `kr`, summoner.byPuuid, 200, recorded 2026-09-24. Riot sends the
/// app windows longest first (`100:120,20:1`).
const SUMMONER: &str = include_str!("../../../tests/fixtures/replay/cold-lookup/02-summoner.byPuuid.json");
/// Real: `asia`, match.byId, 200.
const MATCH: &str = include_str!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.json");
/// Built from SUMMONER (synthetic): an application 429 with `Retry-After: 1`.
const LIMITED: &str =
    include_str!("../../../tests/fixtures/replay/429-typed-application/01-summoner.byPuuid.json");

fn w(window_s: u32, limit: u32, count: u32, remaining_pct: u32) -> RateLimitWindow {
    RateLimitWindow {
        window_s,
        limit,
        count,
        remaining_pct,
    }
}

const T0: i64 = 1_793_000_000_000;

#[test]
fn remaining_pct_rounds_down_and_never_goes_below_zero() {
    assert_eq!(remaining_pct(20, 3), 85);
    assert_eq!(remaining_pct(100, 41), 59);
    assert_eq!(remaining_pct(100, 0), 100);
    assert_eq!(
        remaining_pct(3, 1),
        66,
        "66.7 is 66: headroom is never overstated"
    );
    assert_eq!(remaining_pct(2000, 1), 99);
    assert_eq!(remaining_pct(20, 20), 0);
    assert_eq!(remaining_pct(20, 21), 0, "a 429's count over the limit");
    assert_eq!(remaining_pct(0, 0), 0);
    assert_eq!(remaining_pct(u32::MAX, 0), 100);
}

#[test]
fn windows_pair_by_length_and_drop_an_unmatched_one() {
    let limits = [
        LimitWindow {
            limit: 20,
            seconds: 1,
        },
        LimitWindow {
            limit: 100,
            seconds: 120,
        },
    ];
    let counts = [
        CountWindow {
            count: 41,
            seconds: 120,
        },
        CountWindow { count: 3, seconds: 1 },
    ];
    assert_eq!(windows(&limits, &counts), [w(1, 20, 3, 85), w(120, 100, 41, 59)]);
    assert_eq!(windows(&limits, &counts[..1]), [w(120, 100, 41, 59)]);
    assert_eq!(windows(&limits, &[]), []);
}

/// The parser on real Riot headers: windows sorted, paired, and the scope's
/// headroom the smallest across them.
#[test]
fn reads_recorded_riot_headers() {
    let table = RateLimitTable::default();
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0);
    let report = table.report(0, T0 + 1_000);
    let kr = &report.scopes["kr"];
    // 1:1 of 20 is 95 %; 1:120 of 100 is 99 %.
    assert_eq!(kr.app, [w(1, 20, 1, 95), w(120, 100, 1, 99)]);
    assert_eq!(kr.remaining_pct, Some(95));
    assert_eq!(kr.observed_at.as_deref(), Some(rfc3339(T0).as_str()));
    assert_eq!(kr.methods["summoner.byPuuid"].windows, [w(60, 2000, 1, 99)]);
    assert_eq!(kr.retry_after, LastRetryAfter { s: 0, at: None });
    assert_eq!(report.at, rfc3339(T0 + 1_000));
    assert_eq!(report.schema, 1);
}

/// A 429's `Retry-After` is kept with its time, and its counts (over the
/// limit) read as no headroom.
#[test]
fn a_429_keeps_its_retry_after() {
    let table = RateLimitTable::default();
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0);
    table.record("kr", "summoner.byPuuid", &fixture(LIMITED), T0 + 500);
    let kr = &table.report(1, T0 + 600).scopes["kr"];
    assert_eq!(kr.app, [w(1, 20, 21, 0), w(120, 100, 41, 59)]);
    assert_eq!(kr.remaining_pct, Some(0));
    assert_eq!(
        kr.retry_after,
        LastRetryAfter {
            s: 1,
            at: Some(rfc3339(T0 + 500))
        }
    );
    // A later 200 replaces the windows but not the last Retry-After.
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0 + 2_000);
    let kr = &table.report(0, T0 + 2_000).scopes["kr"];
    assert_eq!(kr.remaining_pct, Some(95));
    assert_eq!(kr.retry_after.s, 1);
}

/// A response without the headers (a service 429, a bare 5xx) leaves the
/// last ones, and one with nothing at all adds no scope.
#[test]
fn missing_headers_leave_what_was_there() {
    let table = RateLimitTable::default();
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0);
    let service_429 = RateLimitHeaders {
        retry_after: Some(5),
        ..RateLimitHeaders::default()
    };
    table.record("kr", "summoner.byPuuid", &service_429, T0 + 10);
    table.record("euw1", "summoner.byPuuid", &RateLimitHeaders::default(), T0 + 10);
    let report = table.report(5, T0 + 20);
    let kr = &report.scopes["kr"];
    assert_eq!(kr.observed_at.as_deref(), Some(rfc3339(T0).as_str()));
    assert_eq!(kr.remaining_pct, Some(95));
    assert_eq!(kr.methods["summoner.byPuuid"].observed_at, rfc3339(T0));
    assert_eq!(kr.retry_after.s, 5);
    assert!(!report.scopes.contains_key("euw1"));
    assert_eq!(report.retry_after_s, 5);
}

/// Methods are kept apart per scope; a scope seen only through a method
/// header has no app windows and no headroom.
#[test]
fn methods_are_per_scope() {
    let table = RateLimitTable::default();
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0);
    table.record("asia", "match.byId", &fixture(MATCH), T0 + 1);
    let only_method = RateLimitHeaders {
        app_limits: None,
        app_counts: None,
        ..fixture(MATCH)
    };
    table.record("sea", "match.byId", &only_method, T0 + 2);
    let report = table.report(0, T0 + 3);
    assert_eq!(
        report.scopes.keys().map(String::as_str).collect::<Vec<_>>(),
        ["asia", "kr", "sea"]
    );
    assert_eq!(
        report.scopes["asia"].methods.keys().collect::<Vec<_>>(),
        ["match.byId"]
    );
    assert_eq!(
        report.scopes["asia"].methods["match.byId"].windows,
        [w(10, 2000, 1, 99)]
    );
    // 2:1 of 20 is 90 %; 3:120 of 100 is 97 %.
    assert_eq!(report.scopes["asia"].remaining_pct, Some(90));
    let sea = &report.scopes["sea"];
    assert_eq!((sea.observed_at.as_deref(), sea.remaining_pct), (None, None));
    assert!(sea.app.is_empty());
    assert_eq!(sea.methods["match.byId"].windows.len(), 1);
}

#[test]
fn metrics_are_the_smallest_headroom_per_scope() {
    let table = RateLimitTable::default();
    assert!(table.metrics().is_empty(), "no key before a scope is seen");
    table.record("kr", "summoner.byPuuid", &fixture(LIMITED), T0);
    table.record("asia", "match.byId", &fixture(MATCH), T0);
    table.record(
        "sea",
        "match.byId",
        &RateLimitHeaders {
            app_limits: None,
            ..fixture(MATCH)
        },
        T0,
    );
    assert_eq!(
        table.metrics(),
        BTreeMap::from([
            ("riot_app_limit_remaining_pct_asia".to_string(), 90),
            ("riot_app_limit_remaining_pct_kr".to_string(), 0),
        ])
    );
}

/// design/14's example body, field for field.
#[test]
fn serialises_in_design_14s_shape() {
    let table = RateLimitTable::default();
    table.record("kr", "summoner.byPuuid", &fixture(SUMMONER), T0);
    let v = serde_json::to_value(table.report(0, T0)).unwrap();
    let at = rfc3339(T0);
    assert_eq!(
        v,
        serde_json::json!({
            "schema": 1,
            "at": at,
            "retry_after_s": 0,
            "scopes": { "kr": {
                "observed_at": at,
                "app": [
                    { "window_s": 1, "limit": 20, "count": 1, "remaining_pct": 95 },
                    { "window_s": 120, "limit": 100, "count": 1, "remaining_pct": 99 }
                ],
                "remaining_pct": 95,
                "retry_after": { "s": 0, "at": null },
                "methods": { "summoner.byPuuid": {
                    "observed_at": at,
                    "windows": [ { "window_s": 60, "limit": 2000, "count": 1, "remaining_pct": 99 } ]
                } }
            } }
        })
    );
}

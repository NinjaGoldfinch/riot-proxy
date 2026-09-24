//! v1's limiter suite (`test/limiter-redis.test.ts` @ c86e631), ported to the
//! in-process API. Each test cites the v1 case it comes from. Cases that only
//! existed because state lived in Redis (a pod killed mid-request, zset member
//! names, config-key TTLs) are ported as the in-process property they protected.
//!
//! All run under `tokio::time::pause()`: time only moves when the test says so.
//! Ignored until the task that implements them (P2-03 acquire, P2-04 observe/freeze,
//! P2-05 priorities) removes the `#[ignore]`.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, advance};

use super::headers::{LimitWindow, RateLimitHeaders, RateLimitType, parse_limits};
use super::{Limiter, MethodUsage, Priority, RateLimited, WindowUsage};

const SCOPE: &str = "test-region";
const NOW: Duration = Duration::ZERO;
const CEILING: f64 = 0.80;

fn w(spec: &str) -> Vec<LimitWindow> {
    parse_limits("test", Some(spec)).expect("valid windows")
}

fn limiter_with_app(spec: &str) -> Limiter {
    let l = Limiter::new(CEILING);
    l.configure_app(SCOPE, &w(spec));
    l
}

async fn take(l: &Limiter, method: &str) -> Result<super::Permit, RateLimited> {
    l.acquire(SCOPE, method, Priority::Interactive, NOW).await
}

async fn take_bulk(l: &Limiter) -> Result<super::Permit, RateLimited> {
    l.acquire(SCOPE, "m", Priority::Bulk, NOW).await
}

fn headers(pairs: &[(&'static str, &str)]) -> RateLimitHeaders {
    let mut h = axum::http::HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, v.parse().expect("header value"));
    }
    RateLimitHeaders::from_headers(&h)
}

fn used(l: &Limiter, window: &str) -> u32 {
    l.usage(SCOPE)
        .into_iter()
        .find(|u| u.window == window)
        .map_or(0, |u| u.used)
}

/// The most admissions inside any `span`-long interval (v1 `worstBurst`).
fn worst_burst(stamps: &[Instant], span: Duration) -> usize {
    stamps
        .iter()
        .map(|&start| stamps.iter().filter(|&&t| t >= start && t < start + span).count())
        .max()
        .unwrap_or(0)
}

// ── acquire (P2-03) ─────────────────────────────────────────────────────────────

/// v1: "admits exactly `limit` requests inside one window".
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn admits_exactly_limit_inside_one_window() {
    let l = limiter_with_app("5:10");
    let mut admitted = 0;
    let mut refused = 0;
    for _ in 0..8 {
        match take(&l, "m").await {
            Ok(_) => admitted += 1,
            Err(_) => refused += 1,
        }
    }
    assert_eq!((admitted, refused), (5, 3));
}

/// v1: "never over-commits a bucket under concurrency".
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn never_over_commits_under_concurrency() {
    let l = Arc::new(limiter_with_app("10:10"));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..50 {
        let l = Arc::clone(&l);
        set.spawn(async move { take(&l, "m").await.is_ok() });
    }
    let admitted = set.join_all().await.into_iter().filter(|ok| *ok).count();
    assert_eq!(admitted, 10);
    assert_eq!(used(&l, "10:10"), 10, "the count is exact, not approximate");
}

/// v1: "requires a token from every window before dispatch (§9.2)".
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn requires_a_token_from_every_window() {
    // A tight short window inside a generous long one: the short one binds.
    let l = limiter_with_app("2:10,100:120");
    take(&l, "m").await.unwrap();
    take(&l, "m").await.unwrap();
    assert!(take(&l, "m").await.is_err());
    // The refused attempt must not have consumed from the wider window (rollback).
    assert_eq!(used(&l, "100:120"), 2);
}

/// v1: "also enforces method buckets on top of app buckets".
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn enforces_method_buckets_on_top_of_app_buckets() {
    let l = limiter_with_app("100:10");
    l.configure_method(SCOPE, "narrow", &w("3:10"));
    let mut admitted = 0;
    for _ in 0..6 {
        if take(&l, "narrow").await.is_ok() {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 3);
    // A different method on the same scope is unaffected.
    assert!(take(&l, "other").await.is_ok());
    // And the refused method attempts did not leak app tokens.
    assert_eq!(used(&l, "100:10"), 4);
}

/// v1: "never admits more than `limit` in any rolling window, boundary included
/// (§9.2)" — the sliding-log property (#17, ADR-023).
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn never_admits_more_than_limit_in_any_rolling_window() {
    let l = limiter_with_app("5:1");
    let opened = Instant::now();
    let mut admitted = Vec::new();

    take(&l, "m").await.unwrap();
    admitted.push(Instant::now());

    // Spend the rest just before a fixed window would lapse…
    advance(Duration::from_millis(820)).await;
    for _ in 0..4 {
        take(&l, "m").await.unwrap();
        admitted.push(Instant::now());
    }

    // …and try again just past the old boundary, where a counter would refill.
    tokio::time::sleep_until(opened + Duration::from_millis(1060)).await;
    for _ in 0..5 {
        if take(&l, "m").await.is_ok() {
            admitted.push(Instant::now());
        }
    }
    assert!(
        worst_burst(&admitted, Duration::from_secs(1)) <= 5,
        "{admitted:?}"
    );
}

/// v1: "waits and succeeds once a window rolls over".
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn waits_and_succeeds_once_a_window_rolls_over() {
    let l = limiter_with_app("1:1");
    take(&l, "m").await.unwrap();
    let permit = l
        .acquire(SCOPE, "m", Priority::Interactive, Duration::from_secs(3))
        .await
        .unwrap();
    // It queued rather than failing: v1 §9.2 "else queue".
    assert!(permit.waited > Duration::ZERO);
    assert!(permit.waited <= Duration::from_secs(1));
}

/// Plan P2-03: beyond the budget, fail fast with the earliest retry time.
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn budget_exceeded_reports_the_next_token() {
    let l = limiter_with_app("1:10");
    let started = Instant::now();
    take(&l, "m").await.unwrap();
    let err = l
        .acquire(SCOPE, "m", Priority::Interactive, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(err.retry_at, started + Duration::from_secs(10));
    assert_eq!(
        Instant::now(),
        started,
        "does not sleep when the wait cannot fit the budget"
    );
}

/// Unknown scopes start from the development-key app limits (v1 `BOOTSTRAP_APP_LIMITS`).
#[tokio::test(start_paused = true)]
#[ignore = "P2-03"]
async fn unknown_scopes_use_the_bootstrap_app_limits() {
    let l = Limiter::new(CEILING);
    for _ in 0..20 {
        take(&l, "m").await.unwrap();
    }
    assert!(take(&l, "m").await.is_err(), "20:1 binds");
    assert_eq!(
        l.usage(SCOPE),
        vec![
            WindowUsage {
                window: "20:1".into(),
                used: 20,
                limit: 20
            },
            WindowUsage {
                window: "100:120".into(),
                used: 20,
                limit: 100
            },
        ]
    );
}

// ── observe and freeze (P2-04) ──────────────────────────────────────────────────

/// v1: "learns limits from response headers and absorbs external usage (§9.1)".
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn learns_limits_from_headers_and_absorbs_external_usage() {
    let l = Limiter::new(CEILING);
    l.observe(
        SCOPE,
        "m",
        &headers(&[
            ("x-app-rate-limit", "20:1,100:120"),
            ("x-app-rate-limit-count", "18:1,40:120"),
            ("x-method-rate-limit", "50:10"),
            ("x-method-rate-limit-count", "5:10"),
        ]),
    );
    // Riot says 18 of 20 used in the 1 s window: our bucket must agree.
    assert_eq!(used(&l, "20:1"), 18);
    assert_eq!(used(&l, "100:120"), 40);
    assert_eq!(
        l.method_usage(SCOPE, &["m"]),
        vec![MethodUsage {
            method: "m".into(),
            windows: vec![WindowUsage {
                window: "50:10".into(),
                used: 5,
                limit: 50
            }]
        }]
    );
}

/// v1: "absorbs Riot counts without disturbing what we already admitted (§9.1)".
/// Our own admission keeps its timestamp; padding is stamped at sync time.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn absorbs_riot_counts_without_disturbing_what_we_admitted() {
    let l = limiter_with_app("100:120");
    take(&l, "m").await.unwrap();
    advance(Duration::from_secs(60)).await;
    l.observe(
        SCOPE,
        "m",
        &headers(&[
            ("x-app-rate-limit", "100:120"),
            ("x-app-rate-limit-count", "47:120"),
        ]),
    );
    assert_eq!(used(&l, "100:120"), 47, "Riot's higher count wins");
    // At t=121 s our own admission ages out on its own timestamp; the padding
    // (stamped at t=60 s) does not.
    advance(Duration::from_secs(61)).await;
    assert_eq!(used(&l, "100:120"), 46);
}

/// Design/05 §Observe: sync never lowers our count (it may include in-flight
/// requests Riot has not seen yet).
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn sync_never_lowers_the_count() {
    let l = limiter_with_app("100:120");
    for _ in 0..10 {
        take(&l, "m").await.unwrap();
    }
    l.observe(
        SCOPE,
        "m",
        &headers(&[
            ("x-app-rate-limit", "100:120"),
            ("x-app-rate-limit-count", "3:120"),
        ]),
    );
    assert_eq!(used(&l, "100:120"), 10);
}

/// v1: "pads with placeholders unique to each sync (§9.1)". In-process there are
/// no member names to collide; the property is that a repeated sync of the same
/// count is idempotent, and that the count climbs back after it drops.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn repeated_syncs_are_idempotent() {
    let l = Limiter::new(CEILING);
    let h = headers(&[
        ("x-app-rate-limit", "100:120"),
        ("x-app-rate-limit-count", "3:120"),
    ]);
    l.observe(SCOPE, "m", &h);
    l.observe(SCOPE, "m", &h);
    assert_eq!(used(&l, "100:120"), 3);
    advance(Duration::from_secs(121)).await;
    assert_eq!(used(&l, "100:120"), 0);
    l.observe(SCOPE, "m", &h);
    assert_eq!(used(&l, "100:120"), 3);
}

/// Design/05 §Observe 1: reconfigure keeps counts for windows matching by `seconds`.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn reconfigure_keeps_counts_for_matching_windows() {
    let l = limiter_with_app("20:1,100:120");
    for _ in 0..5 {
        take(&l, "m").await.unwrap();
    }
    // A production key: bigger limits, one window the same length, one new.
    l.observe(
        SCOPE,
        "m",
        &headers(&[("x-app-rate-limit", "500:10,30000:600,1000:120")]),
    );
    let usage = l.usage(SCOPE);
    assert_eq!(
        usage,
        vec![
            WindowUsage {
                window: "500:10".into(),
                used: 0,
                limit: 500
            },
            WindowUsage {
                window: "1000:120".into(),
                used: 5,
                limit: 1000
            },
            WindowUsage {
                window: "30000:600".into(),
                used: 0,
                limit: 30000
            },
        ]
    );
}

/// v1: "skips the rewrite once that config is actually stored". In-process: seeing
/// the same limits again is a no-op and never resets counts.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn identical_limits_do_not_reset_anything() {
    let l = limiter_with_app("20:1,100:120");
    take(&l, "m").await.unwrap();
    for _ in 0..3 {
        l.observe(SCOPE, "m", &headers(&[("x-app-rate-limit", "20:1,100:120")]));
    }
    assert_eq!(used(&l, "20:1"), 1);
    assert_eq!(used(&l, "100:120"), 1);
}

/// v1: "persists app limits that equal the bootstrap fallback". A dev key's real
/// limits equal the bootstrap ones; the scope must still become known.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn app_limits_equal_to_bootstrap_still_make_the_scope_known() {
    let l = Limiter::new(CEILING);
    take(&l, "m").await.unwrap();
    l.observe(
        SCOPE,
        "m",
        &headers(&[
            ("x-app-rate-limit", "20:1,100:120"),
            ("x-app-rate-limit-count", "2:1,2:120"),
        ]),
    );
    assert!(l.known_scopes().contains(&SCOPE.to_string()));
}

/// v1: "derives known scopes from method configs too".
#[tokio::test(start_paused = true)]
async fn known_scopes_include_method_only_scopes() {
    let l = Limiter::new(CEILING);
    l.configure_method(SCOPE, "match.byId", &w("2000:10"));
    l.configure_method(SCOPE, "account.byPuuid", &w("1000:60"));
    assert!(l.known_scopes().contains(&SCOPE.to_string()));
    assert_eq!(
        l.known_scope_methods().get(SCOPE).cloned(),
        Some(vec!["account.byPuuid".to_string(), "match.byId".to_string()])
    );
}

/// v1: "reports per-method usage for the methods it knows".
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn reports_per_method_usage() {
    let l = limiter_with_app("100:10");
    l.configure_method(SCOPE, "narrow", &w("3:10"));
    take(&l, "narrow").await.unwrap();
    take(&l, "narrow").await.unwrap();
    assert_eq!(
        l.method_usage(SCOPE, &["narrow"]),
        vec![MethodUsage {
            method: "narrow".into(),
            windows: vec![WindowUsage {
                window: "3:10".into(),
                used: 2,
                limit: 3
            }]
        }]
    );
}

/// v1: "freezes the whole scope after a 429 with Retry-After (§9.4)".
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn freezes_the_whole_scope_after_a_typed_429() {
    let metrics = crate::telemetry::metrics_handle().unwrap();
    let count_429 = || {
        metrics
            .render()
            .lines()
            .filter(|l| l.starts_with("proxy_rl_429_total{") && l.contains(SCOPE))
            .count()
    };
    let before = count_429();

    let l = limiter_with_app("100:10");
    l.freeze(SCOPE, Duration::from_secs(2), RateLimitType::Application);
    assert!(l.frozen_for(SCOPE).is_some_and(|d| d > Duration::ZERO));
    // Freezing is a policy action, not an observation: the client owns the counter.
    assert_eq!(count_429(), before);
    // Every method on the scope is blocked, not only the one that tripped it.
    assert!(take(&l, "m").await.is_err());
    assert!(take(&l, "other").await.is_err());

    advance(Duration::from_secs(2)).await;
    assert_eq!(l.frozen_for(SCOPE), None);
    assert!(take(&l, "m").await.is_ok());
}

/// A frozen scope inside the budget waits out the freeze instead of failing.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn a_short_freeze_is_waited_out_within_budget() {
    let l = limiter_with_app("100:10");
    l.freeze(SCOPE, Duration::from_secs(1), RateLimitType::Method);
    let permit = l
        .acquire(SCOPE, "m", Priority::Interactive, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(permit.waited, Duration::from_secs(1));
}

/// Other scopes are unaffected by a freeze.
#[tokio::test(start_paused = true)]
#[ignore = "P2-04"]
async fn a_freeze_is_per_scope() {
    let l = limiter_with_app("100:10");
    l.freeze(
        "other-region",
        Duration::from_secs(30),
        RateLimitType::Application,
    );
    assert!(take(&l, "m").await.is_ok());
}

// ── priorities (P2-05) ─────────────────────────────────────────────────────────

/// v1: "holds bulk work back at the usage ceiling while interactive still passes (§9.3)".
#[tokio::test(start_paused = true)]
#[ignore = "P2-05"]
async fn bulk_is_held_back_at_the_usage_ceiling() {
    let l = limiter_with_app("10:10");
    for _ in 0..8 {
        take(&l, "m").await.unwrap();
    }
    // 8/10 = the 0.80 ceiling.
    assert!(take_bulk(&l).await.is_err());
    assert!(
        take(&l, "m").await.is_ok(),
        "interactive still gets the remainder"
    );
}

/// v1: "holds bulk back while an interactive request is actually queueing".
#[tokio::test(start_paused = true)]
#[ignore = "P2-05"]
async fn bulk_yields_while_an_interactive_request_queues() {
    let l = Arc::new(limiter_with_app("1:10"));
    take(&l, "m").await.unwrap();
    let queued = {
        let l = Arc::clone(&l);
        tokio::spawn(async move {
            l.acquire(SCOPE, "m", Priority::Interactive, Duration::from_millis(1500))
                .await
        })
    };
    tokio::task::yield_now().await;
    assert_eq!(l.interactive_waiters(SCOPE), 1);
    assert!(take_bulk(&l).await.is_err());

    let _ = queued.await;
    // And it withdraws itself on the way out, however it left.
    assert_eq!(l.interactive_waiters(SCOPE), 0);
}

/// v1: "recovers from a waiter leaked by a killed process, under live traffic".
/// In-process the equivalent leak is a cancelled future: dropping a queued
/// interactive acquire must withdraw its waiter, or bulk would stall forever.
#[tokio::test(start_paused = true)]
#[ignore = "P2-05"]
async fn a_cancelled_interactive_waiter_withdraws_itself() {
    let l = Arc::new(limiter_with_app("1:10"));
    take(&l, "m").await.unwrap();
    let queued = {
        let l = Arc::clone(&l);
        tokio::spawn(async move {
            l.acquire(SCOPE, "m", Priority::Interactive, Duration::from_secs(5))
                .await
        })
    };
    tokio::task::yield_now().await;
    assert_eq!(l.interactive_waiters(SCOPE), 1);
    queued.abort();
    let _ = queued.await;
    assert_eq!(l.interactive_waiters(SCOPE), 0);
}

/// v1: "does not count bulk callers as waiters, so they cannot block each other".
#[tokio::test(start_paused = true)]
#[ignore = "P2-05"]
async fn bulk_callers_are_not_counted_as_waiters() {
    let l = limiter_with_app("100:10");
    take_bulk(&l).await.unwrap();
    assert_eq!(l.interactive_waiters(SCOPE), 0);
}

// ── configuration and reporting (P2-02) ─────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn configured_windows_report_empty_usage_in_length_order() {
    let l = Limiter::new(CEILING);
    assert_eq!(
        l.usage(SCOPE),
        vec![
            WindowUsage {
                window: "20:1".into(),
                used: 0,
                limit: 20
            },
            WindowUsage {
                window: "100:120".into(),
                used: 0,
                limit: 100
            },
        ],
        "an untouched scope reports the bootstrap windows"
    );
    assert!(
        l.known_scopes().is_empty(),
        "bootstrap limits do not make a scope known"
    );

    l.configure_app(SCOPE, &w("30000:600,500:10"));
    assert_eq!(
        l.usage(SCOPE),
        vec![
            WindowUsage {
                window: "500:10".into(),
                used: 0,
                limit: 500
            },
            WindowUsage {
                window: "30000:600".into(),
                used: 0,
                limit: 30000
            },
        ]
    );
    assert_eq!(l.known_scopes(), vec![SCOPE.to_string()]);
    assert!(l.known_scope_methods().is_empty());
}

#[tokio::test(start_paused = true)]
async fn unknown_methods_report_no_windows() {
    let l = Limiter::new(CEILING);
    l.configure_method(SCOPE, "narrow", &w("3:10"));
    assert_eq!(
        l.method_usage(SCOPE, &["narrow", "never-seen"]),
        vec![
            MethodUsage {
                method: "narrow".into(),
                windows: vec![WindowUsage {
                    window: "3:10".into(),
                    used: 0,
                    limit: 3
                }]
            },
            MethodUsage {
                method: "never-seen".into(),
                windows: vec![]
            },
        ]
    );
    assert_eq!(l.method_usage("other-scope", &["narrow"])[0].windows, vec![]);
}

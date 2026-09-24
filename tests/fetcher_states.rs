//! The fetcher against a wiremock upstream: every `X-Cache` state (HIT, MISS, STALE,
//! HIT-NEG, ARCHIVE, BYPASS), stale-on-failure, bulk-priority SWR refresh, retries,
//! and the error mapping. Plan P3-05 / P3 exit check. Real time, short TTLs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use riot_proxy::fetcher::{Archive, FetchOptions, Fetcher, XCache};
use riot_proxy::http::ErrorCode;
use riot_proxy::riot::client::RiotRequest;
use riot_proxy::riot::endpoints::Endpoint;
use riot_proxy::riot::limiter::headers::LimitWindow;
use riot_proxy::riot::limiter::{Limiter, Priority};
use riot_proxy::riot::routing::{Platform, Region};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUMMONER: &str = "/lol/summoner/v4/summoners/by-puuid/P";

struct Setup {
    server: MockServer,
    fetcher: Fetcher,
    limiter: Arc<Limiter>,
}

async fn setup(env: &[(&str, &str)]) -> Setup {
    setup_with(env, None).await
}

async fn setup_with(env: &[(&str, &str)], archive: Option<Arc<dyn Archive>>) -> Setup {
    let server = MockServer::start().await;
    let config = common::config(env);
    let limiter = Arc::new(Limiter::new(0.8));
    let fetcher = common::fetcher(&config, &server.uri(), Arc::clone(&limiter), archive);
    Setup {
        server,
        fetcher,
        limiter,
    }
}

fn summoner(platform: Platform) -> RiotRequest {
    let e = Endpoint::by_id("summoner.byPuuid").unwrap();
    RiotRequest::new(e, e.target_for_platform(platform), &["P"]).unwrap()
}

fn spectator() -> RiotRequest {
    let e = Endpoint::by_id("spectator.activeGame").unwrap();
    RiotRequest::new(e, e.target_for_platform(Platform::Euw1), &["P"]).unwrap()
}

fn match_by_id(id: &str) -> RiotRequest {
    let e = Endpoint::by_id("match.byId").unwrap();
    RiotRequest::new(e, e.target_for_region(Region::Europe).unwrap(), &[id]).unwrap()
}

fn ok(body: &'static str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(body)
}

async fn mount(server: &MockServer, p: &str, tpl: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(tpl)
        .mount(server)
        .await;
}

const INTERACTIVE: FetchOptions = FetchOptions {
    priority: Priority::Interactive,
    bypass: false,
};
const BYPASS: FetchOptions = FetchOptions {
    priority: Priority::Interactive,
    bypass: true,
};

async fn upstream_calls(server: &MockServer) -> usize {
    server.received_requests().await.map_or(0, |r| r.len())
}

/// v1: "reports MISS then HIT, and only calls upstream once".
#[tokio::test]
async fn miss_then_hit_with_one_upstream_call() {
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ok(r#"{"level":1}"#)).await;

    let first = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(
        (first.x_cache, first.body.as_ref()),
        (XCache::Miss, br#"{"level":1}"#.as_ref())
    );
    let second = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!((second.x_cache, second.body), (XCache::Hit, first.body));
    assert_eq!(upstream_calls(&s.server).await, 1);
}

/// v1: "coalesces concurrent misses into exactly one upstream call (§8.4)".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hundred_concurrent_misses_make_one_upstream_call() {
    let s = setup(&[]).await;
    mount(
        &s.server,
        SUMMONER,
        ok(r#"{"level":2}"#).set_delay(Duration::from_millis(50)),
    )
    .await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..100 {
        let f = s.fetcher.clone();
        set.spawn(async move { f.fetch(summoner(Platform::Euw1), INTERACTIVE).await });
    }
    let results = set.join_all().await;
    assert_eq!(upstream_calls(&s.server).await, 1);
    assert!(
        results
            .iter()
            .all(|r| r.as_ref().is_ok_and(|r| r.body.as_ref() == br#"{"level":2}"#))
    );
}

/// v1: "negative-caches a 404 so the second lookup costs no quota (§8.3)" → HIT-NEG.
#[tokio::test]
async fn a_404_is_negative_cached_and_served_as_hit_neg() {
    let s = setup(&[]).await;
    mount(
        &s.server,
        "/lol/spectator/v5/active-games/by-summoner/P",
        ResponseTemplate::new(404),
    )
    .await;

    let first = s.fetcher.fetch(spectator(), INTERACTIVE).await.unwrap_err();
    assert_eq!((first.api.code, first.x_cache), (ErrorCode::NotFound, None));
    let second = s.fetcher.fetch(spectator(), INTERACTIVE).await.unwrap_err();
    assert_eq!(
        (second.api.code, second.x_cache),
        (ErrorCode::NotFound, Some(XCache::HitNeg))
    );
    assert_eq!(upstream_calls(&s.server).await, 1);
}

/// Past the soft TTL: STALE at once, then a refresh at *bulk* priority.
#[tokio::test]
async fn stale_is_served_and_refreshed_in_the_background_at_bulk_priority() {
    let metrics = riot_proxy::telemetry::metrics_handle().unwrap();
    let s = setup(&[("CACHE_TTL_OVERRIDES", "summoner=1")]).await;
    mount(
        &s.server,
        "/lol/summoner/v4/summoners/by-puuid/P",
        ok(r#"{"v":1}"#),
    )
    .await;
    s.fetcher
        .fetch(summoner(Platform::Kr), INTERACTIVE)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1100)).await;
    s.server.reset().await;
    mount(&s.server, SUMMONER, ok(r#"{"v":2}"#)).await;

    let stale = s
        .fetcher
        .fetch(summoner(Platform::Kr), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(
        (stale.x_cache, stale.body.as_ref()),
        (XCache::Stale, br#"{"v":1}"#.as_ref())
    );

    let deadline = Instant::now() + Duration::from_secs(3);
    while upstream_calls(&s.server).await == 0 {
        assert!(Instant::now() < deadline, "background refresh never happened");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fresh = s
        .fetcher
        .fetch(summoner(Platform::Kr), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(
        (fresh.x_cache, fresh.body.as_ref()),
        (XCache::Hit, br#"{"v":2}"#.as_ref())
    );

    metrics.run_upkeep();
    let text = metrics.render();
    assert!(
        text.contains(r#"proxy_rl_wait_seconds_count{region="kr",priority="bulk"}"#),
        "{text}"
    );
}

/// v1: "bypasses the cache when asked, but still writes through" → BYPASS.
#[tokio::test]
async fn bypass_skips_reads_and_writes_through() {
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ok(r#"{"n":1}"#)).await;
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    s.server.reset().await;
    mount(&s.server, SUMMONER, ok(r#"{"n":2}"#)).await;

    let bypassed = s.fetcher.fetch(summoner(Platform::Euw1), BYPASS).await.unwrap();
    assert_eq!(
        (bypassed.x_cache, bypassed.body.as_ref()),
        (XCache::Bypass, br#"{"n":2}"#.as_ref())
    );
    let after = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(
        (after.x_cache, after.body.as_ref()),
        (XCache::Hit, br#"{"n":2}"#.as_ref())
    );
}

#[derive(Default)]
struct MemArchive(Mutex<HashMap<String, Bytes>>);

impl Archive for MemArchive {
    fn get(&self, req: &RiotRequest) -> futures_util::future::BoxFuture<'_, Option<Bytes>> {
        let hit = self.0.lock().unwrap().get(&req.path).cloned();
        Box::pin(async move { hit })
    }
    fn put(&self, req: &RiotRequest, body: Bytes) -> futures_util::future::BoxFuture<'_, ()> {
        self.0.lock().unwrap().insert(req.path.clone(), body);
        Box::pin(async {})
    }
}

/// Immutable endpoints: served from the archive (ARCHIVE), archived on a miss.
#[tokio::test]
async fn immutable_reads_come_from_the_archive_and_misses_are_archived() {
    let archive = Arc::new(MemArchive::default());
    archive.0.lock().unwrap().insert(
        "/lol/match/v5/matches/EUW1_1".into(),
        Bytes::from_static(b"{\"archived\":1}"),
    );
    let s = setup_with(&[], Some(archive.clone() as Arc<dyn Archive>)).await;
    mount(&s.server, "/lol/match/v5/matches/EUW1_2", ok(r#"{"fetched":2}"#)).await;

    let a = s.fetcher.fetch(match_by_id("EUW1_1"), INTERACTIVE).await.unwrap();
    assert_eq!(
        (a.x_cache, a.body.as_ref()),
        (XCache::Archive, b"{\"archived\":1}".as_ref())
    );

    let b = s.fetcher.fetch(match_by_id("EUW1_2"), INTERACTIVE).await.unwrap();
    assert_eq!(b.x_cache, XCache::Miss);
    let c = s.fetcher.fetch(match_by_id("EUW1_2"), INTERACTIVE).await.unwrap();
    assert_eq!(
        (c.x_cache, c.body.as_ref()),
        (XCache::Archive, br#"{"fetched":2}"#.as_ref())
    );
    assert_eq!(upstream_calls(&s.server).await, 1, "EUW1_1 never went upstream");
}

/// v1: "serves the stale value when upstream 5xxs (§8.5)", after v1's two retries.
#[tokio::test]
async fn a_persistent_5xx_serves_the_cached_copy_as_stale() {
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ok(r#"{"level":9}"#)).await;
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    s.server.reset().await;
    mount(&s.server, SUMMONER, ResponseTemplate::new(503)).await;

    let r = s.fetcher.fetch(summoner(Platform::Euw1), BYPASS).await.unwrap();
    assert_eq!(
        (r.x_cache, r.body.as_ref()),
        (XCache::Stale, br#"{"level":9}"#.as_ref())
    );
    assert_eq!(upstream_calls(&s.server).await, 3, "one call and two retries");
}

/// v1 client: "recovers when a retried 5xx succeeds".
#[tokio::test]
async fn a_retried_5xx_that_succeeds_is_a_miss() {
    let s = setup(&[]).await;
    Mock::given(path(SUMMONER))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s.server)
        .await;
    mount(&s.server, SUMMONER, ok(r#"{"ok":true}"#)).await;
    let r = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(r.x_cache, XCache::Miss);
    assert_eq!(upstream_calls(&s.server).await, 2);
}

/// With nothing cached, a persistent 5xx is UPSTREAM_ERROR.
#[tokio::test]
async fn a_persistent_5xx_with_nothing_cached_is_upstream_error() {
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ResponseTemplate::new(500)).await;
    let e = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap_err();
    assert_eq!(
        (e.api.code, e.api.status.as_u16()),
        (ErrorCode::UpstreamError, 502)
    );
}

/// v1: "surfaces RATE_LIMITED with a retry hint when the wait budget is blown".
#[tokio::test]
async fn a_blown_budget_is_rate_limited_with_a_retry_hint() {
    let s = setup(&[]).await;
    s.limiter.configure_app(
        "euw1",
        &[LimitWindow {
            limit: 1,
            seconds: 10,
        }],
    );
    s.limiter
        .acquire("euw1", "x", Priority::Interactive, Duration::ZERO)
        .await
        .unwrap();
    let e = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap_err();
    assert_eq!(e.api.code, ErrorCode::RateLimited);
    assert!(
        matches!(e.api.retry_after, Some(9 | 10)),
        "{:?}",
        e.api.retry_after
    );
    assert_eq!(upstream_calls(&s.server).await, 0);
}

/// v1: "serving stale while rate limited".
#[tokio::test]
async fn a_blown_budget_with_a_cached_copy_serves_it_stale() {
    let s = setup(&[]).await;
    s.limiter.configure_app(
        "euw1",
        &[LimitWindow {
            limit: 1,
            seconds: 10,
        }],
    );
    mount(&s.server, SUMMONER, ok(r#"{"cached":1}"#)).await;
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    let r = s.fetcher.fetch(summoner(Platform::Euw1), BYPASS).await.unwrap();
    assert_eq!(
        (r.x_cache, r.body.as_ref()),
        (XCache::Stale, br#"{"cached":1}"#.as_ref())
    );
}

/// v1: "sanitises an upstream auth failure into UPSTREAM_ERROR (§12.2)", never retried.
#[tokio::test]
async fn an_upstream_auth_failure_is_sanitised_and_not_retried() {
    let s = setup(&[]).await;
    mount(
        &s.server,
        SUMMONER,
        ResponseTemplate::new(403).set_body_string("Forbidden: key revoked"),
    )
    .await;
    let e = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap_err();
    assert_eq!(
        (e.api.code, e.api.status.as_u16()),
        (ErrorCode::UpstreamError, 502)
    );
    assert!(!e.api.message.contains("revoked"));
    assert_eq!(upstream_calls(&s.server).await, 1);
}

/// A typed 429 freezes the scope; the fetcher waits it out and retries (v1 §9.4).
#[tokio::test]
async fn a_typed_429_freezes_then_retries() {
    let s = setup(&[]).await;
    Mock::given(path(SUMMONER))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-rate-limit-type", "method")
                .insert_header("retry-after", "1"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s.server)
        .await;
    mount(&s.server, SUMMONER, ok(r#"{"after":"freeze"}"#)).await;
    let started = Instant::now();
    let r = s
        .fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    assert_eq!(r.x_cache, XCache::Miss);
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "waited out the freeze: {:?}",
        started.elapsed()
    );
    assert_eq!(upstream_calls(&s.server).await, 2);
}

/// An untyped (service) 429 backs off without touching the buckets (ADR-021).
#[tokio::test]
async fn a_service_429_backs_off_and_retries() {
    let s = setup(&[]).await;
    Mock::given(path(SUMMONER))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s.server)
        .await;
    mount(&s.server, SUMMONER, ok(r#"{"after":"backoff"}"#)).await;
    let started = Instant::now();
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(390), "500 ms − 20 %: {waited:?}");
    assert_eq!(s.limiter.frozen_for("euw1"), None);
}

/// v1: "reports a re-fetch that changed nothing at the age it already had" and
/// "reports age zero when the payload actually changed".
#[tokio::test]
async fn cache_age_is_content_age() {
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ok(r#"{"same":1}"#)).await;
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let same = s.fetcher.fetch(summoner(Platform::Euw1), BYPASS).await.unwrap();
    assert!(
        same.cache_age >= Duration::from_secs(1),
        "unchanged content keeps its age: {:?}",
        same.cache_age
    );

    s.server.reset().await;
    mount(&s.server, SUMMONER, ok(r#"{"same":2}"#)).await;
    let changed = s.fetcher.fetch(summoner(Platform::Euw1), BYPASS).await.unwrap();
    assert!(
        changed.cache_age < Duration::from_millis(100),
        "{:?}",
        changed.cache_age
    );
}

/// Every response feeds the limiter (v1 §9.1).
#[tokio::test]
async fn responses_feed_the_limiter() {
    let s = setup(&[]).await;
    mount(
        &s.server,
        SUMMONER,
        ok("{}")
            .insert_header("x-app-rate-limit", "500:10,30000:600")
            .insert_header("x-app-rate-limit-count", "7:10,40:600")
            .insert_header("x-method-rate-limit", "1600:60")
            .insert_header("x-method-rate-limit-count", "3:60"),
    )
    .await;
    s.fetcher
        .fetch(summoner(Platform::Na1), INTERACTIVE)
        .await
        .unwrap();
    let usage = s.limiter.usage("na1");
    assert_eq!(
        usage
            .iter()
            .map(|u| (u.window.as_str(), u.used))
            .collect::<Vec<_>>(),
        [("500:10", 7), ("30000:600", 40)]
    );
    assert_eq!(
        s.limiter.method_usage("na1", &["summoner.byPuuid"])[0].windows[0].used,
        3
    );
}

/// Cache hits and misses are counted under v1's state labels.
#[tokio::test]
async fn cache_reads_are_counted() {
    let metrics = riot_proxy::telemetry::metrics_handle().unwrap();
    let s = setup(&[]).await;
    mount(&s.server, SUMMONER, ok("{}")).await;
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    s.fetcher
        .fetch(summoner(Platform::Euw1), INTERACTIVE)
        .await
        .unwrap();
    let text = metrics.render();
    for state in ["hit", "miss"] {
        assert!(
            text.contains(&format!("proxy_cache_reads_total{{state=\"{state}\"}}")),
            "{text}"
        );
    }
}

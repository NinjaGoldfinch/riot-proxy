//! The fetcher: the single funnel every read takes (docs/design/03 §Request
//! lifecycle, v1 `src/fetcher.ts`).
//!
//! ```text
//! archive (immutable) → L1 (HIT / HIT-NEG / STALE + bulk refresh) → single-flight
//!   → limiter → client → observe → cache write → archive write (or rank record)
//! ```
//!
//! The upstream leg owns v1's retry policy (ADR-017, ADR-021): 5xx and network
//! failures retry twice after 250/750 ms; a typed 429 freezes the scope (via
//! `observe`) and re-acquires; a service 429 backs off 500 ms × 2ⁿ, capped at 8 s,
//! up to 3 tries. All waits are jittered ±20 %. When upstream fails or the rate-limit
//! budget runs out and a cached copy is still inside its hard TTL, that copy is
//! served as `STALE` (v1 §8.5).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::Instant;

use crate::cache::ResponseCache;
use crate::cache::keys::{KeyScope, cache_key};
use crate::cache::l1::{CacheEntry, Lookup};
use crate::http::{ApiError, ErrorCode};
use crate::jobs::activity;
use crate::metrics::CACHE_READS_TOTAL;
use crate::riot::client::{RiotClient, RiotErrorKind, RiotRequest};
use crate::riot::endpoints::{Endpoint, Target, TtlPolicy};
use crate::riot::limiter::headers::RateLimitHeaders;
use crate::riot::limiter::{Limiter, Priority, RateLimited};
use crate::singleflight::{SingleFlight, WorkFailed};

/// v1 §5.5: two retries after a 5xx or a network failure.
const SERVER_RETRY_BACKOFF: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(750)];
/// v1 §9.4 / ADR-021: service 429s.
const SERVICE_429_TRIES: u32 = 3;
const SERVICE_429_BASE: Duration = Duration::from_millis(500);
const SERVICE_429_MAX: Duration = Duration::from_secs(8);
/// Typed 429s re-acquire after the freeze; this only stops a pathological loop.
const MAX_ATTEMPTS: u32 = 8;
/// Background refreshes wait rather than fail; this bounds the wait. Jobs
/// wait at most `JOB_YIELD_BUDGET_MS` and give their worker back (SCH-01).
pub const BULK_BUDGET: Duration = Duration::from_secs(15 * 60);
/// A job's trace notes a rate-limit wait from this long on (DEV-19).
const TRACE_WAIT_FROM: Duration = Duration::from_millis(20);

/// `X-Cache` (v1's four plus design 03's `ARCHIVE` and `BYPASS`; ADR-022, ADR-031).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XCache {
    Hit,
    Miss,
    Stale,
    HitNeg,
    Archive,
    Bypass,
}

impl XCache {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "HIT",
            Self::Miss => "MISS",
            Self::Stale => "STALE",
            Self::HitNeg => "HIT-NEG",
            Self::Archive => "ARCHIVE",
            Self::Bypass => "BYPASS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    /// Riot's bytes, untouched.
    pub body: Bytes,
    pub x_cache: XCache,
    /// Age of the content, not of the fetch (`X-Cache-Age`).
    pub cache_age: Duration,
    /// Time since Riot last answered for this key, whether or not the bytes
    /// changed (`X-Cache-Fetched-Age`, SITE-01). `None` from the archive:
    /// immutable data is never fetched again.
    pub fetched_age: Option<Duration>,
}

/// A failed fetch. `x_cache` is set when the answer itself came from cache
/// (`HIT-NEG`), so the route can still send the header.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{api}")]
pub struct FetchError {
    pub api: ApiError,
    pub x_cache: Option<XCache>,
    /// Set when our own limiter had no token inside the budget (not when
    /// Riot answered 429): the first instant one could be free. A job yields
    /// on it (SCH-01).
    pub limited_until: Option<Instant>,
}

impl From<ApiError> for FetchError {
    fn from(api: ApiError) -> Self {
        Self {
            api,
            x_cache: None,
            limited_until: None,
        }
    }
}

impl From<RateLimited> for FetchError {
    fn from(limited: RateLimited) -> Self {
        Self {
            limited_until: Some(limited.retry_at),
            ..ApiError::from(limited).into()
        }
    }
}

impl From<WorkFailed> for FetchError {
    fn from(_: WorkFailed) -> Self {
        ApiError::internal().into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchOptions {
    pub priority: Priority,
    /// Skip every cache read (still writes through): `?refresh=true`, admin only.
    pub bypass: bool,
    /// A job's fetch: bulk, waiting at most `JOB_YIELD_BUDGET_MS` for the
    /// limiter before failing with `limited_until` set (SCH-01).
    pub job: bool,
}

impl FetchOptions {
    /// What every job handler fetches with (design/06 §Bulk limiter priority).
    pub const JOB: Self = Self {
        priority: Priority::Bulk,
        bypass: false,
        job: true,
    };
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            priority: Priority::Interactive,
            bypass: false,
            job: false,
        }
    }
}

/// Immutable payloads (match, timeline) served from the archive: in production
/// [`crate::archive::SqliteArchive`]; [`NoArchive`] where there is none.
pub trait Archive: Send + Sync + 'static {
    /// The stored body for an immutable endpoint, if archived.
    fn get(&self, req: &RiotRequest) -> futures_util::future::BoxFuture<'_, Option<Bytes>>;
    /// Store a freshly fetched immutable body. Failures are the archive's to log.
    fn put(&self, req: &RiotRequest, body: Bytes) -> futures_util::future::BoxFuture<'_, ()>;
    /// Read what the archive keeps from a freshly fetched mutable body (the
    /// players' ranks in league entries, ADR-105); most bodies it ignores.
    fn observe(&self, _req: &RiotRequest, _body: Bytes) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[derive(Debug, Default)]
pub struct NoArchive;

impl Archive for NoArchive {
    fn get(&self, _req: &RiotRequest) -> futures_util::future::BoxFuture<'_, Option<Bytes>> {
        Box::pin(async { None })
    }
    fn put(&self, _req: &RiotRequest, _body: Bytes) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// What the upstream leg hands back to every single-flight waiter.
#[derive(Debug, Clone)]
struct Fetched {
    body: Bytes,
    content_at: Instant,
    /// The cached copy's own fetch time when `stale`.
    fetched_at: Instant,
    /// Upstream failed and this is the cached copy.
    stale: bool,
}

struct Inner {
    client: RiotClient,
    limiter: Arc<Limiter>,
    cache: Arc<ResponseCache>,
    archive: Arc<dyn Archive>,
    scope: KeyScope,
    policy: TtlPolicy,
    interactive_budget: Duration,
    job_budget: Duration,
    swr: bool,
}

#[derive(Clone)]
pub struct Fetcher {
    inner: Arc<Inner>,
    flight: Arc<SingleFlight<String, Fetched, FetchError>>,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher")
            .field("scope", &self.inner.scope)
            .finish_non_exhaustive()
    }
}

/// Everything a [`Fetcher`] is built from.
pub struct FetcherParts {
    pub client: RiotClient,
    pub limiter: Arc<Limiter>,
    pub cache: Arc<ResponseCache>,
    pub archive: Arc<dyn Archive>,
    pub scope: KeyScope,
    pub policy: TtlPolicy,
    /// `CLIENT_WAIT_BUDGET_MS`
    pub interactive_budget: Duration,
    /// `JOB_YIELD_BUDGET_MS`
    pub job_budget: Duration,
    /// `STALE_WHILE_REVALIDATE`
    pub swr: bool,
}

impl Fetcher {
    pub fn new(parts: FetcherParts) -> Self {
        let FetcherParts {
            client,
            limiter,
            cache,
            archive,
            scope,
            policy,
            interactive_budget,
            job_budget,
            swr,
        } = parts;
        Self {
            inner: Arc::new(Inner {
                client,
                limiter,
                cache,
                archive,
                scope,
                policy,
                interactive_budget,
                job_budget,
                swr,
            }),
            flight: Arc::new(SingleFlight::new()),
        }
    }

    pub fn cache(&self) -> &Arc<ResponseCache> {
        &self.inner.cache
    }

    pub fn limiter(&self) -> &Arc<Limiter> {
        &self.inner.limiter
    }

    pub fn key_scope(&self) -> &KeyScope {
        &self.inner.scope
    }

    pub async fn fetch(&self, req: RiotRequest, opts: FetchOptions) -> Result<FetchResult, FetchError> {
        // The job running this fetch, if any, logs what it got (DEV-19).
        let label = activity::tracing_job().then(|| format!("{} {}", req.endpoint.id, req.path));
        let started = Instant::now();
        let out = self.fetch_inner(req, opts).await;
        if let Some(label) = label {
            let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
            match &out {
                Ok(r) => activity::event("riot", format!("{label} → {}", r.x_cache.as_str()), Some(ms)),
                Err(e) => activity::event(
                    "error",
                    format!("{label} → {} {}", e.api.code.as_str(), e.api.message),
                    Some(ms),
                ),
            }
        }
        out
    }

    async fn fetch_inner(&self, req: RiotRequest, opts: FetchOptions) -> Result<FetchResult, FetchError> {
        let key = cache_key(&self.inner.scope, &req);
        let now = Instant::now();

        if !opts.bypass {
            // 1. The archive short-circuits immutable data entirely.
            if req.endpoint.immutable
                && let Some(body) = self.inner.archive.get(&req).await
            {
                record("hit");
                return Ok(FetchResult {
                    body,
                    x_cache: XCache::Archive,
                    cache_age: Duration::ZERO,
                    fetched_age: None,
                });
            }

            // 2. L1: fresh positive or negative, or stale with a background refresh.
            match self.inner.cache.get(&key).await {
                Lookup::Fresh(e) if e.is_negative() => {
                    record("neg");
                    return Err(FetchError {
                        x_cache: Some(XCache::HitNeg),
                        ..ApiError::not_found("Resource not found upstream (negative-cached)").into()
                    });
                }
                Lookup::Fresh(e) => {
                    record("hit");
                    return Ok(served(&e, XCache::Hit, now));
                }
                Lookup::Stale(e) => {
                    record("stale");
                    if self.inner.swr {
                        self.refresh_in_background(req, key);
                    }
                    return Ok(served(&e, XCache::Stale, now));
                }
                Lookup::Miss => {}
            }
        }

        // 3. Miss (or bypass): coalesce, then go upstream.
        record("miss");
        let inner = Arc::clone(&self.inner);
        let budget = match (opts.job, opts.priority) {
            (true, _) => inner.job_budget,
            (false, Priority::Interactive) => inner.interactive_budget,
            (false, Priority::Bulk) => BULK_BUDGET,
        };
        let (work_key, priority) = (key.clone(), opts.priority);
        // The upstream leg runs on its own task; it logs to this job.
        let current = activity::current();
        let flight = self
            .flight
            .run(key, move || {
                activity::within(current, upstream(inner, req, work_key, priority, budget))
            })
            .await;
        let fetched = flight.value?;
        let x_cache = match (fetched.stale, opts.bypass) {
            (true, _) => XCache::Stale,
            (false, true) => XCache::Bypass,
            (false, false) => XCache::Miss,
        };
        let now = Instant::now();
        Ok(FetchResult {
            body: fetched.body,
            x_cache,
            cache_age: now.saturating_duration_since(fetched.content_at),
            fetched_age: Some(now.saturating_duration_since(fetched.fetched_at)),
        })
    }

    /// v1 §8.5: refresh past the soft TTL at bulk priority, without the caller waiting.
    fn refresh_in_background(&self, req: RiotRequest, key: String) {
        let (inner, flight) = (Arc::clone(&self.inner), Arc::clone(&self.flight));
        tokio::spawn(async move {
            let work_key = key.clone();
            let out = flight
                .run(key, move || {
                    upstream(inner, req, work_key, Priority::Bulk, BULK_BUDGET)
                })
                .await;
            if let Err(e) = out.value {
                tracing::debug!(error = %e, "background refresh failed");
            }
        });
    }
}

fn served(entry: &CacheEntry, x_cache: XCache, now: Instant) -> FetchResult {
    FetchResult {
        body: entry.body.clone(),
        x_cache,
        cache_age: entry.age(now),
        fetched_age: Some(entry.fetch_age(now)),
    }
}

fn record(state: &'static str) {
    metrics::counter!(CACHE_READS_TOTAL, "state" => state).increment(1);
}

/// ±20 % jitter (v1), so retries from many callers don't line up.
fn jitter(base: Duration) -> Duration {
    let r = getrandom::u64().unwrap_or(u64::MAX / 2);
    // 0.8 + 0.4 × r/u64::MAX
    #[allow(clippy::cast_precision_loss)]
    let factor = 0.8 + 0.4 * (r as f64 / u64::MAX as f64);
    base.mul_f64(factor)
}

/// A cached copy still inside its hard TTL, for when upstream can't answer.
async fn cached_copy(inner: &Inner, key: &str) -> Option<Fetched> {
    match inner.cache.get(key).await {
        Lookup::Fresh(e) | Lookup::Stale(e) if !e.is_negative() => Some(Fetched {
            body: e.body.clone(),
            content_at: e.content_at,
            fetched_at: e.fetched_at,
            stale: true,
        }),
        _ => None,
    }
}

/// The upstream leg: limiter, HTTP, observe, retries, cache and archive writes,
/// negatives. Runs once per key at a time (single-flight), on its own task.
/// `budget` bounds each wait for the limiter.
async fn upstream(
    inner: Arc<Inner>,
    mut req: RiotRequest,
    key: String,
    priority: Priority,
    budget: Duration,
) -> Result<Fetched, FetchError> {
    let endpoint: &'static Endpoint = req.endpoint;
    let targets = req.targets();
    // Clusters that answered a service 429 during this request: tried last.
    let mut avoid: Vec<Target> = Vec::new();
    let (mut server_retries, mut service_tries) = (0usize, 0u32);

    let traced = activity::tracing_job();
    for _ in 0..MAX_ATTEMPTS {
        if traced {
            activity::now(format!("rate limiter: {} on {}", endpoint.id, req.target.scope()));
        }
        let waiting = Instant::now();
        let target = match acquire_target(
            &inner.limiter,
            &targets,
            &avoid,
            endpoint.method_scope_key,
            priority,
            budget,
        )
        .await
        {
            Ok(t) => t,
            Err(limited) => {
                if let Some(copy) = cached_copy(&inner, &key).await {
                    tracing::warn!(method = endpoint.id, "serving stale while rate limited");
                    return Ok(copy);
                }
                return Err(limited.into());
            }
        };
        req.target = target;
        let scope = target.scope();
        let waited = waiting.elapsed();
        if traced {
            if waited >= TRACE_WAIT_FROM {
                activity::event(
                    "wait",
                    format!(
                        "waited for the {scope} rate limit ({})",
                        endpoint.method_scope_key
                    ),
                    Some(i64::try_from(waited.as_millis()).unwrap_or(i64::MAX)),
                );
            }
            activity::now(format!("calling Riot: {} {} on {scope}", endpoint.id, req.path));
        }
        let sent = Instant::now();

        let (outcome, headers) = match inner.client.send(&req).await {
            Ok(res) => {
                let h = RateLimitHeaders::from_headers(&res.headers);
                (Ok(res), h)
            }
            Err(e) => {
                let h = RateLimitHeaders::from_headers(&e.headers);
                (Err(e), h)
            }
        };
        // The limiter sees every response, errors included (v1 §9.1).
        inner.limiter.observe(scope, endpoint.method_scope_key, &headers);
        // A success shows as the fetch's own line; a failure here may be
        // retried, so each one is logged.
        if traced && let Err(e) = &outcome {
            let status = e
                .status
                .map_or_else(|| format!("{:?}", e.kind), |s| s.to_string());
            activity::event(
                "error",
                format!("Riot answered {status} on {scope}"),
                Some(i64::try_from(sent.elapsed().as_millis()).unwrap_or(i64::MAX)),
            );
        }

        let err = match outcome {
            Ok(res) => {
                let ttls = inner.policy.ttls(endpoint);
                if endpoint.immutable {
                    inner.archive.put(&req, res.body.clone()).await;
                } else {
                    inner.archive.observe(&req, res.body.clone()).await;
                }
                let fetched_at = Instant::now();
                let content_at = match inner.cache.put(&key, endpoint, res.body.clone(), &ttls).await {
                    Some(entry) => entry.content_at,
                    None => fetched_at,
                };
                return Ok(Fetched {
                    body: res.body,
                    content_at,
                    fetched_at,
                    stale: false,
                });
            }
            Err(e) => e,
        };

        match &err.kind {
            RiotErrorKind::NotFound => {
                if let Some(ttl) = inner.policy.ttls(endpoint).negative {
                    inner.cache.put_negative(&key, endpoint, ttl).await;
                }
                return Err(err.to_api_error().into());
            }
            RiotErrorKind::UpstreamAuth | RiotErrorKind::UnexpectedStatus => {
                return Err(err.to_api_error().into());
            }
            RiotErrorKind::RateLimited {
                limit_type: Some(_),
                retry_after: Some(_),
            } => {
                // `observe` froze the scope; the next acquire waits it out, or
                // fails with RATE_LIMITED if the freeze outlasts the budget. A
                // picked account lookup moves to a cluster with room instead.
                continue;
            }
            RiotErrorKind::RateLimited { .. } => {
                // A picked account lookup tries every other cluster before backing off.
                if targets.len() > 1 && !avoid.contains(&target) {
                    avoid.push(target);
                    if avoid.len() < targets.len() {
                        tracing::info!(
                            method = endpoint.id,
                            cluster = scope,
                            "service 429, trying the next cluster"
                        );
                        continue;
                    }
                }
                if service_tries < SERVICE_429_TRIES {
                    let backoff = (SERVICE_429_BASE * 2u32.pow(service_tries)).min(SERVICE_429_MAX);
                    service_tries += 1;
                    tracing::warn!(
                        method = endpoint.id,
                        try_ = service_tries,
                        ?backoff,
                        "service 429, backing off"
                    );
                    if traced {
                        activity::event(
                            "wait",
                            format!("service 429 on {scope}: backing off {backoff:?}"),
                            None,
                        );
                    }
                    tokio::time::sleep(jitter(backoff)).await;
                    continue;
                }
                return Err(err.to_api_error().into());
            }
            RiotErrorKind::UpstreamUnavailable => {
                if let Some(&backoff) = SERVER_RETRY_BACKOFF.get(server_retries) {
                    server_retries += 1;
                    tracing::warn!(method = endpoint.id, status = ?err.status, try_ = server_retries, "upstream unavailable, retrying");
                    if traced {
                        activity::event(
                            "wait",
                            format!("Riot unavailable on {scope}: retrying in {backoff:?}"),
                            None,
                        );
                    }
                    tokio::time::sleep(jitter(backoff)).await;
                    continue;
                }
                if let Some(copy) = cached_copy(&inner, &key).await {
                    tracing::warn!(method = endpoint.id, status = ?err.status, "serving stale on upstream failure");
                    return Ok(copy);
                }
                return Err(err.to_api_error().into());
            }
        }
    }
    Err(ApiError::new(ErrorCode::UpstreamError, "Upstream request failed").into())
}

/// A permit on the first target with room now, in `targets` order with the
/// `avoid`ed ones last. With none free, wait for whichever frees first, inside
/// `budget`. One target is a plain acquire (ADR-066).
async fn acquire_target(
    limiter: &Limiter,
    targets: &[Target],
    avoid: &[Target],
    method: &str,
    priority: Priority,
    budget: Duration,
) -> Result<Target, RateLimited> {
    if let [only] = targets {
        limiter.acquire(only.scope(), method, priority, budget).await?;
        return Ok(*only);
    }
    let ordered = targets
        .iter()
        .filter(|t| !avoid.contains(t))
        .chain(targets.iter().filter(|t| avoid.contains(t)));
    let mut soonest: Option<(Instant, Target)> = None;
    for &t in ordered {
        // A zero budget never waits: it takes a token now or says when one frees.
        match limiter.acquire(t.scope(), method, priority, Duration::ZERO).await {
            Ok(_) => return Ok(t),
            Err(l) if soonest.is_none_or(|(at, _)| l.retry_at < at) => soonest = Some((l.retry_at, t)),
            Err(_) => {}
        }
    }
    let Some((_, t)) = soonest else {
        // `targets` is never empty; an empty list has nothing to wait for.
        return Err(RateLimited {
            retry_at: Instant::now() + budget,
        });
    };
    limiter.acquire(t.scope(), method, priority, budget).await?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_cache_strings_are_v1_plus_design() {
        let all = [
            XCache::Hit,
            XCache::Miss,
            XCache::Stale,
            XCache::HitNeg,
            XCache::Archive,
            XCache::Bypass,
        ];
        let names: Vec<&str> = all.iter().map(|x| x.as_str()).collect();
        assert_eq!(names, ["HIT", "MISS", "STALE", "HIT-NEG", "ARCHIVE", "BYPASS"]);
    }

    fn clusters() -> Vec<Target> {
        crate::riot::routing::Region::ACCOUNT_PICK
            .iter()
            .map(|r| Target::Region(*r))
            .collect()
    }

    /// ADR-066: the first cluster with room wins; avoided clusters go last.
    #[tokio::test(start_paused = true)]
    async fn the_picker_takes_the_first_free_cluster_and_tries_avoided_ones_last() {
        let limiter = Limiter::new(0.8);
        let all = clusters();
        let pick = |avoid: Vec<Target>| {
            let (limiter, all) = (&limiter, &all);
            async move {
                acquire_target(
                    limiter,
                    all,
                    &avoid,
                    "m",
                    Priority::Interactive,
                    Duration::from_secs(2),
                )
                .await
                .unwrap()
                .scope()
            }
        };
        assert_eq!(pick(vec![]).await, "asia");
        assert_eq!(pick(vec![all[0]]).await, "americas");
        assert_eq!(
            pick(vec![all[0], all[1], all[2]]).await,
            "asia",
            "all avoided: order again"
        );
    }

    /// With every cluster frozen, wait for the soonest, inside the budget.
    #[tokio::test(start_paused = true)]
    async fn with_none_free_the_picker_waits_for_the_soonest_cluster() {
        use crate::riot::limiter::headers::RateLimitType;
        let limiter = Limiter::new(0.8);
        for (scope, ms) in [("asia", 5000), ("americas", 1000), ("europe", 3000)] {
            limiter.freeze(scope, Duration::from_millis(ms), RateLimitType::Method);
        }
        let started = Instant::now();
        let got = acquire_target(
            &limiter,
            &clusters(),
            &[],
            "m",
            Priority::Interactive,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(got.scope(), "americas");
        assert_eq!(started.elapsed(), Duration::from_millis(1000));

        let short = acquire_target(
            &limiter,
            &clusters(),
            &[],
            "m",
            Priority::Interactive,
            Duration::ZERO,
        )
        .await;
        assert!(short.is_ok(), "americas has room again");
        for scope in ["asia", "americas", "europe"] {
            limiter.freeze(scope, Duration::from_secs(10), RateLimitType::Method);
        }
        let late = acquire_target(
            &limiter,
            &clusters(),
            &[],
            "m",
            Priority::Interactive,
            Duration::from_secs(2),
        )
        .await;
        assert!(late.is_err(), "nothing frees inside the budget");
    }

    /// A pinned request is a plain acquire on its one target.
    #[tokio::test(start_paused = true)]
    async fn a_single_target_is_never_moved() {
        use crate::riot::limiter::headers::RateLimitType;
        let limiter = Limiter::new(0.8);
        limiter.freeze("europe", Duration::from_secs(10), RateLimitType::Method);
        let europe = [Target::Region(crate::riot::routing::Region::Europe)];
        let r = acquire_target(
            &limiter,
            &europe,
            &[],
            "m",
            Priority::Interactive,
            Duration::from_secs(2),
        )
        .await;
        assert!(r.is_err());
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        for _ in 0..1000 {
            let j = jitter(Duration::from_millis(1000));
            assert!(
                j >= Duration::from_millis(800) && j <= Duration::from_millis(1200),
                "{j:?}"
            );
        }
    }

    #[test]
    fn service_backoff_schedule_matches_v1() {
        let schedule: Vec<Duration> = (0..5)
            .map(|n| (SERVICE_429_BASE * 2u32.pow(n)).min(SERVICE_429_MAX))
            .collect();
        assert_eq!(
            schedule,
            [500, 1000, 2000, 4000, 8000].map(Duration::from_millis).to_vec(),
            "500 ms × 2ⁿ capped at 8 s"
        );
    }
}

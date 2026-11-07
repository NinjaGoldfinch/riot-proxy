//! `/status`'s checks (STS-02b, design/14 §Checks). A background task runs
//! `riot_api`, `cache` and `storage` every `STATUS_CHECK_INTERVAL_S` and
//! stores each result; `/status` reads what is stored. `riot_key` makes no
//! call of its own: it reads the Riot client's last verdict on the key when
//! `/status` is read.
//!
//! `riot_api` probes `lol-status-v4` `platform-data` on each of
//! `STATUS_CHECK_PLATFORMS`, through the limiter at bulk priority and never
//! through the response cache. The first platform is the one that can make it
//! `down` (ADR-206); it is probed every interval while idle and every 60 s
//! while real traffic reached its scope in the last 60 s. The others are
//! probed every 60 s and only degrade it. A platform whose probe was skipped
//! `STALE_AFTER_SKIPS` attempts in a row degrades it too.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use super::{Check, Status, StatusLevel};
use crate::app::AppState;
use crate::archive::objects::Objects;
use crate::build_info::rfc3339;
use crate::db::{Db, DbError};
use crate::riot::client::{KeyVerdict, RequestError, RiotClient, RiotErrorKind, RiotRequest};
use crate::riot::endpoints::{Endpoint, Target};
use crate::riot::limiter::headers::RateLimitHeaders;
use crate::riot::limiter::{Limiter, Priority};
use crate::riot::routing::Platform;

pub const RIOT_API: &str = "riot_api";
pub const RIOT_KEY: &str = "riot_key";
pub const CACHE: &str = "cache";
pub const STORAGE: &str = "storage";

/// The probe's endpoint.
const PROBE_METHOD: &str = "status.platformData";
/// An active first platform's cadence, and every other platform's.
pub const SLOW_EVERY: Duration = Duration::from_secs(60);
/// Traffic this recent on the first platform's scope makes it active.
const ACTIVE_WITHIN_MS: i64 = 60_000;
/// No limiter room within this skips a probe.
const PROBE_ROOM: Duration = Duration::from_secs(1);
/// A platform skipped this many attempts in a row degrades `riot_api`
/// (owner, ADR-206): its last result is then too old to stand for Riot.
pub const STALE_AFTER_SKIPS: u32 = 4;
const CACHE_WITHIN: Duration = Duration::from_secs(2);
const STORAGE_WITHIN: Duration = Duration::from_secs(5);
/// `riot_key` is `degraded` this long before `RIOT_API_KEY_EXPIRES_AT`.
pub const KEY_WARNING_MS: i64 = 48 * 3_600_000;
const PENDING: &str = "pending";

/// What the checks task stored last, by check name.
#[derive(Debug, Default)]
pub struct Results(Mutex<BTreeMap<&'static str, Check>>);

impl Results {
    fn set(&self, name: &'static str, check: Check) {
        self.lock().insert(name, check);
    }

    fn get(&self, name: &str) -> Option<Check> {
        self.lock().get(name).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<&'static str, Check>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `/status`'s `checks`, in design/14's order: `riot_api` when a platform is
/// probed, `riot_key`, `cache`, and `storage` when there is a bucket.
pub fn report(state: &AppState) -> Vec<Check> {
    let status = &state.status;
    let pending = |name: &str| Check {
        name: name.into(),
        status: StatusLevel::Ok,
        latency_ms: None,
        checked_at: rfc3339(status.started_at_ms),
        detail: PENDING.into(),
    };
    let stored = |name: &str| status.results.get(name).unwrap_or_else(|| pending(name));
    let mut out = Vec::with_capacity(4);
    if !state.config.status_check_platforms.is_empty() {
        let freeze = state.limiter.longest_freeze().map_or(0, ceil_secs);
        out.push(frozen(stored(RIOT_API), freeze));
    }
    out.push(riot_key(
        state.fetcher.client().watch().key().as_ref(),
        state.config.riot_api_key_expires_at_ms,
        crate::clock::Clock::now().unix_ms,
        status.started_at_ms,
    ));
    out.push(stored(CACHE));
    if state.db.objects().is_some() {
        out.push(stored(STORAGE));
    }
    out
}

/// Whole seconds, rounded up.
pub fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// `riot_api` is `degraded` while any scope is frozen after a 429
/// (`riot_retry_after_s > 0`), read when `/status` is.
fn frozen(mut check: Check, retry_after_s: u64) -> Check {
    if retry_after_s > 0 {
        check.status = check.status.max(StatusLevel::Degraded);
        push(
            &mut check.detail,
            format!("rate limited by Riot for {retry_after_s} s more"),
        );
    }
    check
}

fn push(detail: &mut String, part: impl AsRef<str>) {
    if detail == PENDING {
        detail.clear();
    }
    if !detail.is_empty() {
        detail.push_str("; ");
    }
    detail.push_str(part.as_ref());
}

/// `riot_key` from the last 2xx, 401 or 403 and the key's expiry.
pub fn riot_key(
    verdict: Option<&KeyVerdict>,
    expires_at_ms: Option<i64>,
    now_ms: i64,
    started_at_ms: i64,
) -> Check {
    let (mut status, mut detail, at) = match verdict {
        None => (StatusLevel::Ok, PENDING.to_string(), started_at_ms),
        Some(v) if v.accepted => (StatusLevel::Ok, String::new(), v.at_ms),
        Some(v) => (
            StatusLevel::Down,
            format!("Riot answered {} to {} on {}", v.status, v.method, v.scope),
            v.at_ms,
        ),
    };
    if let Some(expires) = expires_at_ms.filter(|&e| e - now_ms < KEY_WARNING_MS) {
        status = status.max(StatusLevel::Degraded);
        let verb = if expires <= now_ms { "expired" } else { "expires" };
        push(&mut detail, format!("the key {verb} at {}", rfc3339(expires)));
    }
    Check {
        name: RIOT_KEY.into(),
        status,
        latency_ms: None,
        checked_at: rfc3339(at),
        detail,
    }
}

/// What one probe of one platform saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Riot answered with `status`: a 2xx, or a 401, 403, 404 or other
    /// status that is not Riot being out (a rejected key shows on `riot_key`).
    Answered { status: u16 },
    /// A 429.
    RateLimited,
    /// A 5xx, or no answer at all.
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub probe: Probe,
    pub latency_ms: u64,
    /// Unix ms.
    pub at_ms: i64,
}

/// One probed platform: its last result, and how many attempts in a row,
/// up to the last, were skipped for want of limiter room (the result then
/// still stands, until `STALE_AFTER_SKIPS`).
#[derive(Debug, Clone, Default)]
pub struct Platformed {
    pub last: Option<ProbeResult>,
    pub skips: u32,
}

/// `riot_api` from each platform's last probe, the first platform's first.
pub fn riot_api(platforms: &[(Platform, &Platformed)], started_at_ms: i64) -> Check {
    let mut check = Check {
        name: RIOT_API.into(),
        status: StatusLevel::Ok,
        latency_ms: None,
        checked_at: rfc3339(started_at_ms),
        detail: String::new(),
    };
    for (i, (platform, p)) in platforms.iter().enumerate() {
        let first = i == 0;
        if first {
            if let Some(r) = &p.last {
                check.latency_ms = Some(r.latency_ms);
                check.checked_at = rfc3339(r.at_ms);
            } else {
                check.detail = PENDING.into();
            }
        }
        match p.last.as_ref().map(|r| &r.probe) {
            None | Some(Probe::Answered { status: 200..=299 }) => {}
            Some(Probe::Answered { status }) => {
                push(&mut check.detail, format!("{platform}: Riot answered {status}"))
            }
            Some(Probe::RateLimited) => {
                check.status = check.status.max(StatusLevel::Degraded);
                push(&mut check.detail, format!("{platform}: Riot answered 429"));
            }
            Some(Probe::Failed { detail }) => {
                let level = if first {
                    StatusLevel::Down
                } else {
                    StatusLevel::Degraded
                };
                check.status = check.status.max(level);
                push(&mut check.detail, format!("{platform}: {detail}"));
            }
        }
        if p.skips >= STALE_AFTER_SKIPS {
            check.status = check.status.max(StatusLevel::Degraded);
            push(
                &mut check.detail,
                format!(
                    "{platform}: probe skipped {} times in a row, no room in the rate limiter",
                    p.skips
                ),
            );
        } else if p.skips > 0 {
            push(
                &mut check.detail,
                format!("{platform}: probe skipped, no room in the rate limiter"),
            );
        }
    }
    check
}

/// Which platforms are due at `now`: the first every interval while idle and
/// every 60 s while `active`; the others every 60 s. `last` is each one's
/// last attempt.
pub fn due(last: &[Option<Instant>], active: bool, now: Instant) -> Vec<usize> {
    let slow = |at: Option<Instant>| at.is_none_or(|at| now.saturating_duration_since(at) >= SLOW_EVERY);
    last.iter()
        .enumerate()
        .filter(|&(i, &at)| (i == 0 && !active) || slow(at))
        .map(|(i, _)| i)
        .collect()
}

/// The checks task: what it calls and what it remembers between rounds.
pub struct Checker {
    client: RiotClient,
    limiter: Arc<Limiter>,
    db: Db,
    status: Arc<Status>,
    interval: Duration,
    platforms: Vec<Probed>,
    /// Whether the last round's `cache` check failed.
    cache_failed: bool,
}

/// A platform the checks probe: its request, built once, and what it saw.
struct Probed {
    platform: Platform,
    request: RiotRequest,
    seen: Platformed,
    /// When it was last due, probed or skipped.
    attempted: Option<Instant>,
}

/// Why the checks task could not be built: the probe's request does not
/// resolve, which is a bug in the endpoint table, never a runtime state.
#[derive(Debug, thiserror::Error)]
pub enum CheckerError {
    #[error("the status probe's endpoint {PROBE_METHOD} is not in the endpoint table")]
    NoEndpoint,
    #[error("the status probe's request on {platform} does not build: {source}")]
    Request {
        platform: Platform,
        #[source]
        source: RequestError,
    },
}

impl Checker {
    /// Builds each platform's probe request once, so a round never mistakes
    /// a request that does not build for a skipped probe.
    pub fn new(state: &AppState) -> Result<Self, CheckerError> {
        let endpoint = Endpoint::by_id(PROBE_METHOD).ok_or(CheckerError::NoEndpoint)?;
        let platforms = state
            .config
            .status_check_platforms
            .iter()
            .map(|&platform| {
                let request = RiotRequest::new(endpoint, Target::Platform(platform), &[])
                    .map_err(|source| CheckerError::Request { platform, source })?;
                Ok(Probed {
                    platform,
                    request,
                    seen: Platformed::default(),
                    attempted: None,
                })
            })
            .collect::<Result<_, CheckerError>>()?;
        Ok(Self {
            client: state.fetcher.client().clone(),
            limiter: Arc::clone(&state.limiter),
            db: state.db.clone(),
            status: Arc::clone(&state.status),
            interval: Duration::from_secs(state.config.status_check_interval_s.into()),
            platforms,
            cache_failed: false,
        })
    }

    /// Run a round now and then every interval, until aborted.
    pub fn spawn(mut self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut every = tokio::time::interval(self.interval);
            every.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                let now = every.tick().await;
                self.round(now).await;
            }
        })
    }

    /// One round at `now`: the due probes, `cache` and `storage`, run
    /// concurrently, each stored as soon as it is known. A probe left
    /// hanging until the client's timeout never holds back `cache` or
    /// `storage`; the round ends when all three have.
    pub async fn round(&mut self, now: Instant) {
        let active = self.platforms.first().is_some_and(|p| {
            self.client
                .watch()
                .last_traffic_ms(p.platform.as_str())
                .is_some_and(|at| crate::clock::Clock::now().unix_ms - at < ACTIVE_WITHIN_MS)
        });
        let last: Vec<Option<Instant>> = self.platforms.iter().map(|p| p.attempted).collect();
        let due = due(&last, active, now);
        let (client, limiter, db, status) = (&self.client, &*self.limiter, &self.db, &*self.status);
        let platforms = &mut self.platforms;
        let probes = async {
            if platforms.is_empty() {
                return;
            }
            let probed = futures_util::future::join_all(
                due.iter().map(|&i| probe(client, limiter, &platforms[i].request)),
            )
            .await;
            for (&i, result) in due.iter().zip(probed) {
                let p = &mut platforms[i];
                p.attempted = Some(now);
                match result {
                    Some(r) => {
                        p.seen.last = Some(r);
                        p.seen.skips = 0;
                    }
                    None => p.seen.skips = p.seen.skips.saturating_add(1),
                }
            }
            let view: Vec<(Platform, &Platformed)> =
                platforms.iter().map(|p| (p.platform, &p.seen)).collect();
            status
                .results
                .set(RIOT_API, riot_api(&view, status.started_at_ms));
        };
        let after_failure = self.cache_failed;
        let cache = async {
            let (check, failed) = cache(db, CACHE_WITHIN, after_failure).await;
            status.results.set(CACHE, check);
            failed
        };
        let storage = async {
            if let Some(check) = storage(db.objects()).await {
                status.results.set(STORAGE, check);
            }
        };
        let ((), cache_failed, ()) = tokio::join!(probes, cache, storage);
        self.cache_failed = cache_failed;
    }
}

/// One `platform-data` call; `None` when the limiter had no bulk room within
/// a second. The limiter observes the answer like any.
async fn probe(client: &RiotClient, limiter: &Limiter, req: &RiotRequest) -> Option<ProbeResult> {
    let scope = req.target.scope();
    let method = req.endpoint.method_scope_key;
    limiter
        .acquire(scope, method, Priority::Bulk, PROBE_ROOM)
        .await
        .ok()?;
    let started = Instant::now();
    let out = client.send_probe(req).await;
    let headers = match &out {
        Ok(res) => RateLimitHeaders::from_headers(&res.headers),
        Err(e) => RateLimitHeaders::from_headers(&e.headers),
    };
    limiter.observe(scope, method, &headers);
    let probe = match out {
        Ok(res) => Probe::Answered { status: res.status },
        Err(e) => match (e.kind, e.status) {
            (RiotErrorKind::RateLimited { .. }, _) => Probe::RateLimited,
            (RiotErrorKind::UpstreamUnavailable, Some(status)) => Probe::Failed {
                detail: format!("Riot answered {status}"),
            },
            (RiotErrorKind::UpstreamUnavailable, None) => Probe::Failed {
                detail: "no answer from Riot".into(),
            },
            (_, status) => Probe::Answered {
                status: status.unwrap_or(0),
            },
        },
    };
    Some(ProbeResult {
        probe,
        latency_ms: millis(started.elapsed()),
        at_ms: crate::clock::Clock::now().unix_ms,
    })
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `cache`: one read of L2's table within `within`, and whether it failed.
/// An error is `down`. A timeout is `degraded`, and `down` only when the
/// round before failed too (owner, ADR-206): the wait includes the one for
/// a free reader, so a busy pool alone reads slow for a round, not gone.
async fn cache(db: &Db, within: Duration, after_failure: bool) -> (Check, bool) {
    let started = Instant::now();
    let read = db.read(|c| {
        c.query_row("SELECT count(*) FROM (SELECT 1 FROM cache LIMIT 1)", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(DbError::from)
    });
    let slow = format!("SQLite did not answer a read within {} s", within.as_secs());
    let (failure, level) = match tokio::time::timeout(within, read).await {
        Ok(Ok(_)) => (None, StatusLevel::Ok),
        Ok(Err(e)) => (Some(format!("SQLite read failed: {e}")), StatusLevel::Down),
        Err(_) if after_failure => (
            Some(format!("{slow}, after a failure the round before")),
            StatusLevel::Down,
        ),
        Err(_) => (Some(slow), StatusLevel::Degraded),
    };
    let failed = failure.is_some();
    (timed(CACHE, started, failure, level), failed)
}

/// `storage`, with a bucket: one listing within 5 s, or `degraded`.
async fn storage(objects: Option<&Objects>) -> Option<Check> {
    let objects = objects?;
    let started = Instant::now();
    let failure = match tokio::time::timeout(STORAGE_WITHIN, objects.probe()).await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("the bucket's listing failed: {e}")),
        Err(_) => Some(format!(
            "the bucket did not answer a listing within {} s",
            STORAGE_WITHIN.as_secs()
        )),
    };
    Some(timed(STORAGE, started, failure, StatusLevel::Degraded))
}

fn timed(name: &str, started: Instant, failure: Option<String>, failed: StatusLevel) -> Check {
    Check {
        name: name.into(),
        status: if failure.is_some() {
            failed
        } else {
            StatusLevel::Ok
        },
        latency_ms: Some(millis(started.elapsed())),
        checked_at: rfc3339(crate::clock::Clock::now().unix_ms),
        detail: failure.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;

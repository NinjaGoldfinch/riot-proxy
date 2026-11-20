//! Outbound events (STS-04, design/14 §Outbound events). When `EVENTS_URL`
//! is set, a state change is POSTed there as the contract's event: the key
//! rejected, Riot's rate limit, `riot_api` down and `cache` down, each with
//! its recovery. Transitions only, never per check interval.
//!
//! Three parts:
//! - [`Events`], the handle the Riot client's watch and the checks hold.
//!   [`Events::emit`] builds the envelope and pushes it onto a bounded queue
//!   with `try_send`: it never awaits, so nothing on the request path waits
//!   for delivery. A full queue drops the event with a warning.
//! - [`Tracker`], the event pairs as a state machine with no I/O.
//! - [`deliver`], the one worker that POSTs each event, with its own HTTP
//!   client (never the Riot client's, so never the Riot key).
//!
//! The key's verdict needs no state here: [`UpstreamWatch`] already says when
//! it turns rejected. The tracker is in memory, so a restart forgets it; a key
//! still rejected after a restart fires `riot_key_rejected` again, once.
//!
//! [`UpstreamWatch`]: crate::riot::client::UpstreamWatch

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use super::{Check, SCHEMA, SERVICE, StatusLevel};
use crate::app::AppState;
use crate::build_info::rfc3339;
use crate::config::EventsUrl;
use crate::riot::client::KeyVerdict;

/// Events waiting for the worker; one more is dropped with a warning.
pub const QUEUE: usize = 64;
/// One delivery attempt's limit.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// The waits before the second to fifth attempts; after the fifth the event
/// is dropped.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];
/// `riot_rate_limited` fires when `riot_retry_after_s` goes above this.
pub const RATE_LIMITED_ABOVE_S: u64 = 10;
/// `riot_upstream_down` fires on this many check rounds in a row with
/// `riot_api` `down`.
pub const DOWN_ROUNDS: u32 = 3;

/// The contract's `event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    RiotKeyRejected,
    RiotRateLimited,
    RiotRateLimitCleared,
    RiotUpstreamDown,
    RiotUpstreamRecovered,
    CacheUnavailable,
    CacheRecovered,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RiotKeyRejected => "riot_key_rejected",
            Self::RiotRateLimited => "riot_rate_limited",
            Self::RiotRateLimitCleared => "riot_rate_limit_cleared",
            Self::RiotUpstreamDown => "riot_upstream_down",
            Self::RiotUpstreamRecovered => "riot_upstream_recovered",
            Self::CacheUnavailable => "cache_unavailable",
            Self::CacheRecovered => "cache_recovered",
        }
    }

    /// design/14's table.
    pub fn severity(self) -> Severity {
        match self {
            Self::RiotKeyRejected | Self::RiotUpstreamDown | Self::CacheUnavailable => Severity::Critical,
            Self::RiotRateLimited => Severity::Warning,
            Self::RiotRateLimitCleared | Self::RiotUpstreamRecovered | Self::CacheRecovered => Severity::Info,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// One event as POSTed: the shared contract's envelope, `schema: 1`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub schema: u32,
    pub service: &'static str,
    pub host: String,
    pub event: Kind,
    pub severity: Severity,
    /// RFC 3339, UTC.
    pub at: String,
    pub detail: Value,
}

/// An event the tracker fired, before it has an envelope.
pub type Fired = (Kind, Value);

/// The event pairs other than the key's, as a state machine: each method is
/// one observation and returns the event it fires, if any. Nothing fires
/// twice in a row from one pair, and a closing event only after its opening one.
#[derive(Debug, Default)]
pub struct Tracker {
    /// `riot_api` `down` rounds in a row.
    down_rounds: u32,
    upstream_down: bool,
    cache_down: bool,
    rate_limited: bool,
}

impl Tracker {
    /// One round's stored `riot_api` (`None` when no platform is probed):
    /// `riot_upstream_down` on the [`DOWN_ROUNDS`]th `down` round in a row,
    /// `riot_upstream_recovered` on the first round after it that is not.
    pub fn riot_api(&mut self, check: Option<&Check>) -> Option<Fired> {
        match check.filter(|c| c.status == StatusLevel::Down) {
            Some(c) => {
                self.down_rounds = self.down_rounds.saturating_add(1);
                (self.down_rounds >= DOWN_ROUNDS && !self.upstream_down).then(|| {
                    self.upstream_down = true;
                    (Kind::RiotUpstreamDown, json!({ "detail": c.detail }))
                })
            }
            None => {
                self.down_rounds = 0;
                std::mem::take(&mut self.upstream_down).then(|| (Kind::RiotUpstreamRecovered, json!({})))
            }
        }
    }

    /// One round's `cache`: `cache_unavailable` when it goes `down`,
    /// `cache_recovered` when it no longer is.
    pub fn cache(&mut self, check: Option<&Check>) -> Option<Fired> {
        match check.filter(|c| c.status == StatusLevel::Down) {
            Some(c) => (!self.cache_down).then(|| {
                self.cache_down = true;
                (Kind::CacheUnavailable, json!({ "detail": c.detail }))
            }),
            None => std::mem::take(&mut self.cache_down).then(|| (Kind::CacheRecovered, json!({}))),
        }
    }

    /// `riot_retry_after_s` as sampled each round: `riot_rate_limited` when
    /// it is above [`RATE_LIMITED_ABOVE_S`], `riot_rate_limit_cleared` when
    /// it is back to 0 after one.
    pub fn retry_after(&mut self, retry_after_s: u64) -> Option<Fired> {
        if retry_after_s == 0 {
            return std::mem::take(&mut self.rate_limited).then(|| (Kind::RiotRateLimitCleared, json!({})));
        }
        self.frozen(retry_after_s)
    }

    /// A freeze of `retry_after_s` just set by a 429, seen on the response
    /// path so one that starts and ends between two rounds is not missed.
    /// It can only open the pair: whether the freeze is over is the rounds'
    /// to say, from the limiter.
    pub fn frozen(&mut self, retry_after_s: u64) -> Option<Fired> {
        (retry_after_s > RATE_LIMITED_ABOVE_S && !self.rate_limited).then(|| {
            self.rate_limited = true;
            (Kind::RiotRateLimited, json!({ "retry_after_s": retry_after_s }))
        })
    }
}

/// The handle that emits events: cheap to clone, and a no-op without
/// `EVENTS_URL` ([`Events::none`], the default).
#[derive(Clone, Default)]
pub struct Events(Option<Arc<Inner>>);

struct Inner {
    tx: mpsc::Sender<Event>,
    host: String,
    tracker: Mutex<Tracker>,
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_on() { "Events(on)" } else { "Events(off)" })
    }
}

/// What a no-op [`Events`] borrows as.
pub static NONE: Events = Events(None);

impl Events {
    /// Events that go nowhere.
    pub const fn none() -> Self {
        Self(None)
    }

    /// Events from `host` onto a queue of [`QUEUE`], and its receiving end
    /// for [`deliver`].
    pub fn channel(host: impl Into<String>) -> (Self, mpsc::Receiver<Event>) {
        Self::with_capacity(host, QUEUE)
    }

    fn with_capacity(host: impl Into<String>, capacity: usize) -> (Self, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(capacity);
        let inner = Inner {
            tx,
            host: host.into(),
            tracker: Mutex::new(Tracker::default()),
        };
        (Self(Some(Arc::new(inner))), rx)
    }

    /// Whether events go anywhere.
    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    /// Queue `kind` at `at_ms`; never waits. A full queue drops it and warns.
    pub fn emit(&self, kind: Kind, detail: Value, at_ms: i64) {
        let Some(inner) = &self.0 else { return };
        let event = Event {
            schema: SCHEMA,
            service: SERVICE,
            host: inner.host.clone(),
            event: kind,
            severity: kind.severity(),
            at: rfc3339(at_ms),
            detail,
        };
        match inner.tx.try_send(event) {
            Ok(()) => tracing::info!(event = %kind, "event queued"),
            Err(TrySendError::Full(_)) => {
                tracing::warn!(event = %kind, queue = QUEUE, "event queue full; event dropped");
            }
            Err(TrySendError::Closed(_)) => {
                tracing::debug!(event = %kind, "event delivery stopped; event dropped");
            }
        }
    }

    /// The key's verdict just turned rejected: `riot_key_rejected`, with the
    /// answer's status, method and scope.
    pub fn key_rejected(&self, verdict: &KeyVerdict) {
        self.emit(
            Kind::RiotKeyRejected,
            json!({ "status": verdict.status, "method": verdict.method, "scope": verdict.scope }),
            verdict.at_ms,
        );
    }

    /// A 429 just froze a scope for `retry_after_s` ([`Tracker::frozen`]).
    pub fn frozen(&self, retry_after_s: u64, at_ms: i64) {
        self.track(at_ms, |t| t.frozen(retry_after_s));
    }

    /// The end of a check round: its stored `riot_api` and `cache`, and
    /// `riot_retry_after_s` read now.
    pub fn round(&self, riot_api: Option<&Check>, cache: Option<&Check>, retry_after_s: u64, at_ms: i64) {
        self.track(at_ms, |t| t.riot_api(riot_api));
        self.track(at_ms, |t| t.cache(cache));
        self.track(at_ms, |t| t.retry_after(retry_after_s));
    }

    fn track(&self, at_ms: i64, observe: impl FnOnce(&mut Tracker) -> Option<Fired>) {
        let Some(inner) = &self.0 else { return };
        // The lock is released before the event is queued.
        let fired = observe(
            &mut inner
                .tracker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if let Some((kind, detail)) = fired {
            self.emit(kind, detail, at_ms);
        }
    }
}

/// One delivery attempt: `Ok` on any 2xx, else why not.
pub trait Post: Send + Sync + 'static {
    fn post(&self, body: Bytes) -> impl Future<Output = Result<(), String>> + Send;
}

/// [`Post`] over HTTP to `EVENTS_URL`, with a client of its own: no Riot
/// key, the embedded roots (a `FROM scratch` image has no CA store), and
/// [`TIMEOUT`] per attempt.
pub struct Http {
    client: reqwest::Client,
    url: EventsUrl,
    timeout: Duration,
}

impl Http {
    pub fn new(url: EventsUrl, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .tls_certs_only(crate::riot::client::webpki_roots())
            .build()?;
        Ok(Self { client, url, timeout })
    }
}

impl Post for Http {
    async fn post(&self, body: Bytes) -> Result<(), String> {
        let res = self
            .client
            .post(self.url.url().clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            // reqwest's message names the URL, which may hold a token.
            .map_err(|e| {
                if e.is_timeout() {
                    format!("no answer within {} s", self.timeout.as_secs_f64())
                } else {
                    e.without_url().to_string()
                }
            })?;
        let status = res.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("answered {}", status.as_u16()))
        }
    }
}

/// The worker: each event in turn, up to 1 + `BACKOFF.len()` attempts,
/// until the queue's senders are all gone or the task is aborted.
pub async fn deliver<P: Post>(post: P, mut rx: mpsc::Receiver<Event>, events_host: String) {
    while let Some(event) = rx.recv().await {
        deliver_one(&post, &event, &events_host).await;
    }
}

/// One event: whether it was delivered.
pub async fn deliver_one<P: Post>(post: &P, event: &Event, events_host: &str) -> bool {
    let body = match serde_json::to_vec(event) {
        Ok(body) => Bytes::from(body),
        Err(e) => {
            tracing::warn!(event = %event.event, error = %e, "event not serialisable; dropped");
            return false;
        }
    };
    let mut waits = BACKOFF.iter();
    let mut attempt = 1;
    loop {
        match post.post(body.clone()).await {
            Ok(()) => {
                tracing::info!(event = %event.event, attempt, events_host, "event delivered");
                return true;
            }
            Err(why) => match waits.next() {
                Some(&wait) => {
                    tracing::debug!(event = %event.event, attempt, events_host, %why, "event not delivered; retrying");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                None => {
                    tracing::warn!(event = %event.event, attempts = attempt, events_host, %why, "event not delivered; dropped");
                    return false;
                }
            },
        }
    }
}

/// Start events when `EVENTS_URL` is set: the handle goes on the Riot
/// client's watch (which the checks read it from) and the worker is
/// spawned. `None` without the variable; the caller aborts the worker at
/// shutdown, and what is still queued then is lost.
pub fn start(state: &AppState) -> Result<Option<JoinHandle<()>>, reqwest::Error> {
    let Some(url) = state.config.events_url.clone() else {
        return Ok(None);
    };
    let events_host = url.host();
    let post = Http::new(url, TIMEOUT)?;
    let (events, rx) = Events::channel(state.status.host());
    state.fetcher.client().watch().set_events(events);
    tracing::info!(events_host, "outbound events on");
    Ok(Some(tokio::spawn(deliver(post, rx, events_host))))
}

#[cfg(test)]
mod tests;

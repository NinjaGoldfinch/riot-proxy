#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use tokio::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::status::{Check, StatusLevel};

fn check(name: &str, status: StatusLevel, detail: &str) -> Check {
    Check {
        name: name.into(),
        status,
        latency_ms: None,
        checked_at: String::new(),
        detail: detail.into(),
    }
}

fn kinds(fired: &[Option<Fired>]) -> Vec<Option<Kind>> {
    fired.iter().map(|f| f.as_ref().map(|(k, _)| *k)).collect()
}

// ---- the tracker ----

/// Transitions only: each pair fires once per change, never per round.
#[test]
fn each_pair_fires_once_per_transition() {
    use StatusLevel::{Degraded, Down, Ok};
    let mut t = Tracker::default();
    let cache: Vec<_> = [Ok, Down, Down, Down, Degraded, Ok, Down, Down]
        .into_iter()
        .map(|s| t.cache(Some(&check("cache", s, "SQLite did not answer within 2 s"))))
        .collect();
    use Kind::{CacheRecovered as R, CacheUnavailable as U};
    assert_eq!(
        kinds(&cache),
        [None, Some(U), None, None, Some(R), None, Some(U), None],
        "degraded is not down"
    );
    assert_eq!(
        cache[1].as_ref().unwrap().1,
        json!({ "detail": "SQLite did not answer within 2 s" })
    );
    assert_eq!(cache[4].as_ref().unwrap().1, json!({}));

    let mut t = Tracker::default();
    let limited: Vec<_> = [0, 30, 25, 12, 3, 0, 0, 11]
        .into_iter()
        .map(|s| t.retry_after(s))
        .collect();
    use Kind::{RiotRateLimitCleared as C, RiotRateLimited as L};
    assert_eq!(
        kinds(&limited),
        [None, Some(L), None, None, None, Some(C), None, Some(L)]
    );
    assert_eq!(limited[1].as_ref().unwrap().1, json!({ "retry_after_s": 30 }));
    assert_eq!(limited[5].as_ref().unwrap().1, json!({}));
}

/// design/14: `riot_retry_after_s` going *above* 10 fires; 10 does not.
#[test]
fn a_freeze_of_10_fires_nothing_and_11_fires() {
    let mut t = Tracker::default();
    assert!(t.retry_after(10).is_none());
    assert!(t.frozen(10).is_none());
    assert_eq!(t.retry_after(11).map(|(k, _)| k), Some(Kind::RiotRateLimited));
}

/// The response path can open the pair but never clear it: a 429 carrying
/// `Retry-After: 0` says nothing about the limiter's longest freeze.
#[test]
fn a_429_opens_the_rate_limit_pair_and_only_a_round_clears_it() {
    let mut t = Tracker::default();
    assert_eq!(t.frozen(30).map(|(k, _)| k), Some(Kind::RiotRateLimited));
    assert!(t.frozen(60).is_none(), "already limited");
    assert!(t.frozen(0).is_none());
    assert!(t.retry_after(29).is_none(), "the round sees the same freeze");
    assert_eq!(t.retry_after(0).map(|(k, _)| k), Some(Kind::RiotRateLimitCleared));
}

/// `riot_upstream_down` on the third `down` round in a row; a round that is
/// not `down` starts the count again.
#[test]
fn riot_upstream_down_fires_on_the_third_down_round_in_a_row() {
    use StatusLevel::{Degraded, Down, Ok};
    let mut t = Tracker::default();
    let mut feed = |s: StatusLevel| t.riot_api(Some(&check("riot_api", s, "oc1: no answer from Riot")));
    let fired: Vec<_> = [Down, Down, Degraded, Down, Down, Down, Down, Ok, Down]
        .into_iter()
        .map(&mut feed)
        .collect();
    use Kind::{RiotUpstreamDown as D, RiotUpstreamRecovered as R};
    assert_eq!(
        kinds(&fired),
        [None, None, None, None, None, Some(D), None, Some(R), None]
    );
    assert_eq!(
        fired[5].as_ref().unwrap().1,
        json!({ "detail": "oc1: no answer from Riot" })
    );
    assert_eq!(fired[7].as_ref().unwrap().1, json!({}));
}

/// A closing event needs its opening one; no platform probed is not `down`.
#[test]
fn recovery_events_fire_only_after_their_opening_event() {
    use StatusLevel::{Down, Ok};
    let mut t = Tracker::default();
    assert!(t.riot_api(Some(&check("riot_api", Ok, ""))).is_none());
    assert!(t.riot_api(None).is_none());
    assert!(t.cache(Some(&check("cache", Ok, ""))).is_none());
    assert!(t.cache(None).is_none());
    assert!(t.retry_after(0).is_none());
    // Two `down` rounds and back: no `riot_upstream_down`, so no recovery.
    for s in [Down, Down, Ok] {
        assert!(t.riot_api(Some(&check("riot_api", s, ""))).is_none());
    }
}

// ---- the handle ----

#[test]
fn the_envelope_is_the_contracts_event() {
    let (events, mut rx) = Events::channel("ngf-prod-syd1");
    events.key_rejected(&KeyVerdict {
        accepted: false,
        status: 403,
        method: "summoner.byPuuid",
        scope: "oc1",
        at_ms: 1_793_840_400_000,
    });
    let event = rx.try_recv().unwrap();
    // design/14's example, byte for byte apart from key order.
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "schema": 1, "service": "riot-proxy", "host": "ngf-prod-syd1", "event": "riot_key_rejected",
                "severity": "critical", "at": "2026-11-05T01:00:00Z",
                "detail": { "status": 403, "method": "summoner.byPuuid", "scope": "oc1" } })
    );
    for (kind, severity) in [
        (Kind::RiotKeyRejected, "critical"),
        (Kind::RiotRateLimited, "warning"),
        (Kind::RiotRateLimitCleared, "info"),
        (Kind::RiotUpstreamDown, "critical"),
        (Kind::RiotUpstreamRecovered, "info"),
        (Kind::CacheUnavailable, "critical"),
        (Kind::CacheRecovered, "info"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        assert_eq!(serde_json::to_value(kind.severity()).unwrap(), severity, "{kind}");
    }
}

/// A round feeds all three pairs, through one tracker shared with the
/// response path.
#[test]
fn a_round_and_a_429_share_the_tracker() {
    let (events, mut rx) = Events::channel("h");
    events.frozen(30, 1_000);
    let down = check("cache", StatusLevel::Down, "SQLite did not answer within 2 s");
    events.round(None, Some(&down), 29, 2_000);
    events.round(None, None, 0, 3_000);
    let got: Vec<&str> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|e| e.event.as_str())
        .collect();
    assert_eq!(
        got,
        [
            "riot_rate_limited",
            "cache_unavailable",
            "cache_recovered",
            "riot_rate_limit_cleared"
        ]
    );
}

#[test]
fn without_a_url_nothing_is_queued_or_tracked() {
    let events = Events::none();
    assert!(!events.is_on());
    events.emit(Kind::CacheRecovered, json!({}), 0);
    events.frozen(30, 0);
    events.round(None, None, 0, 0);
    assert!(!Events::default().is_on());
}

/// The 65th event finds the queue full: dropped with a warning, and the
/// caller does not wait.
#[tokio::test(start_paused = true)]
async fn a_full_queue_drops_the_event_and_never_blocks() {
    let (events, mut rx) = Events::channel("h");
    let (_, text) = logged(async {
        for i in 0..QUEUE {
            events.emit(Kind::CacheRecovered, json!({ "i": i }), 0);
        }
        let started = Instant::now();
        events.emit(Kind::CacheUnavailable, json!({}), 0);
        assert_eq!(started.elapsed(), Duration::ZERO);
    })
    .await;
    assert!(text.contains("event queue full; event dropped"), "{text}");
    assert!(text.contains("cache_unavailable"), "{text}");
    let queued: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(queued.len(), QUEUE);
    assert!(queued.iter().all(|e| e.event == Kind::CacheRecovered));
}

// ---- delivery ----

/// A [`Post`] that answers from a script and records when each attempt came.
#[derive(Clone)]
struct Scripted {
    answers: Arc<Mutex<Vec<Result<(), String>>>>,
    at: Arc<Mutex<Vec<Duration>>>,
    started: Instant,
}

impl Scripted {
    /// Answers in order; once they run out, every attempt fails.
    fn new(answers: Vec<Result<(), String>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into_iter().rev().collect())),
            at: Arc::default(),
            started: Instant::now(),
        }
    }

    fn attempts(&self) -> Vec<u64> {
        self.at.lock().unwrap().iter().map(Duration::as_secs).collect()
    }
}

impl Post for Scripted {
    async fn post(&self, _body: Bytes) -> Result<(), String> {
        self.at.lock().unwrap().push(self.started.elapsed());
        self.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("answered 503".into()))
    }
}

fn event() -> Event {
    let (events, mut rx) = Events::channel("h");
    events.emit(Kind::CacheUnavailable, json!({ "detail": "x" }), 0);
    rx.try_recv().unwrap()
}

/// Retries after 1, 2, 4 and 8 s, then dropped with a warning after the
/// fifth attempt.
#[tokio::test(start_paused = true)]
async fn five_attempts_then_dropped() {
    let post = Scripted::new(vec![]);
    let (delivered, text) = logged(deliver_one(&post, &event(), "hooks.example.com")).await;
    assert!(!delivered);
    assert_eq!(post.attempts(), [0, 1, 3, 7, 15]);
    assert!(text.contains("event not delivered; dropped"), "{text}");
    assert!(
        text.contains("attempts=5") && text.contains("answered 503"),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_2xx_stops_the_retries() {
    let post = Scripted::new(vec![Err("answered 500".into()), Err("no answer".into()), Ok(())]);
    assert!(deliver_one(&post, &event(), "h").await);
    assert_eq!(post.attempts(), [0, 1, 3]);
}

/// The worker takes events in turn and ends when every sender is gone.
#[tokio::test(start_paused = true)]
async fn the_worker_delivers_each_event_in_turn() {
    let post = Scripted::new(vec![Ok(()), Err("answered 500".into()), Ok(())]);
    let (events, rx) = Events::channel("h");
    events.emit(Kind::CacheUnavailable, json!({}), 0);
    events.emit(Kind::CacheRecovered, json!({}), 0);
    drop(events);
    deliver(post.clone(), rx, "h".into()).await;
    assert_eq!(post.attempts(), [0, 0, 1]);
}

fn http(server: &MockServer, timeout: Duration) -> Http {
    let url = crate::config::EventsUrl::parse(&format!("{}/hook/s3cr3t", server.uri())).unwrap();
    Http::new(url, timeout).unwrap()
}

/// Over HTTP, in real time (paused time would make the client's timeout
/// fire while it waits on the mock's socket): any 2xx is delivered, the
/// body is the event as JSON, and nothing like a Riot key is sent.
#[tokio::test]
async fn http_posts_the_event_as_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook/s3cr3t"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let e = event();
    let body = Bytes::from(serde_json::to_vec(&e).unwrap());
    http(&server, TIMEOUT).post(body).await.unwrap();
    let got = server.received_requests().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].headers.get("content-type").unwrap(), "application/json");
    assert!(got[0].headers.get(crate::riot::client::X_RIOT_TOKEN).is_none());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&got[0].body).unwrap(),
        serde_json::to_value(&e).unwrap()
    );
}

#[tokio::test]
async fn a_non_2xx_and_a_timeout_are_failures_that_never_name_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;
    let post = http(&server, Duration::from_millis(200));
    assert_eq!(post.post(Bytes::new()).await, Err("answered 500".into()));
    let timed_out = post.post(Bytes::new()).await.unwrap_err();
    assert_eq!(timed_out, "no answer within 0.2 s");

    // Nobody listening: reqwest's own message, without the URL's token.
    let closed = MockServer::start().await;
    let post = http(&closed, TIMEOUT);
    drop(closed);
    let why = post.post(Bytes::new()).await.unwrap_err();
    assert!(!why.contains("s3cr3t"), "{why}");
}

async fn logged<F: std::future::Future>(f: F) -> (F::Output, String) {
    use tracing_subscriber::layer::SubscriberExt;
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
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_ansi(false),
    );
    let out = {
        let _guard = tracing::subscriber::set_default(subscriber);
        f.await
    };
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, text)
}

//! AVL-01 (ADR-209): `serve` accepts as soon as it is listening, the boot
//! catch-up runs behind the server, and a shutdown during the catch-up stops
//! it and returns.
//!
//! `serve` runs in-process against wiremock, as in `p6_exit.rs`. The catch-up
//! is held open by [`CatchUpHold`] (as a minutes-long check on a large archive
//! holds it on production), and shutdown is a [`Notify`] rather than SIGTERM:
//! `shutdown.rs` sends this process SIGTERM, which must not stop these tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use riot_proxy::cli::serve::{CatchUpHold, ServeOptions, serve_with};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wiremock::MockServer;

type Lines = Arc<Mutex<Vec<String>>>;

/// What `serve` logs, one `message key=value…` string per event. The `it`
/// binary's one global tracing subscriber, installed by the first `boot`:
/// every in-process `serve` in this binary sets `skip_tracing_init`, and the
/// layer keeps only `riot_proxy::cli::serve`'s events, so the other tests are
/// not affected (and a line from another in-process `serve` can only add to
/// the vector, never remove from it).
fn serve_log() -> Lines {
    static LOG: OnceLock<Lines> = OnceLock::new();
    Arc::clone(LOG.get_or_init(|| {
        let lines: Lines = Arc::default();
        let only_serve = tracing_subscriber::filter::Targets::new()
            .with_target("riot_proxy::cli::serve", tracing::Level::INFO);
        tracing_subscriber::registry()
            .with(ServeLog(Arc::clone(&lines)).with_filter(only_serve))
            .try_init()
            .expect("nothing else in the it binary installs a global subscriber");
        lines
    }))
}

struct ServeLog(Lines);

impl<S: tracing::Subscriber> Layer<S> for ServeLog {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        #[derive(Default)]
        struct Line {
            message: String,
            fields: String,
        }
        impl Visit for Line {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.fields.push_str(&format!(" {}={value}", field.name()));
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{value:?}");
                } else {
                    self.fields.push_str(&format!(" {}={value:?}", field.name()));
                }
            }
        }
        let mut line = Line::default();
        event.record(&mut line);
        self.0
            .lock()
            .unwrap()
            .push(format!("{}{}", line.message, line.fields));
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Booted {
    task: JoinHandle<anyhow::Result<()>>,
    port: u16,
    hold: CatchUpHold,
    stop: Arc<Notify>,
    /// `serve`'s log lines, this binary's (see [`serve_log`]).
    log: Lines,
    /// When the socket first took a connection: `serve` logs "listening"
    /// right after `bind`, and the kernel completes connects from then on,
    /// whether or not anything accepts them.
    listening: Instant,
}

async fn boot(data_dir: &std::path::Path, riot_url: &str) -> Booted {
    let port = free_port();
    let config = common::config(&[
        ("DATA_DIR", data_dir.to_str().unwrap()),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("LOG_LEVEL", "warn"),
    ]);
    let hold = CatchUpHold::default();
    let stop = Arc::new(Notify::new());
    // Installed before `serve` logs anything.
    let log = serve_log();
    let options = ServeOptions {
        riot_base_url: Some(riot_url.to_string()),
        skip_tracing_init: true,
        // Unmocked: the sync fails and retries, but never leaves the machine.
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock(riot_url)),
        catch_up_hold: Some(hold.clone()),
        shutdown: Some(Arc::clone(&stop)),
        ..ServeOptions::default()
    };
    let task = tokio::spawn(serve_with(config, options));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        assert!(!task.is_finished(), "serve returned before binding");
        assert!(Instant::now() < deadline, "serve never bound");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Booted {
        task,
        port,
        hold,
        stop,
        log,
        listening: Instant::now(),
    }
}

async fn until(what: &str, secs: u64, probe: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !probe() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn get(http: &reqwest::Client, port: u16, path: &str) -> reqwest::StatusCode {
    http.get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .status()
}

/// The 11-08 outage: "listening" logged, then minutes of catch-up with
/// nothing accepting. Now the first request is answered within a second of
/// "listening", while the catch-up has not finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serves_within_a_second_of_listening_while_the_catch_up_runs() {
    let dir = tempfile::tempdir().unwrap();
    let upstream = MockServer::start().await;
    // A one-second budget: a server that is not accepting fails here. Built
    // before `boot`, so the budget measures `listening` to the first 200 and
    // nothing else.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let b = boot(dir.path(), &upstream.uri()).await;

    assert_eq!(get(&http, b.port, "/healthz").await, 200);
    let first = b.listening.elapsed();
    assert!(
        !b.hold.done.load(Ordering::SeqCst),
        "answered before the catch-up finished"
    );
    eprintln!("listening -> first /healthz 200: {} ms", first.as_millis());
    assert!(first < Duration::from_secs(1), "{first:?}");

    // The catch-up is under way (interrupted jobs re-queued, the checks held)
    // and `/readyz` keeps its meaning: writer and limiter, both ready.
    until("the catch-up to reach the hold", 10, || {
        b.hold.reached.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(get(&http, b.port, "/readyz").await, 200);
    assert_eq!(get(&http, b.port, "/healthz").await, 200);
    assert!(!b.hold.done.load(Ordering::SeqCst));

    // Released, it finishes and the workers start; then a clean shutdown.
    b.hold.release.notify_one();
    until("the catch-up to finish", 10, || {
        b.hold.done.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(get(&http, b.port, "/healthz").await, 200);
    drop(http);
    b.stop.notify_one();
    tokio::time::timeout(Duration::from_secs(30), b.task)
        .await
        .expect("serve returns after shutdown")
        .unwrap()
        .unwrap();
}

/// The 11-08 SIGKILL: a SIGTERM during the gap was parked until the gap
/// ended. Now shutdown stops the catch-up where it is, drains and returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_during_the_catch_up_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let upstream = MockServer::start().await;
    let b = boot(dir.path(), &upstream.uri()).await;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    until("the catch-up to reach the hold", 10, || {
        b.hold.reached.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(get(&http, b.port, "/healthz").await, 200);
    drop(http);

    // Never released: as a check still walking the archive.
    let mark = b.log.lock().unwrap().len();
    let at = Instant::now();
    b.stop.notify_one();
    tokio::time::timeout(Duration::from_secs(5), b.task)
        .await
        .expect("serve returns within 5 s of the shutdown signal")
        .unwrap()
        .unwrap();
    eprintln!(
        "shutdown during the catch-up -> returned: {} ms",
        at.elapsed().as_millis()
    );
    assert!(
        !b.hold.done.load(Ordering::SeqCst),
        "the catch-up was stopped, not finished"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", b.port)).await.is_err(),
        "no longer listening"
    );
    // Logged as a shutdown, not as the server ending on its own: the line
    // the runbook's journal grep looks for, with the reason that tells the
    // two apart (ADR-209).
    let since = b.log.lock().unwrap()[mark..].to_vec();
    assert!(
        since.iter().any(|l| {
            l.starts_with("boot catch-up stopped; job workers not started")
                && l.contains("reason=shutdown signal")
        }),
        "the stop was logged with reason=shutdown signal: {since:?}"
    );
}

//! The durable job queue with real workers (plan P6-03): every job runs exactly
//! once and never twice at the same time, a restart resumes what a dead process
//! left running, panics and unknown kinds are contained, and enqueueing wakes an
//! idle worker without waiting for the poll.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::{Handler, Job, JobError, NewJob, Registry, Scheduler};
use serde_json::json;
use tokio::sync::Notify;

async fn states(db: &Db) -> HashMap<String, usize> {
    db.read(|c| {
        let mut s = c.prepare("SELECT state, count(*) FROM jobs GROUP BY state")?;
        let rows = s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                usize::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
            ))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>().map_err(DbError::from)
    })
    .await
    .unwrap()
}

async fn until(what: &str, mut check: impl AsyncFnMut() -> bool) {
    for _ in 0..500 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Sleeps, and records how many times each job ran and the most copies of any
/// one job ever running at once.
#[derive(Default)]
struct Slow {
    running: Mutex<HashMap<String, usize>>,
    runs: Mutex<HashMap<String, usize>>,
    worst_overlap: AtomicUsize,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
}

struct SlowHandler(Arc<Slow>);

impl Handler for SlowHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        let this = &self.0;
        Box::pin(async move {
            let overlap = {
                let mut r = this.running.lock().unwrap();
                let n = r.entry(job.id.clone()).or_default();
                *n += 1;
                *n
            };
            this.worst_overlap.fetch_max(overlap, Ordering::SeqCst);
            let now = this.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            this.most_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            this.in_flight.fetch_sub(1, Ordering::SeqCst);
            *this.running.lock().unwrap().get_mut(&job.id).unwrap() -= 1;
            *this.runs.lock().unwrap().entry(job.id.clone()).or_default() += 1;
            Ok(())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_job_runs_twice_even_with_eight_workers() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 4).unwrap();
    let slow = Arc::new(Slow::default());
    let s = Scheduler::new(
        db.clone(),
        Registry::new().with("slow", SlowHandler(Arc::clone(&slow))),
    );
    let mut ids = Vec::new();
    for n in 0..40 {
        ids.push(
            s.enqueue(NewJob::new("slow", 10_000, json!({"n": n})))
                .await
                .unwrap()
                .id,
        );
    }
    let workers = s.start(8);
    until("every job to finish", async || {
        states(&db).await.get("done") == Some(&40)
    })
    .await;
    workers.shutdown(Duration::from_secs(1)).await;

    let runs = slow.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 40);
    assert!(
        runs.values().all(|&n| n == 1),
        "each job ran exactly once: {runs:?}"
    );
    assert_eq!(
        slow.worst_overlap.load(Ordering::SeqCst),
        1,
        "never twice at once"
    );
    let most = slow.most_in_flight.load(Ordering::SeqCst);
    assert!(
        (2..=8).contains(&most),
        "ran concurrently, within JOB_CONCURRENCY: {most}"
    );
}

/// Blocks its first run forever (a process that dies mid-job); later runs pass.
struct Stuck {
    started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

impl Handler for Stuck {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn a_restart_resumes_a_job_the_old_process_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("riot-proxy.db");
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let registry = || {
        Registry::new().with(
            "backfill:player",
            Stuck {
                started: Arc::clone(&started),
                calls: Arc::clone(&calls),
            },
        )
    };

    {
        let db = Db::open(&path, 2).unwrap();
        let s = Scheduler::new(db.clone(), registry());
        s.enqueue(NewJob::new("backfill:player", 20_000, json!({})).dedupe("P1"))
            .await
            .unwrap();
        let workers = s.start(2);
        started.notified().await;
        // The process "dies": the handler never finishes, the row stays running.
        workers.shutdown(Duration::from_millis(50)).await;
        assert_eq!(states(&db).await, HashMap::from([("running".to_string(), 1)]));
        // While running, the same work is not queued twice.
        assert!(
            !s.enqueue(NewJob::new("backfill:player", 20_000, json!({})).dedupe("P1"))
                .await
                .unwrap()
                .created
        );
    }

    let db = Db::open(&path, 2).unwrap();
    let s = Scheduler::new(db.clone(), registry());
    assert_eq!(s.recover().await.unwrap(), 1);
    let workers = s.start(2);
    until("the job to resume and finish", async || {
        states(&db).await.get("done") == Some(&1)
    })
    .await;
    workers.shutdown(Duration::from_secs(1)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one interrupted run, one completed"
    );
    assert_eq!(states(&db).await, HashMap::from([("done".to_string(), 1)]));
}

struct Panics;

impl Handler for Panics {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move { panic!("boom") })
    }
}

#[tokio::test]
async fn panics_retry_and_unknown_kinds_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let s = Scheduler::new(db.clone(), Registry::new().with("boom", Panics));
    s.enqueue(NewJob::new("boom", 1, json!({}))).await.unwrap();
    s.enqueue(NewJob::new("nobody:handles", 1, json!({})))
        .await
        .unwrap();
    let workers = s.start(2);
    until("both outcomes", async || {
        let st = states(&db).await;
        st.get("pending") == Some(&1) && st.get("failed") == Some(&1)
    })
    .await;
    workers.shutdown(Duration::from_secs(1)).await;
    let errors: HashMap<String, (String, String)> = db
        .read(|c| {
            let mut s = c.prepare("SELECT kind, state, error FROM jobs")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
            rows.collect::<Result<HashMap<_, _>, _>>().map_err(DbError::from)
        })
        .await
        .unwrap();
    assert_eq!(
        errors["boom"],
        ("pending".into(), "handler panicked".into()),
        "retried with backoff"
    );
    assert_eq!(
        errors["nobody:handles"],
        ("failed".into(), "no handler for job kind 'nobody:handles'".into())
    );
}

struct Counts(Arc<AtomicUsize>);

impl Handler for Counts {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn enqueue_wakes_an_idle_worker_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let s = Scheduler::new(db.clone(), Registry::new().with("k", Counts(Arc::clone(&n))));
    let workers = s.start(1);
    tokio::time::sleep(Duration::from_millis(100)).await; // idle, waiting
    let sent = tokio::time::Instant::now();
    s.enqueue(NewJob::new("k", 1, json!({}))).await.unwrap();
    until("the job to run", async || n.load(Ordering::SeqCst) == 1).await;
    assert!(
        sent.elapsed() < Duration::from_millis(500),
        "woken, not polled: {:?}",
        sent.elapsed()
    );
    workers.shutdown(Duration::from_secs(1)).await;
}

/// Starts, then waits for `release` before recording that it got to the end.
struct Gated {
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    release: Arc<Notify>,
}

impl Handler for Gated {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

/// DEV-22: a halt aborts what is running, claims nothing while held, and the
/// workers pick the queue up again once it is dropped.
#[tokio::test]
async fn a_halt_stops_running_jobs_and_holds_claims_until_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let (started, finished) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let release = Arc::new(Notify::new());
    let s = Scheduler::new(
        db.clone(),
        Registry::new().with(
            "backfill:player",
            Gated {
                started: Arc::clone(&started),
                finished: Arc::clone(&finished),
                release: Arc::clone(&release),
            },
        ),
    );
    for p in ["P1", "P2"] {
        s.enqueue(NewJob::new("backfill:player", 20_000, json!({})).dedupe(p))
            .await
            .unwrap();
    }
    let workers = s.start(4);
    until("both jobs to start", async || started.load(Ordering::SeqCst) == 2).await;

    let halt = s.queue().halt(Duration::from_secs(5)).await;
    assert_eq!((halt.stopped, halt.settled), (2, true));
    release.notify_waiters();
    s.enqueue(NewJob::new("backfill:player", 20_000, json!({})).dedupe("P3"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        finished.load(Ordering::SeqCst),
        0,
        "aborted jobs never reach their end"
    );
    assert_eq!(
        started.load(Ordering::SeqCst),
        2,
        "nothing is claimed while halted"
    );
    assert_eq!(
        states(&db).await,
        HashMap::from([("running".to_string(), 2), ("pending".to_string(), 1)]),
        "an aborted job records no outcome"
    );

    drop(halt);
    until("the queued job to be claimed again", async || {
        started.load(Ordering::SeqCst) == 3
    })
    .await;
    release.notify_waiters();
    until("it to finish", async || finished.load(Ordering::SeqCst) == 1).await;
    workers.shutdown(Duration::from_secs(1)).await;
}

/// A halt with nothing running returns at once and changes nothing.
#[tokio::test]
async fn an_idle_halt_settles_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let s = Scheduler::new(db, Registry::new());
    let workers = s.start(2);
    let halt = s.queue().halt(Duration::from_secs(5)).await;
    assert_eq!((halt.stopped, halt.settled), (0, true));
    drop(halt);
    workers.shutdown(Duration::from_secs(1)).await;
}

//! The durable job queue (docs/design/06 §Scheduler): the `jobs` table is the
//! queue, `JOB_CONCURRENCY` workers claim the highest-priority ready row, run
//! its handler and mark it done or reschedule it with backoff.
//!
//! - **Enqueue** is `INSERT OR IGNORE` against `UNIQUE(kind, dedupe_key)` while
//!   pending or running, so queueing work that is already queued is a no-op.
//! - **Claim** is one `UPDATE … RETURNING` on the single writer, so no two
//!   workers can take the same row.
//! - **Wake**: `enqueue` notifies an idle worker; idle workers also re-check
//!   every second.
//! - **Backoff**: a retryable failure goes back to `pending` after
//!   `2^attempts × 30 s ± 20 %`; the fifth failure is final (`failed`).
//! - **Restart**: rows left `running` by a process that died are reset to
//!   `pending` on boot ([`Scheduler::recover`]), so the work resumes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::metrics::{JOBS_PENDING, JOBS_TOTAL};

/// design/06: `failed` after five attempts.
pub const MAX_ATTEMPTS: u32 = 5;
/// design/06: backoff base, `2^attempts × 30 s`.
pub const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// design/06: idle workers re-check at least this often.
pub const IDLE_POLL: Duration = Duration::from_secs(1);
/// How often `jobs_pending{kind}` is refreshed.
const PENDING_SAMPLE: Duration = Duration::from_secs(15);

/// A job to queue. `priority`: lower runs first (design/06 bands).
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: &'static str,
    /// Dedupes while pending or running; `None` never dedupes.
    pub dedupe_key: Option<String>,
    pub priority: i64,
    pub payload: serde_json::Value,
    /// Not before this unix-ms instant; `None` is now.
    pub run_after: Option<i64>,
}

impl NewJob {
    pub fn new(kind: &'static str, priority: i64, payload: serde_json::Value) -> Self {
        Self {
            kind,
            dedupe_key: None,
            priority,
            payload,
            run_after: None,
        }
    }

    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }
}

/// What `enqueue` did (v1's `queued` / `already-queued`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueued {
    pub id: String,
    /// `false`: an identical job was already pending or running; `id` is that one.
    pub created: bool,
}

impl Enqueued {
    pub fn status(&self) -> &'static str {
        if self.created { "queued" } else { "already-queued" }
    }
}

/// A claimed job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub dedupe_key: Option<String>,
    pub priority: i64,
    /// JSON.
    pub payload: String,
    /// Including this run.
    pub attempts: u32,
    pub run_after: i64,
}

impl Job {
    pub fn payload<T: serde::de::DeserializeOwned>(&self) -> Result<T, JobError> {
        serde_json::from_str(&self.payload).map_err(|e| JobError::Fail(format!("bad payload: {e}")))
    }
}

/// How a handler failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// Try again later, with backoff (Riot down, rate limited, …).
    #[error("{0}")]
    Retry(String),
    /// Retrying cannot help (bad payload, unknown kind).
    #[error("{0}")]
    Fail(String),
}

/// A job handler, registered per kind. Handlers must be idempotent: a job can
/// run again after a crash between its work and its `done` (design/06).
pub trait Handler: Send + Sync + 'static {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>>;
}

/// `2^attempts × 30 s`, scaled by `jitter` ∈ [0.8, 1.2] (design/06's ± 20 %).
pub fn backoff(attempts: u32, jitter: f64) -> Duration {
    let factor = 2u32.saturating_pow(attempts.min(20));
    BACKOFF_BASE
        .saturating_mul(factor)
        .mul_f64(jitter.clamp(0.8, 1.2))
}

fn random_jitter() -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let unit = getrandom::u64().unwrap_or(u64::MAX / 2) as f64 / u64::MAX as f64;
    0.8 + 0.4 * unit
}

const COLUMNS: &str = "id, kind, dedupe_key, priority, payload, attempts, run_after";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        kind: r.get(1)?,
        dedupe_key: r.get(2)?,
        priority: r.get(3)?,
        payload: r.get(4)?,
        attempts: r.get(5)?,
        run_after: r.get(6)?,
    })
}

/// A job id: a ULID from one monotonic generator, so ids made in the same
/// millisecond still sort in creation order. Claims break priority ties on it
/// (oldest first) and the admin list sorts on it (newest first).
fn next_id() -> String {
    static GENERATOR: std::sync::Mutex<ulid::Generator> = std::sync::Mutex::new(ulid::Generator::new());
    GENERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generate()
        .unwrap_or_else(|_| ulid::Ulid::generate())
        .to_string()
}

/// Queue a job on `conn`, inside the caller's transaction if it has one, so a
/// handler can fan out in the same write that records its own progress.
pub fn enqueue_on(conn: &Connection, job: &NewJob, now_ms: i64) -> rusqlite::Result<Enqueued> {
    let id = next_id();
    let n = conn.execute(
        "INSERT OR IGNORE INTO jobs (id, kind, dedupe_key, priority, payload, run_after)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            job.kind,
            job.dedupe_key,
            job.priority,
            job.payload.to_string(),
            job.run_after.unwrap_or(now_ms)
        ],
    )?;
    if n > 0 {
        return Ok(Enqueued { id, created: true });
    }
    let existing: String = conn.query_row(
        "SELECT id FROM jobs WHERE kind = ?1 AND dedupe_key = ?2 AND state IN ('pending', 'running')",
        params![job.kind, job.dedupe_key],
        |r| r.get(0),
    )?;
    Ok(Enqueued {
        id: existing,
        created: false,
    })
}

/// Enqueueing, shareable with handlers that fan out (they hold a `Queue` while
/// the [`Scheduler`] holds them). Cheap to clone.
#[derive(Debug, Clone)]
pub struct Queue {
    db: Db,
    notify: Arc<Notify>,
}

impl Queue {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            notify: Arc::new(Notify::new()),
        }
    }

    /// Queue a job and wake an idle worker.
    pub async fn enqueue(&self, job: NewJob) -> Result<Enqueued, DbError> {
        let now = Clock::now().unix_ms;
        let out = self
            .db
            .write(move |c| enqueue_on(c, &job, now).map_err(DbError::from))
            .await?;
        if out.created {
            self.notify.notify_one();
        }
        Ok(out)
    }

    /// Queue several jobs in one write; returns how many were new.
    pub async fn enqueue_all(&self, jobs: Vec<NewJob>) -> Result<usize, DbError> {
        if jobs.is_empty() {
            return Ok(0);
        }
        let now = Clock::now().unix_ms;
        let created = self
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let mut created = 0;
                for job in &jobs {
                    created += usize::from(enqueue_on(&tx, job, now)?.created);
                }
                tx.commit()?;
                Ok::<_, DbError>(created)
            })
            .await?;
        if created > 0 {
            self.wake_all();
        }
        Ok(created)
    }

    /// Wake a worker after jobs were queued with [`enqueue_on`].
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Wake every idle worker (after a fan-out queued many jobs at once).
    pub fn wake_all(&self) {
        self.notify.notify_waiters();
    }

    pub fn db(&self) -> &Db {
        &self.db
    }
}

/// A `jobs` row as the admin routes show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub dedupe_key: Option<String>,
    pub priority: i64,
    pub payload: String,
    pub state: String,
    pub attempts: u32,
    pub run_after: i64,
    pub claimed_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

/// The four states a row can be in (V0001).
pub const STATES: [&str; 4] = ["pending", "running", "done", "failed"];

const ROW_COLUMNS: &str =
    "id, kind, dedupe_key, priority, payload, state, attempts, run_after, claimed_at, finished_at, error";

fn full_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        dedupe_key: r.get(2)?,
        priority: r.get(3)?,
        payload: r.get(4)?,
        state: r.get(5)?,
        attempts: r.get(6)?,
        run_after: r.get(7)?,
        claimed_at: r.get(8)?,
        finished_at: r.get(9)?,
        error: r.get(10)?,
    })
}

/// Why an admin action on a job did nothing.
#[derive(Debug, thiserror::Error)]
pub enum JobAction {
    #[error("no such job")]
    NotFound,
    /// The row is in a state the action does not apply to.
    #[error("job {id} is {state}")]
    WrongState { id: String, state: String },
    /// Retrying would duplicate work already pending or running.
    #[error("an identical job is already queued ({0})")]
    Duplicate(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl Queue {
    /// Rows, newest first, optionally one state and one kind.
    pub async fn list(
        &self,
        state: Option<&'static str>,
        kind: Option<String>,
        limit: u32,
    ) -> Result<Vec<JobRow>, DbError> {
        self.db
            .read(move |c| {
                let mut s = c.prepare(&format!(
                    "SELECT {ROW_COLUMNS} FROM jobs
                      WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR kind = ?2)
                      ORDER BY id DESC LIMIT ?3"
                ))?;
                let rows = s.query_map(rusqlite::params![state, kind, limit], full_row)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
            })
            .await
    }

    pub async fn get(&self, id: &str) -> Result<Option<JobRow>, DbError> {
        let id = id.to_string();
        self.db
            .read(move |c| {
                c.query_row(
                    &format!("SELECT {ROW_COLUMNS} FROM jobs WHERE id = ?1"),
                    [id],
                    full_row,
                )
                .optional()
                .map_err(DbError::from)
            })
            .await
    }

    /// Put a `failed` job back to `pending` now, with its attempts reset
    /// (design/06: "`/v1/admin/jobs` lists and retries failed rows").
    pub async fn retry(&self, id: &str) -> Result<JobRow, JobAction> {
        let (id, now) = (id.to_string(), Clock::now().unix_ms);
        let out = self
            .db
            .write(move |c| {
                let tx = c.transaction().map_err(DbError::from)?;
                let row = tx
                    .query_row(&format!("SELECT {ROW_COLUMNS} FROM jobs WHERE id = ?1"), [&id], full_row)
                    .optional()
                    .map_err(DbError::from)?
                    .ok_or(JobAction::NotFound)?;
                if row.state != "failed" {
                    return Err(JobAction::WrongState { id, state: row.state });
                }
                if let Some(key) = &row.dedupe_key {
                    let live: Option<String> = tx
                        .query_row(
                            "SELECT id FROM jobs WHERE kind = ?1 AND dedupe_key = ?2 AND state IN ('pending', 'running')",
                            rusqlite::params![row.kind, key],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(DbError::from)?;
                    if let Some(other) = live {
                        return Err(JobAction::Duplicate(other));
                    }
                }
                tx.execute(
                    "UPDATE jobs SET state = 'pending', attempts = 0, run_after = ?2, claimed_at = NULL,
                            finished_at = NULL, error = NULL WHERE id = ?1",
                    rusqlite::params![id, now],
                )
                .map_err(DbError::from)?;
                let updated = tx
                    .query_row(&format!("SELECT {ROW_COLUMNS} FROM jobs WHERE id = ?1"), [&id], full_row)
                    .map_err(DbError::from)?;
                tx.commit().map_err(DbError::from)?;
                Ok(updated)
            })
            .await?;
        self.notify.notify_one();
        Ok(out)
    }

    /// Cancel a `pending` job: it becomes `failed` with `cancelled`. A running
    /// job cannot be taken back from its worker.
    pub async fn cancel(&self, id: &str) -> Result<JobRow, JobAction> {
        let (id, now) = (id.to_string(), Clock::now().unix_ms);
        self.db
            .write(move |c| {
                let n = c
                    .execute(
                        "UPDATE jobs SET state = 'failed', finished_at = ?2, error = 'cancelled'
                          WHERE id = ?1 AND state = 'pending'",
                        rusqlite::params![id, now],
                    )
                    .map_err(DbError::from)?;
                let row = c
                    .query_row(
                        &format!("SELECT {ROW_COLUMNS} FROM jobs WHERE id = ?1"),
                        [&id],
                        full_row,
                    )
                    .optional()
                    .map_err(DbError::from)?
                    .ok_or(JobAction::NotFound)?;
                if n == 0 {
                    return Err(JobAction::WrongState { id, state: row.state });
                }
                Ok(row)
            })
            .await
    }

    /// Rows by kind and state.
    pub async fn stats(&self) -> Result<Vec<(String, String, i64)>, DbError> {
        self.db
            .read(|c| {
                let mut s = c.prepare(
                    "SELECT kind, state, count(*) FROM jobs GROUP BY kind, state ORDER BY kind, state",
                )?;
                let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
            })
            .await
    }
}

/// The queue and its handlers. Cheap to clone.
#[derive(Clone)]
pub struct Scheduler {
    queue: Queue,
    db: Db,
    notify: Arc<Notify>,
    handlers: Arc<HashMap<&'static str, Arc<dyn Handler>>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut kinds: Vec<_> = self.handlers.keys().collect();
        kinds.sort();
        f.debug_struct("Scheduler")
            .field("handlers", &kinds)
            .finish_non_exhaustive()
    }
}

/// Handlers by kind, collected before the workers start.
#[derive(Default)]
pub struct Registry(HashMap<&'static str, Arc<dyn Handler>>);

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, kind: &'static str, handler: impl Handler) -> Self {
        self.0.insert(kind, Arc::new(handler));
        self
    }
}

impl Scheduler {
    pub fn new(db: Db, handlers: Registry) -> Self {
        Self::with_queue(Queue::new(db), handlers)
    }

    /// A scheduler over a queue its handlers already hold.
    pub fn with_queue(queue: Queue, handlers: Registry) -> Self {
        Self {
            db: queue.db.clone(),
            notify: Arc::clone(&queue.notify),
            queue,
            handlers: Arc::new(handlers.0),
        }
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Queue a job and wake an idle worker.
    pub async fn enqueue(&self, job: NewJob) -> Result<Enqueued, DbError> {
        self.queue.enqueue(job).await
    }

    /// Wake a worker after jobs were queued with [`enqueue_on`].
    pub fn wake(&self) {
        self.queue.wake();
    }

    /// Wake every idle worker (after a fan-out queued many jobs at once).
    pub fn wake_all(&self) {
        self.queue.wake_all();
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Boot: rows a dead process left `running` go back to `pending`. Their
    /// attempt stays counted. Returns how many.
    pub async fn recover(&self) -> Result<usize, DbError> {
        self.db
            .write(|c| {
                c.execute(
                    "UPDATE jobs SET state = 'pending', claimed_at = NULL WHERE state = 'running'",
                    [],
                )
                .map_err(DbError::from)
            })
            .await
    }

    /// Claim the highest-priority ready job (design/06 §Claiming).
    pub async fn claim(&self, now_ms: i64) -> Result<Option<Job>, DbError> {
        self.db
            .write(move |c| {
                c.query_row(
                    &format!(
                        "UPDATE jobs SET state = 'running', claimed_at = ?1, attempts = attempts + 1
                          WHERE id = (SELECT id FROM jobs
                                       WHERE state = 'pending' AND run_after <= ?1
                                       ORDER BY priority ASC, run_after ASC, id ASC
                                       LIMIT 1)
                          RETURNING {COLUMNS}"
                    ),
                    [now_ms],
                    row,
                )
                .optional()
                .map_err(DbError::from)
            })
            .await
    }

    /// Record a run's outcome: `done`, back to `pending` with backoff, or `failed`.
    pub async fn finish(
        &self,
        job: &Job,
        outcome: &Result<(), JobError>,
        now_ms: i64,
    ) -> Result<(), DbError> {
        let status = if outcome.is_ok() { "completed" } else { "failed" };
        metrics::counter!(JOBS_TOTAL, "job" => job.kind.clone(), "status" => status).increment(1);
        let id = job.id.clone();
        let (state, run_after, error): (&str, Option<i64>, Option<String>) = match outcome {
            Ok(()) => ("done", None, None),
            Err(JobError::Retry(e)) if job.attempts < MAX_ATTEMPTS => {
                let wait = backoff(job.attempts, random_jitter());
                let at = now_ms.saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
                ("pending", Some(at), Some(e.clone()))
            }
            Err(JobError::Retry(e) | JobError::Fail(e)) => ("failed", None, Some(e.clone())),
        };
        if let Some(e) = &error {
            tracing::warn!(job = %job.kind, id = %job.id, attempts = job.attempts, next = state, error = %e, "job failed");
        }
        self.db
            .write(move |c| {
                match run_after {
                    Some(at) => c.execute(
                        "UPDATE jobs SET state = 'pending', run_after = ?2, claimed_at = NULL, error = ?3 WHERE id = ?1",
                        params![id, at, error],
                    ),
                    None => c.execute(
                        "UPDATE jobs SET state = ?2, finished_at = ?3, error = ?4 WHERE id = ?1",
                        params![id, state, now_ms, error],
                    ),
                }
                .map(|_| ())
                .map_err(DbError::from)
            })
            .await
    }

    /// Run one job to completion (a panic is a retryable failure).
    async fn run(&self, job: Job) {
        let outcome = match self.handlers.get(job.kind.as_str()) {
            None => Err(JobError::Fail(format!("no handler for job kind '{}'", job.kind))),
            Some(handler) => {
                let (handler, j) = (Arc::clone(handler), job.clone());
                // Spawned so a panic is contained; the guard aborts the handler
                // if this worker is itself aborted at shutdown.
                let mut task = AbortOnDrop(tokio::spawn(async move { handler.run(&j).await }));
                match (&mut task.0).await {
                    Ok(r) => r,
                    Err(e) if e.is_panic() => Err(JobError::Retry("handler panicked".into())),
                    Err(_) => return, // aborted at shutdown: `recover` re-queues it
                }
            }
        };
        if let Err(e) = self.finish(&job, &outcome, Clock::now().unix_ms).await {
            tracing::error!(error = %e, id = %job.id, "could not record job outcome");
        }
    }

    /// `jobs_pending{kind}` for every kind that has pending rows, zero for the rest.
    pub async fn pending_by_kind(&self) -> Result<HashMap<String, i64>, DbError> {
        self.db
            .read(|c| {
                let mut s =
                    c.prepare("SELECT kind, count(*) FROM jobs WHERE state = 'pending' GROUP BY kind")?;
                let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<Result<HashMap<_, _>, _>>().map_err(DbError::from)
            })
            .await
    }

    /// Start `concurrency` workers and the pending-count sampler.
    pub fn start(&self, concurrency: usize) -> Workers {
        let (stop, stopped) = watch::channel(false);
        let mut set = JoinSet::new();
        for _ in 0..concurrency.max(1) {
            let (me, mut stopped) = (self.clone(), stopped.clone());
            set.spawn(async move {
                while !*stopped.borrow() {
                    match me.claim(Clock::now().unix_ms).await {
                        Ok(Some(job)) => me.run(job).await,
                        Ok(None) => {
                            tokio::select! {
                                () = me.notify.notified() => {}
                                () = tokio::time::sleep(IDLE_POLL) => {}
                                _ = stopped.changed() => {}
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "job claim failed");
                            tokio::time::sleep(IDLE_POLL).await;
                        }
                    }
                }
            });
        }
        let (me, mut stopped) = (self.clone(), stopped);
        set.spawn(async move {
            let mut seen: Vec<String> = Vec::new();
            loop {
                if let Ok(counts) = me.pending_by_kind().await {
                    for kind in seen.iter().filter(|k| !counts.contains_key(*k)) {
                        metrics::gauge!(JOBS_PENDING, "kind" => kind.clone()).set(0.0);
                    }
                    for (kind, n) in &counts {
                        #[allow(clippy::cast_precision_loss)]
                        metrics::gauge!(JOBS_PENDING, "kind" => kind.clone()).set(*n as f64);
                    }
                    seen = counts.into_keys().collect();
                }
                tokio::select! {
                    () = tokio::time::sleep(PENDING_SAMPLE) => {}
                    _ = stopped.changed() => break,
                }
            }
        });
        Workers { stop, set }
    }
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The running workers.
pub struct Workers {
    stop: watch::Sender<bool>,
    set: JoinSet<()>,
}

impl Workers {
    /// Stop claiming, let running jobs finish for up to `grace`, then abort the
    /// rest; their rows stay `running` and [`Scheduler::recover`] re-queues them.
    pub async fn shutdown(mut self, grace: Duration) {
        let _ = self.stop.send(true);
        let drained =
            tokio::time::timeout(grace, async { while self.set.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!("jobs still running at shutdown; they resume on the next boot");
            self.set.abort_all();
            while self.set.join_next().await.is_some() {}
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ids {
    #[test]
    fn ids_made_together_still_sort_in_order() {
        let ids: Vec<String> = (0..1000).map(|_| super::next_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }
}

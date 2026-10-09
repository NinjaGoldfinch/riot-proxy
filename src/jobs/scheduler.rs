//! The durable job queue (docs/design/06 §Scheduler): the `jobs` table is the
//! queue, `JOB_CONCURRENCY` workers claim the highest-priority ready row, run
//! its handler and mark it done or reschedule it with backoff.
//!
//! - **Enqueue** is `INSERT OR IGNORE` against `UNIQUE(kind, dedupe_key)` while
//!   pending or running, so queueing work that is already queued is a no-op.
//! - **Claim** is one `UPDATE … RETURNING` on the single writer, so no two
//!   workers can take the same row. It skips work the rate limiter has no
//!   bulk room for and spreads workers over lanes (SCH-01, [`claim_sql`]).
//! - **Wake**: `enqueue` notifies an idle worker; an idle worker otherwise
//!   sleeps until the next delayed row is due or a blocked lane frees up.
//! - **Backoff**: a retryable failure goes back to `pending` after
//!   `2^attempts × 30 s ± 20 %`; the fifth failure is final (`failed`).
//! - **Yield**: a job the limiter would make wait goes back to `pending`
//!   until the limiter has room, without using an attempt.
//!
//! [`claim_sql`]: crate::db::store::claim_sql
//! - **Restart**: rows left `running` by a process that died are reset to
//!   `pending` on boot ([`Scheduler::recover`]), so the work resumes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

use crate::clock::Clock;
use crate::db::store::{ClaimFilter, SqliteStore, Store};
use crate::db::{Db, DbError};
use crate::fetcher::FetchError;
use crate::jobs::activity::{self, Activity};
use crate::metrics::{JOBS_PENDING, JOBS_TOTAL};
use crate::riot::limiter::Limiter;

/// design/06: `failed` after five attempts.
pub const MAX_ATTEMPTS: u32 = 5;
/// design/06: backoff base, `2^attempts × 30 s`.
pub const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// How long an idle worker sleeps when nothing is delayed or blocked (and
/// after a failed claim). `enqueue` wakes it sooner.
pub const IDLE_POLL: Duration = Duration::from_secs(1);
/// The shortest idle sleep, so a wake time already past cannot spin a worker.
const MIN_IDLE: Duration = Duration::from_millis(10);
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
    /// The rate limiter has no room until `retry_at` (unix ms): give the
    /// worker back and run again then (SCH-01). Not a failure and not an
    /// attempt. `payload`, when set, replaces the job's, so a job can resume
    /// where it stopped.
    #[error("rate limited until {retry_at}")]
    Yield { retry_at: i64, payload: Option<String> },
}

impl JobError {
    /// A fetch's failure as a job outcome: the limiter's budget running out
    /// yields; anything else is retried with backoff.
    pub fn from_fetch(e: &FetchError) -> Self {
        match e.limited_until {
            Some(at) => Self::Yield {
                retry_at: Clock::now().to_unix_ms(at),
                payload: None,
            },
            None => Self::Retry(format!("{}: {}", e.api.code.as_str(), e.api.message)),
        }
    }

    /// Yield the worker now and come back after other ready work had its turn.
    pub fn take_turns() -> Self {
        Self::Yield {
            retry_at: Clock::now().unix_ms,
            payload: None,
        }
    }
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

pub(crate) const COLUMNS: &str = "id, kind, dedupe_key, priority, payload, attempts, run_after";

pub(crate) fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
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
pub(crate) fn next_id() -> String {
    static GENERATOR: std::sync::Mutex<ulid::Generator> = std::sync::Mutex::new(ulid::Generator::new());
    GENERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generate()
        .unwrap_or_else(|_| ulid::Ulid::generate())
        .to_string()
}

/// Cancel, on `conn`, the pending jobs of `kinds` whose payload has `key` =
/// `value` (a crawl's queued legs): they become `failed` / `cancelled`, as
/// `Queue::cancel` leaves one, so they stay visible. Running ones notice on
/// their own. Returns how many.
pub fn cancel_pending_on(
    conn: &Connection,
    kinds: &[&str],
    key: &str,
    value: &str,
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let path = format!("$.{key}");
    let mut n = 0;
    for kind in kinds {
        n += conn.execute(
            "UPDATE jobs SET state = 'failed', finished_at = ?4, error = 'cancelled'
              WHERE state = 'pending' AND kind = ?1 AND json_extract(payload, ?2) = ?3",
            rusqlite::params![kind, path, value, now_ms],
        )?;
    }
    Ok(n)
}

/// Queue a job on `conn`, inside the caller's transaction if it has one, so a
/// handler can fan out in the same write that records its own progress.
pub fn enqueue_on(conn: &Connection, job: &NewJob, now_ms: i64) -> rusqlite::Result<Enqueued> {
    let id = next_id();
    let lane = crate::jobs::lanes::of(job.kind, &job.payload);
    let n = conn.execute(
        "INSERT OR IGNORE INTO jobs (id, kind, dedupe_key, priority, payload, run_after, lane, method)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            job.kind,
            job.dedupe_key,
            job.priority,
            job.payload.to_string(),
            job.run_after.unwrap_or(now_ms),
            lane.as_ref().map(|l| l.lane),
            lane.as_ref().map(|l| l.method),
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
    control: Arc<Control>,
}

/// Holding the workers still and stopping what they run (DEV-22), for the dev
/// reset, which must not wipe the queue under a running job. Shared by every
/// clone of the queue, so the route and the workers see the same state.
#[derive(Debug, Default)]
struct Control {
    /// Live [`Halt`]s. While any is held, no worker claims.
    halts: AtomicUsize,
    /// Workers between deciding to claim and recording the outcome.
    busy: AtomicUsize,
    /// Running handler tasks, by run.
    running: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_run: AtomicU64,
    /// Rung when `busy` drops.
    settled: Notify,
}

impl Control {
    fn halted(&self) -> bool {
        self.halts.load(Ordering::SeqCst) > 0
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<u64, tokio::task::AbortHandle>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Track a handler task; it is aborted at once if a halt is on (one may
    /// have started between the claim and here).
    fn track(&self, task: &tokio::task::JoinHandle<Result<(), JobError>>) -> u64 {
        let run = self.next_run.fetch_add(1, Ordering::SeqCst);
        self.running().insert(run, task.abort_handle());
        if self.halted() {
            task.abort();
        }
        run
    }

    fn untrack(&self, run: u64) {
        self.running().remove(&run);
    }

    fn enter(&self) -> bool {
        self.busy.fetch_add(1, Ordering::SeqCst);
        if self.halted() {
            self.leave();
            return false;
        }
        true
    }

    fn leave(&self) {
        self.busy.fetch_sub(1, Ordering::SeqCst);
        self.settled.notify_waiters();
    }
}

/// Workers held still: nothing is claimed until it is dropped. Made by
/// [`Queue::halt`].
#[derive(Debug)]
pub struct Halt {
    queue: Queue,
    /// Handler tasks aborted.
    pub stopped: usize,
    /// Every worker came back within the wait.
    pub settled: bool,
}

impl Drop for Halt {
    fn drop(&mut self) {
        self.queue.control.halts.fetch_sub(1, Ordering::SeqCst);
        self.queue.wake_all();
    }
}

impl Queue {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            notify: Arc::new(Notify::new()),
            control: Arc::new(Control::default()),
        }
    }

    /// Stop the workers: no more claims, every running handler aborted, then
    /// wait up to `wait` for the workers to come back (DEV-22). An aborted
    /// job records no outcome; its row stays `running` for the caller to
    /// delete, or for [`Scheduler::recover`] at the next boot. Claims resume
    /// when the returned [`Halt`] is dropped. A worker in another process
    /// (`ROLE=worker`) is not reached.
    pub async fn halt(&self, wait: Duration) -> Halt {
        let c = &self.control;
        c.halts.fetch_add(1, Ordering::SeqCst);
        let stopped = {
            let running = c.running();
            for task in running.values() {
                task.abort();
            }
            running.len()
        };
        let settled = tokio::time::timeout(wait, async {
            loop {
                let settled = c.settled.notified();
                tokio::pin!(settled);
                settled.as_mut().enable();
                if c.busy.load(Ordering::SeqCst) == 0 {
                    return;
                }
                settled.await;
            }
        })
        .await
        .is_ok();
        Halt {
            queue: self.clone(),
            stopped,
            settled,
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

    /// Queue a job, or, when a pending duplicate holds its dedupe key, lift
    /// that row to `job`'s priority and make it ready now (never lower or
    /// later than it was), then wake a worker. A running duplicate is left
    /// alone. For work someone asked for by hand that should not wait behind
    /// the queue (DEV-18).
    pub async fn enqueue_or_promote(&self, job: NewJob) -> Result<Enqueued, DbError> {
        let now = Clock::now().unix_ms;
        let out = self
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let out = enqueue_on(&tx, &job, now)?;
                if !out.created {
                    tx.execute(
                        "UPDATE jobs SET priority = MIN(priority, ?2), run_after = MIN(run_after, ?3)
                          WHERE id = ?1 AND state = 'pending'",
                        params![out.id, job.priority, job.run_after.unwrap_or(now)],
                    )?;
                }
                tx.commit()?;
                Ok::<_, DbError>(out)
            })
            .await?;
        self.notify.notify_one();
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

pub(crate) const ROW_COLUMNS: &str =
    "id, kind, dedupe_key, priority, payload, state, attempts, run_after, claimed_at, finished_at, error";

pub(crate) fn full_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
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

/// The queue as the workers see it (DEV-13): what is running, and what they
/// claim next, in the claim order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueView {
    /// Oldest claim first.
    pub running: Vec<JobRow>,
    /// Ready jobs in the order `claim` takes them: `priority, run_after, id`.
    pub next: Vec<JobRow>,
    /// Pending and due now.
    pub ready: i64,
    /// Pending but waiting out a backoff or a schedule.
    pub delayed: i64,
    /// When the soonest delayed job comes due.
    pub next_delayed_at: Option<i64>,
}

/// The priority order inside a lane (design/06 §Claiming). Across lanes the
/// claim also skips blocked work and spreads workers (SCH-01), so the views
/// that list "up next" in this order show the order of work, not exactly
/// which job a worker takes next.
pub(crate) const CLAIM_ORDER: &str = "priority ASC, run_after ASC, id ASC";

/// design/06's priority bands as a sort key: 0–99, 100–9 999, then each
/// 10 000 (polls, backfill and ladder, maintenance). Inside the best band a
/// claim prefers the least busy lane; across bands priority wins.
pub(crate) const CLAIM_BAND: &str = "CASE WHEN j.priority < 100 THEN 0 WHEN j.priority < 10000 THEN 100 \
     ELSE j.priority - j.priority % 10000 END";

impl Queue {
    /// Running jobs and the next `limit` a worker would claim (DEV-13).
    pub async fn view(&self, now_ms: i64, limit: u32) -> Result<QueueView, DbError> {
        self.db
            .read(move |c| {
                let running = c
                    .prepare(&format!(
                        "SELECT {ROW_COLUMNS} FROM jobs WHERE state = 'running' ORDER BY claimed_at ASC, id ASC"
                    ))?
                    .query_map([], full_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                let next = c
                    .prepare(&format!(
                        "SELECT {ROW_COLUMNS} FROM jobs WHERE state = 'pending' AND run_after <= ?1
                          ORDER BY {CLAIM_ORDER} LIMIT ?2"
                    ))?
                    .query_map(rusqlite::params![now_ms, limit], full_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                let (ready, delayed, next_delayed_at) = c.query_row(
                    "SELECT count(*) FILTER (WHERE run_after <= ?1),
                            count(*) FILTER (WHERE run_after > ?1),
                            min(run_after) FILTER (WHERE run_after > ?1)
                       FROM jobs WHERE state = 'pending'",
                    [now_ms],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                Ok(QueueView { running, next, ready, delayed, next_delayed_at })
            })
            .await
    }

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

    /// Jobs of `kinds` whose payload names `puuid`, counted by `(kind, state)`,
    /// and the newest of each kind (DEV-03). Uncapped; a scan of the queue,
    /// which keeps `done` rows for seven days, so it is for admin views only.
    pub async fn for_puuid(
        &self,
        puuid: &str,
        kinds: &'static [&'static str],
    ) -> Result<(Vec<(String, String, i64)>, Vec<JobRow>), DbError> {
        let puuid = puuid.to_string();
        self.db
            .read(move |c| {
                let marks = vec!["?"; kinds.len()].join(",");
                let mut args: Vec<&dyn rusqlite::ToSql> = vec![&puuid];
                args.extend(kinds.iter().map(|k| k as &dyn rusqlite::ToSql));
                let mut s = c.prepare(&format!(
                    "SELECT kind, state, count(*) FROM jobs
                      WHERE json_extract(payload, '$.puuid') = ?1 AND kind IN ({marks})
                      GROUP BY kind, state ORDER BY kind, state"
                ))?;
                let counts = s
                    .query_map(args.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut latest = Vec::new();
                for kind in kinds {
                    let row = c
                        .query_row(
                            &format!(
                                "SELECT {ROW_COLUMNS} FROM jobs
                                  WHERE kind = ?1 AND json_extract(payload, '$.puuid') = ?2
                                  ORDER BY id DESC LIMIT 1"
                            ),
                            rusqlite::params![kind, puuid],
                            full_row,
                        )
                        .optional()?;
                    latest.extend(row);
                }
                Ok((counts, latest))
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
    /// The limiter the handlers' fetches go through. Without one every lane
    /// counts as open.
    limiter: Option<Arc<Limiter>>,
    /// What each worker is doing (DEV-19).
    activity: Activity,
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
            limiter: None,
            activity: Activity::new(),
        }
    }

    /// Claim around what `limiter` has no bulk room for (SCH-01).
    #[must_use]
    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        self.limiter = Some(limiter);
        self
    }

    /// Record the workers' activity in `activity` (shared with the admin routes).
    #[must_use]
    pub fn with_activity(mut self, activity: Activity) -> Self {
        self.activity = activity;
        self
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn activity(&self) -> &Activity {
        &self.activity
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

    /// Claim the best ready job the limiter has room for (design/06
    /// §Claiming), through the engine seam.
    pub async fn claim(&self, now_ms: i64) -> Result<Option<Job>, DbError> {
        let filter = match &self.limiter {
            Some(limiter) => {
                let blocked = limiter.bulk_blocked();
                let methods = blocked.methods.iter().map(|(l, m, _)| (l.as_str(), m.as_str()));
                ClaimFilter::new(
                    crate::jobs::lanes::all().filter(|l| !blocked.scope_blocked(l)),
                    methods,
                )
            }
            None => ClaimFilter::open(),
        };
        SqliteStore::new(self.db.clone()).claim_job(now_ms, &filter).await
    }

    /// How long an idle worker sleeps: until the next delayed row is due or
    /// the first blocked lane or method frees up, whichever is sooner
    /// (SCH-01). With neither, [`IDLE_POLL`].
    pub async fn idle_for(&self) -> Duration {
        let clock = Clock::now();
        let freed = self
            .limiter
            .as_ref()
            .and_then(|l| l.bulk_blocked().earliest())
            .map(|at| at.saturating_duration_since(clock.instant));
        let now_ms = clock.unix_ms;
        let due = self
            .db
            .read(move |c| {
                c.query_row(
                    "SELECT min(run_after) FROM jobs WHERE state = 'pending' AND run_after > ?1",
                    [now_ms],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .map_err(DbError::from)
            })
            .await
            .ok()
            .flatten()
            .map(|at| Duration::from_millis(u64::try_from(at - now_ms).unwrap_or(0)));
        match (freed, due) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => IDLE_POLL,
        }
        .max(MIN_IDLE)
    }

    /// Boot: give pending rows queued before lanes existed (V0007) their
    /// lane, so the claim can spread and skip them too. Returns how many.
    pub async fn assign_lanes(&self) -> Result<usize, DbError> {
        self.db
            .write(|c| {
                let tx = c.transaction()?;
                let rows: Vec<(String, String, String)> = {
                    let mut s = tx.prepare(
                        "SELECT id, kind, payload FROM jobs WHERE state = 'pending' AND lane IS NULL",
                    )?;
                    let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    rows.collect::<Result<_, _>>()?
                };
                let mut n = 0;
                {
                    let mut update = tx.prepare("UPDATE jobs SET lane = ?2, method = ?3 WHERE id = ?1")?;
                    for (id, kind, payload) in rows {
                        let payload = serde_json::from_str(&payload).unwrap_or_default();
                        if let Some(lane) = crate::jobs::lanes::of(&kind, &payload) {
                            n += update.execute(params![id, lane.lane, lane.method])?;
                        }
                    }
                }
                tx.commit()?;
                Ok(n)
            })
            .await
    }

    /// Record a run's outcome: `done`, back to `pending` with backoff, or
    /// `failed`. A yield goes back to `pending` as if never claimed (SCH-01):
    /// the attempt is returned, the last real failure stays on the row, and
    /// it is not counted in `jobs_total`.
    pub async fn finish(
        &self,
        job: &Job,
        outcome: &Result<(), JobError>,
        now_ms: i64,
    ) -> Result<(), DbError> {
        let id = job.id.clone();
        let next = match outcome {
            Ok(()) => Next::Done,
            Err(JobError::Yield { retry_at, payload }) => Next::Yield(*retry_at, payload.clone()),
            Err(JobError::Retry(e)) if job.attempts < MAX_ATTEMPTS => {
                let wait = backoff(job.attempts, random_jitter());
                let at = now_ms.saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
                Next::Backoff(at, e.clone())
            }
            Err(JobError::Retry(e) | JobError::Fail(e)) => Next::Failed(e.clone()),
        };
        match &next {
            Next::Done => {
                metrics::counter!(JOBS_TOTAL, "job" => job.kind.clone(), "status" => "completed")
                    .increment(1);
            }
            Next::Yield(at, _) => {
                tracing::debug!(job = %job.kind, id = %job.id, retry_at = at, "job yielded to the rate limiter");
            }
            Next::Backoff(_, e) | Next::Failed(e) => {
                metrics::counter!(JOBS_TOTAL, "job" => job.kind.clone(), "status" => "failed").increment(1);
                let state = if matches!(next, Next::Failed(_)) {
                    "failed"
                } else {
                    "pending"
                };
                tracing::warn!(job = %job.kind, id = %job.id, attempts = job.attempts, next = state, error = %e, "job failed");
            }
        }
        self.db
            .write(move |c| {
                match next {
                    Next::Done => c.execute(
                        "UPDATE jobs SET state = 'done', finished_at = ?2, error = NULL WHERE id = ?1",
                        params![id, now_ms],
                    ),
                    Next::Yield(at, payload) => c.execute(
                        "UPDATE jobs SET state = 'pending', run_after = ?2, claimed_at = NULL,
                                attempts = max(attempts - 1, 0), payload = coalesce(?3, payload)
                          WHERE id = ?1",
                        params![id, at, payload],
                    ),
                    Next::Backoff(at, e) => c.execute(
                        "UPDATE jobs SET state = 'pending', run_after = ?2, claimed_at = NULL, error = ?3 WHERE id = ?1",
                        params![id, at, e],
                    ),
                    Next::Failed(e) => c.execute(
                        "UPDATE jobs SET state = 'failed', finished_at = ?2, error = ?3 WHERE id = ?1",
                        params![id, now_ms, e],
                    ),
                }
                .map(|_| ())
                .map_err(DbError::from)
            })
            .await
    }

    /// Run one job to completion on worker `worker` (0-based); a panic is a
    /// retryable failure. The handler runs with the job as its task's current
    /// job, so what it does lands in the job's trace (DEV-19).
    async fn run(&self, job: Job, worker: usize) {
        self.activity.claimed(worker, &job.id, &job.kind, job.attempts);
        let outcome = match self.handlers.get(job.kind.as_str()) {
            None => Err(JobError::Fail(format!("no handler for job kind '{}'", job.kind))),
            Some(handler) => {
                let (handler, j) = (Arc::clone(handler), job.clone());
                let current = activity::Current::new(self.activity.clone(), &job.id);
                // Spawned so a panic is contained; the guard aborts the handler
                // if this worker is itself aborted at shutdown.
                let mut task = AbortOnDrop(tokio::spawn(activity::within(Some(current), async move {
                    handler.run(&j).await
                })));
                let control = &self.queue.control;
                let run = control.track(&task.0);
                let out = (&mut task.0).await;
                control.untrack(run);
                match out {
                    Ok(r) => r,
                    Err(e) if e.is_panic() => Err(JobError::Retry("handler panicked".into())),
                    Err(_) => {
                        // Aborted at shutdown (`recover` re-queues it) or by a
                        // halt (the dev reset deletes it).
                        self.activity.finished(&job.id, "aborted");
                        return;
                    }
                }
            }
        };
        self.activity.finished(&job.id, &outcome_text(&job, &outcome));
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
        let concurrency = concurrency.max(1);
        self.activity.set_workers(concurrency);
        for worker in 0..concurrency {
            let (me, mut stopped) = (self.clone(), stopped.clone());
            set.spawn(async move {
                while !*stopped.borrow() {
                    let control = Arc::clone(&me.queue.control);
                    if !control.enter() {
                        // Halted: wait for the halt to end (it wakes everyone).
                        let woken = me.notify.notified();
                        tokio::pin!(woken);
                        woken.as_mut().enable();
                        if control.halted() {
                            tokio::select! {
                                () = woken => {}
                                () = tokio::time::sleep(IDLE_POLL) => {}
                                _ = stopped.changed() => {}
                            }
                        }
                        continue;
                    }
                    let claimed = me.claim(Clock::now().unix_ms).await;
                    if let Ok(Some(job)) = claimed {
                        me.run(job, worker).await;
                        control.leave();
                        continue;
                    }
                    control.leave();
                    match claimed {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            // Armed before the sleep is worked out, so an
                            // enqueue meanwhile still wakes this worker.
                            let woken = me.notify.notified();
                            tokio::pin!(woken);
                            woken.as_mut().enable();
                            let idle = me.idle_for().await;
                            tokio::select! {
                                () = woken => {}
                                () = tokio::time::sleep(idle) => {}
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

/// Where a finished run leaves its row.
enum Next {
    Done,
    /// `run_after`, and the payload to resume with.
    Yield(i64, Option<String>),
    /// `run_after`, and the error.
    Backoff(i64, String),
    Failed(String),
}

/// How a run ended, as the trace says it: what [`Scheduler::finish`] makes
/// of the outcome.
fn outcome_text(job: &Job, outcome: &Result<(), JobError>) -> String {
    match outcome {
        Ok(()) => "done".into(),
        Err(JobError::Retry(e)) if job.attempts < MAX_ATTEMPTS => format!("retry later: {e}"),
        Err(JobError::Retry(e) | JobError::Fail(e)) => format!("failed: {e}"),
        Err(JobError::Yield { retry_at, .. }) => format!(
            "yielded: no rate-limit room until {}",
            crate::clock::iso_ms(*retry_at).unwrap_or_default()
        ),
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

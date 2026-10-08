//! What each worker is doing right now (DEV-19, design/06 §Live activity): an
//! in-memory record the `/dev/jobs` page reads through
//! `GET /v1/admin/jobs/activity` and `GET /v1/admin/jobs/{id}/activity`.
//!
//! The scheduler opens a trace when a worker claims a job and closes it with
//! the outcome. While the handler runs, the job is the task's *current job*
//! (a tokio task-local), so code anywhere below it, the fetcher included, can
//! say what it is doing with [`step`] (the job's "now" line, also logged) and
//! [`event`] (logged only), without being handed anything. Outside a job both
//! are no-ops. Work a handler spawns onto another task is not traced unless
//! that task is wrapped in [`within`] with [`current`].
//!
//! Nothing is persisted: a trace lives in this process only, at most
//! [`EVENTS_PER_JOB`] events each, and the last [`FINISHED_KEPT`] finished
//! traces are kept so a tab opened on a job that just ended still shows how it
//! went. A process with no workers (`ROLE=api`) has no traces.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Serialize, Serializer};
use utoipa::ToSchema;

use crate::clock::{Clock, iso_ms};

/// Unix ms held, ISO 8601 sent, like every other admin timestamp.
fn iso<S: Serializer>(ms: &i64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&iso_ms(*ms).unwrap_or_default())
}

fn iso_opt<S: Serializer>(ms: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match ms.and_then(iso_ms) {
        Some(v) => s.serialize_str(&v),
        None => s.serialize_none(),
    }
}

/// Events kept per trace; older ones are dropped and counted.
pub const EVENTS_PER_JOB: usize = 200;
/// Finished traces kept, newest first.
pub const FINISHED_KEPT: usize = 200;

/// One thing a job did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Increases by one per event within a trace; `?after=` reads past it.
    pub seq: u64,
    #[serde(serialize_with = "iso")]
    #[schema(value_type = String)]
    pub at: i64,
    /// `job` (claimed, finished), `step` (a handler's progress), `riot` (a
    /// call and how it was answered), `wait` (rate limiter, backoff) or `error`.
    pub kind: &'static str,
    pub text: String,
    /// How long it took, where that means something.
    #[schema(required = true)]
    pub ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct Trace {
    kind: String,
    worker: usize,
    attempt: u32,
    started_at: i64,
    finished_at: Option<i64>,
    outcome: Option<String>,
    now: Option<(String, i64)>,
    events: VecDeque<Event>,
    next_seq: u64,
    dropped: u64,
}

impl Trace {
    fn push(&mut self, kind: &'static str, text: String, ms: Option<i64>, at: i64) {
        if self.events.len() == EVENTS_PER_JOB {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(Event {
            seq: self.next_seq,
            at,
            kind,
            text,
            ms,
        });
        self.next_seq += 1;
    }
}

#[derive(Debug, Default)]
struct State {
    /// By worker index: the job it runs, if any, and since when it has been in
    /// that state (claimed, or idle).
    workers: Vec<(Option<String>, i64)>,
    traces: HashMap<String, Trace>,
    finished: VecDeque<String>,
}

/// The record, shared by the scheduler (writes) and the admin routes (reads).
#[derive(Debug, Clone, Default)]
pub struct Activity(Arc<Mutex<State>>);

/// A worker and its job, for the overview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerView {
    /// 1-based.
    pub worker: usize,
    #[schema(required = true)]
    pub job_id: Option<String>,
    #[schema(required = true)]
    pub kind: Option<String>,
    /// When the worker took the job, or went idle.
    #[serde(serialize_with = "iso")]
    #[schema(value_type = String)]
    pub since: i64,
    /// The job's latest step.
    #[schema(required = true)]
    pub now: Option<String>,
    /// When the job got to that step.
    #[serde(serialize_with = "iso_opt")]
    #[schema(required = true, value_type = Option<String>)]
    pub now_since: Option<i64>,
    /// Events the job has logged so far.
    pub events: u64,
}

/// A job that finished in this process, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinishedView {
    pub job_id: String,
    pub kind: String,
    pub worker: usize,
    #[serde(serialize_with = "iso")]
    #[schema(value_type = String)]
    pub started_at: i64,
    #[serde(serialize_with = "iso")]
    #[schema(value_type = String)]
    pub finished_at: i64,
    /// `done`, `retry later: …`, `failed: …`, `yielded: …` (SCH-01) or `aborted` (shutdown).
    pub outcome: String,
}

/// One job's trace, from event `after` on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TraceView {
    pub kind: String,
    /// 1-based.
    pub worker: usize,
    /// Which attempt this trace is of.
    pub attempt: u32,
    #[serde(serialize_with = "iso")]
    #[schema(value_type = String)]
    pub started_at: i64,
    #[serde(serialize_with = "iso_opt")]
    #[schema(required = true, value_type = Option<String>)]
    pub finished_at: Option<i64>,
    #[schema(required = true)]
    pub outcome: Option<String>,
    #[schema(required = true)]
    pub now: Option<String>,
    #[serde(serialize_with = "iso_opt")]
    #[schema(required = true, value_type = Option<String>)]
    pub now_since: Option<i64>,
    /// Events after `after`, oldest first.
    pub events: Vec<Event>,
    /// The `seq` the next event will get; pass it back as `after`.
    pub next_seq: u64,
    /// Events dropped from the front (only the last [`EVENTS_PER_JOB`] are kept).
    pub dropped: u64,
}

impl Activity {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `n` workers, all idle.
    pub fn set_workers(&self, n: usize) {
        let now = Clock::now().unix_ms;
        self.lock().workers = vec![(None, now); n];
    }

    /// Worker `worker` (0-based) claimed `id`: a new trace, replacing an
    /// earlier attempt's.
    pub fn claimed(&self, worker: usize, id: &str, kind: &str, attempt: u32) {
        let now = Clock::now().unix_ms;
        let mut s = self.lock();
        if let Some(slot) = s.workers.get_mut(worker) {
            *slot = (Some(id.to_string()), now);
        }
        s.finished.retain(|f| f != id);
        let mut trace = Trace {
            kind: kind.to_string(),
            worker,
            attempt,
            started_at: now,
            finished_at: None,
            outcome: None,
            now: Some(("starting".into(), now)),
            events: VecDeque::new(),
            next_seq: 0,
            dropped: 0,
        };
        trace.push(
            "job",
            format!("claimed by worker {} (attempt {attempt})", worker + 1),
            None,
            now,
        );
        s.traces.insert(id.to_string(), trace);
    }

    /// `id` ended with `outcome`; its worker is idle again.
    pub fn finished(&self, id: &str, outcome: &str) {
        let now = Clock::now().unix_ms;
        let mut s = self.lock();
        let Some(trace) = s.traces.get_mut(id) else {
            return;
        };
        let (worker, ms) = (trace.worker, now - trace.started_at);
        trace.finished_at = Some(now);
        trace.outcome = Some(outcome.to_string());
        trace.now = None;
        trace.push("job", outcome.to_string(), Some(ms), now);
        if let Some(slot) = s.workers.get_mut(worker)
            && slot.0.as_deref() == Some(id)
        {
            *slot = (None, now);
        }
        s.finished.push_front(id.to_string());
        while s.finished.len() > FINISHED_KEPT {
            if let Some(old) = s.finished.pop_back() {
                s.traces.remove(&old);
            }
        }
    }

    /// Log `kind`/`text` on `id` (`kind` `None`: only set the "now" line),
    /// and make it the "now" line when `now_line`.
    fn record(&self, id: &str, kind: Option<&'static str>, text: String, ms: Option<i64>, now_line: bool) {
        let at = Clock::now().unix_ms;
        let mut s = self.lock();
        if let Some(trace) = s.traces.get_mut(id)
            && trace.finished_at.is_none()
        {
            if now_line {
                trace.now = Some((text.clone(), at));
            }
            if let Some(kind) = kind {
                trace.push(kind, text, ms, at);
            }
        }
    }

    /// Every worker, in order, with its job's latest step.
    pub fn workers(&self) -> Vec<WorkerView> {
        let s = self.lock();
        s.workers
            .iter()
            .enumerate()
            .map(|(i, (job, since))| {
                let trace = job.as_ref().and_then(|id| s.traces.get(id));
                WorkerView {
                    worker: i + 1,
                    job_id: job.clone(),
                    kind: trace.map(|t| t.kind.clone()),
                    since: *since,
                    now: trace.and_then(|t| t.now.as_ref().map(|n| n.0.clone())),
                    now_since: trace.and_then(|t| t.now.as_ref().map(|n| n.1)),
                    events: trace.map_or(0, |t| t.next_seq),
                }
            })
            .collect()
    }

    /// Finished traces, newest first, at most `limit`.
    pub fn finished_jobs(&self, limit: usize) -> Vec<FinishedView> {
        let s = self.lock();
        s.finished
            .iter()
            .filter_map(|id| {
                let t = s.traces.get(id)?;
                Some(FinishedView {
                    job_id: id.clone(),
                    kind: t.kind.clone(),
                    worker: t.worker + 1,
                    started_at: t.started_at,
                    finished_at: t.finished_at?,
                    outcome: t.outcome.clone()?,
                })
            })
            .take(limit)
            .collect()
    }

    /// `id`'s trace with the events whose `seq` is at least `after`.
    pub fn trace(&self, id: &str, after: u64) -> Option<TraceView> {
        let s = self.lock();
        let t = s.traces.get(id)?;
        Some(TraceView {
            kind: t.kind.clone(),
            worker: t.worker + 1,
            attempt: t.attempt,
            started_at: t.started_at,
            finished_at: t.finished_at,
            outcome: t.outcome.clone(),
            now: t.now.as_ref().map(|n| n.0.clone()),
            now_since: t.now.as_ref().map(|n| n.1),
            events: t.events.iter().filter(|e| e.seq >= after).cloned().collect(),
            next_seq: t.next_seq,
            dropped: t.dropped,
        })
    }
}

/// The job a task is running and where its trace lives.
#[derive(Debug, Clone)]
pub struct Current {
    activity: Activity,
    id: Arc<str>,
}

tokio::task_local! {
    static CURRENT: Current;
}

impl Current {
    pub fn new(activity: Activity, id: &str) -> Self {
        Self {
            activity,
            id: id.into(),
        }
    }
}

/// The task's current job, to carry into a task it spawns.
pub fn current() -> Option<Current> {
    CURRENT.try_with(Clone::clone).ok()
}

/// Run `fut` with `current` as its job (or none).
pub async fn within<F: Future>(current: Option<Current>, fut: F) -> F::Output {
    match current {
        Some(c) => CURRENT.scope(c, fut).await,
        None => fut.await,
    }
}

fn with(f: impl FnOnce(&Current)) {
    let _ = CURRENT.try_with(f);
}

/// The current job is now doing `text`: its "now" line, and an event.
pub fn step(text: impl Into<String>) {
    with(|c| c.activity.record(&c.id, Some("step"), text.into(), None, true));
}

/// Log an event of `kind` on the current job, without changing its "now" line.
pub fn event(kind: &'static str, text: impl Into<String>, ms: Option<i64>) {
    with(|c| c.activity.record(&c.id, Some(kind), text.into(), ms, false));
}

/// Set the current job's "now" line to `text` without logging it: for what
/// is momentary (waiting on the limiter, a call in flight) and reported by
/// an [`event`] once it is over.
pub fn now(text: impl Into<String>) {
    with(|c| c.activity.record(&c.id, None, text.into(), None, true));
}

/// Whether this task is running a job (so callers can skip building text).
pub fn tracing_job() -> bool {
    CURRENT.try_with(|_| ()).is_ok()
}

#[cfg(test)]
mod tests;

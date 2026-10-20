//! `/healthz` and `/readyz`. Both are public: no key, no quota (v1 `routes/health.ts`).
//! `/metrics` is served by `telemetry::metrics_router`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::cache::keys::KeyScope;
use crate::db::{Db, DbError, WriterStatus};

/// A readiness probe that cannot get a write slot this quickly counts as not ready.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the probe itself waits for another process's write lock. Shorter than
/// [`READY_TIMEOUT`], so a probe that cannot get the lock reports it rather than
/// occupying the writer, where a concurrent `/readyz` would take it for a busy
/// write making progress, for the connection's 5 s `busy_timeout`.
const PROBE_LOCK_WAIT: Duration = Duration::from_millis(1500);

/// The writer connection's `busy_timeout` (design 04), restored after the probe.
const WRITER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A busy writer that has made no progress for this long is stalled (WR-02,
/// ADR-180). Tests shorten it with `Db::set_writer_stall`.
const WRITER_STALL: Duration = Duration::from_secs(5);

/// A job's start counts as progress for at most this long (and never longer than
/// the stall threshold) until SQLite stamps: a healthy job stamps within
/// milliseconds, one waiting on another process's lock never does.
const START_GRACE: Duration = Duration::from_millis(1500);

/// With no progress stamp within the stall threshold, the writer thread's CPU
/// clock is read twice this fraction of it apart (250 ms by default)…
const CPU_SAMPLE_DIVISOR: u32 = 20;
/// …and a thread that used at least this share of the gap is computing (a sort
/// SQLite runs as one step), not waiting on a lock or sleeping.
const CPU_BUSY_DIVISOR: u32 = 4;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(healthz))
        .routes(routes!(readyz))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    pub ok: bool,
}

/// Liveness: answers while the process is up. Touches nothing.
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "ops",
    summary = "Liveness",
    description = "Answers as long as the process is up. It touches nothing, so a 200 says only that \
                   the process is serving; use `/readyz` to decide whether to send traffic.",
    security(()),
    responses((status = 200, description = "Up", body = Health)),
)]
async fn healthz() -> Json<Health> {
    Json(Health { ok: true })
}

/// v1 returned `{ok, redis, postgres, keyScope}`. v2 has one store, so `sqlite` replaces
/// the two backend booleans, and `limiter` says the checkpoint was restored
/// (design/07). `keyScope` is v1's (ADR-011). `sqliteReaders`/`sqliteReadersFree`
/// show the read pool, so a leak shows before reads fail (ADR-113). `writer*` say
/// what the writer thread is doing (WR-02).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub ok: bool,
    pub sqlite: bool,
    pub limiter: bool,
    pub key_scope: String,
    /// SQLite read connections in the pool.
    pub sqlite_readers: usize,
    /// Of those, how many are idle now. Informational: 0 under load is normal and
    /// doesn't make the probe fail.
    pub sqlite_readers_free: usize,
    /// The writer thread is running a write now.
    pub writer_busy: bool,
    /// For how long, in ms; 0 when idle.
    pub writer_busy_ms: u64,
    /// Writes queued behind it.
    pub writer_queued: usize,
}

/// Readiness: the writer thread is alive and can take SQLite's write lock (or, while
/// it runs a write, is making progress), and the limiter checkpoint has been restored.
/// 503 carries the same body, so it names what is not ready (v1 behaviour).
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "ops",
    summary = "Readiness",
    description = "SQLite can take a write and the limiter checkpoint is restored. While the writer runs \
                   a write, `sqlite` says whether SQLite has made progress on it in the last 5 s instead \
                   of queueing a probe behind it, so a long rebuild stays ready and a stuck write does \
                   not. **The 503 carries the same body as the 200**, so the booleans name what is not \
                   ready. `sqliteReadersFree`, `writerBusy`, `writerBusyMs` and `writerQueued` are \
                   informational; they don't affect `ok`.",
    security(()),
    responses(
        (status = 200, description = "Ready", body = Ready),
        (status = 503, description = "Not ready", body = Ready),
    ),
)]
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Ready>) {
    // Read before the probe: its own run is not what `writer*` should report.
    let writer = state.db.writer_status();
    let sqlite = sqlite_ready(&state.db, &writer).await;
    let limiter = state.limiter_restored.load(std::sync::atomic::Ordering::Acquire);
    let ok = sqlite && limiter;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Ready {
            ok,
            sqlite,
            limiter,
            key_scope: KeyScope::from_key(&state.config.riot_api_key).to_string(),
            sqlite_readers: state.db.readers(),
            sqlite_readers_free: state.db.readers_free(),
            writer_busy: writer.busy(),
            writer_busy_ms: writer
                .busy_for
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            writer_queued: writer.queued,
        }),
    )
}

/// Idle writer: queue `BEGIN IMMEDIATE; ROLLBACK;` and wait for it, which also
/// catches another process holding the write lock. Busy writer: never queue behind
/// it; it is ready while SQLite keeps making progress on its job. A probe that
/// times out still queued (a job started just before it) falls back to that rule.
async fn sqlite_ready(db: &Db, writer: &WriterStatus) -> bool {
    let stall = db.writer_stall().unwrap_or(WRITER_STALL);
    if writer.busy() {
        return progressing(db, writer, stall).await;
    }
    let started = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&started);
    let probe = db.write(move |c| {
        flag.store(true, Ordering::Release);
        c.busy_timeout(PROBE_LOCK_WAIT)?;
        let result = c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;");
        c.busy_timeout(WRITER_BUSY_TIMEOUT)?;
        result.map_err(DbError::from)
    });
    match tokio::time::timeout(READY_TIMEOUT, probe).await {
        Ok(result) => result.is_ok(),
        Err(_) if !started.load(Ordering::Acquire) => progressing(db, &db.writer_status(), stall).await,
        Err(_) => false,
    }
}

/// A busy writer makes progress if SQLite stamped it within `stall` (a job that
/// found the writer idle counts for [`START_GRACE`]), or, failing that, if the
/// thread is using CPU: a sort runs as one VM step and stamps
/// nothing for seconds, while a wait in `busy_timeout` or a sleep uses none.
async fn progressing(db: &Db, writer: &WriterStatus, stall: Duration) -> bool {
    if !writer.busy() {
        return false;
    }
    let fresh_for = if writer.progress_from_sqlite {
        stall
    } else {
        stall.min(START_GRACE)
    };
    if writer.since_progress.is_some_and(|d| d < fresh_for) {
        return true;
    }
    let Some(before) = db.writer_cpu_time() else {
        return false;
    };
    let gap = stall / CPU_SAMPLE_DIVISOR;
    tokio::time::sleep(gap).await;
    let used = db
        .writer_cpu_time()
        .map_or(Duration::ZERO, |after| after.saturating_sub(before));
    // Not a job finishing in the gap: a write that waited out `busy_timeout` and
    // failed with `database is locked` finishes too.
    used >= gap / CPU_BUSY_DIVISOR
}

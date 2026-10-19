//! What the writer thread is doing, for `/readyz` and the dashboard (WR-02, ADR-180).
//! The thread stamps atomics as it starts and finishes each job, and a progress
//! handler on its connection stamps them while SQLite executes, so a reader can
//! tell a long write that is making progress from one that is stuck, without
//! queueing anything behind it. SQLite calls the handler only between the VM's
//! loop steps, so a large sort (one opcode) goes seconds without a stamp; the
//! writer thread's CPU clock tells that apart from waiting on a lock or sleeping.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use tokio::sync::mpsc;

/// VM instructions between progress stamps. On a 1 M-row insert, row by row or as
/// one statement, 1000 costs nothing measurable, 100 up to about 1 % and 10 costs
/// 4 to 10 % (ADR-180, `progress_handler_overhead`); a stamp every 1000
/// instructions is still microseconds apart. SQLite counts them per statement, so
/// a long statement or a cached one run many times stamps; a run of short, freshly
/// prepared statements stamps through the commit hook instead.
pub(super) const PROGRESS_OPS: i32 = 1000;

/// A job slower than this logs a warning.
const SLOW_WRITE_WARN: Duration = Duration::from_secs(1);

/// The writer's stamps, in milliseconds since `base` plus one, so 0 means never.
#[derive(Debug)]
pub(super) struct Stamps {
    base: Instant,
    busy_since: AtomicU64,
    last_progress: AtomicU64,
    /// `last_progress` came from SQLite (a progress or commit stamp), not from a
    /// job starting.
    from_sqlite: AtomicBool,
    last_finished: AtomicU64,
    /// Jobs finished since open.
    finished: AtomicU64,
    /// Test hook: `/readyz`'s stall threshold in ms, 0 for its default.
    stall_override: AtomicU64,
    /// The writer thread's CPU-time clock, set as it is spawned.
    cpu_clock: OnceLock<CpuClock>,
}

impl Stamps {
    pub(super) fn new() -> Self {
        Self {
            base: Instant::now(),
            busy_since: AtomicU64::new(0),
            last_progress: AtomicU64::new(0),
            from_sqlite: AtomicBool::new(false),
            last_finished: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            stall_override: AtomicU64::new(0),
            cpu_clock: OnceLock::new(),
        }
    }

    /// CPU time the writer thread has used, where the platform can say.
    pub(super) fn cpu_time(&self) -> Option<Duration> {
        self.cpu_clock.get().and_then(CpuClock::read)
    }

    fn now(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_millis())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    pub(super) fn progress(&self) {
        self.last_progress.store(self.now(), Ordering::Relaxed);
        self.from_sqlite.store(true, Ordering::Relaxed);
    }

    /// A job starts. Only one that found the writer idle counts as progress: a job
    /// taken off a queue behind another inherits its stamps, so writes that each
    /// wait out `busy_timeout` on another process's lock never look fresh.
    fn start(&self, after_idle: bool) {
        let now = self.now();
        if after_idle {
            self.last_progress.store(now, Ordering::Relaxed);
            self.from_sqlite.store(false, Ordering::Relaxed);
        }
        self.busy_since.store(now, Ordering::Release);
    }

    fn finish(&self) {
        let now = self.now();
        self.last_finished.store(now, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        self.busy_since.store(0, Ordering::Release);
    }

    pub(super) fn set_stall(&self, stall: Duration) {
        let ms = u64::try_from(stall.as_millis()).unwrap_or(u64::MAX);
        self.stall_override.store(ms, Ordering::Relaxed);
    }

    pub(super) fn stall(&self) -> Option<Duration> {
        match self.stall_override.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    pub(super) fn status(&self, queued: usize) -> WriterStatus {
        let busy_since = self.busy_since.load(Ordering::Acquire);
        let last_progress = self.last_progress.load(Ordering::Relaxed);
        let last_finished = self.last_finished.load(Ordering::Relaxed);
        let finished = self.finished.load(Ordering::Relaxed);
        let now = self.now();
        let unix_now = unix_ms();
        // A stamp as a Unix time: as long ago as it is on the monotonic clock.
        let unix = |stamp: u64| (stamp != 0).then(|| unix_now.saturating_sub(now.saturating_sub(stamp)));
        let ago = |stamp: u64| Duration::from_millis(now.saturating_sub(stamp));
        WriterStatus {
            busy_since: unix(busy_since),
            last_progress: unix(last_progress).unwrap_or(0),
            last_finished: unix(last_finished).unwrap_or(0),
            queued,
            finished,
            busy_for: (busy_since != 0).then(|| ago(busy_since)),
            since_progress: (last_progress != 0).then(|| ago(last_progress)),
            progress_from_sqlite: self.from_sqlite.load(Ordering::Relaxed),
        }
    }
}

/// The writer thread at one instant ([`super::Db::writer_status`]). Times are Unix
/// ms, 0 for never; the durations come from the monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterStatus {
    /// When the running job started; `None` while the writer is idle.
    pub busy_since: Option<u64>,
    /// When SQLite last executed on the writer's behalf (or a job found it idle).
    pub last_progress: u64,
    /// When the last job finished.
    pub last_finished: u64,
    /// Jobs waiting behind the running one.
    pub queued: usize,
    /// Jobs finished since the database was opened.
    pub finished: u64,
    /// How long the running job has run.
    pub busy_for: Option<Duration>,
    /// How long since the last progress stamp.
    pub since_progress: Option<Duration>,
    /// That stamp came from SQLite executing, not from a job finding the writer
    /// idle.
    pub progress_from_sqlite: bool,
}

impl WriterStatus {
    pub fn busy(&self) -> bool {
        self.busy_since.is_some()
    }
}

/// Another thread's CPU-time clock (`pthread_getcpuclockid`). Linux only; other
/// platforms report none, and `/readyz` then judges by the stamps alone.
#[derive(Debug, Clone, Copy)]
struct CpuClock(#[cfg(target_os = "linux")] libc::clockid_t);

impl CpuClock {
    /// The clock of the thread `handle` was spawned as.
    #[cfg(target_os = "linux")]
    fn of(handle: &std::thread::JoinHandle<()>) -> Option<Self> {
        use std::os::unix::thread::JoinHandleExt;
        let mut id: libc::clockid_t = 0;
        // SAFETY: the handle holds the thread, so its pthread_t is valid here, and
        // `id` is a valid out pointer for the call.
        let rc = unsafe { libc::pthread_getcpuclockid(handle.as_pthread_t(), &raw mut id) };
        (rc == 0).then_some(Self(id))
    }

    #[cfg(not(target_os = "linux"))]
    fn of(_handle: &std::thread::JoinHandle<()>) -> Option<Self> {
        None
    }

    #[cfg(target_os = "linux")]
    fn read(&self) -> Option<Duration> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid out pointer; a clock id whose thread has gone
        // makes the call fail, which is handled.
        let rc = unsafe { libc::clock_gettime(self.0, &raw mut ts) };
        let secs = u64::try_from(ts.tv_sec).ok()?;
        let nanos = u32::try_from(ts.tv_nsec).ok()?;
        (rc == 0).then(|| Duration::new(secs, nanos))
    }

    #[cfg(not(target_os = "linux"))]
    fn read(&self) -> Option<Duration> {
        None
    }
}

/// Remember the writer thread's CPU clock, from its spawn handle.
pub(super) fn register_thread(stamps: &Stamps, handle: &std::thread::JoinHandle<()>) {
    if let Some(clock) = CpuClock::of(handle) {
        let _ = stamps.cpu_clock.set(clock);
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Stamp progress from the writer connection: every [`PROGRESS_OPS`] VM
/// instructions, and at every commit.
pub(super) fn install_progress(conn: &Connection, stamps: &Arc<Stamps>) -> rusqlite::Result<()> {
    let on_ops = Arc::clone(stamps);
    conn.progress_handler(
        PROGRESS_OPS,
        Some(move || {
            on_ops.progress();
            false
        }),
    )?;
    let on_commit = Arc::clone(stamps);
    conn.commit_hook(Some(move || {
        on_commit.progress();
        false
    }))
}

pub(super) type WriteFn = Box<dyn FnOnce(&mut Connection) + Send>;

/// A queued write and when it was queued.
pub(super) struct WriteJob {
    pub(super) queued_at: Instant,
    pub(super) run: WriteFn,
}

pub(super) fn set_queue_gauge(queued: usize) {
    metrics::gauge!(crate::metrics::SQLITE_WRITE_QUEUE).set(queued as f64);
}

/// Run one job, stamping and timing it. A panic inside it is contained and still
/// clears `busy_since`. `after_idle` says the queue was empty when the previous
/// job finished; returns whether it is empty now.
pub(super) fn run_job(
    conn: &mut Connection,
    job: WriteJob,
    stamps: &Stamps,
    rx: &mpsc::Receiver<WriteJob>,
    after_idle: bool,
) -> bool {
    let started = Instant::now();
    metrics::histogram!(crate::metrics::SQLITE_WRITE_WAIT_SECONDS)
        .record(started.duration_since(job.queued_at).as_secs_f64());
    stamps.start(after_idle);
    // The job's oneshot sender is dropped on panic, which the caller sees as
    // WriterGone; the connection stays usable for the next job.
    let run = job.run;
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(conn))).is_err();
    stamps.finish();
    let took = started.elapsed();
    metrics::histogram!(crate::metrics::SQLITE_WRITE_SECONDS).record(took.as_secs_f64());
    let queued = rx.len();
    set_queue_gauge(queued);
    if panicked {
        tracing::error!("a SQLite write closure panicked");
    }
    if took > SLOW_WRITE_WARN {
        let duration_ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
        tracing::warn!(duration_ms, queued, "slow SQLite write");
    }
    queued == 0
}

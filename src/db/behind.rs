//! Detached writes (AVL-02, ADR-210): a write that stores what a request
//! fetched runs on its own task, so the response does not wait for the writer.
//!
//! The task still goes through [`Db::write`](super::Db::write), so it joins the
//! one writer's FIFO queue and keeps its order there. When the writer is busy
//! (a long job write) the task waits, not the caller; one that waits longer
//! than [`SLOW`] logs a warning. The tasks are bounded: past [`MAX_WRITES`]
//! writes or [`MAX_BYTES`] of bodies in flight, a new one is dropped and
//! counted (`sqlite_detached_writes_dropped_total`). Shutdown waits for the
//! rest ([`Db::flush_detached`](super::Db::flush_detached)).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

use crate::metrics::{SQLITE_DETACHED_WRITES, SQLITE_DETACHED_WRITES_DROPPED_TOTAL};

/// Detached writes in flight before a new one is dropped.
pub const MAX_WRITES: usize = 4096;
/// Body bytes in flight before a new write is dropped: what the waiting tasks
/// hold in memory. One write is always admitted, however large.
pub const MAX_BYTES: usize = 256 * 1024 * 1024;
/// A detached write that took this long, queue wait included, logs a warning.
pub const SLOW: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) struct Detached {
    state: Mutex<State>,
    /// Notified when the last write in flight finishes.
    idle: Notify,
    dropped: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct State {
    writes: usize,
    bytes: usize,
    max_writes: usize,
    max_bytes: usize,
}

impl Detached {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                writes: 0,
                bytes: 0,
                max_writes: MAX_WRITES,
                max_bytes: MAX_BYTES,
            }),
            idle: Notify::new(),
            dropped: AtomicU64::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Count a write in if the bound has room; `false` when it has none, and
    /// the caller then drops the write ([`Detached::count_dropped`]) or makes
    /// it some other way.
    pub(super) fn admit(&self, bytes: usize) -> bool {
        let mut s = self.state();
        let fits = s.writes == 0 || (s.writes < s.max_writes && s.bytes.saturating_add(bytes) <= s.max_bytes);
        if fits {
            s.writes += 1;
            s.bytes = s.bytes.saturating_add(bytes);
            set_gauge(s.writes);
        }
        fits
    }

    /// A write the bound had no room for was dropped.
    pub(super) fn count_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        metrics::counter!(SQLITE_DETACHED_WRITES_DROPPED_TOTAL).increment(1);
    }

    /// A write admitted with `bytes` has finished (or its task was dropped).
    pub(super) fn release(&self, bytes: usize) {
        let mut s = self.state();
        s.writes = s.writes.saturating_sub(1);
        s.bytes = s.bytes.saturating_sub(bytes);
        set_gauge(s.writes);
        if s.writes == 0 {
            self.idle.notify_waiters();
        }
    }

    pub(super) fn in_flight(&self) -> usize {
        self.state().writes
    }

    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(super) fn set_limits(&self, writes: usize, bytes: usize) {
        let mut s = self.state();
        s.max_writes = writes.max(1);
        s.max_bytes = bytes;
    }

    /// Wait until no detached write is in flight. Writes admitted while this
    /// waits are waited for too.
    pub(super) async fn settle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            // Registered before the check, so a release in between still wakes us.
            notified.as_mut().enable();
            if self.in_flight() == 0 {
                return;
            }
            notified.await;
        }
    }
}

fn set_gauge(writes: usize) {
    #[allow(clippy::cast_precision_loss)]
    metrics::gauge!(SQLITE_DETACHED_WRITES).set(writes as f64);
}

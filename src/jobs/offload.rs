//! `archive:offload` (design/12 §Offload job, OBJ-03): pack local bodies into
//! the bucket as soon as a full pack of a kind is ready, and free the local
//! copies of packed bodies once their match is older than
//! `ARCHIVE_BODY_LOCAL_DAYS`.
//!
//! It is queued when a pack is full ([`Pending`]: a running count of unpacked
//! bytes per kind, raised by every archived body), once at boot, and after
//! each daily `maintenance`. A run packs while a pack is due, matches first,
//! then frees in batches, then re-reads what is left. Registered only when
//! `ARCHIVE_OBJECT_URL` is set; unset is the rollback. It calls no Riot route,
//! so it has no lane; `JOB_KIND_LIMITS` runs one at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::json;

use crate::archive::objects::Objects;
use crate::archive::offload::{self, FREE_BATCH};
use crate::archive::pack::Kind;
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{Enqueued, Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};
use crate::metrics::{ARCHIVE_OFFLOAD_BYTES_TOTAL, ARCHIVE_OFFLOAD_PENDING, ARCHIVE_PACKS};

const DAY_MS: i64 = 86_400_000;
const KINDS: [Kind; 2] = [Kind::Match, Kind::Timeline];

fn slot(kind: Kind) -> usize {
    match kind {
        Kind::Match => 0,
        Kind::Timeline => 1,
    }
}

/// Queue the offload, at most one pending or running.
pub async fn enqueue(queue: &Queue) -> Result<Enqueued, DbError> {
    queue
        .enqueue(
            NewJob::new(kinds::ARCHIVE_OFFLOAD, priority::MAINTENANCE, json!({}))
                .dedupe(kinds::ARCHIVE_OFFLOAD),
        )
        .await
}

/// Unpacked bytes per kind, and the trigger that queues the offload when a
/// kind has a full pack. The archive raises it with every body it stores; a
/// run sets it to what it left unpacked. Lost on restart, which is why boot
/// queues a run too.
#[derive(Debug)]
pub struct Pending {
    bytes: [AtomicU64; 2],
    target: u64,
    /// A job was queued since the last run started; stops every later body
    /// from queueing again (the dedupe would absorb it, at a write each).
    queued: AtomicBool,
    queue: Queue,
}

impl Pending {
    pub fn new(queue: Queue, target_bytes: u64) -> Self {
        Self {
            bytes: [AtomicU64::new(0), AtomicU64::new(0)],
            target: target_bytes,
            queued: AtomicBool::new(false),
            queue,
        }
    }

    pub fn bytes(&self, kind: Kind) -> u64 {
        self.bytes[slot(kind)].load(Ordering::Relaxed)
    }

    /// A body of `stored` zstd bytes was archived. Queues the offload the
    /// first time a kind reaches a full pack.
    pub async fn add(&self, kind: Kind, stored: u64) {
        let total = self.bytes[slot(kind)].fetch_add(stored, Ordering::Relaxed) + stored;
        if total < self.target || self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        match enqueue(&self.queue).await {
            Ok(_) => tracing::debug!(%kind, total, "a full pack is ready; offload queued"),
            Err(e) => {
                self.queued.store(false, Ordering::Release);
                tracing::warn!(error = %e, "could not queue the offload");
            }
        }
    }

    fn set(&self, kind: Kind, bytes: u64) {
        self.bytes[slot(kind)].store(bytes, Ordering::Relaxed);
    }

    /// A run started: the next full pack may queue another.
    fn rearm(&self) {
        self.queued.store(false, Ordering::Release);
    }
}

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub packs: usize,
    pub packed_bodies: usize,
    pub uploaded_bytes: u64,
    pub freed: usize,
}

#[derive(Debug)]
pub struct OffloadContext {
    pub db: Db,
    pub objects: Objects,
    pub pending: Arc<Pending>,
    pub local_days: u32,
    pub target_bytes: u64,
    pub pace: Duration,
}

impl OffloadContext {
    /// One run as of `now_ms`. A store or database error fails it: nothing
    /// unverified is marked packed and nothing unpacked is freed, and the
    /// next run carries on (design/12).
    pub async fn run(&self, now_ms: i64) -> Result<Report, JobError> {
        let fail = |e: &dyn std::fmt::Display| JobError::Retry(e.to_string());
        self.pending.rearm();
        let cutoff = now_ms - i64::from(self.local_days) * DAY_MS;
        let mut report = Report::default();

        for kind in KINDS {
            while let Some(ids) = offload::next_pack(&self.db, kind, self.target_bytes, cutoff)
                .await
                .map_err(|e| fail(&e))?
            {
                let Some(written) = offload::write_pack(&self.db, &self.objects, kind, &ids, now_ms)
                    .await
                    .map_err(|e| fail(&e))?
                else {
                    break;
                };
                tracing::info!(%kind, key = %written.key, bodies = written.bodies, bytes = written.bytes, "pack written");
                metrics::counter!(ARCHIVE_OFFLOAD_BYTES_TOTAL, "kind" => kind.as_str())
                    .increment(written.bytes);
                report.packs += 1;
                report.packed_bodies += written.bodies;
                report.uploaded_bytes += written.bytes;
                tokio::time::sleep(self.pace).await;
            }
        }

        for kind in KINDS {
            loop {
                let ids = offload::next_free(&self.db, kind, cutoff, FREE_BATCH)
                    .await
                    .map_err(|e| fail(&e))?;
                if ids.is_empty() {
                    break;
                }
                let freed = offload::free(&self.db, kind, &ids, now_ms)
                    .await
                    .map_err(|e| fail(&e))?;
                report.freed += freed;
                if freed == 0 {
                    break;
                }
                tokio::time::sleep(self.pace).await;
            }
        }

        self.measure(cutoff).await.map_err(|e| fail(&e))?;
        tracing::info!(?report, "offload done");
        Ok(report)
    }

    /// Re-read what is left: the trigger's counters and the gauges.
    async fn measure(&self, cutoff_ms: i64) -> Result<(), crate::archive::matches::ArchiveError> {
        for kind in KINDS {
            let b = offload::backlog(&self.db, kind, cutoff_ms).await?;
            self.pending.set(kind, b.unpacked_bytes);
            #[allow(clippy::cast_precision_loss)]
            {
                metrics::gauge!(ARCHIVE_OFFLOAD_PENDING, "kind" => kind.as_str(), "step" => "pack")
                    .set(b.to_pack as f64);
                metrics::gauge!(ARCHIVE_OFFLOAD_PENDING, "kind" => kind.as_str(), "step" => "free")
                    .set(b.to_free as f64);
            }
        }
        let packs: i64 = self
            .db
            .read(|c| Ok::<_, DbError>(c.query_row("SELECT count(*) FROM body_packs", [], |r| r.get(0))?))
            .await?;
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!(ARCHIVE_PACKS).set(packs as f64);
        Ok(())
    }
}

pub struct OffloadHandler(pub Arc<OffloadContext>);

impl Handler for OffloadHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move { self.0.run(Clock::now().unix_ms).await.map(|_| ()) })
    }
}

#[cfg(test)]
mod tests;

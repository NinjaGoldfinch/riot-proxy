//! `archive:index-snapshot` (design/12 §Replica and backups, OBJ-04): once a
//! day, after `maintenance`, write `body_packs` and `body_locations` to the
//! bucket's `index/`, so a lost database and a lost backup never need every
//! pack re-read. Registered only when `ARCHIVE_OBJECT_URL` is set.

use futures_util::future::BoxFuture;
use serde_json::json;

use crate::archive::objects::Objects;
use crate::archive::recovery;
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{Enqueued, Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};

/// Queue a snapshot, at most one pending or running.
pub async fn enqueue(queue: &Queue) -> Result<Enqueued, DbError> {
    queue
        .enqueue(
            NewJob::new(kinds::ARCHIVE_INDEX_SNAPSHOT, priority::MAINTENANCE, json!({}))
                .dedupe(kinds::ARCHIVE_INDEX_SNAPSHOT),
        )
        .await
}

pub struct IndexSnapshotHandler {
    pub db: Db,
    pub objects: Objects,
}

impl Handler for IndexSnapshotHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            let s = recovery::write_snapshot(&self.db, &self.objects, Clock::now().unix_ms)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
            tracing::info!(
                date = %s.date,
                packs = s.packs,
                locations = s.locations,
                last_pack = s.last_pack.as_deref().unwrap_or(""),
                "index snapshot written"
            );
            Ok(())
        })
    }
}

//! `timelines:backfill` (TL-02, ADR-133): fetch the timelines of archived
//! ranked matches that have none, newest patch first and, within a patch,
//! newest game first, so the builds people look at fill in soonest.
//!
//! - One job per match-v5 region, in that region's lane, at
//!   [`priority::TIMELINE_BACKFILL`], below every other band: it gets a worker
//!   only when nothing else wants one, and its requests go through the bulk
//!   lane like any job's ([`FetchOptions::JOB`]), so the interactive share and
//!   `BULK_USAGE_CEILING` hold, and a 429's `Retry-After` freezes the scope
//!   and yields the job until then.
//! - Each turn fetches up to [`BATCH`] timelines, then gives its worker back
//!   and comes again. What is left is read from the archive every turn, so a
//!   restart resumes and a second run finds nothing to do.
//! - A timeline Riot answers 404 for (a game older than Riot keeps, say) is
//!   marked in `timeline_gaps` and not asked for again.
//! - A turn that stored timelines queues `builds:extract`, which derives
//!   their `match_builds` rows; the analytics recompute picks them up on its
//!   own schedule.
//! - `TIMELINE_BACKFILL_PATCHES` bounds it to the newest patches with ranked
//!   matches; 0, the default, turns it off.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::fetcher::{FetchOptions, Fetcher};
use crate::http::ErrorCode;
use crate::jobs::scheduler::{Handler, Job, JobError, NewJob, Queue, enqueue_on};
use crate::jobs::{activity, analytics, kinds, priority};
use crate::metrics::{TIMELINE_BACKFILL_FETCHED_TOTAL, TIMELINE_BACKFILL_NOT_FOUND_TOTAL};
use crate::riot::endpoints::Endpoint;
use crate::riot::routing::{Platform, Region};
use crate::routes::passthrough::request;

/// Timelines fetched per turn before the job gives its worker back.
pub const BATCH: usize = 25;

/// The queues whose matches feed set builds: ranked solo and flex.
pub const RANKED_QUEUES: [i64; 2] = [420, 440];

/// `timeline_gaps.reason` for a timeline Riot answered 404 for.
pub const NOT_FOUND: &str = "not_found";

/// What `timelines:backfill` needs.
#[derive(Clone)]
pub struct TimelinesContext {
    pub fetcher: Fetcher,
    pub queue: Queue,
    /// `TIMELINE_BACKFILL_PATCHES`: how many of the newest patches to cover; 0 is off.
    pub patches: u32,
}

/// One region's job, deduped per region.
pub fn job(region: Region) -> NewJob {
    NewJob::new(
        kinds::TIMELINES_BACKFILL,
        priority::TIMELINE_BACKFILL,
        json!({"region": region.as_str()}),
    )
    .dedupe(region.as_str())
}

/// Queue a job for every region that has none pending or running. Returns
/// how many were new. The hourly tick calls it only while the backfill is on,
/// and a job finds nothing to do while it is off.
pub async fn enqueue_all(queue: &Queue) -> Result<usize, DbError> {
    let now = Clock::now().unix_ms;
    let created = queue
        .db()
        .write(move |c| {
            let tx = c.transaction()?;
            let mut created = 0;
            for region in Region::ALL {
                created += usize::from(enqueue_on(&tx, &job(region), now)?.created);
            }
            tx.commit()?;
            Ok::<_, DbError>(created)
        })
        .await?;
    if created > 0 {
        queue.wake_all();
    }
    Ok(created)
}

/// SQL: ranked queue ids, for an `IN (…)`.
fn ranked_in() -> String {
    RANKED_QUEUES.map(|q| q.to_string()).join(",")
}

/// The newest `n` patches with ranked matches in the archive, newest first.
pub fn patches_on(c: &Connection, n: u32) -> rusqlite::Result<Vec<String>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = c.prepare_cached(&format!(
        "SELECT patch FROM (SELECT DISTINCT patch FROM matches
                             WHERE queue_id IN ({}) AND patch IS NOT NULL)
          ORDER BY {} LIMIT ?1",
        ranked_in(),
        crate::archive::analytics::PATCH_DESC,
    ))?;
    let rows = stmt.query_map([n], |r| r.get(0))?;
    rows.collect()
}

/// A match id's platform prefix, in SQL (`OC1` of `OC1_712417978`).
const PREFIX: &str = "substr(m.match_id, 1, instr(m.match_id, '_') - 1)";

/// The match id prefixes of a region's platforms (`OC1`, `PH2`, …).
fn prefixes(region: Region) -> Vec<String> {
    Platform::ALL
        .iter()
        .filter(|p| p.region() == region)
        .map(|p| p.as_str().to_uppercase())
        .collect()
}

/// Up to `limit` matches of `region` to fetch a timeline for, in order:
/// `patches` as given (newest first), then newest game first. A match with a
/// timeline or a mark in `timeline_gaps` is left out. `(match id, patch)`.
pub fn next_batch_on(
    c: &Connection,
    region: Region,
    patches: &[String],
    limit: usize,
) -> rusqlite::Result<Vec<(String, String)>> {
    let prefixes = prefixes(region);
    let marks = vec!["?"; prefixes.len()].join(",");
    // One patch at a time, each read by `matches_patch_queue`.
    let mut stmt = c.prepare_cached(&format!(
        "SELECT m.match_id FROM matches m
          WHERE m.patch = ? AND m.queue_id IN ({ranked}) AND {PREFIX} IN ({marks})
            AND NOT EXISTS (SELECT 1 FROM timelines t WHERE t.match_id = m.match_id)
            AND NOT EXISTS (SELECT 1 FROM timeline_gaps g WHERE g.match_id = m.match_id)
          ORDER BY m.game_end_ms DESC, m.match_id DESC LIMIT ?",
        ranked = ranked_in(),
    ))?;
    let mut out = Vec::new();
    for patch in patches {
        let left = limit.saturating_sub(out.len());
        if left == 0 {
            break;
        }
        let args = std::iter::once(patch.clone())
            .chain(prefixes.iter().cloned())
            .chain(std::iter::once(left.to_string()));
        let ids = stmt
            .query_map(params_from_iter(args), |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.extend(ids.into_iter().map(|id| (id, patch.clone())));
    }
    Ok(out)
}

/// Remember that Riot won't serve this match's timeline.
pub fn mark_gap_on(c: &Connection, match_id: &str, reason: &str, now_ms: i64) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO timeline_gaps (match_id, reason, marked_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (match_id) DO UPDATE SET reason = excluded.reason, marked_at = excluded.marked_at",
        rusqlite::params![match_id, reason, now_ms],
    )?;
    Ok(())
}

/// One patch's ranked matches and their timelines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatchProgress {
    /// `major.minor`; `None` on the totals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Ranked matches archived.
    pub matches: i64,
    /// Of those, with a timeline archived.
    pub with_timeline: i64,
    /// Without one, and marked: Riot answered 404.
    pub not_found: i64,
    /// Without one and not marked: what the backfill still has to fetch.
    pub left: i64,
}

/// One region's share of what is left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionProgress {
    /// match-v5 region: `americas`, `europe`, `asia` or `sea`.
    pub region: String,
    pub left: i64,
    /// The patch its job is working on: the newest with anything left.
    pub patch: Option<String>,
}

/// `GET /v1/admin/timelines/backfill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `TIMELINE_BACKFILL_PATCHES`; 0 means the backfill is off.
    pub configured_patches: u32,
    /// Per patch, newest first, over the patches asked about.
    pub patches: Vec<PatchProgress>,
    pub regions: Vec<RegionProgress>,
    pub totals: PatchProgress,
}

/// Progress over the newest `n` patches with ranked matches.
pub fn progress_on(c: &Connection, configured: u32, n: u32) -> rusqlite::Result<Progress> {
    let patches = patches_on(c, n)?;
    let mut by_patch: BTreeMap<String, PatchProgress> = BTreeMap::new();
    let mut by_region: BTreeMap<&'static str, (i64, Option<String>)> =
        Region::ALL.iter().map(|r| (r.as_str(), (0, None))).collect();
    let mut stmt = c.prepare_cached(&format!(
        "SELECT {PREFIX},
                COUNT(*),
                SUM(EXISTS (SELECT 1 FROM timelines t WHERE t.match_id = m.match_id)),
                SUM(NOT EXISTS (SELECT 1 FROM timelines t WHERE t.match_id = m.match_id)
                    AND EXISTS (SELECT 1 FROM timeline_gaps g WHERE g.match_id = m.match_id))
           FROM matches m WHERE m.patch = ?1 AND m.queue_id IN ({}) GROUP BY 1",
        ranked_in(),
    ))?;
    for patch in &patches {
        let p = by_patch.entry(patch.clone()).or_insert_with(|| PatchProgress {
            patch: Some(patch.clone()),
            ..PatchProgress::default()
        });
        let rows = stmt.query_map([patch], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (prefix, matches, with, gone) = row?;
            let left = matches - with - gone;
            p.matches += matches;
            p.with_timeline += with;
            p.not_found += gone;
            p.left += left;
            // `patches` is newest first, so the first patch with work is the region's current one.
            if let Some(region) = Platform::parse(&prefix).ok().map(Platform::region)
                && let Some((l, current)) = by_region.get_mut(region.as_str())
            {
                *l += left;
                if left > 0 && current.is_none() {
                    *current = Some(patch.clone());
                }
            }
        }
    }
    let patches: Vec<PatchProgress> = patches.iter().filter_map(|p| by_patch.remove(p)).collect();
    let totals = patches.iter().fold(PatchProgress::default(), |mut t, p| {
        t.matches += p.matches;
        t.with_timeline += p.with_timeline;
        t.not_found += p.not_found;
        t.left += p.left;
        t
    });
    let regions = by_region
        .into_iter()
        .map(|(region, (left, patch))| RegionProgress {
            region: region.to_string(),
            left,
            patch,
        })
        .collect();
    Ok(Progress {
        configured_patches: configured,
        patches,
        regions,
        totals,
    })
}

/// [`progress_on`] on a reader.
pub async fn progress(db: &Db, configured: u32, n: u32) -> Result<Progress, DbError> {
    db.read(move |c| Ok::<_, DbError>(progress_on(c, configured, n)?))
        .await
}

fn store(e: &impl std::fmt::Display) -> JobError {
    JobError::Retry(format!("store: {e}"))
}

impl TimelinesContext {
    fn db(&self) -> &Db {
        self.queue.db()
    }

    /// One turn for one region: up to [`BATCH`] timelines, then the worker
    /// goes back (`take_turns`) and the job comes again until nothing is left.
    pub async fn backfill(&self, job: &Job) -> Result<(), JobError> {
        let raw: serde_json::Value = job.payload()?;
        let region = raw
            .get("region")
            .and_then(serde_json::Value::as_str)
            .and_then(|r| Region::parse(r).ok())
            .ok_or_else(|| JobError::Fail(format!("not a region: {raw}")))?;
        if self.patches == 0 {
            tracing::info!(
                region = region.as_str(),
                "TIMELINE_BACKFILL_PATCHES is 0; nothing to backfill"
            );
            return Ok(());
        }
        let n = self.patches;
        let batch = self
            .db()
            .read(move |c| {
                let patches = patches_on(c, n)?;
                Ok::<_, DbError>(next_batch_on(c, region, &patches, BATCH)?)
            })
            .await
            .map_err(|e| store(&e))?;
        if batch.is_empty() {
            tracing::info!(region = region.as_str(), "timeline backfill: nothing left");
            return Ok(());
        }
        let target = Endpoint::by_id("match.timeline").and_then(|e| e.target_for_region(region));
        let (mut fetched, mut missing) = (0u64, 0u64);
        let mut outcome = Err(JobError::take_turns());
        for (k, (id, patch)) in batch.iter().enumerate() {
            activity::step(format!(
                "patch {patch}: the timeline of {id} ({} of {} this turn; {fetched} fetched, {missing} not found)",
                k + 1,
                batch.len()
            ));
            let req = match request("match.timeline", target, &[id], &[]) {
                Ok(r) => r,
                Err(e) => {
                    outcome = Err(JobError::Fail(format!(
                        "timeline request for {id}: {}",
                        e.message
                    )));
                    break;
                }
            };
            match self.fetcher.fetch(req, FetchOptions::JOB).await {
                Ok(_) => fetched += 1,
                // Gone from Riot: say so once, and never ask again.
                Err(e) if e.api.code == ErrorCode::NotFound && e.limited_until.is_none() => {
                    missing += 1;
                    let (id, now) = (id.clone(), Clock::now().unix_ms);
                    self.db()
                        .write(move |c| Ok::<_, DbError>(mark_gap_on(c, &id, NOT_FOUND, now)?))
                        .await
                        .map_err(|e| store(&e))?;
                }
                // No room (our limiter, or a 429's Retry-After): yield until there is.
                // Anything else: back off and try again.
                Err(e) => {
                    outcome = Err(JobError::from_fetch(&e));
                    break;
                }
            }
        }
        metrics::counter!(TIMELINE_BACKFILL_FETCHED_TOTAL, "region" => region.as_str()).increment(fetched);
        metrics::counter!(TIMELINE_BACKFILL_NOT_FOUND_TOTAL, "region" => region.as_str()).increment(missing);
        if fetched > 0 {
            // New timelines: derive their builds. Deduped, so a busy backfill
            // keeps one `builds:extract` queued, not one per turn.
            if let Err(e) = self.queue.enqueue(analytics::builds_job()).await {
                tracing::warn!(error = %e, "could not queue builds:extract after a timeline backfill turn");
            }
        }
        tracing::debug!(
            region = region.as_str(),
            fetched,
            missing,
            "timeline backfill turn"
        );
        outcome
    }
}

/// `timelines:backfill`.
pub struct TimelinesBackfillHandler(pub Arc<TimelinesContext>);

impl Handler for TimelinesBackfillHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.backfill(job))
    }
}

#[cfg(test)]
mod tests;

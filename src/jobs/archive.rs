//! `archive:match` and `backfill:player` (docs/design/06 §Job catalogue; v1
//! `archiveMatchJob`, `backfillPlayer`, `match-walk.ts`).
//!
//! - `archive:match` fetches a match at bulk priority. The fetcher archives it
//!   (with its facts) on the way through, and the handler announces it with
//!   `match.archived` only when it was new.
//! - `backfill:player` walks a player's match ids 100 at a time, up to its
//!   limit, and queues the unarchived ones. Its progress is recorded on the
//!   player's row (`players.backfill_state`) after every page, so a walk
//!   interrupted by a crash or a Riot error resumes from its last page.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::archive::matches;
use crate::clock::Clock;
use crate::events::{self, Event};
use crate::fetcher::{FetchError, FetchOptions, Fetcher, XCache};
use crate::http::ErrorCode;
use crate::jobs::activity;
use crate::jobs::scheduler::{Enqueued, Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};
use crate::metrics::BACKFILLS_QUEUED_TOTAL;
use crate::players;
use crate::riot::endpoints::Endpoint;
use crate::riot::routing::Platform;
use crate::routes::passthrough::request;
use crate::ws::Hub;

/// v1 `BACKFILL_PAGE`.
pub const BACKFILL_PAGE: i64 = 100;

/// What the archive handlers share.
#[derive(Clone)]
pub struct ArchiveContext {
    pub fetcher: Fetcher,
    pub queue: Queue,
    pub hub: Hub,
    pub key_scope: String,
    /// `ARCHIVE_TIMELINES`: the default for jobs that do not say.
    pub archive_timelines: bool,
    /// `LOOKUP_BACKFILL_LIMIT`: a walk at least this deep counts as complete (v1).
    pub lookup_backfill_limit: u32,
}

/// `archive:match` payload (v1 `ArchiveMatchJob`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveMatch {
    pub match_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub puuid: Option<String>,
    #[serde(default)]
    pub fetch_timeline: Option<bool>,
}

/// `backfill:player` payload (v1 `BackfillPlayerJob`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillPlayer {
    pub puuid: String,
    pub platform: String,
    /// How many ids deep to walk; v1's default is 500.
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_timeline: Option<bool>,
    /// Walk only one queue's ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<i64>,
    /// `lookup`, `track`, `catchup` or `admin` (v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn default_limit() -> u32 {
    500
}

/// `players.backfill_state` (design/04 `{done, cursor, limit}`, v1 #44's stamps).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillState {
    /// When the latest walk started (v1 `historyBackfillStartedAt`).
    pub started_at: i64,
    /// Set once a walk has accounted for the player's history (v1
    /// `historyBackfilledAt`). A set `doneAt` stops further lookup backfills.
    #[serde(default)]
    pub done_at: Option<i64>,
    /// How many ids deep the walk has read (v1 `historyBackfillDepth`).
    #[serde(default)]
    pub depth: i64,
    /// Where an interrupted walk resumes; `None` when no walk is in progress.
    #[serde(default)]
    pub cursor: Option<i64>,
    /// The limit of the walk the cursor belongs to.
    #[serde(default)]
    pub limit: u32,
}

impl BackfillState {
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        raw.and_then(|r| serde_json::from_str(r).ok())
    }
}

/// A backfill's archive jobs rank by depth in blocks of ten (v1
/// `backfillPriority`; design/06's `100 + depth / 10`).
pub fn depth_priority(depth: i64) -> i64 {
    priority::ARCHIVE_DEPTH + depth.max(0) / 10
}

/// TL-01 (ADR-118): queue the half of an archived match the archive lacks, as
/// an `archive:match` that asks for the timeline. Its match is fetched when
/// missing; its timeline when missing and `want_timeline` (`ARCHIVE_TIMELINES`).
/// A pending duplicate (a walk's job, say) is lifted to `priority` and made to
/// ask for the timeline too. Runs inside the caller's write; returns whether
/// anything was queued or lifted, so the caller knows to wake a worker.
pub fn queue_missing_half_on(
    conn: &rusqlite::Connection,
    match_id: &str,
    want_timeline: bool,
    priority: i64,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let has = |table: &str| {
        conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE match_id = ?1)"),
            [match_id],
            |r| r.get::<_, bool>(0),
        )
    };
    let missing = !has("matches")? || (want_timeline && !has("timelines")?);
    if !missing {
        return Ok(false);
    }
    let payload = ArchiveMatch {
        match_id: match_id.to_string(),
        puuid: None,
        fetch_timeline: Some(true),
    };
    let job = NewJob::new(
        kinds::ARCHIVE_MATCH,
        priority,
        serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
    )
    .dedupe(match_id);
    let out = crate::jobs::enqueue_or_promote_on(conn, &job, now_ms)?;
    if !out.created {
        conn.execute(
            "UPDATE jobs SET payload = json_set(payload, '$.fetchTimeline', json('true'))
              WHERE id = ?1 AND state = 'pending'",
            [&out.id],
        )?;
    }
    Ok(true)
}

/// TL-01's catch-up, run at boot: queue every archived match without its
/// timeline (when `want_timeline`) and every archived timeline without its
/// match, at `priority::ARCHIVE_DEPTH`. A match whose finished `archive:match`
/// already asked for its timeline is skipped, so a timeline Riot won't serve
/// is not asked for on every restart: a `done` row goes after seven days and
/// it is tried again then; a `failed` one stays. Returns how many were queued
/// or lifted.
pub async fn queue_missing_halves(queue: &Queue, want_timeline: bool) -> Result<usize, crate::db::DbError> {
    let now = Clock::now().unix_ms;
    let n = queue
        .db()
        .write(move |c| {
            let tx = c.transaction()?;
            let ids: Vec<String> = {
                let without_timeline = if want_timeline {
                    "SELECT match_id FROM matches m
                      WHERE NOT EXISTS (SELECT 1 FROM timelines t WHERE t.match_id = m.match_id)
                     UNION "
                } else {
                    ""
                };
                let mut s = tx.prepare(&format!(
                    "{without_timeline}
                     SELECT match_id FROM timelines t
                      WHERE NOT EXISTS (SELECT 1 FROM matches m WHERE m.match_id = t.match_id)
                     EXCEPT
                     SELECT dedupe_key FROM jobs
                      WHERE kind = '{kind}' AND state IN ('done', 'failed')
                        AND json_extract(payload, '$.fetchTimeline') = 1",
                    kind = kinds::ARCHIVE_MATCH,
                ))?;
                let rows = s.query_map([], |r| r.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            let mut n = 0;
            for id in &ids {
                n += usize::from(queue_missing_half_on(
                    &tx,
                    id,
                    want_timeline,
                    priority::ARCHIVE_DEPTH,
                    now,
                )?);
            }
            tx.commit()?;
            Ok::<_, crate::db::DbError>(n)
        })
        .await?;
    if n > 0 {
        queue.wake_all();
    }
    Ok(n)
}

/// Queue a history walk for a player, deduped per player while pending or
/// running (v1 `enqueueBackfill`), and count it by reason and outcome.
pub async fn enqueue_backfill(queue: &Queue, walk: &BackfillPlayer) -> Result<Enqueued, crate::db::DbError> {
    let payload = serde_json::to_value(walk).unwrap_or_else(|_| json!({}));
    let job = NewJob::new(kinds::BACKFILL_PLAYER, priority::BACKFILL, payload).dedupe(walk.puuid.clone());
    let out = queue.enqueue(job).await?;
    let reason = walk.reason.clone().unwrap_or_else(|| "admin".into());
    metrics::counter!(BACKFILLS_QUEUED_TOTAL, "reason" => reason, "status" => out.status()).increment(1);
    Ok(out)
}

/// A fetch failure: retried with backoff, or a yield when the limiter has no room.
fn retry(e: &FetchError) -> JobError {
    JobError::from_fetch(e)
}

fn store(e: &impl std::fmt::Display) -> JobError {
    JobError::Retry(format!("store: {e}"))
}

const BULK: FetchOptions = FetchOptions::JOB;

impl ArchiveContext {
    fn db(&self) -> &crate::db::Db {
        self.queue.db()
    }

    // ── archive:match ───────────────────────────────────────────────────────

    pub async fn archive_match(&self, job: &Job) -> Result<(), JobError> {
        let a: ArchiveMatch = job.payload()?;
        let region = Platform::from_match_id(&a.match_id)
            .map(Platform::region)
            .ok_or_else(|| JobError::Fail(format!("cannot derive region from match id '{}'", a.match_id)))?;
        let fetch = |id: &'static str| {
            let target = Endpoint::by_id(id).and_then(|e| e.target_for_region(region));
            request(id, target, &[&a.match_id], &[]).map_err(FetchError::from)
        };
        let req = fetch("match.byId").map_err(|e| retry(&e))?;
        activity::step(format!("fetching match {}", a.match_id));
        let got = match self.fetcher.fetch(req, BULK).await {
            Ok(r) => r,
            // A match id Riot does not know is not worth retrying.
            Err(e) if e.api.code == ErrorCode::NotFound => {
                return Err(JobError::Fail(format!("match {} not found", a.match_id)));
            }
            Err(e) => return Err(retry(&e)),
        };

        // v1 announced every run; v2 only a match that entered the archive
        // now. Announced before the timeline, so a run that yields on the
        // timeline (and finds the match archived when it comes back) does
        // not lose the event.
        if got.x_cache != XCache::Archive {
            announce(&self.hub, a.puuid.clone(), &a.match_id, &got.body);
        }

        if a.fetch_timeline.unwrap_or(self.archive_timelines) {
            activity::step(format!("fetching the timeline of {}", a.match_id));
            match fetch("match.timeline") {
                Ok(req) => match self.fetcher.fetch(req, BULK).await {
                    Ok(_) => {}
                    // No room for it now: come back for it, rather than
                    // archive the match without the timeline asked for.
                    Err(e) if e.limited_until.is_some() => return Err(retry(&e)),
                    // Timelines are large and optional: never fail the archive over one (v1).
                    Err(e) => {
                        tracing::warn!(match_id = %a.match_id, error = %e.api.message, "timeline fetch failed");
                    }
                },
                Err(e) => tracing::warn!(match_id = %a.match_id, error = %e.api.message, "timeline request"),
            }
        }
        Ok(())
    }

    // ── backfill:player ─────────────────────────────────────────────────────

    async fn save(&self, puuid: &str, state: &BackfillState) -> Result<(), JobError> {
        let (scope, puuid) = (self.key_scope.clone(), puuid.to_string());
        let json = serde_json::to_string(state).unwrap_or_default();
        self.db()
            .write(move |c| {
                c.execute(
                    "UPDATE players SET backfill_state = ?1 WHERE key_scope = ?2 AND puuid = ?3",
                    rusqlite::params![json, scope, puuid],
                )
                .map(|_| ())
                .map_err(crate::db::DbError::from)
            })
            .await
            .map_err(|e| store(&e))
    }

    pub async fn backfill_player(&self, job: &Job) -> Result<(), JobError> {
        let b: BackfillPlayer = job.payload()?;
        let platform = Platform::parse(&b.platform).map_err(|e| JobError::Fail(e.message))?;
        let limit = i64::from(b.limit);

        // Claim the walk before doing any of it (v1 #44): the row exists, and
        // a stamp without a completion reads as "tried, did not finish".
        let row = players::Upsert {
            puuid: &b.puuid,
            platform: platform.as_str(),
            ..players::Upsert::default()
        };
        let player = players::upsert(self.db(), &self.key_scope, row, Clock::now().unix_ms)
            .await
            .map_err(|e| store(&e))?;
        let previous = BackfillState::parse(player.backfill_state.as_deref());
        let mut state = match previous {
            // Resume a walk of the same limit that stopped part-way.
            Some(s) if s.cursor.is_some() && s.limit == b.limit => s,
            prev => BackfillState {
                started_at: Clock::now().unix_ms,
                done_at: prev.as_ref().and_then(|p| p.done_at),
                depth: prev.as_ref().map_or(0, |p| p.depth),
                cursor: Some(0),
                limit: b.limit,
            },
        };
        self.save(&b.puuid, &state).await?;

        let mut start = state.cursor.unwrap_or(0);
        let mut queued = 0usize;
        let mut ran_out = false;
        while start < limit {
            let count = BACKFILL_PAGE.min(limit - start);
            activity::step(format!("match ids {start}–{} of {limit}", start + count));
            let target =
                Endpoint::by_id("match.idsByPuuid").and_then(|e| e.target_for_region(platform.region()));
            let req = request(
                "match.idsByPuuid",
                target,
                &[&b.puuid],
                &[
                    ("start", Some(start.to_string())),
                    ("count", Some(count.to_string())),
                    ("queue", b.queue_id.map(|q| q.to_string())),
                ],
            )
            .map_err(|api| retry(&api.into()))?;
            // An error here is retried; the saved cursor makes the retry resume.
            let body = self.fetcher.fetch(req, BULK).await.map_err(|e| retry(&e))?.body;
            let ids: Vec<String> = serde_json::from_slice(&body).unwrap_or_default();
            if ids.is_empty() {
                ran_out = true;
                break;
            }
            let n = i64::try_from(ids.len()).unwrap_or(i64::MAX);
            let unarchived = matches::filter_unarchived(self.db(), &ids)
                .await
                .map_err(|e| store(&e))?;
            let position: std::collections::HashMap<&str, i64> = ids
                .iter()
                .enumerate()
                .map(|(i, id)| (id.as_str(), start + i64::try_from(i).unwrap_or(0)))
                .collect();
            let jobs: Vec<NewJob> = unarchived
                .iter()
                .map(|id| {
                    let payload = ArchiveMatch {
                        match_id: id.clone(),
                        puuid: Some(b.puuid.clone()),
                        fetch_timeline: b.fetch_timeline,
                    };
                    NewJob::new(
                        kinds::ARCHIVE_MATCH,
                        depth_priority(position.get(id.as_str()).copied().unwrap_or(start)),
                        serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
                    )
                    .dedupe(id.clone())
                })
                .collect();
            queued += self.queue.enqueue_all(jobs).await.map_err(|e| store(&e))?;

            start += n;
            state.depth = start;
            state.cursor = Some(start);
            self.save(&b.puuid, &state).await?;
            if n < count {
                ran_out = true; // the end of this player's history
                break;
            }
        }

        // v1 `walkIsComplete`: the history ran out (unfiltered), or the walk
        // was as deep as a lookup asks for. A shallow walk must not stop the
        // deep one a lookup would do.
        let complete = (ran_out && b.queue_id.is_none()) || b.limit >= self.lookup_backfill_limit;
        if complete {
            state.done_at = Some(Clock::now().unix_ms);
        }
        state.cursor = None;
        self.save(&b.puuid, &state).await?;
        tracing::info!(
            puuid = %b.puuid,
            queued,
            depth = state.depth,
            complete,
            reason = b.reason.as_deref().unwrap_or("admin"),
            "backfill finished"
        );
        Ok(())
    }
}

/// `match.archived` for a match that just entered the archive.
fn announce(hub: &Hub, puuid: Option<String>, match_id: &str, body: &[u8]) {
    #[derive(Deserialize)]
    struct Body {
        metadata: Option<Metadata>,
    }
    #[derive(Deserialize)]
    struct Metadata {
        #[serde(default)]
        participants: Vec<String>,
    }
    let participants = serde_json::from_slice::<Body>(body)
        .ok()
        .and_then(|b| b.metadata)
        .map(|m| m.participants)
        .unwrap_or_default();
    let patch = matches::extract(body).ok().map(|m| m.patch);
    events::publish(
        hub,
        &Event::MatchArchived {
            puuid,
            match_id: match_id.to_string(),
            patch,
            participants,
        },
    );
}

pub struct ArchiveMatchHandler(pub Arc<ArchiveContext>);

impl Handler for ArchiveMatchHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.archive_match(job))
    }
}

pub struct BackfillPlayerHandler(pub Arc<ArchiveContext>);

impl Handler for BackfillPlayerHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.backfill_player(job))
    }
}

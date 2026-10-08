//! The ladder crawl (design/06 §Job catalogue; v1 `jobs/ladder-crawl.ts`):
//! `ladder:crawl` creates the crawl row and fans out one `ladder:apex` per apex
//! tier and one `ladder:walk` per (tier, division) down to the floor. Each
//! records what it saw; whichever ends the stage's last leg moves the crawl on.
//!
//! The stages are what make a ten-participant match cost one `match.byId`: not
//! one id is collected until every page of the ladder is in, and not one match
//! is fetched until every id is (v1). `ladder:collect` walks 25 players' match
//! ids into the crawl's set; `ladder:archive` hands the set's unarchived ids to
//! `archive:match`. A finished crawl queues `names:backfill`.
//!
//! Crawl state lives in SQLite (`store`), where v1 used Redis for the legs and
//! cursors; see ADR-054.

pub mod store;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::events::{self, Event};
use crate::fetcher::{FetchError, FetchOptions, Fetcher};
use crate::http::{ApiError, ErrorCode};
use crate::jobs::kinds;
use crate::jobs::scheduler::{Handler, Job, JobError, MAX_ATTEMPTS, NewJob, Queue, enqueue_on};
use crate::riot::endpoints::Endpoint;
use crate::riot::ladder::{
    APEX_TIERS, DIVISIONS, PAGED_TIERS, RANKED_QUEUES, apex_endpoint, tiers_at_or_above,
};
use crate::riot::limiter::Priority;
use crate::riot::routing::Platform;
use crate::routes::passthrough::request;
use crate::ws::Hub;

use store::{Crawl, Ended, Entry, Stage};

/// Order inside the ladder band (v1 `LADDER_PRIORITY`): the fan-out first, the
/// apex leagues (three requests for the most valuable slice) before the paged
/// walks, and the later stages after enumeration, so a second ladder's
/// enumeration does not queue behind this one's match-id walk.
pub mod order {
    use crate::jobs::priority::BACKFILL;
    pub const CRAWL: i64 = BACKFILL;
    pub const APEX: i64 = BACKFILL + 1;
    pub const WALK: i64 = BACKFILL + 2;
    pub const COLLECT: i64 = BACKFILL + 3;
    pub const ARCHIVE: i64 = BACKFILL + 4;
    /// `archive:match` for a match a crawl found: after every lookup's
    /// depth-ranked archive jobs (v1: "a crawl yields to somebody looking up a
    /// player") and after the polls, which v1 ran on their own queue.
    pub const MATCH: i64 = BACKFILL + 5;
}

/// Pages a walk covers between checks that its crawl is still running (v1).
const CANCEL_CHECK_PAGES: u32 = 10;

pub struct LadderContext {
    pub fetcher: Fetcher,
    pub queue: Queue,
    pub hub: Hub,
    pub key_scope: String,
    /// `LADDER_TIER_FLOOR`, validated at boot.
    pub tier_floor: String,
    /// `LADDER_BACKFILL_LIMIT`: match ids collected per player; 0 ends a
    /// crawl at enumeration (v1).
    pub backfill_limit: u32,
    /// `LOOKUP_BACKFILL_LIMIT`: a collect walk at least this deep stamps the
    /// player's history done (v1 `walkIsComplete`).
    pub lookup_backfill_limit: u32,
    /// `ARCHIVE_TIMELINES`, for the archive jobs the crawl queues.
    pub archive_timelines: bool,
}

/// Players one `ladder:collect` job walks (v1 `COLLECT_BATCH`).
pub const COLLECT_BATCH: usize = 25;
/// Ids handed to the archive queue at a time (v1 `ARCHIVE_BATCH`).
const ARCHIVE_BATCH: usize = 100;
/// A match-id page (v1 `BACKFILL_PAGE`).
const ID_PAGE: u32 = 100;

/// `ladder:collect`'s payload (v1 `LadderCollectJob`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectJob {
    pub crawl_id: String,
    pub platform: String,
    pub queue: String,
    pub puuids: Vec<String>,
    /// Position of the batch in the candidate list; names the leg.
    pub offset: usize,
}

impl CollectJob {
    pub fn leg(&self) -> String {
        format!("{}:{}", kinds::LADDER_COLLECT, self.offset)
    }
}

/// `ladder:archive`'s payload (v1 `LadderArchiveJob`). One per crawl: the set
/// is only de-duplicated if one reader drains it (v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveLeg {
    pub crawl_id: String,
    pub platform: String,
    pub queue: String,
}

/// `ladder:crawl`'s payload and `POST /v1/admin/ladder/crawl`'s request (v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlRequest {
    pub platform: String,
    pub queue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_floor: Option<String>,
}

/// `ladder:apex` and `ladder:walk` payloads (v1). `division` is set for a walk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegJob {
    pub crawl_id: String,
    pub platform: String,
    pub queue: String,
    pub tier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub division: Option<String>,
}

impl LegJob {
    /// The leg's name within its crawl: `ladder:apex:MASTER`,
    /// `ladder:walk:DIAMOND:I` (v1 `ladderLegId`, minus the crawl id, which
    /// is the row's other key).
    pub fn leg(&self) -> String {
        match &self.division {
            Some(d) => format!("{}:{}:{d}", kinds::LADDER_WALK, self.tier),
            None => format!("{}:{}", kinds::LADDER_APEX, self.tier),
        }
    }
}

/// What starting a crawl did (v1 `StartCrawlResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub crawl_id: String,
    /// `false`: one was already running for this ladder; this is its id.
    pub created: bool,
    pub platform: String,
    pub queue: String,
    /// Jobs fanned out (0 when not created).
    pub legs: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Invalid(ApiError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// v1's `assertTier` / `assertRankedQueue` wording.
fn checked(
    req: &CrawlRequest,
    default_floor: &str,
) -> Result<(Platform, &'static str, &'static str), ApiError> {
    let platform = Platform::parse(&req.platform)?;
    let queue = RANKED_QUEUES
        .into_iter()
        .find(|q| *q == req.queue)
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::Validation,
                format!(
                    "Unknown ranked queue '{}'. Expected one of: {}",
                    req.queue,
                    RANKED_QUEUES.join(", ")
                ),
            )
        })?;
    let raw = req.tier_floor.as_deref().unwrap_or(default_floor);
    let upper = raw.to_ascii_uppercase();
    let floor = crate::riot::ladder::tiers()
        .find(|t| *t == upper)
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::Validation,
                format!(
                    "Unknown tier '{raw}'. Expected one of: {}",
                    crate::riot::ladder::tiers().collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
    Ok((platform, queue, floor))
}

/// How many enumerate legs a crawl down to `floor` fans out: one per apex
/// tier, one per (tier, division) below. The activity view's total (DEV-13).
pub fn enumerate_legs(floor: &str) -> usize {
    tiers_at_or_above(floor)
        .iter()
        .map(|t| {
            if APEX_TIERS.contains(t) {
                1
            } else if PAGED_TIERS.contains(t) {
                DIVISIONS.len()
            } else {
                0
            }
        })
        .sum()
}

/// Create the crawl and fan out its legs, all in one transaction: the row,
/// the legs and their jobs exist together or not at all. One live crawl per
/// ladder: a second start answers the running crawl's id (v1). Makes no
/// upstream call, which is what lets the admin route answer with a crawl id.
pub async fn start_crawl(
    queue: &Queue,
    key_scope: &str,
    default_floor: &str,
    req: &CrawlRequest,
) -> Result<Started, StartError> {
    let (platform, ladder, floor) = checked(req, default_floor).map_err(StartError::Invalid)?;
    let platform = platform.as_str().to_string();
    let scope = key_scope.to_string();
    let id = crate::jobs::scheduler::next_id();
    let now = Clock::now().unix_ms;
    let started = queue
        .db()
        .write(move |c| {
            let tx = c.transaction()?;
            let Some(crawl) = store::insert_crawl(&tx, &id, &scope, &platform, ladder, floor, now)? else {
                let running = store::running_on(&tx, &scope, &platform, ladder)?;
                return Ok::<_, DbError>(running.map(|r| Started {
                    crawl_id: r.id,
                    created: false,
                    platform,
                    queue: ladder.to_string(),
                    legs: 0,
                }));
            };
            let tiers = tiers_at_or_above(floor);
            let legs: Vec<LegJob> = tiers
                .iter()
                .filter(|t| APEX_TIERS.contains(t))
                .map(|t| (*t, None))
                .chain(
                    tiers
                        .iter()
                        .filter(|t| PAGED_TIERS.contains(t))
                        .flat_map(|t| DIVISIONS.map(|d| (*t, Some(d.to_string())))),
                )
                .map(|(tier, division)| LegJob {
                    crawl_id: crawl.id.clone(),
                    platform: platform.clone(),
                    queue: ladder.to_string(),
                    tier: tier.to_string(),
                    division,
                })
                .collect();
            store::add_legs(&tx, &crawl.id, &legs.iter().map(LegJob::leg).collect::<Vec<_>>())?;
            for leg in &legs {
                let (kind, rank) = if leg.division.is_some() {
                    (kinds::LADDER_WALK, order::WALK)
                } else {
                    (kinds::LADDER_APEX, order::APEX)
                };
                let job = NewJob::new(kind, rank, serde_json::to_value(leg).unwrap_or_default())
                    .dedupe(format!("{}:{}", crawl.id, leg.leg()));
                enqueue_on(&tx, &job, now)?;
            }
            tx.commit()?;
            Ok(Some(Started {
                crawl_id: crawl.id,
                created: true,
                platform,
                queue: ladder.to_string(),
                legs: legs.len(),
            }))
        })
        .await?;
    // The index refused the insert, so a crawl was running a moment ago; if it
    // is gone, it finished in between and the caller should try again (v1).
    let started = started.ok_or_else(|| {
        StartError::Invalid(ApiError::new(
            ErrorCode::Validation,
            "The running crawl of this ladder finished meanwhile; start again",
        ))
    })?;
    if started.created {
        queue.wake_all();
        tracing::info!(crawl = %started.crawl_id, platform = %started.platform, queue = %started.queue,
            tier_floor = floor, legs = started.legs, "ladder crawl started");
    } else {
        tracing::info!(crawl = %started.crawl_id, "ladder crawl already running; returning the live one");
    }
    Ok(started)
}

/// Queue a `ladder:crawl` job for one ladder (the tick), deduped per ladder.
pub async fn enqueue_crawl(queue: &Queue, platform: &str, ladder: &str) -> Result<bool, DbError> {
    let req = CrawlRequest {
        platform: platform.to_string(),
        queue: ladder.to_string(),
        tier_floor: None,
    };
    let job = NewJob::new(
        kinds::LADDER_CRAWL,
        order::CRAWL,
        serde_json::to_value(&req).unwrap_or_default(),
    )
    .dedupe(format!("{platform}:{ladder}"));
    Ok(queue.enqueue(job).await?.created)
}

fn retry(e: &FetchError) -> JobError {
    JobError::Retry(format!("{}: {}", e.api.code.as_str(), e.api.message))
}

fn store_err(e: &dyn std::fmt::Display) -> JobError {
    JobError::Retry(format!("store: {e}"))
}

/// league-v4's entry, narrowed to what the ladder stores (v1 `RiotLeagueEntry`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RiotEntry {
    puuid: Option<String>,
    rank: Option<String>,
    league_points: Option<i64>,
    wins: Option<i64>,
    losses: Option<i64>,
    veteran: Option<bool>,
    inactive: Option<bool>,
    fresh_blood: Option<bool>,
    hot_streak: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RiotLeagueList {
    #[serde(default)]
    entries: Vec<RiotEntry>,
}

/// Riot's entry as a row. The tier is the leg's: the apex lists put it on the
/// wrapper and leave it off every entry (v1). Apex entries report `I`.
fn to_entry(raw: RiotEntry, tier: &str) -> Option<Entry> {
    let division = raw.rank.unwrap_or_else(|| "I".into()).to_ascii_uppercase();
    Some(Entry {
        puuid: raw.puuid?,
        tier: tier.to_string(),
        division: if DIVISIONS.contains(&division.as_str()) {
            division
        } else {
            "I".into()
        },
        league_points: raw.league_points.unwrap_or(0),
        wins: raw.wins.unwrap_or(0),
        losses: raw.losses.unwrap_or(0),
        veteran: raw.veteran.unwrap_or(false),
        inactive: raw.inactive.unwrap_or(false),
        fresh_blood: raw.fresh_blood.unwrap_or(false),
        hot_streak: raw.hot_streak.unwrap_or(false),
    })
}

impl LadderContext {
    fn db(&self) -> &Db {
        self.queue.db()
    }

    async fn get(
        &self,
        id: &'static str,
        platform: Platform,
        params: &[&str],
        query: &[(&str, Option<String>)],
    ) -> Result<bytes::Bytes, FetchError> {
        let target = Endpoint::by_id(id).map(|e| e.target_for_platform(platform));
        let req = request(id, target, params, query).map_err(|api| FetchError { api, x_cache: None })?;
        let opts = FetchOptions {
            // The bulk budget (15 min) is the generous wait v1 gave ladder
            // pages: a crawl has nowhere else to be.
            priority: Priority::Bulk,
            bypass: false,
        };
        self.fetcher.fetch(req, opts).await.map(|r| r.body)
    }

    async fn running(&self, crawl_id: &str) -> Result<Option<Crawl>, JobError> {
        let crawl = store::get(self.db(), &self.key_scope, crawl_id)
            .await
            .map_err(|e| store_err(&e))?;
        Ok(crawl.filter(|c| c.status == "running"))
    }

    pub async fn crawl(&self, job: &Job) -> Result<(), JobError> {
        let req: CrawlRequest = job.payload()?;
        match start_crawl(&self.queue, &self.key_scope, &self.tier_floor, &req).await {
            Ok(_) => Ok(()),
            Err(StartError::Invalid(e)) => Err(JobError::Fail(e.message)),
            Err(StartError::Db(e)) => Err(store_err(&e)),
        }
    }

    /// An apex league arrives whole: one request, one write, one leg (v1).
    pub async fn apex(&self, job: &Job) -> Result<(), JobError> {
        let leg: LegJob = job.payload()?;
        let result = self.apex_leg(&leg).await;
        self.settle(job, &leg.crawl_id, &leg.leg(), result).await
    }

    async fn apex_leg(&self, leg: &LegJob) -> Result<(), JobError> {
        if self.running(&leg.crawl_id).await?.is_none() {
            return Ok(());
        }
        let platform = Platform::parse(&leg.platform).map_err(|e| JobError::Fail(e.message))?;
        let id = apex_endpoint(&leg.tier)
            .ok_or_else(|| JobError::Fail(format!("'{}' is not an apex tier", leg.tier)))?;
        let body = self
            .get(id, platform, &[&leg.queue], &[])
            .await
            .map_err(|e| retry(&e))?;
        let list: RiotLeagueList =
            serde_json::from_slice(&body).map_err(|e| JobError::Retry(format!("league list: {e}")))?;
        let entries: Vec<Entry> = list
            .entries
            .into_iter()
            .filter_map(|e| to_entry(e, &leg.tier))
            .collect();
        self.record(leg, &entries, None).await
    }

    /// One (tier, division), page by page until an empty one: a short page is
    /// not the end, the ladder churns under the walk (v1). Resumes on the page
    /// after the last one stored.
    pub async fn walk(&self, job: &Job) -> Result<(), JobError> {
        let leg: LegJob = job.payload()?;
        let result = self.walk_leg(&leg).await;
        self.settle(job, &leg.crawl_id, &leg.leg(), result).await
    }

    async fn walk_leg(&self, leg: &LegJob) -> Result<(), JobError> {
        let platform = Platform::parse(&leg.platform).map_err(|e| JobError::Fail(e.message))?;
        let division = leg
            .division
            .clone()
            .ok_or_else(|| JobError::Fail("a walk needs a division".into()))?;
        let name = leg.leg();
        // No row: this leg already ended (a re-run after a crash).
        let Some(cursor) = store::leg(self.db(), &leg.crawl_id, &name)
            .await
            .map_err(|e| store_err(&e))?
        else {
            return Ok(());
        };
        let mut page = cursor.unwrap_or(1).max(1);
        let mut walked = 0u32;
        loop {
            // A running job cannot be taken back, so a long walk asks (v1).
            if walked.is_multiple_of(CANCEL_CHECK_PAGES) && self.running(&leg.crawl_id).await?.is_none() {
                tracing::info!(crawl = %leg.crawl_id, leg = %name, page, "ladder walk stopping; crawl is not running");
                return Ok(());
            }
            let body = self
                .get(
                    "league.entriesByTier",
                    platform,
                    &[&leg.queue, &leg.tier, &division],
                    &[("page", Some(page.to_string()))],
                )
                .await
                .map_err(|e| retry(&e))?;
            let raw: Vec<RiotEntry> =
                serde_json::from_slice(&body).map_err(|e| JobError::Retry(format!("league page: {e}")))?;
            if raw.is_empty() {
                return Ok(());
            }
            let entries: Vec<Entry> = raw.into_iter().filter_map(|e| to_entry(e, &leg.tier)).collect();
            self.record(leg, &entries, Some((&name, page + 1))).await?;
            page += 1;
            walked += 1;
        }
    }

    /// Store a page and count it (v1 `countPage` + counters + `recordPlayers`).
    async fn record(
        &self,
        leg: &LegJob,
        entries: &[Entry],
        cursor: Option<(&str, i64)>,
    ) -> Result<(), JobError> {
        store::write_page(
            self.db(),
            store::Page {
                key_scope: &self.key_scope,
                crawl_id: &leg.crawl_id,
                platform: &leg.platform,
                queue: &leg.queue,
                entries,
                cursor,
                now: Clock::now().unix_ms,
            },
        )
        .await
        .map_err(|e| store_err(&e))?;
        let (platform, queue) = (leg.platform.clone(), leg.queue.clone());
        metrics::counter!(crate::metrics::LADDER_PAGES_TOTAL, "platform" => platform.clone(), "queue" => queue.clone())
            .increment(1);
        if !entries.is_empty() {
            metrics::counter!(crate::metrics::LADDER_ENTRIES_TOTAL, "platform" => platform, "queue" => queue)
                .increment(entries.len() as u64);
        }
        Ok(())
    }

    /// End the leg when the job is over: on success, and on a failure that
    /// will not be retried, as failed (v1 `isFinalAttempt`). A leg retried
    /// later stays outstanding.
    async fn settle(
        &self,
        job: &Job,
        crawl_id: &str,
        leg: &str,
        result: Result<(), JobError>,
    ) -> Result<(), JobError> {
        let failed = match &result {
            Ok(()) => false,
            Err(JobError::Retry(_)) if job.attempts < MAX_ATTEMPTS => return result,
            Err(_) => true,
        };
        if let Err(e) = self.end_leg(crawl_id, leg, failed).await {
            // The job is retried and ends the leg then; the row is still there.
            return Err(store_err(&e));
        }
        result
    }

    // ── collect ─────────────────────────────────────────────────────────────

    /// One batch of players, walked for match ids only, into the crawl's set.
    /// Nothing here fetches a match: that is the archive stage's, after every
    /// collect job has finished, which is what makes one match one
    /// `match.byId` however many of its players are on this ladder (v1).
    pub async fn collect(&self, job: &Job) -> Result<(), JobError> {
        let batch: CollectJob = job.payload()?;
        let result = self.collect_batch(&batch).await;
        self.settle(job, &batch.crawl_id, &batch.leg(), result).await
    }

    async fn collect_batch(&self, batch: &CollectJob) -> Result<(), JobError> {
        let platform = Platform::parse(&batch.platform).map_err(|e| JobError::Fail(e.message))?;
        let queue_id = crate::riot::ladder::queue_id(&batch.queue)
            .ok_or_else(|| JobError::Fail(format!("'{}' is not a ranked queue", batch.queue)))?;
        let mut new_ids = 0;
        for puuid in &batch.puuids {
            // Per player: a player is one or two requests (v1).
            if self.running(&batch.crawl_id).await?.is_none() {
                tracing::info!(crawl = %batch.crawl_id, "ladder collect stopping; crawl is not running");
                break;
            }
            let now = Clock::now().unix_ms;
            store::mark_walk_started(self.db(), &self.key_scope, puuid, now)
                .await
                .map_err(|e| store_err(&e))?;
            new_ids += self
                .collect_one(&batch.crawl_id, platform, queue_id, puuid)
                .await?;
        }
        if new_ids > 0 {
            let crawl = batch.crawl_id.clone();
            self.db()
                .write(move |c| store::bump(c, &crawl, store::Counter::MatchIdsSeen, new_ids))
                .await
                .map_err(|e| store_err(&e))?;
            metrics::counter!(crate::metrics::LADDER_MATCH_IDS_TOTAL,
                "platform" => batch.platform.clone(), "queue" => batch.queue.clone())
            .increment(u64::try_from(new_ids).unwrap_or(0));
        }
        Ok(())
    }

    /// One player's ranked ids for this ladder's queue, up to
    /// `LADDER_BACKFILL_LIMIT`, into the set; returns how many were new. A 404
    /// skips the player: a ladder of thousands holds accounts moved or deleted
    /// since the page that named them, and one must not cost the run (v1).
    async fn collect_one(
        &self,
        crawl_id: &str,
        platform: Platform,
        queue_id: u32,
        puuid: &str,
    ) -> Result<i64, JobError> {
        let limit = self.backfill_limit;
        let (mut start, mut new_ids) = (0u32, 0i64);
        while start < limit {
            let count = ID_PAGE.min(limit - start);
            let target =
                Endpoint::by_id("match.idsByPuuid").and_then(|e| e.target_for_region(platform.region()));
            let req = request(
                "match.idsByPuuid",
                target,
                &[puuid],
                &[
                    ("start", Some(start.to_string())),
                    ("count", Some(count.to_string())),
                    ("queue", Some(queue_id.to_string())),
                ],
            )
            .map_err(|api| retry(&FetchError { api, x_cache: None }))?;
            let opts = FetchOptions {
                priority: Priority::Bulk,
                bypass: false,
            };
            let ids: Vec<String> = match self.fetcher.fetch(req, opts).await {
                Ok(r) => serde_json::from_slice(&r.body).unwrap_or_default(),
                Err(e) if e.api.code == ErrorCode::NotFound => {
                    tracing::warn!(%puuid, "ladder collect skipping a player match-v5 does not know");
                    return Ok(new_ids);
                }
                Err(e) => return Err(retry(&e)),
            };
            let n = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            if n > 0 {
                new_ids += store::add_match_ids(self.db(), crawl_id, ids)
                    .await
                    .map_err(|e| store_err(&e))?;
            }
            start += n;
            if n < count {
                break;
            }
        }
        // v1 `walkIsComplete` for a queue-filtered walk: running out of ranked
        // ids says nothing of the rest, so only a lookup-deep walk counts.
        if limit >= self.lookup_backfill_limit {
            store::mark_walk_complete(
                self.db(),
                &self.key_scope,
                puuid,
                i64::from(start),
                Clock::now().unix_ms,
            )
            .await
            .map_err(|e| store_err(&e))?;
        }
        Ok(new_ids)
    }

    // ── archive ─────────────────────────────────────────────────────────────

    /// The crawl's de-duplicated ids, minus what the archive holds, onto the
    /// archive queue, a batch at a time. Each batch's jobs, its removal from
    /// the set and the counter commit together, so a crash re-reads at most a
    /// batch whose jobs are deduped anyway (v1 dropped after queueing).
    pub async fn archive(&self, job: &Job) -> Result<(), JobError> {
        let leg: ArchiveLeg = job.payload()?;
        let result = self.archive_set(&leg).await;
        self.settle(job, &leg.crawl_id, kinds::LADDER_ARCHIVE, result)
            .await
    }

    async fn archive_set(&self, leg: &ArchiveLeg) -> Result<(), JobError> {
        let (mut seen, mut queued) = (0usize, 0usize);
        loop {
            if self.running(&leg.crawl_id).await?.is_none() {
                tracing::info!(crawl = %leg.crawl_id, seen, "ladder archive stopping; crawl is not running");
                return Ok(());
            }
            let batch = store::peek_match_ids(self.db(), &leg.crawl_id, ARCHIVE_BATCH)
                .await
                .map_err(|e| store_err(&e))?;
            if batch.is_empty() {
                break;
            }
            let unarchived = crate::archive::matches::filter_unarchived(self.db(), &batch)
                .await
                .map_err(|e| store_err(&e))?;
            let (n, batch_len) = (unarchived.len(), batch.len());
            let (crawl, timelines, now) =
                (leg.crawl_id.clone(), self.archive_timelines, Clock::now().unix_ms);
            self.db()
                .write(move |c| {
                    let tx = c.transaction()?;
                    for id in &unarchived {
                        let payload = crate::jobs::archive::ArchiveMatch {
                            match_id: id.clone(),
                            puuid: None,
                            fetch_timeline: Some(timelines),
                        };
                        let job = NewJob::new(
                            kinds::ARCHIVE_MATCH,
                            order::MATCH,
                            serde_json::to_value(payload).unwrap_or_default(),
                        )
                        .dedupe(id.clone());
                        enqueue_on(&tx, &job, now)?;
                    }
                    store::drop_match_ids(&tx, &crawl, &batch)?;
                    store::bump(
                        &tx,
                        &crawl,
                        store::Counter::MatchesQueued,
                        i64::try_from(unarchived.len()).unwrap_or(0),
                    )?;
                    tx.commit()?;
                    Ok::<_, DbError>(())
                })
                .await
                .map_err(|e| store_err(&e))?;
            if n > 0 {
                self.queue.wake_all();
                metrics::counter!(crate::metrics::LADDER_MATCHES_QUEUED_TOTAL,
                    "platform" => leg.platform.clone(), "queue" => leg.queue.clone())
                .increment(n as u64);
            }
            seen += batch_len;
            queued += n;
        }
        tracing::info!(crawl = %leg.crawl_id, seen, queued, "ladder matches handed to the archive queue");
        Ok(())
    }

    /// End a leg and act on what that did to the crawl.
    pub async fn end_leg(&self, crawl_id: &str, leg: &str, failed: bool) -> Result<Ended, DbError> {
        let (crawl, leg) = (crawl_id.to_string(), leg.to_string());
        let backfill_limit = self.backfill_limit;
        let scope = self.key_scope.clone();
        let now = Clock::now().unix_ms;
        let ended = self
            .db()
            .write(move |c| {
                let tx = c.transaction()?;
                let ended = store::end_leg(&tx, &crawl, &leg, failed, now, &mut |tx, crawl| {
                    advance(tx, &scope, crawl, backfill_limit, now)
                })?;
                if let Ended::Finished(crawl) = &ended {
                    // Completed or failed alike: a name read out of a match
                    // that did land is correct either way (v1).
                    enqueue_on(&tx, &crate::jobs::names::job(), now)?;
                    // Only a clean run: aggregating part of a ladder as the
                    // whole is worse than keeping the previous numbers (v1).
                    if crawl.status == "completed" {
                        let job = crate::jobs::analytics::aggregate_job(&crawl.platform, &crawl.queue);
                        enqueue_on(&tx, &job, now)?;
                    }
                }
                tx.commit()?;
                Ok::<_, DbError>(ended)
            })
            .await?;
        if !matches!(ended, Ended::Nothing) {
            self.queue.wake_all();
        }
        self.announce(&ended);
        Ok(ended)
    }

    /// Events and metrics for a committed transition.
    fn announce(&self, ended: &Ended) {
        match ended {
            Ended::Nothing => {}
            Ended::Phase(crawl) => {
                tracing::info!(crawl = %crawl.id, phase = %crawl.phase, "ladder crawl moved on");
                events::publish(&self.hub, &phase_event(crawl));
            }
            Ended::Finished(crawl) => {
                let duration_s = (crawl.finished_at.unwrap_or(crawl.started_at) - crawl.started_at) / 1000;
                tracing::info!(crawl = %crawl.id, status = %crawl.status, entries = crawl.counters.entries_seen,
                    pages = crawl.counters.pages_fetched, "ladder crawl finished");
                #[allow(clippy::cast_precision_loss)]
                metrics::histogram!(crate::metrics::LADDER_CRAWL_DURATION_SECONDS,
                    "platform" => crawl.platform.clone(), "queue" => crawl.queue.clone(), "status" => crawl.status.clone())
                .record(duration_s as f64);
                events::publish(&self.hub, &phase_event(crawl));
                // Only a clean run is announced (v1).
                if crawl.status == "completed" {
                    events::publish(
                        &self.hub,
                        &Event::LadderCrawlCompleted {
                            crawl_id: crawl.id.clone(),
                            platform: crawl.platform.clone(),
                            queue: crawl.queue.clone(),
                            entries: crawl.counters.entries_seen,
                            players: crawl.counters.players_discovered,
                            duration_s,
                        },
                    );
                }
            }
        }
    }
}

/// Where a stage's end leads. Enumeration leads to collection, unless
/// `LADDER_BACKFILL_LIMIT=0` walks nobody: then the crawl is done (v1).
fn next_stage(crawl: &Crawl, backfill_limit: u32) -> Stage {
    match crawl.phase.as_str() {
        "enumerate" if backfill_limit == 0 => Stage::Complete,
        "enumerate" => Stage::Phase("collect"),
        "collect" => Stage::Phase("archive"),
        _ => Stage::Complete,
    }
}

/// Move the crawl into its next stage on `tx`, queueing that stage's jobs in
/// the same transaction that ended the last one, so the hand-over cannot be
/// lost to a crash. A collect stage with nobody to walk (everyone was walked
/// since the crawl started) goes straight on to archive (v1).
fn advance(
    tx: &rusqlite::Transaction<'_>,
    key_scope: &str,
    crawl: &Crawl,
    backfill_limit: u32,
    now: i64,
) -> Result<Stage, DbError> {
    let stage = next_stage(crawl, backfill_limit);
    match (crawl.phase.as_str(), &stage) {
        ("enumerate", Stage::Phase(_)) => {
            let players = store::collect_candidates(tx, key_scope, crawl)?;
            store::bump(
                tx,
                &crawl.id,
                store::Counter::BackfillsEnqueued,
                i64::try_from(players.len()).unwrap_or(0),
            )?;
            if players.is_empty() {
                queue_archive(tx, crawl, now)?;
                return Ok(Stage::Phase("archive"));
            }
            let jobs: Vec<CollectJob> = players
                .chunks(COLLECT_BATCH)
                .enumerate()
                .map(|(i, batch)| CollectJob {
                    crawl_id: crawl.id.clone(),
                    platform: crawl.platform.clone(),
                    queue: crawl.queue.clone(),
                    puuids: batch.to_vec(),
                    offset: i * COLLECT_BATCH,
                })
                .collect();
            store::add_legs(
                tx,
                &crawl.id,
                &jobs.iter().map(CollectJob::leg).collect::<Vec<_>>(),
            )?;
            for job in &jobs {
                let new = NewJob::new(
                    kinds::LADDER_COLLECT,
                    order::COLLECT,
                    serde_json::to_value(job).unwrap_or_default(),
                )
                .dedupe(format!("{}:{}", crawl.id, job.leg()));
                enqueue_on(tx, &new, now)?;
            }
            tracing::info!(crawl = %crawl.id, players = players.len(), jobs = jobs.len(), "ladder crawl collecting match ids");
        }
        ("collect", Stage::Phase(_)) => queue_archive(tx, crawl, now)?,
        _ => {}
    }
    Ok(stage)
}

/// The archive stage's one leg and job.
fn queue_archive(tx: &rusqlite::Transaction<'_>, crawl: &Crawl, now: i64) -> Result<(), DbError> {
    let leg = kinds::LADDER_ARCHIVE.to_string();
    store::add_legs(tx, &crawl.id, std::slice::from_ref(&leg))?;
    let payload = ArchiveLeg {
        crawl_id: crawl.id.clone(),
        platform: crawl.platform.clone(),
        queue: crawl.queue.clone(),
    };
    let job = NewJob::new(
        kinds::LADDER_ARCHIVE,
        order::ARCHIVE,
        serde_json::to_value(&payload).unwrap_or_default(),
    )
    .dedupe(format!("{}:{leg}", crawl.id));
    enqueue_on(tx, &job, now)?;
    Ok(())
}

/// `crawl.phase` (design/06): the stage the crawl is now in, or how it ended,
/// with its counters.
pub fn phase_event(crawl: &Crawl) -> Event {
    let phase = if crawl.status == "running" {
        crawl.phase.clone()
    } else {
        crawl.status.clone()
    };
    Event::CrawlPhase {
        crawl_id: crawl.id.clone(),
        platform: crawl.platform.clone(),
        queue: crawl.queue.clone(),
        phase,
        stats: json!(crawl.counters),
    }
}

pub struct LadderCrawlHandler(pub Arc<LadderContext>);
pub struct LadderApexHandler(pub Arc<LadderContext>);
pub struct LadderWalkHandler(pub Arc<LadderContext>);
pub struct LadderCollectHandler(pub Arc<LadderContext>);
pub struct LadderArchiveHandler(pub Arc<LadderContext>);

impl Handler for LadderCollectHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.collect(job))
    }
}

impl Handler for LadderArchiveHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.archive(job))
    }
}

impl Handler for LadderCrawlHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.crawl(job))
    }
}

impl Handler for LadderApexHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.apex(job))
    }
}

impl Handler for LadderWalkHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.walk(job))
    }
}

#[cfg(test)]
mod tests;

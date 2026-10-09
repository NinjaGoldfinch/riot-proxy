//! `ranks:lookup` (DEV-29, ADR-110): look up the ranks of the platform's
//! archived players that analytics count under `UNKNOWN`, so their games land
//! in a tier. A completed crawl queues one per (platform, queue).
//!
//! The first run plans: up to `RANK_LOOKUP_LIMIT` players the ladder does not
//! hold for this queue and nobody looked up within `RANK_LOOKUP_RECHECK_S`,
//! most archived games first (`rank_lookup_queue`). Each lookup is one
//! `league.entriesByPuuid` through the fetcher, whose `Archive::observe`
//! records the rank (ADR-105). The job takes turns every [`TURN`] lookups and
//! resumes from the list, so a crash or a yield repeats nothing. An empty list
//! queues `aggregate:analytics`, which counts the players under their tiers.

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{LadderContext, order, retry, store_err};
use crate::archive::ranks::{self, Ladder};
use crate::clock::Clock;
use crate::http::ErrorCode;
use crate::jobs::activity;
use crate::jobs::kinds;
use crate::jobs::scheduler::{Handler, Job, JobError, NewJob};
use crate::riot::routing::Platform;

/// Lookups per turn before the job gives its worker back (SCH-01).
pub const TURN: usize = 25;

/// How long a finished list waits for a recompute already running, which may
/// have read the ranks before the last lookups landed, to end.
const RECOMPUTE_WAIT_MS: i64 = 5_000;

/// `ranks:lookup`'s payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankLookup {
    pub platform: String,
    pub queue: String,
    /// Set once the run has a list to work through: a turn after the first
    /// carries on with it instead of planning again.
    #[serde(default)]
    pub planned: bool,
}

/// The job a completed crawl queues, one per ladder.
pub fn job(platform: &str, queue: &str) -> NewJob {
    let payload = RankLookup {
        platform: platform.to_string(),
        queue: queue.to_string(),
        planned: false,
    };
    NewJob::new(
        kinds::RANKS_LOOKUP,
        order::RANKS,
        serde_json::to_value(payload).unwrap_or_default(),
    )
    .dedupe(format!("{platform}:{queue}"))
}

impl LadderContext {
    pub async fn lookup_ranks(&self, job: &Job) -> Result<(), JobError> {
        let mut run: RankLookup = job.payload()?;
        if self.rank_lookup_limit == 0 {
            return Ok(());
        }
        let platform = Platform::parse(&run.platform).map_err(|e| JobError::Fail(e.message))?;
        let queue_id = crate::riot::ladder::queue_id(&run.queue)
            .ok_or_else(|| JobError::Fail(format!("'{}' is not a ranked queue", run.queue)))?;
        let ladder = Ladder {
            key_scope: self.key_scope.clone(),
            platform: platform.as_str().to_string(),
            queue: run.queue.clone(),
            queue_id: i64::from(queue_id),
        };
        if !run.planned {
            // A list left by a run that failed is carried on before planning.
            let mut waiting = ranks::waiting(self.db(), &ladder)
                .await
                .map_err(|e| store_err(&e))?;
            if waiting == 0 {
                let since = Clock::now().unix_ms - i64::from(self.rank_lookup_recheck_s) * 1000;
                let planned = ranks::plan(self.db(), &ladder, self.rank_lookup_limit, since)
                    .await
                    .map_err(|e| store_err(&e))?;
                tracing::info!(platform = %ladder.platform, queue = %ladder.queue, planned, "rank lookups planned");
                waiting = i64::try_from(planned).unwrap_or(i64::MAX);
            }
            if waiting == 0 {
                activity::step("every archived player is placed or was looked up recently");
                return Ok(());
            }
            run.planned = true;
        }
        let turn = |run: &RankLookup, retry_at: i64| JobError::Yield {
            retry_at,
            payload: serde_json::to_string(run).ok(),
        };
        for done in 0..TURN {
            let Some((puuid, games)) = ranks::take(self.db(), &ladder).await.map_err(|e| store_err(&e))?
            else {
                return self.ranks_done(&ladder, &run).await;
            };
            activity::step(format!(
                "rank {} of {TURN} this turn on {}",
                done + 1,
                ladder.platform
            ));
            match self.get("league.entriesByPuuid", platform, &[&puuid], &[]).await {
                // `Archive::observe` recorded the rank and stamped the lookup.
                Ok(_) => {}
                Err(e) if e.api.code == ErrorCode::NotFound => {
                    ranks::stamp(
                        self.db(),
                        &self.key_scope,
                        &ladder.platform,
                        &puuid,
                        Clock::now().unix_ms,
                    )
                    .await
                    .map_err(|e| store_err(&e))?;
                }
                Err(e) => {
                    return Err(match retry(&e) {
                        // Back on the list: the limiter, not the player, said no.
                        JobError::Yield { retry_at, .. } => {
                            ranks::put_back(self.db(), &ladder, &puuid, games)
                                .await
                                .map_err(|e| store_err(&e))?;
                            turn(&run, retry_at)
                        }
                        other => other,
                    });
                }
            }
            metrics::counter!(crate::metrics::RANK_LOOKUPS_TOTAL,
                "platform" => ladder.platform.clone(), "queue" => ladder.queue.clone())
            .increment(1);
        }
        Err(turn(&run, Clock::now().unix_ms))
    }

    /// The list is done: count the players under their tiers now rather than
    /// at the next crawl. A pending recompute will see every lookup; a running
    /// one may not, and blocks queueing another, so the job waits it out.
    async fn ranks_done(&self, ladder: &Ladder, run: &RankLookup) -> Result<(), JobError> {
        let job = crate::jobs::analytics::aggregate_job(&ladder.platform, &ladder.queue);
        let queued = self.queue.enqueue(job).await.map_err(|e| store_err(&e))?;
        if !queued.created {
            let id = queued.id;
            let running = self
                .db()
                .read(move |c| {
                    Ok::<_, crate::db::DbError>(c.query_row(
                        "SELECT state = 'running' FROM jobs WHERE id = ?1",
                        [id],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .map_err(|e| store_err(&e))?;
            if running {
                activity::step("waiting for the running recompute to end");
                return Err(JobError::Yield {
                    retry_at: Clock::now().unix_ms + RECOMPUTE_WAIT_MS,
                    payload: serde_json::to_string(run).ok(),
                });
            }
        }
        tracing::info!(platform = %ladder.platform, queue = %ladder.queue, "rank lookups done; analytics queued");
        Ok(())
    }
}

pub struct RanksLookupHandler(pub Arc<LadderContext>);

impl Handler for RanksLookupHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.lookup_ranks(job))
    }
}

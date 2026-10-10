//! `aggregate:analytics` and `facts:reextract` (design/06 §Job catalogue; v1
//! `jobs/analytics.ts`).
//!
//! `aggregate:analytics` rebuilds one ladder's analytics tables from the
//! facts: champions (slices, stats, bans), then matchups, then builds, each
//! step its own transaction (v1), and announces `analytics.updated`.
//! `facts:reextract` re-derives the facts of every match below the current
//! `FACTS_VERSION`, a batch at a time, from the stored bodies; no Riot call.
//! `builds:extract` derives `match_builds` from the archived timelines the
//! same way (BLD-01), and `tiers:backfill` stamps the tiers of matches
//! archived before V0016 (THR-02); `aggregate:analytics` runs both before its
//! rebuild.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::archive::analytics::{self, Scope};
use crate::archive::builds::{self, BUILDS_VERSION, BuildFact, ItemCatalog};
use crate::archive::facts::FACTS_VERSION;
use crate::archive::{matches, tiers};
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::events::{self, Event};
use crate::jobs::activity;
use crate::jobs::scheduler::{Enqueued, Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};
use crate::r#static::Mirror;
use crate::ws::Hub;

/// v1's pause between re-extraction batches, so a sweep of a large archive
/// leaves the writer free for everything else.
const REEXTRACT_PACE: Duration = Duration::from_millis(50);

/// Timelines per `builds:extract` batch. A timeline is about 0.8 MB once
/// decompressed, so a batch holds about 20 MB where a facts batch of 500
/// match bodies holds about 40 MB.
const BUILDS_BATCH: i64 = 25;

/// Matches per `tiers:backfill` batch: ten participant rows each.
const TIERS_BATCH: i64 = 500;

/// `aggregate:analytics`'s payload (v1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ladder {
    pub platform: String,
    pub queue: String,
}

/// One rebuild per ladder at a time: a queued one already reads what a second
/// would (v1 deduplicated per ladder).
pub fn aggregate_job(platform: &str, queue: &str) -> NewJob {
    let ladder = Ladder {
        platform: platform.to_string(),
        queue: queue.to_string(),
    };
    NewJob::new(
        kinds::AGGREGATE_ANALYTICS,
        priority::MAINTENANCE,
        serde_json::to_value(ladder).unwrap_or_default(),
    )
    .dedupe(format!("{platform}:{queue}"))
}

pub async fn enqueue_aggregate(queue: &Queue, platform: &str, ladder: &str) -> Result<Enqueued, DbError> {
    queue.enqueue(aggregate_job(platform, ladder)).await
}

/// A recompute asked for by hand (`POST /v1/admin/analytics/recompute`,
/// DEV-18): it goes ahead of every queued job, so the next free worker
/// rebuilds from the matches archived so far instead of after a crawl's
/// downloads. No running job is interrupted. A rebuild already queued for
/// the ladder is moved up rather than queued twice. It makes no Riot call.
pub async fn enqueue_aggregate_now(queue: &Queue, platform: &str, ladder: &str) -> Result<Enqueued, DbError> {
    let mut job = aggregate_job(platform, ladder);
    job.priority = priority::INTERACTIVE;
    queue.enqueue_or_promote(job).await
}

/// One sweep at a time (v1).
pub fn reextract_job() -> NewJob {
    NewJob::new(kinds::FACTS_REEXTRACT, priority::MAINTENANCE, json!({})).dedupe(kinds::FACTS_REEXTRACT)
}

/// One `builds:extract` at a time (BLD-01).
pub fn builds_job() -> NewJob {
    NewJob::new(kinds::BUILDS_EXTRACT, priority::MAINTENANCE, json!({})).dedupe(kinds::BUILDS_EXTRACT)
}

/// One `tiers:backfill` at a time (THR-02).
pub fn tiers_job() -> NewJob {
    NewJob::new(kinds::TIERS_BACKFILL, priority::MAINTENANCE, json!({})).dedupe(kinds::TIERS_BACKFILL)
}

/// Queue `tiers:backfill` when a ranked match has facts and no tier stamps
/// (at boot, after V0016). Returns whether one was queued.
pub async fn backfill_tiers_if_needed(queue: &Queue) -> Result<bool, DbError> {
    if !queue
        .db()
        .read(|c| Ok::<_, DbError>(tiers::any_unstamped(c)?))
        .await?
    {
        return Ok(false);
    }
    Ok(queue.enqueue(tiers_job()).await?.created)
}

/// Queue a rebuild of each of the key scope's ladders whose analytics rows
/// V0017 couldn't split by tier and side (at boot, ADR-129). Returns how many
/// were queued.
pub async fn split_if_needed(queue: &Queue, key_scope: &str) -> Result<usize, DbError> {
    let key_scope = key_scope.to_string();
    let ladders = queue
        .db()
        .read(move |c| analytics::unsplit_ladders(c, &key_scope))
        .await?;
    let mut queued = 0;
    for (platform, ladder) in ladders {
        if enqueue_aggregate(queue, &platform, &ladder).await?.created {
            queued += 1;
        }
    }
    Ok(queued)
}

/// Matches whose facts an older `FACTS_VERSION` derived.
pub async fn stale_matches(db: &Db) -> Result<i64, DbError> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM matches WHERE facts_version IS NULL OR facts_version < ?1",
            [FACTS_VERSION],
            |r| r.get(0),
        )?)
    })
    .await
}

/// Queue a sweep when the archive holds stale facts (at boot, after a
/// version bump). Returns whether one was queued.
pub async fn reextract_if_stale(queue: &Queue) -> Result<bool, DbError> {
    if stale_matches(queue.db()).await? == 0 {
        return Ok(false);
    }
    Ok(queue.enqueue(reextract_job()).await?.created)
}

/// One ladder's last recompute (v1 `AnalyticsRunSummary`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub at: i64,
    pub status: String,
    pub ms: i64,
    pub steps: BTreeMap<String, f64>,
    pub rows: BTreeMap<String, i64>,
    pub games: i64,
}

/// A ladder's last run, keyed by ladder: the next run replaces it.
pub async fn record_run(db: &Db, key_scope: &str, ladder: &Ladder, run: &Run) -> Result<(), DbError> {
    let (scope, l, run) = (key_scope.to_string(), ladder.clone(), run.clone());
    db.write(move |c| {
        c.execute(
            "INSERT OR REPLACE INTO analytics_runs (key_scope, platform, queue, at, status, ms, steps, rows, games)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                scope,
                l.platform,
                l.queue,
                run.at,
                run.status,
                run.ms,
                serde_json::to_string(&run.steps).unwrap_or_else(|_| "{}".into()),
                serde_json::to_string(&run.rows).unwrap_or_else(|_| "{}".into()),
                run.games
            ],
        )?;
        Ok(())
    })
    .await
}

/// Every ladder's last run, newest first (v1 `listAnalyticsRuns`).
pub async fn runs(db: &Db, key_scope: &str) -> Result<Vec<(Ladder, Run)>, DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        let mut stmt = c.prepare(
            "SELECT platform, queue, at, status, ms, steps, rows, games FROM analytics_runs
              WHERE key_scope = ?1 ORDER BY at DESC",
        )?;
        let rows = stmt
            .query_map([scope], |r| {
                let steps: String = r.get(5)?;
                let rows: String = r.get(6)?;
                Ok((
                    Ladder {
                        platform: r.get(0)?,
                        queue: r.get(1)?,
                    },
                    Run {
                        at: r.get(2)?,
                        status: r.get(3)?,
                        ms: r.get(4)?,
                        steps: serde_json::from_str(&steps).unwrap_or_default(),
                        rows: serde_json::from_str(&rows).unwrap_or_default(),
                        games: r.get(7)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

pub struct AnalyticsContext {
    pub queue: Queue,
    pub hub: Hub,
    pub key_scope: String,
    /// `AGGREGATE_PATCH_LIMIT`: patches rebuilt, newest first; 0 is all.
    pub patch_limit: u32,
    /// `FACTS_REEXTRACT_BATCH`.
    pub reextract_batch: u32,
    /// Where `builds:extract` reads the newest mirrored `item.json`.
    pub mirror: Arc<Mirror>,
}

fn store(e: &dyn std::fmt::Display) -> JobError {
    JobError::Retry(format!("store: {e}"))
}

type Step = fn(&mut rusqlite::Connection, &Scope) -> Result<analytics::Written, DbError>;

impl AnalyticsContext {
    fn db(&self) -> &Db {
        self.queue.db()
    }

    pub async fn aggregate(&self, job: &Job) -> Result<(), JobError> {
        let ladder: Ladder = job.payload()?;
        crate::riot::routing::Platform::parse(&ladder.platform).map_err(|e| JobError::Fail(e.message))?;
        let queue_id = crate::riot::ladder::queue_id(&ladder.queue)
            .ok_or_else(|| JobError::Fail(format!("'{}' is not a ranked queue", ladder.queue)))?;
        let started = Instant::now();
        // A crawl can grow the archive tenfold between rebuilds; fresh
        // statistics keep the rebuild's joins on their keys (ADR-102).
        if let Err(e) = self.db().optimize().await {
            tracing::warn!(error = %e, "could not refresh the planner statistics");
        }
        // Set builds are read from `match_builds`, so timelines archived since
        // the last run are derived first. A failure leaves the rows as they
        // were and doesn't hold up the rest of the rebuild.
        activity::step("extracting builds".to_string());
        if let Err(e) = self.extract_builds().await {
            tracing::warn!(error = ?e, "could not extract builds");
        }
        // Facts without a tier stamp don't count, so matches archived before
        // the stamps existed are stamped first (THR-02). Once they are, this
        // reads nothing.
        activity::step("stamping tiers".to_string());
        if let Err(e) = self.backfill_tiers().await {
            tracing::warn!(error = ?e, "could not stamp match tiers");
        }
        let mut steps = BTreeMap::new();
        let result = self.rebuild(&ladder, i64::from(queue_id), &mut steps).await;
        let status = if result.is_ok() { "completed" } else { "failed" };
        metrics::counter!(crate::metrics::AGGREGATE_RUNS_TOTAL,
            "platform" => ladder.platform.clone(), "queue" => ladder.queue.clone(), "status" => status)
        .increment(1);
        let ms = started.elapsed().as_millis();
        let games: i64 = match &result {
            Ok(_) => self.games(&ladder).await.unwrap_or(0),
            Err(_) => 0,
        };
        // The dashboard's record of the run, failed ones with the steps that
        // finished (v1 `recordAnalyticsRun`).
        let run = Run {
            at: Clock::now().unix_ms,
            status: status.into(),
            ms: i64::try_from(ms).unwrap_or(i64::MAX),
            steps,
            rows: result.as_ref().cloned().unwrap_or_default(),
            games,
        };
        if let Err(e) = record_run(self.db(), &self.key_scope, &ladder, &run).await {
            tracing::warn!(error = %e, "could not record the analytics run");
        }
        let tables = result?;
        tracing::info!(platform = %ladder.platform, queue = %ladder.queue, ?tables, games, ms, "analytics recomputed");
        events::publish(
            &self.hub,
            &Event::AnalyticsUpdated {
                platform: ladder.platform,
                queue: ladder.queue,
                duration_s: i64::try_from((ms + 500) / 1000).unwrap_or(i64::MAX),
                tables,
            },
        );
        Ok(())
    }

    /// The three steps, in v1's order, each timed; rows written by table.
    async fn rebuild(
        &self,
        ladder: &Ladder,
        queue_id: i64,
        steps_done: &mut BTreeMap<String, f64>,
    ) -> Result<BTreeMap<String, i64>, JobError> {
        let steps: [(&str, Step); 3] = [
            ("champions", analytics::rebuild_champions),
            ("matchups", analytics::rebuild_matchups),
            ("builds", analytics::rebuild_builds),
        ];
        // A ladder with rows V0017 couldn't split by tier and side is rebuilt
        // over every patch once, which replaces them all (ADR-129).
        let (key_scope, l) = (self.key_scope.clone(), ladder.clone());
        let full = self
            .db()
            .read(move |c| analytics::unsplit(c, &key_scope, &l.platform, &l.queue))
            .await
            .map_err(|e| store(&e))?;
        let mut tables = BTreeMap::new();
        for (name, step) in steps {
            activity::step(format!(
                "rebuilding {name} for {} {}",
                ladder.platform, ladder.queue
            ));
            let started = Instant::now();
            let (key_scope, l, limit) = (self.key_scope.clone(), ladder.clone(), self.patch_limit);
            let written = self
                .db()
                .write(move |c| {
                    let scope = Scope {
                        key_scope,
                        platform: l.platform,
                        queue: l.queue,
                        queue_id,
                        patches: if full {
                            None
                        } else {
                            analytics::recent_patches(c, queue_id, limit)?
                        },
                        now: Clock::now().unix_ms,
                    };
                    step(c, &scope)
                })
                .await
                .map_err(|e| store(&e))?;
            let secs = started.elapsed().as_secs_f64();
            metrics::histogram!(crate::metrics::AGGREGATE_DURATION_SECONDS,
                "platform" => ladder.platform.clone(), "queue" => ladder.queue.clone(), "step" => name)
            .record(secs);
            steps_done.insert(name.to_string(), secs);
            for (table, n) in written {
                #[allow(clippy::cast_precision_loss)]
                metrics::gauge!(crate::metrics::AGGREGATE_ROWS,
                    "platform" => ladder.platform.clone(), "queue" => ladder.queue.clone(), "table" => table)
                .set(n as f64);
                tables.insert(table.to_string(), n);
            }
        }
        if full {
            let (key_scope, l) = (self.key_scope.clone(), ladder.clone());
            self.db()
                .write(move |c| analytics::mark_split(c, &key_scope, &l.platform, &l.queue))
                .await
                .map_err(|e| store(&e))?;
        }
        Ok(tables)
    }

    /// Games in the ladder's stats, for the log line (v1 `games`).
    async fn games(&self, ladder: &Ladder) -> Result<i64, DbError> {
        let (scope, l) = (self.key_scope.clone(), ladder.clone());
        self.db()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT coalesce(sum(games), 0) FROM champion_stats WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3",
                    params![scope, l.platform, l.queue],
                    |r| r.get(0),
                )?)
            })
            .await
    }

    /// Re-derive every stale match's rows from its body, a batch at a time.
    /// Resumable without a cursor: a match it has done carries the current
    /// version and is not selected again.
    pub async fn reextract(&self, _job: &Job) -> Result<(), JobError> {
        let total = stale_matches(self.db()).await.map_err(|e| store(&e))?;
        let batch = i64::from(self.reextract_batch.max(1));
        let mut done = 0i64;
        loop {
            let ids: Vec<String> = self
                .db()
                .read(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT match_id FROM matches WHERE facts_version IS NULL OR facts_version < ?1
                          ORDER BY match_id LIMIT ?2",
                    )?;
                    let rows = stmt
                        .query_map(params![FACTS_VERSION, batch], |r| r.get(0))?
                        .collect::<Result<Vec<String>, _>>()?;
                    Ok::<_, DbError>(rows)
                })
                .await
                .map_err(|e| store(&e))?;
            if ids.is_empty() {
                break;
            }
            let bodies = matches::get_many(self.db(), &ids).await.map_err(|e| store(&e))?;
            let derived: Vec<(String, Option<matches::Derived>)> = tokio::task::spawn_blocking(move || {
                ids.into_iter()
                    .map(|id| {
                        let d = bodies.get(&id).and_then(|b| matches::derive(b).ok());
                        (id, d)
                    })
                    .collect()
            })
            .await
            .map_err(|e| store(&e))?;
            let n = i64::try_from(derived.len()).unwrap_or(0);
            let scope = self.key_scope.clone();
            self.db()
                .write(move |c| {
                    let tx = c.transaction()?;
                    for (id, d) in &derived {
                        match d {
                            Some(d) => {
                                // The facts stay with the key scope whose PUUIDs the body holds.
                                let owner: Option<String> =
                                    rusqlite::OptionalExtension::optional(tx.query_row(
                                        "SELECT key_scope FROM match_facts WHERE match_id = ?1 LIMIT 1",
                                        [id],
                                        |r| r.get(0),
                                    ))?;
                                matches::write_derived(&tx, id, owner.as_deref().unwrap_or(&scope), d)?;
                            }
                            // A body that no longer derives: stamp it so the sweep moves on.
                            None => {
                                tx.execute(
                                    "UPDATE matches SET facts_version = ?2 WHERE match_id = ?1",
                                    params![id, FACTS_VERSION],
                                )?;
                            }
                        }
                    }
                    tx.commit()?;
                    Ok::<_, DbError>(())
                })
                .await
                .map_err(|e| store(&e))?;
            done += n;
            #[allow(clippy::cast_precision_loss)]
            metrics::gauge!(crate::metrics::FACTS_REEXTRACT_PROGRESS)
                .set((done as f64 / total.max(1) as f64).min(1.0));
            tokio::time::sleep(REEXTRACT_PACE).await;
        }
        metrics::gauge!(crate::metrics::FACTS_REEXTRACT_PROGRESS).set(1.0);
        tracing::info!(matches = done, "facts re-extracted");
        Ok(())
    }

    /// `builds:extract` (BLD-01): derive `match_builds` for every archived
    /// timeline whose match has facts and no rows at the current
    /// `BUILDS_VERSION`, a batch at a time, with the newest mirrored
    /// `item.json`. Without a mirror it logs and does nothing. Returns the
    /// matches it read.
    ///
    /// It walks the timelines in id order: a timeline that gives no rows (one
    /// that no longer parses) isn't selected again in this sweep, and is
    /// tried again by the next one.
    pub async fn extract_builds(&self) -> Result<i64, JobError> {
        let Some(bytes) = self.mirror.read("item", None).await else {
            tracing::info!("no item.json mirrored yet; builds not extracted");
            return Ok(0);
        };
        let catalog = match ItemCatalog::from_item_json(&bytes) {
            Ok(c) if !c.is_empty() => Arc::new(c),
            Ok(_) => {
                tracing::warn!("the mirrored item.json lists no items; builds not extracted");
                return Ok(0);
            }
            Err(e) => {
                tracing::warn!(error = %e, "the mirrored item.json doesn't parse; builds not extracted");
                return Ok(0);
            }
        };
        let mut after = String::new();
        let mut done = 0i64;
        loop {
            let from = after.clone();
            // (match id, key scope, puuid → championId from its facts)
            let todo: Vec<(String, String, HashMap<String, i64>)> = self
                .db()
                .read(move |c| {
                    // The rows stay with the key scope whose PUUIDs the match holds.
                    let mut stmt = c.prepare(
                        "SELECT t.match_id,
                                (SELECT f.key_scope FROM match_facts f WHERE f.match_id = t.match_id LIMIT 1)
                           FROM timelines t
                          WHERE t.match_id > ?1
                            AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = t.match_id)
                            AND NOT EXISTS (SELECT 1 FROM match_builds b
                                             WHERE b.match_id = t.match_id AND b.builds_version = ?2)
                          ORDER BY t.match_id LIMIT ?3",
                    )?;
                    let rows = stmt
                        .query_map(params![from, BUILDS_VERSION, BUILDS_BATCH], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    // BLD-05 needs each player's champion, which the timeline doesn't name.
                    let mut champs =
                        c.prepare_cached("SELECT puuid, champion_id FROM match_facts WHERE match_id = ?1")?;
                    let mut out = Vec::with_capacity(rows.len());
                    for (id, scope) in rows {
                        let champions = champs
                            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?
                            .collect::<Result<HashMap<String, i64>, _>>()?;
                        out.push((id, scope, champions));
                    }
                    Ok::<_, DbError>(out)
                })
                .await
                .map_err(|e| store(&e))?;
            let Some((last, _, _)) = todo.last() else {
                break;
            };
            after.clone_from(last);
            let ids: Vec<String> = todo.iter().map(|(id, _, _)| id.clone()).collect();
            let bodies = matches::get_timelines(self.db(), &ids)
                .await
                .map_err(|e| store(&e))?;
            let cat = Arc::clone(&catalog);
            let derived: Vec<(String, String, Vec<BuildFact>)> = tokio::task::spawn_blocking(move || {
                todo.into_iter()
                    .map(|(id, scope, champions)| {
                        let rows = bodies
                            .get(&id)
                            .map(|b| builds::extract(b, &cat, &champions))
                            .unwrap_or_default();
                        (id, scope, rows)
                    })
                    .collect()
            })
            .await
            .map_err(|e| store(&e))?;
            done += i64::try_from(derived.len()).unwrap_or(0);
            self.db()
                .write(move |c| {
                    let tx = c.transaction()?;
                    for (id, scope, rows) in &derived {
                        builds::write(&tx, id, scope, rows)?;
                    }
                    tx.commit()?;
                    Ok::<_, DbError>(())
                })
                .await
                .map_err(|e| store(&e))?;
            tokio::time::sleep(REEXTRACT_PACE).await;
        }
        tracing::info!(matches = done, "builds extracted");
        Ok(done)
    }
}

impl AnalyticsContext {
    /// `tiers:backfill` (THR-02, ADR-127): stamp every ranked match that has
    /// facts and no tier stamps, a batch at a time, from the ladder and league
    /// lookups as they are now. That is the tier every rebuild before V0016
    /// used, so the next rebuild's output is unchanged. Walks in id order, so
    /// a match that stamps nothing isn't read twice in one sweep. Returns the
    /// matches it stamped.
    pub async fn backfill_tiers(&self) -> Result<i64, JobError> {
        let mut after = String::new();
        let mut done = 0i64;
        loop {
            let from = after.clone();
            // Found on a reader, so the writer isn't held while a mostly
            // stamped archive is walked; a match archived meanwhile was
            // stamped by its archive and is skipped by `INSERT OR IGNORE`.
            let todo = self
                .db()
                .read(move |c| Ok::<_, DbError>(tiers::unstamped(c, &from, TIERS_BATCH)?))
                .await
                .map_err(|e| store(&e))?;
            let n = todo.last().map(|(id, _)| (id.clone(), todo.len()));
            self.db()
                .write(move |c| {
                    let now = Clock::now().unix_ms;
                    let tx = c.transaction()?;
                    for (id, queue_id) in &todo {
                        tiers::stamp(&tx, id, *queue_id, now)?;
                    }
                    tx.commit()?;
                    Ok::<_, DbError>(())
                })
                .await
                .map_err(|e| store(&e))?;
            let Some((last, n)) = n else {
                break;
            };
            after = last;
            done += i64::try_from(n).unwrap_or(0);
            tokio::time::sleep(REEXTRACT_PACE).await;
        }
        if done > 0 {
            tracing::info!(matches = done, "match tiers stamped");
        }
        Ok(done)
    }
}

pub struct AggregateHandler(pub Arc<AnalyticsContext>);
pub struct ReextractHandler(pub Arc<AnalyticsContext>);
pub struct BuildsHandler(pub Arc<AnalyticsContext>);
pub struct TiersHandler(pub Arc<AnalyticsContext>);

impl Handler for TiersHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async { self.0.backfill_tiers().await.map(|_| ()) })
    }
}

impl Handler for AggregateHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.aggregate(job))
    }
}

impl Handler for ReextractHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.reextract(job))
    }
}

impl Handler for BuildsHandler {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async { self.0.extract_builds().await.map(|_| ()) })
    }
}

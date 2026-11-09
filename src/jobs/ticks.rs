//! Ticks (docs/design/06 §Scheduler): repeating timers that only *enqueue*.
//! A tick remembers its last run without a table of its own (ADR-214): at
//! boot each loop reads when its kind last ran (the `jobs` table's `done`
//! rows, or for the per-ladder kinds the crawl and analytics run logs) and
//! fires first at that time plus its period, at once when that is past or
//! nothing says. So a restart inside the day queues no `maintenance`,
//! `names:backfill` or `ddragon:sync`, and a tick missed while the process was
//! down still fires on the next boot. The polls and `aggregate:delta` fire at
//! once on every boot: they are cheap and deduped, and liveness resumes
//! straight away. A tick missed because the process stalled is skipped rather
//! than burst.
//!
//! The poll ticks fan out one job per tracked player (v1 `fanOut`), deduped by
//! PUUID, so a slow poll is never queued twice. Adding or removing a tracked
//! player needs no scheduler change (v1). The kinds queued once per ladder
//! (`ladder:crawl`, `aggregate:analytics`) or once per region
//! (`timelines:backfill`) run one loop per ladder or region, each on its own
//! clock.

use std::future::Future;
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};

use crate::clock::Clock;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::jobs::scheduler::{NewJob, Scheduler, enqueue_on};
use crate::jobs::{kinds, priority};
use crate::players;
use crate::riot::routing::Region;

/// Run `action` after `first`, then every `period` until `stop` flips. The
/// clock starts when `every` is called, not when the future is first polled.
pub fn every<F, Fut>(
    period: Duration,
    first: Duration,
    mut stop: watch::Receiver<bool>,
    mut action: F,
) -> impl Future<Output = ()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let start = Instant::now() + first;
    async move {
        let mut interval = tokio::time::interval_at(start, period.max(Duration::from_millis(1)));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            // A tick's enqueue can wait behind a long job write; a stop drops it
            // there rather than hold shutdown until that write ends (AVL-04).
            tokio::select! {
                _ = interval.tick() => tokio::select! {
                    () = action() => {}
                    _ = stop.changed() => return,
                },
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
        }
    }
}

/// Queue one `kind` job per tracked player, in one write. Returns how many were
/// new (already-queued players are skipped by the dedupe).
pub async fn fan_out(scheduler: &Scheduler, key_scope: &str, kind: &'static str) -> Result<usize, DbError> {
    let tracked = players::list_tracked(scheduler.db(), key_scope).await?;
    if tracked.is_empty() {
        return Ok(0);
    }
    let now = Clock::now().unix_ms;
    let created = scheduler
        .db()
        .write(move |c| {
            let tx = c.transaction()?;
            let mut created = 0;
            for p in &tracked {
                let job = NewJob::new(
                    kind,
                    priority::POLL,
                    json!({"puuid": p.puuid, "platform": p.platform}),
                )
                .dedupe(p.puuid.clone());
                created += usize::from(enqueue_on(&tx, &job, now)?.created);
            }
            tx.commit()?;
            Ok::<_, DbError>(created)
        })
        .await?;
    if created > 0 {
        scheduler.wake_all();
    }
    Ok(created)
}

/// Queue a job that exists once at a time (`ddragon:sync`, `maintenance`).
pub async fn singleton(scheduler: &Scheduler, kind: &'static str, priority: i64) -> Result<bool, DbError> {
    let job = NewJob::new(kind, priority, json!({})).dedupe(kind);
    Ok(scheduler.enqueue(job).await?.created)
}

/// How often the timeline backfill is queued again for every region (TL-02).
pub const TIMELINE_BACKFILL_EVERY: Duration = Duration::from_secs(3_600);

/// Each tick and its period (config names are v1's).
pub fn schedule(config: &Config) -> Vec<(&'static str, Duration)> {
    let s = |n: u32| Duration::from_secs(u64::from(n));
    vec![
        (kinds::POLL_LIVE, s(config.track_poll_live_s)),
        (kinds::POLL_RANK, s(config.track_poll_rank_s)),
        (kinds::POLL_MATCHES, s(config.track_poll_match_s)),
        (kinds::DDRAGON_SYNC, s(config.ddragon_sync_s)),
        // v1: daily.
        (kinds::MAINTENANCE, Duration::from_secs(86_400)),
        // v1: daily, for the players every other route names nothing for; a
        // crawl also queues one when it ends.
        (kinds::NAMES_BACKFILL, Duration::from_secs(86_400)),
    ]
    .into_iter()
    // v1: off unless LADDER_CRAWL_S is set; a crawl's cost is opt-in.
    .chain((config.ladder_crawl_s > 0).then(|| (kinds::LADDER_CRAWL, s(config.ladder_crawl_s))))
    // v1: off unless AGGREGATE_INTERVAL_S is set; a crawl's end recomputes anyway.
    .chain(
        (config.aggregate_interval_s > 0)
            .then(|| (kinds::AGGREGATE_ANALYTICS, s(config.aggregate_interval_s))),
    )
    // THR-03: on by default; each run counts what arrived since the last.
    .chain((config.aggregate_delta_s > 0).then(|| (kinds::AGGREGATE_DELTA, s(config.aggregate_delta_s))))
    // TL-02: off unless TIMELINE_BACKFILL_PATCHES is set. Hourly, so a region
    // whose job finished starts again once newer matches arrive without one.
    .chain(
        (config.timeline_backfill_patches > 0)
            .then_some((kinds::TIMELINES_BACKFILL, TIMELINE_BACKFILL_EVERY)),
    )
    .collect()
}

/// The ladders the crawl tick starts: every `LADDER_PLATFORMS` × `LADDER_QUEUES` (v1).
pub fn ladders(config: &Config) -> Vec<(String, String)> {
    config
        .ladder_platforms
        .iter()
        .flat_map(|p| {
            config
                .ladder_queues
                .iter()
                .map(move |q| (p.as_str().to_string(), q.clone()))
        })
        .collect()
}

/// The ticks `serve` runs: every one, now that each has its handler.
pub fn running_schedule(config: &Config) -> Vec<(&'static str, Duration)> {
    schedule(config)
}

/// What one tick loop queues for: the whole kind, or one ladder or region of
/// a kind queued once per ladder or region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A singleton, a poll's fan-out, or `aggregate:delta`.
    Kind,
    /// `ladder:crawl` or `aggregate:analytics` for one ladder.
    Ladder { platform: String, queue: String },
    /// `timelines:backfill` for one match-v5 region.
    Region(Region),
}

/// One tick loop: a kind, its period and what it queues for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    pub kind: &'static str,
    pub period: Duration,
    pub target: Target,
}

impl Tick {
    /// The ladder (`platform:queue`) or region the loop is for; empty for a kind.
    pub fn key(&self) -> String {
        match &self.target {
            Target::Kind => String::new(),
            Target::Ladder { platform, queue } => format!("{platform}:{queue}"),
            Target::Region(r) => r.as_str().to_string(),
        }
    }
}

/// The loops a schedule runs: one per kind, and one per ladder or region for
/// the kinds queued once per ladder (`ladder:crawl`, `aggregate:analytics`)
/// or per region (`timelines:backfill`), so each remembers its own last run.
pub fn loops(schedule: &[(&'static str, Duration)], ladders: &[(String, String)]) -> Vec<Tick> {
    let mut out = Vec::new();
    for &(kind, period) in schedule {
        let tick = |target| Tick { kind, period, target };
        match kind {
            kinds::LADDER_CRAWL | kinds::AGGREGATE_ANALYTICS => {
                out.extend(ladders.iter().map(|(platform, queue)| {
                    tick(Target::Ladder {
                        platform: platform.clone(),
                        queue: queue.clone(),
                    })
                }));
            }
            kinds::TIMELINES_BACKFILL => out.extend(Region::ALL.into_iter().map(|r| tick(Target::Region(r)))),
            _ => out.push(tick(Target::Kind)),
        }
    }
    out
}

/// Whether a kind's first tick waits for its last run plus its period
/// (ADR-214). The polls and `aggregate:delta` are cheap and deduped, and a
/// restart should resume liveness at once, so they fire on every boot.
pub fn remembers(kind: &str) -> bool {
    !matches!(
        kind,
        kinds::POLL_LIVE | kinds::POLL_RANK | kinds::POLL_MATCHES | kinds::AGGREGATE_DELTA
    )
}

/// A kind's last completion. `jobs_state_kind` (V0022) serves it from its
/// `(state, kind)` range, which `maintenance`'s 7-day trim bounds.
pub const LAST_DONE_SQL: &str = "SELECT max(finished_at) FROM jobs WHERE state = 'done' AND kind = ?1";
/// One region's last completion: the same range, narrowed by `dedupe_key`.
pub const LAST_DONE_KEYED_SQL: &str =
    "SELECT max(finished_at) FROM jobs WHERE state = 'done' AND kind = ?1 AND dedupe_key = ?2";
/// A ladder's last crawl start, from the crawl log (`ladder_crawls_recent`),
/// which no trim shortens; a crawl started by hand counts too.
pub const LAST_CRAWL_SQL: &str =
    "SELECT max(started_at) FROM ladder_crawls WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3";
/// A ladder's last completed rebuild, from its one `analytics_runs` row.
pub const LAST_REBUILD_SQL: &str = "SELECT at FROM analytics_runs
  WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3 AND kind = 'rebuild' AND status = 'completed'";

/// When `tick`'s kind last ran for its target, unix ms; `None` when nothing
/// says (a fresh database, or a run older than what is kept).
pub async fn last_run(db: &Db, key_scope: &str, tick: &Tick) -> Result<Option<i64>, DbError> {
    let (scope, kind, target) = (key_scope.to_string(), tick.kind, tick.target.clone());
    db.read(move |c| {
        let last: Option<Option<i64>> = match (kind, &target) {
            (kinds::LADDER_CRAWL, Target::Ladder { platform, queue }) => c
                .query_row(LAST_CRAWL_SQL, (&scope, platform, queue), |r| r.get(0))
                .optional()?,
            (kinds::AGGREGATE_ANALYTICS, Target::Ladder { platform, queue }) => c
                .query_row(LAST_REBUILD_SQL, (&scope, platform, queue), |r| r.get(0))
                .optional()?,
            (_, Target::Kind) => c.query_row(LAST_DONE_SQL, [kind], |r| r.get(0)).optional()?,
            (_, Target::Ladder { platform, queue }) => c
                .query_row(LAST_DONE_KEYED_SQL, (kind, format!("{platform}:{queue}")), |r| {
                    r.get(0)
                })
                .optional()?,
            (_, Target::Region(region)) => c
                .query_row(LAST_DONE_KEYED_SQL, (kind, region.as_str()), |r| r.get(0))
                .optional()?,
        };
        Ok::<_, DbError>(last.flatten())
    })
    .await
}

/// How long after `now_ms` a tick last run at `last` first fires: at
/// `last + period`, at once when that is past or there is no last run, and
/// never later than one period (a run stamped in the future, after the wall
/// clock stepped back, waits one period).
pub fn first_delay(last: Option<i64>, period: Duration, now_ms: i64) -> Duration {
    let Some(last) = last else {
        return Duration::ZERO;
    };
    let period_ms = i64::try_from(period.as_millis()).unwrap_or(i64::MAX);
    let due_in = last.saturating_add(period_ms).saturating_sub(now_ms);
    Duration::from_millis(u64::try_from(due_in).unwrap_or(0)).min(period)
}

/// When `tick` first fires, read at boot. A read that fails fires at once,
/// as every tick did before ADR-214, so a tick is never lost to it.
pub async fn first_fire(db: &Db, key_scope: &str, tick: &Tick, now_ms: i64) -> Duration {
    if !remembers(tick.kind) {
        return Duration::ZERO;
    }
    match last_run(db, key_scope, tick).await {
        Ok(last) => first_delay(last, tick.period, now_ms),
        Err(e) => {
            tracing::warn!(kind = tick.kind, key = %tick.key(), error = %e, "tick could not read its last run; firing now");
            Duration::ZERO
        }
    }
}

/// Queue what `tick` queues. Returns how many jobs were new.
pub async fn fire(scheduler: &Scheduler, key_scope: &str, tick: &Tick) -> Result<usize, DbError> {
    match (tick.kind, &tick.target) {
        (kinds::POLL_LIVE | kinds::POLL_RANK | kinds::POLL_MATCHES, _) => {
            fan_out(scheduler, key_scope, tick.kind).await
        }
        // Deduped per ladder; a crawl already running for one is left alone
        // by the job (v1 one-live-crawl rule).
        (kinds::LADDER_CRAWL, Target::Ladder { platform, queue }) => {
            crate::jobs::ladder::enqueue_crawl(scheduler.queue(), platform, queue)
                .await
                .map(usize::from)
        }
        // Deduped per ladder (v1).
        (kinds::AGGREGATE_ANALYTICS, Target::Ladder { platform, queue }) => {
            crate::jobs::analytics::enqueue_aggregate(scheduler.queue(), platform, queue)
                .await
                .map(|e| usize::from(e.created))
        }
        // Deduped per region.
        (kinds::TIMELINES_BACKFILL, Target::Region(region)) => scheduler
            .enqueue(crate::jobs::timelines::job(*region))
            .await
            .map(|e| usize::from(e.created)),
        _ => singleton(scheduler, tick.kind, priority::MAINTENANCE)
            .await
            .map(usize::from),
    }
}

/// The running tick loops.
pub struct Ticks {
    stop: watch::Sender<bool>,
    set: JoinSet<()>,
}

impl Ticks {
    pub fn start(scheduler: &Scheduler, key_scope: &str, schedule: Vec<(&'static str, Duration)>) -> Self {
        Self::start_with(scheduler, key_scope, schedule, Vec::new())
    }

    /// As [`Ticks::start`], with the ladders a `ladder:crawl` or
    /// `aggregate:analytics` tick queues for. Each loop first reads when it
    /// is due ([`first_fire`], against the clock read here), and a loop that
    /// waits logs one `tick deferred` line.
    pub fn start_with(
        scheduler: &Scheduler,
        key_scope: &str,
        schedule: Vec<(&'static str, Duration)>,
        ladders: Vec<(String, String)>,
    ) -> Self {
        let (stop, stopped) = watch::channel(false);
        let mut set = JoinSet::new();
        let boot = Clock::now();
        for tick in loops(&schedule, &ladders) {
            let (scheduler, scope, stopped) = (scheduler.clone(), key_scope.to_string(), stopped.clone());
            set.spawn(async move {
                let due = first_fire(scheduler.db(), &scope, &tick, boot.unix_ms).await;
                if !due.is_zero() {
                    match tick.target {
                        Target::Kind => {
                            tracing::info!(kind = %tick.kind, next_in_s = due.as_secs(), "tick deferred")
                        }
                        _ => tracing::info!(
                            kind = %tick.kind,
                            key = %tick.key(),
                            next_in_s = due.as_secs(),
                            "tick deferred"
                        ),
                    }
                }
                let first = due.saturating_sub(boot.instant.elapsed());
                every(tick.period, first, stopped, move || {
                    let (scheduler, scope, tick) = (scheduler.clone(), scope.clone(), tick.clone());
                    async move {
                        match fire(&scheduler, &scope, &tick).await {
                            Ok(n) => tracing::debug!(tick = tick.kind, key = %tick.key(), queued = n, "tick"),
                            Err(e) => {
                                tracing::warn!(tick = tick.kind, key = %tick.key(), error = %e, "tick could not enqueue")
                            }
                        }
                    }
                })
                .await;
            });
        }
        Self { stop, set }
    }

    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        while self.set.join_next().await.is_some() {}
    }
}

#[cfg(test)]
mod tests;

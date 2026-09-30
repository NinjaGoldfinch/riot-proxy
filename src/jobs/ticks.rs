//! Ticks (docs/design/06 §Scheduler): repeating timers that only *enqueue*.
//! Nothing is persisted: a tick missed while the process was down fires on the
//! next boot (the first tick of every loop is immediate), and a tick missed
//! because the process stalled is skipped rather than burst.
//!
//! The poll ticks fan out one job per tracked player (v1 `fanOut`), deduped by
//! PUUID, so a slow poll is never queued twice. Adding or removing a tracked
//! player needs no scheduler change (v1).

use std::future::Future;
use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

use crate::clock::Clock;
use crate::config::Config;
use crate::db::DbError;
use crate::jobs::scheduler::{NewJob, Scheduler, enqueue_on};
use crate::jobs::{kinds, priority};
use crate::players;

/// Run `action` now and then every `period` until `stop` flips.
pub async fn every<F, Fut>(period: Duration, mut stop: watch::Receiver<bool>, mut action: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut interval = tokio::time::interval(period.max(Duration::from_millis(1)));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => action().await,
            _ = stop.changed() => return,
        }
        if *stop.borrow() {
            return;
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
    ]
}

/// The ticks whose handlers exist today: the three polls and `ddragon:sync`.
pub fn running_schedule(config: &Config) -> Vec<(&'static str, Duration)> {
    schedule(config)
        .into_iter()
        .filter(|(kind, _)| *kind != kinds::MAINTENANCE)
        .collect()
}

/// The running tick loops.
pub struct Ticks {
    stop: watch::Sender<bool>,
    set: JoinSet<()>,
}

impl Ticks {
    pub fn start(scheduler: &Scheduler, key_scope: &str, schedule: Vec<(&'static str, Duration)>) -> Self {
        let (stop, stopped) = watch::channel(false);
        let mut set = JoinSet::new();
        for (kind, period) in schedule {
            let (scheduler, scope, stopped) = (scheduler.clone(), key_scope.to_string(), stopped.clone());
            set.spawn(every(period, stopped, move || {
                let (scheduler, scope) = (scheduler.clone(), scope.clone());
                async move {
                    let result = match kind {
                        kinds::POLL_LIVE | kinds::POLL_RANK | kinds::POLL_MATCHES => {
                            fan_out(&scheduler, &scope, kind).await
                        }
                        _ => singleton(&scheduler, kind, priority::MAINTENANCE)
                            .await
                            .map(usize::from),
                    };
                    match result {
                        Ok(n) => tracing::debug!(tick = kind, queued = n, "tick"),
                        Err(e) => tracing::warn!(tick = kind, error = %e, "tick could not enqueue"),
                    }
                }
            }));
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

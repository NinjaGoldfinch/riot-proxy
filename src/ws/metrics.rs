//! The `metrics` topic's clock (v1 `MetricsBroadcaster`): every
//! `METRICS_INTERVAL_S` a snapshot (`stats::Stats::snapshot`) is published,
//! but only while someone holds the topic, so it costs nothing while nobody
//! watches (v1).

use std::sync::Arc;
use std::time::Duration;

use crate::events::{self, Event};
use crate::stats::Stats;
use crate::ws::Topic;
use crate::ws::protocol::METRICS;

/// One tick: publish a snapshot if anyone is listening. Returns whether it did.
pub async fn tick(stats: &Stats) -> bool {
    if stats.hub.receivers(&Topic::named(METRICS)) == 0 {
        return false;
    }
    match stats.snapshot().await {
        Ok(snap) => {
            let value = serde_json::to_value(&snap).unwrap_or_default();
            events::publish(&stats.hub, &Event::MetricsSnapshot(value));
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "metrics snapshot failed");
            false
        }
    }
}

/// Tick every `interval` until the task is aborted.
pub fn spawn(stats: Arc<Stats>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(interval);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&stats).await;
        }
    })
}

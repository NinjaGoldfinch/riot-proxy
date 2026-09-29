//! The `metrics` topic's clock (v1 `MetricsBroadcaster`): every
//! `METRICS_INTERVAL_S` a snapshot is published, but only while someone holds
//! the topic, so it costs nothing while nobody watches (v1).
//!
//! The snapshot here carries v1's `v`, `keyScope`, `totals`, `ws` and `events`
//! sections; the rest of v1's document (queues, cache, limiter, flows, ladder,
//! analytics) arrives with the dashboard in P7-06.

use std::time::Duration;

use serde_json::{Value, json};

use crate::archive::matches;
use crate::db::Db;
use crate::events::{self, Event};
use crate::players;
use crate::ws::protocol::METRICS;
use crate::ws::{Hub, Topic};

/// The snapshot as far as v2 builds it today.
pub async fn snapshot(hub: &Hub, db: &Db, key_scope: &str) -> Value {
    let archived = matches::stats(db).await.map(|s| s.matches).unwrap_or_default();
    let (known, tracked) = players::counts(db, key_scope).await.unwrap_or_default();
    json!({
        "v": 1,
        "keyScope": key_scope,
        "totals": {"archivedMatches": archived, "trackedPlayers": tracked, "knownPlayers": known},
        "ws": {"connections": hub.connections(), "subscriptions": hub.subscriptions()},
        "events": hub.event_counts(),
    })
}

/// One tick: publish a snapshot if anyone is listening. Returns whether it did.
pub async fn tick(hub: &Hub, db: &Db, key_scope: &str) -> bool {
    if hub.receivers(&Topic::named(METRICS)) == 0 {
        return false;
    }
    let snap = snapshot(hub, db, key_scope).await;
    events::publish(hub, &Event::MetricsSnapshot(snap));
    true
}

/// Tick every `interval` until the task is aborted.
pub fn spawn(hub: Hub, db: Db, key_scope: String, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(interval);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&hub, &db, &key_scope).await;
        }
    })
}

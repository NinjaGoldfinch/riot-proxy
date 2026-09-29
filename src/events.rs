//! The realtime event catalogue (docs/design/06 §Events): v1's names and
//! payloads plus `crawl.phase` (owner decision, ADR-045). The enum is tagged by
//! `event`, the key v1's frames use, so every name is known to the compiler.
//!
//! [`publish`] serialises an event once into v1's frame, `{op, event, topic,
//! at, data}`, and hands it to the hub. Publishing is fire-and-forget: losing an
//! event must never fail the job that produced it (v1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::clock::Clock;
use crate::metrics::EVENTS_PUBLISHED_TOTAL;
use crate::ws::protocol::{FIREHOSE, LADDER, METRICS, PATCH};
use crate::ws::{Hub, Topic};

/// A ranked standing, as `rank.changed` reports it (v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rank {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lp: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum Event {
    /// A tracked player entered a game (spectator-v5).
    #[serde(rename = "game.started", rename_all = "camelCase")]
    GameStarted {
        puuid: String,
        platform: String,
        game_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        queue_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        champion_id: Option<i64>,
    },
    /// The game ended. v1 sent `puuid` and `gameId`; the rest are design/06's.
    #[serde(rename = "game.ended", rename_all = "camelCase")]
    GameEnded {
        puuid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        game_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        queue_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        champion_id: Option<i64>,
    },
    /// A player's standing in one ranked queue moved; `null` is unranked.
    #[serde(rename = "rank.changed")]
    RankChanged {
        puuid: String,
        queue: String,
        before: Option<Rank>,
        after: Option<Rank>,
    },
    /// A match entered the archive. `puuid` is absent for crawl archives, which
    /// belong to no one player (v1); `patch` and `participants` are design/06's.
    #[serde(rename = "match.archived", rename_all = "camelCase")]
    MatchArchived {
        #[serde(skip_serializing_if = "Option::is_none")]
        puuid: Option<String>,
        match_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        participants: Vec<String>,
    },
    /// Data Dragon published a new version.
    #[serde(rename = "patch.new")]
    PatchNew { version: String },
    /// A ladder crawl moved to a new stage (design/06): enumerate, collect,
    /// archive, done. `stats` are its running counters (P7-02 fixes their shape).
    #[serde(rename = "crawl.phase", rename_all = "camelCase")]
    CrawlPhase {
        crawl_id: String,
        platform: String,
        queue: String,
        phase: String,
        stats: serde_json::Value,
    },
    /// A crawl finished cleanly, with its counters (v1).
    #[serde(rename = "ladder.crawl.completed", rename_all = "camelCase")]
    LadderCrawlCompleted {
        crawl_id: String,
        platform: String,
        queue: String,
        entries: i64,
        players: i64,
        duration_s: i64,
    },
    /// The analytics tables were rebuilt for one ladder; `tables` is rows
    /// written by table name (v1).
    #[serde(rename = "analytics.updated", rename_all = "camelCase")]
    AnalyticsUpdated {
        platform: String,
        queue: String,
        duration_s: i64,
        tables: BTreeMap<String, i64>,
    },
    /// The dashboard's operational snapshot (its shape arrives with P7-06).
    #[serde(rename = "metrics.snapshot")]
    MetricsSnapshot(serde_json::Value),
}

/// Every event name, for documentation and tests.
pub const NAMES: [&str; 9] = [
    "game.started",
    "game.ended",
    "rank.changed",
    "match.archived",
    "patch.new",
    "crawl.phase",
    "ladder.crawl.completed",
    "analytics.updated",
    "metrics.snapshot",
];

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GameStarted { .. } => "game.started",
            Self::GameEnded { .. } => "game.ended",
            Self::RankChanged { .. } => "rank.changed",
            Self::MatchArchived { .. } => "match.archived",
            Self::PatchNew { .. } => "patch.new",
            Self::CrawlPhase { .. } => "crawl.phase",
            Self::LadderCrawlCompleted { .. } => "ladder.crawl.completed",
            Self::AnalyticsUpdated { .. } => "analytics.updated",
            Self::MetricsSnapshot(_) => "metrics.snapshot",
        }
    }

    /// Where v1 published each event. A match archived for no one player goes
    /// to the firehose alone (v1 used `player:`, which nobody can hold).
    pub fn topic(&self) -> Topic {
        match self {
            Self::GameStarted { puuid, .. }
            | Self::GameEnded { puuid, .. }
            | Self::RankChanged { puuid, .. } => Topic::player(puuid),
            Self::MatchArchived { puuid: Some(p), .. } => Topic::player(p),
            Self::MatchArchived { puuid: None, .. } => Topic::named(FIREHOSE),
            Self::PatchNew { .. } => Topic::named(PATCH),
            Self::CrawlPhase { .. } | Self::LadderCrawlCompleted { .. } | Self::AnalyticsUpdated { .. } => {
                Topic::named(LADDER)
            }
            Self::MetricsSnapshot(_) => Topic::named(METRICS),
        }
    }

    /// v1's frame plus `op` (ADR-045): `{op, event, topic, at, data}`.
    pub fn frame(&self, at: i64) -> String {
        #[derive(Deserialize)]
        struct Tagged<'a> {
            #[serde(borrow)]
            data: &'a RawValue,
        }
        #[derive(Serialize)]
        struct Frame<'a> {
            op: &'static str,
            event: &'static str,
            topic: &'a str,
            at: i64,
            data: &'a RawValue,
        }
        let tagged = serde_json::to_string(self).unwrap_or_default();
        let topic = self.topic();
        let Ok(Tagged { data }) = serde_json::from_str::<Tagged<'_>>(&tagged) else {
            tracing::error!(event = self.name(), "event did not serialise");
            return String::new();
        };
        serde_json::to_string(&Frame {
            op: "event",
            event: self.name(),
            topic: topic.as_str(),
            at,
            data,
        })
        .unwrap_or_default()
    }
}

/// Publish an event now. Returns the sockets it reached.
pub fn publish(hub: &Hub, event: &Event) -> usize {
    publish_at(hub, event, Clock::now().unix_ms)
}

/// [`publish`] with a fixed timestamp (tests).
pub fn publish_at(hub: &Hub, event: &Event, at: i64) -> usize {
    let name = event.name();
    metrics::counter!(EVENTS_PUBLISHED_TOTAL, "name" => name).increment(1);
    let frame = event.frame(at);
    if frame.is_empty() {
        return 0;
    }
    let reached = hub.publish(&event.topic(), name, frame.into());
    tracing::debug!(event = name, topic = %event.topic(), reached, "event published");
    reached
}

#[cfg(test)]
mod tests;

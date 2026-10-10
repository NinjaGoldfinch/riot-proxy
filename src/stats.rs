//! The dashboard's documents (plan P7-06; v1 `stats/snapshot.ts`,
//! `stats/history.ts`): the `metrics.snapshot` sent on the `metrics` topic and
//! by `GET /v1/admin/metrics`, and the history points sampled every
//! `METRICS_HISTORY_INTERVAL_S` into `metrics_history`.
//!
//! The shape is v1's `MetricsSnapshot` (`v: 1`). Where v1 read Redis, BullMQ or
//! a separate worker process, v2 reads what replaced them (ADR-058): the
//! `jobs` table grouped into v1's six queue names, the in-process limiter, and
//! the workers running in this very process.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use metrics_exporter_prometheus::PrometheusHandle;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::riot::limiter::Limiter;
use crate::riot::routing::{Platform, Region};
use crate::routes::admin::LadderCrawlSummary;
use crate::routes::players::{js_number, round4};
use crate::r#static::Mirror;
use crate::ws::Hub;

/// v1 `METRICS_HISTORY_MAX_POINTS`: 24 hours at 60 s.
pub const HISTORY_MAX_POINTS: i64 = 1440;

/// v1's six BullMQ queues, which the dashboard draws; every v2 job kind
/// belongs to one.
pub const QUEUES: [&str; 6] = ["poll", "archive", "backfill", "ddragon", "ladder", "maintenance"];

/// The v1 queue a job kind ran on.
pub fn queue_of(kind: &str) -> &'static str {
    match kind.split(':').next().unwrap_or_default() {
        "poll" => "poll",
        "archive" => "archive",
        "backfill" => "backfill",
        "ddragon" => "ddragon",
        "ladder" => "ladder",
        // maintenance, aggregate:analytics, facts:reextract, names:backfill (v1).
        _ => "maintenance",
    }
}

/// What a snapshot is read from.
pub struct Stats {
    pub hub: Hub,
    pub db: Db,
    pub key_scope: String,
    pub limiter: Arc<Limiter>,
    pub metrics: PrometheusHandle,
    pub mirror: Arc<Mirror>,
    pub started: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub archived_matches: i64,
    pub tracked_players: i64,
    pub known_players: i64,
    /// Snapshot only (not in history points).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub active_consumers: Option<i64>,
}

/// One v1 queue's jobs by state (v1 `QueueCounts`).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct QueueCounts {
    pub active: i64,
    /// Pending and due.
    pub waiting: i64,
    /// Always 0: every v2 job has a priority, so `waiting` holds them all.
    pub prioritized: i64,
    /// Pending, waiting out a backoff.
    pub delayed: i64,
    /// Always 0: v2's ticks are timers, not parked jobs.
    pub scheduled: i64,
    /// Failed in the last 24 hours.
    pub failed: i64,
    /// Done in the last hour.
    pub completed: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WsCounts {
    pub connections: u64,
    pub subscriptions: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CacheCounts {
    pub hit: u64,
    pub miss: u64,
    pub neg: u64,
    pub stale: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WindowUse {
    /// `limit:seconds`.
    pub window: String,
    pub used: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MethodUse {
    pub method: String,
    pub windows: Vec<WindowUse>,
}

/// One rate-limit scope's usage (v1's `limiter[]`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LimiterScope {
    pub scope: String,
    /// `platform`, `region` or `other`.
    pub kind: &'static str,
    pub label: String,
    pub frozen_ms: u64,
    pub windows: Vec<WindowUse>,
    pub methods: Vec<MethodUse>,
}

/// The job workers. They run in this process, so a snapshot that answers is
/// one taken while they are alive (v1 read a separate worker's heartbeat).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Worker {
    pub alive: bool,
    pub last_seen_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Flows {
    /// `reason:status` → backfills queued (`proxy_backfills_queued_total`).
    pub backfills_queued: BTreeMap<String, u64>,
    /// `part:outcome` → refresh claims (`proxy_refresh_claims_total`).
    pub refresh_claims: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LadderSection {
    pub running: Vec<LadderCrawlSummary>,
    #[schema(required = true)]
    pub last_completed: Option<LadderCrawlSummary>,
    /// Ladder entries this key scope holds.
    pub entries: i64,
}

/// One ladder's last analytics recompute (v1 `AnalyticsRunSummary`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub platform: String,
    pub queue: String,
    /// Unix ms.
    pub at: i64,
    pub status: String,
    pub ms: i64,
    pub steps: BTreeMap<String, f64>,
    pub rows: BTreeMap<String, i64>,
    pub games: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopChampion {
    pub champion_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub champion_name: Option<String>,
    pub games: i64,
    #[serde(serialize_with = "js_number")]
    pub win_rate: f64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsSection {
    pub last_runs: Vec<RunSummary>,
    /// The newest run's ladder, its latest patch, top five by games.
    pub top_champions: Vec<TopChampion>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessStats {
    pub uptime_seconds: f64,
    pub rss_bytes: u64,
}

/// v1 `MetricsSnapshot`, field for field and in v1's order.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Bumped only when a field changes meaning.
    pub v: u8,
    pub key_scope: String,
    pub totals: Totals,
    pub queues: BTreeMap<String, QueueCounts>,
    pub ws: WsCounts,
    /// Frames published per event name since the process started.
    pub events: BTreeMap<String, u64>,
    pub cache: CacheCounts,
    pub limiter: Vec<LimiterScope>,
    pub worker: Worker,
    pub flows: Flows,
    pub ladder: LadderSection,
    pub analytics: AnalyticsSection,
    pub process: ProcessStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct HistoryQueues {
    pub active: i64,
    /// Waiting and delayed.
    pub pending: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAnalytics {
    /// Rows the last runs wrote, all ladders.
    pub rows: i64,
    /// Seconds since the newest run; null before any.
    pub age_seconds: Option<i64>,
}

/// v1 `MetricsHistoryPoint`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HistoryPoint {
    /// Unix ms.
    pub t: i64,
    pub totals: Totals,
    pub queues: HistoryQueues,
    pub analytics: HistoryAnalytics,
    pub cache: CacheCounts,
}

/// The process's resident set, from `/proc/self/status` where there is one.
fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

fn kind_of(scope: &str) -> (&'static str, String) {
    if let Ok(p) = scope.parse::<Platform>() {
        ("platform", p.label().to_string())
    } else if let Ok(r) = scope.parse::<Region>() {
        ("region", r.label().to_string())
    } else {
        ("other", scope.to_string())
    }
}

impl Stats {
    fn counter(&self, name: &str, keys: &[&str]) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for (labels, value) in crate::telemetry::counter_values(&self.metrics, name) {
            let key = keys
                .iter()
                .map(|k| labels.get(*k).map_or("", String::as_str))
                .collect::<Vec<_>>()
                .join(":");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                *out.entry(key).or_default() += value as u64;
            }
        }
        out
    }

    fn cache(&self) -> CacheCounts {
        let c = self.counter(crate::metrics::CACHE_READS_TOTAL, &["state"]);
        let get = |k: &str| c.get(k).copied().unwrap_or(0);
        CacheCounts {
            hit: get("hit"),
            miss: get("miss"),
            neg: get("neg"),
            stale: get("stale"),
        }
    }

    fn limiter(&self) -> Vec<LimiterScope> {
        let uses = |w: Vec<crate::riot::limiter::WindowUsage>| {
            w.into_iter()
                .map(|w| WindowUse {
                    window: w.window,
                    used: w.used,
                    limit: w.limit,
                })
                .collect()
        };
        // Every scope with limits learned from Riot, app or method (v1 listed
        // the scopes with a stored configuration of either kind).
        let mut by_scope = self.limiter.known_scope_methods();
        for scope in self.limiter.known_scopes() {
            by_scope.entry(scope).or_default();
        }
        by_scope
            .into_iter()
            .map(|(scope, methods)| {
                let (kind, label) = kind_of(&scope);
                let names: Vec<&str> = methods.iter().map(String::as_str).collect();
                LimiterScope {
                    kind,
                    label,
                    frozen_ms: self
                        .limiter
                        .frozen_for(&scope)
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                    windows: uses(self.limiter.usage(&scope)),
                    methods: self
                        .limiter
                        .method_usage(&scope, &names)
                        .into_iter()
                        .map(|m| MethodUse {
                            method: m.method,
                            windows: uses(m.windows),
                        })
                        .collect(),
                    scope,
                }
            })
            .collect()
    }

    async fn totals(&self) -> Result<Totals, DbError> {
        let scope = self.key_scope.clone();
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM matches),
                            (SELECT COUNT(*) FROM players WHERE key_scope = ?1 AND tracked = 1),
                            (SELECT COUNT(*) FROM players WHERE key_scope = ?1),
                            (SELECT COUNT(*) FROM consumers WHERE revoked_at IS NULL)",
                    [scope],
                    |r| {
                        Ok(Totals {
                            archived_matches: r.get(0)?,
                            tracked_players: r.get(1)?,
                            known_players: r.get(2)?,
                            active_consumers: Some(r.get(3)?),
                        })
                    },
                )?)
            })
            .await
    }

    async fn queues(&self, now: i64) -> Result<BTreeMap<String, QueueCounts>, DbError> {
        let rows: Vec<(String, [i64; 5])> = self
            .db
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT kind,
                            coalesce(sum(state = 'running'), 0),
                            coalesce(sum(state = 'pending' AND run_after <= ?1), 0),
                            coalesce(sum(state = 'pending' AND run_after > ?1), 0),
                            coalesce(sum(state = 'failed' AND finished_at > ?1 - 86400000), 0),
                            coalesce(sum(state = 'done' AND finished_at > ?1 - 3600000), 0)
                       FROM jobs GROUP BY kind",
                )?;
                let rows = stmt
                    .query_map([now], |r| {
                        Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?]))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?;
        let mut out: BTreeMap<String, QueueCounts> = QUEUES
            .iter()
            .map(|q| ((*q).to_string(), QueueCounts::default()))
            .collect();
        for (kind, [active, waiting, delayed, failed, completed]) in rows {
            let q = out.entry(queue_of(&kind).to_string()).or_default();
            q.active += active;
            q.waiting += waiting;
            q.delayed += delayed;
            q.failed += failed;
            q.completed += completed;
        }
        Ok(out)
    }

    async fn ladder(&self) -> Result<LadderSection, DbError> {
        let scope = self.key_scope.clone();
        let (running, last, entries) = self
            .db
            .read(move |c| {
                let crawls = |sql: &str| -> Result<Vec<String>, DbError> {
                    let mut stmt = c.prepare(sql)?;
                    let ids = stmt
                        .query_map([&scope], |r| r.get(0))?
                        .collect::<Result<Vec<String>, _>>()?;
                    Ok(ids)
                };
                let running = crawls(
                    "SELECT id FROM ladder_crawls WHERE key_scope = ?1 AND status = 'running' ORDER BY started_at DESC",
                )?;
                let last = crawls(
                    "SELECT id FROM ladder_crawls WHERE key_scope = ?1 AND finished_at IS NOT NULL
                      ORDER BY finished_at DESC LIMIT 1",
                )?;
                let entries: i64 = c.query_row(
                    "SELECT COUNT(*) FROM ladder_entries WHERE key_scope = ?1",
                    [&scope],
                    |r| r.get(0),
                )?;
                Ok::<_, DbError>((running, last, entries))
            })
            .await?;
        let mut summaries = Vec::new();
        for id in running {
            summaries.push(self.crawl_summary(&id).await?);
        }
        let last_completed = match last.first() {
            Some(id) => Some(self.crawl_summary(id).await?),
            None => None,
        };
        Ok(LadderSection {
            running: summaries.into_iter().flatten().collect(),
            last_completed: last_completed.flatten(),
            entries,
        })
    }

    async fn crawl_summary(&self, id: &str) -> Result<Option<LadderCrawlSummary>, DbError> {
        let Some(crawl) = crate::jobs::ladder::store::get(&self.db, &self.key_scope, id).await? else {
            return Ok(None);
        };
        let crawl_id = crawl.id.clone();
        let pending = if crawl.status == "running" {
            self.db
                .read(move |c| crate::jobs::ladder::store::pending_legs(c, &crawl_id))
                .await?
        } else {
            0
        };
        Ok(Some(LadderCrawlSummary::new(crawl, pending)))
    }

    async fn analytics(&self) -> Result<AnalyticsSection, DbError> {
        let runs = crate::jobs::analytics::runs(&self.db, &self.key_scope).await?;
        let last_runs: Vec<RunSummary> = runs
            .into_iter()
            .map(|(l, r)| RunSummary {
                platform: l.platform,
                queue: l.queue,
                at: r.at,
                status: r.status,
                ms: r.ms,
                steps: r.steps,
                rows: r.rows,
                games: r.games,
            })
            .collect();
        let Some(newest) = last_runs.first() else {
            return Ok(AnalyticsSection {
                last_runs,
                top_champions: vec![],
            });
        };
        let (platform, queue) = (newest.platform.clone(), newest.queue.clone());
        let patch =
            crate::archive::analytics::latest_patch(&self.db, &self.key_scope, Some(&platform), &queue)
                .await?;
        let top = match patch {
            Some(p) => {
                let read = crate::archive::analytics::Read {
                    key_scope: self.key_scope.clone(),
                    platform: Some(platform),
                    queue,
                    patch: Some(p),
                    tier: None,
                    role: None,
                    champion_id: None,
                    min_games: 0,
                    limit: 5_000,
                    remakes: false,
                };
                // Summed over tiers: the five most played on the ladder.
                let mut by_champion: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
                for r in crate::archive::analytics::stats(&self.db, read).await? {
                    let e = by_champion.entry(r.champion_id).or_default();
                    e.0 += r.games;
                    e.1 += r.wins;
                }
                let mut top: Vec<(i64, (i64, i64))> = by_champion.into_iter().collect();
                top.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
                top.truncate(5);
                top
            }
            None => vec![],
        };
        let ids: Vec<i64> = top.iter().map(|(id, _)| *id).collect();
        let names = self.mirror.champion_names(&ids).await;
        #[allow(clippy::cast_precision_loss)]
        let top_champions = top
            .into_iter()
            .map(|(id, (games, wins))| TopChampion {
                champion_id: id,
                champion_name: names.get(&id).cloned(),
                games,
                win_rate: if games > 0 {
                    round4(wins as f64 / games as f64)
                } else {
                    0.0
                },
            })
            .collect();
        Ok(AnalyticsSection {
            last_runs,
            top_champions,
        })
    }

    /// v1 `buildMetricsSnapshot`.
    pub async fn snapshot(&self) -> Result<Snapshot, DbError> {
        let now = Clock::now().unix_ms;
        let (totals, queues, ladder, analytics) =
            tokio::try_join!(self.totals(), self.queues(now), self.ladder(), self.analytics())?;
        Ok(Snapshot {
            v: 1,
            key_scope: self.key_scope.clone(),
            totals,
            queues,
            ws: WsCounts {
                connections: u64::try_from(self.hub.connections()).unwrap_or(u64::MAX),
                subscriptions: self.hub.subscriptions(),
            },
            events: self.hub.event_counts().into_iter().collect(),
            cache: self.cache(),
            limiter: self.limiter(),
            worker: Worker {
                alive: true,
                last_seen_ms: Some(0),
            },
            flows: Flows {
                backfills_queued: self.counter(crate::metrics::BACKFILLS_QUEUED_TOTAL, &["reason", "status"]),
                refresh_claims: self.counter(crate::metrics::REFRESH_CLAIMS_TOTAL, &["part", "outcome"]),
            },
            ladder,
            analytics,
            process: ProcessStats {
                uptime_seconds: self.started.elapsed().as_secs_f64(),
                rss_bytes: rss_bytes(),
            },
        })
    }

    /// v1 `buildHistoryPoint`.
    pub async fn history_point(&self, now: i64) -> Result<HistoryPoint, DbError> {
        let (mut totals, queues, runs) = tokio::try_join!(
            self.totals(),
            self.queues(now),
            crate::jobs::analytics::runs(&self.db, &self.key_scope)
        )?;
        totals.active_consumers = None;
        let q = queues.values().fold(HistoryQueues::default(), |mut acc, c| {
            acc.active += c.active;
            acc.pending += c.waiting + c.delayed;
            acc.failed += c.failed;
            acc
        });
        Ok(HistoryPoint {
            t: now,
            totals,
            queues: q,
            analytics: HistoryAnalytics {
                rows: runs.iter().map(|(_, r)| r.rows.values().sum::<i64>()).sum(),
                age_seconds: runs.first().map(|(_, r)| ((now - r.at + 500) / 1000).max(0)),
            },
            cache: self.cache(),
        })
    }

    /// Store one point and keep the newest [`HISTORY_MAX_POINTS`] (v1 LTRIM).
    pub async fn record_point(&self, now: i64) -> Result<(), DbError> {
        let point = self.history_point(now).await?;
        let json = serde_json::to_string(&point).unwrap_or_default();
        self.db
            .write(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO metrics_history (at, point) VALUES (?1, ?2)",
                    params![point.t, json],
                )?;
                c.execute(
                    "DELETE FROM metrics_history WHERE at NOT IN
                       (SELECT at FROM metrics_history ORDER BY at DESC LIMIT ?1)",
                    [HISTORY_MAX_POINTS],
                )?;
                Ok(())
            })
            .await
    }

    /// Every stored point, oldest first; unreadable ones are skipped (v1).
    pub async fn history(&self) -> Result<Vec<HistoryPoint>, DbError> {
        let raw: Vec<String> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare("SELECT point FROM metrics_history ORDER BY at")?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?;
        Ok(raw.iter().filter_map(|p| serde_json::from_str(p).ok()).collect())
    }
}

/// Sample a history point every `interval`, whether or not anyone watches
/// (v1 `MetricsHistoryRecorder`); the first after one interval.
pub fn spawn_history(stats: Arc<Stats>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            if let Err(e) = stats.record_point(Clock::now().unix_ms).await {
                tracing::warn!(error = %e, "metrics history point failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_job_kind_lands_in_one_of_v1s_queues() {
        for (kind, queue) in [
            ("poll:live", "poll"),
            ("poll:matches", "poll"),
            ("archive:match", "archive"),
            ("backfill:player", "backfill"),
            ("ddragon:sync", "ddragon"),
            ("ladder:walk", "ladder"),
            ("ladder:archive", "ladder"),
            ("maintenance", "maintenance"),
            ("aggregate:analytics", "maintenance"),
            ("facts:reextract", "maintenance"),
            ("builds:extract", "maintenance"),
            ("tiers:backfill", "maintenance"),
            ("names:backfill", "maintenance"),
        ] {
            assert_eq!(queue_of(kind), queue, "{kind}");
            assert!(QUEUES.contains(&queue));
        }
    }

    #[test]
    fn rate_limit_scopes_are_labelled_by_kind() {
        assert_eq!(kind_of("kr"), ("platform", "Korea".to_string()));
        assert_eq!(kind_of("europe"), ("region", "Europe".to_string()));
        assert_eq!(kind_of("odd"), ("other", "odd".to_string()));
    }
}

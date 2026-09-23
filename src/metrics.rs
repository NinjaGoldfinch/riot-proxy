//! The metric catalogue. Names, types, labels and histogram buckets are v1's
//! (`src/metrics.ts`) byte for byte, so `ops/grafana/riot-proxy-dashboard.json` and
//! `ops/prometheus-alerts.yml` work unchanged. `docs/design/metrics.md` documents
//! each entry; a test keeps the two in sync.

use metrics::{Unit, describe_counter, describe_gauge, describe_histogram};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Debug)]
pub struct MetricDef {
    pub name: &'static str,
    pub kind: Kind,
    pub labels: &'static [&'static str],
    pub help: &'static str,
    /// Histogram bucket upper bounds (seconds). Empty for counters and gauges.
    pub buckets: &'static [f64],
}

pub const REQUESTS_TOTAL: &str = "proxy_requests_total";
pub const UPSTREAM_REQUESTS_TOTAL: &str = "proxy_upstream_requests_total";
pub const UPSTREAM_LATENCY_SECONDS: &str = "proxy_upstream_latency_seconds";
pub const RL_WAIT_SECONDS: &str = "proxy_rl_wait_seconds";
pub const RL_429_TOTAL: &str = "proxy_rl_429_total";
pub const CACHE_READS_TOTAL: &str = "proxy_cache_reads_total";
pub const WS_CONNECTIONS: &str = "proxy_ws_connections";
pub const JOBS_TOTAL: &str = "proxy_jobs_total";
pub const BACKFILLS_QUEUED_TOTAL: &str = "proxy_backfills_queued_total";
pub const REFRESH_CLAIMS_TOTAL: &str = "proxy_refresh_claims_total";
pub const LADDER_PAGES_TOTAL: &str = "proxy_ladder_pages_total";
pub const LADDER_ENTRIES_TOTAL: &str = "proxy_ladder_entries_total";
pub const LADDER_MATCH_IDS_TOTAL: &str = "proxy_ladder_match_ids_total";
pub const LADDER_MATCHES_QUEUED_TOTAL: &str = "proxy_ladder_matches_queued_total";
pub const LADDER_CRAWL_DURATION_SECONDS: &str = "proxy_ladder_crawl_duration_seconds";
pub const AGGREGATE_RUNS_TOTAL: &str = "proxy_aggregate_runs_total";
pub const AGGREGATE_DURATION_SECONDS: &str = "proxy_aggregate_duration_seconds";
pub const AGGREGATE_ROWS: &str = "proxy_aggregate_rows";
pub const FACTS_REEXTRACT_PROGRESS: &str = "proxy_facts_reextract_progress";
pub const ARCHIVED_MATCHES_TOTAL: &str = "proxy_archived_matches_total";

const UPSTREAM_LATENCY_BUCKETS: &[f64] = &[0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0];
const RL_WAIT_BUCKETS: &[f64] = &[0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0];
const LADDER_CRAWL_BUCKETS: &[f64] = &[
    10.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0, 14_400.0, 28_800.0,
];
const AGGREGATE_BUCKETS: &[f64] = &[1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0];

const fn def(
    name: &'static str,
    kind: Kind,
    labels: &'static [&'static str],
    help: &'static str,
    buckets: &'static [f64],
) -> MetricDef {
    MetricDef {
        name,
        kind,
        labels,
        help,
        buckets,
    }
}

/// Every v1 metric, in v1's declaration order. v2 additions (design/07 §Observability)
/// are appended by the tasks that implement them.
pub const CATALOGUE: &[MetricDef] = &[
    def(
        REQUESTS_TOTAL,
        Kind::Counter,
        &["route", "status", "cache"],
        "Downstream requests served by the proxy",
        &[],
    ),
    def(
        UPSTREAM_REQUESTS_TOTAL,
        Kind::Counter,
        &["region", "method", "status"],
        "Requests dispatched to the Riot API",
        &[],
    ),
    def(
        UPSTREAM_LATENCY_SECONDS,
        Kind::Histogram,
        &["region", "method"],
        "Riot API round-trip latency",
        UPSTREAM_LATENCY_BUCKETS,
    ),
    def(
        RL_WAIT_SECONDS,
        Kind::Histogram,
        &["region", "priority"],
        "Time spent waiting on the rate limiter before dispatch",
        RL_WAIT_BUCKETS,
    ),
    def(
        RL_429_TOTAL,
        Kind::Counter,
        &["region", "type"],
        "Upstream 429s received, by rate limit type",
        &[],
    ),
    def(
        CACHE_READS_TOTAL,
        Kind::Counter,
        &["state"],
        "Cacheable reads served, by cache outcome",
        &[],
    ),
    def(
        WS_CONNECTIONS,
        Kind::Gauge,
        &[],
        "Currently open WebSocket connections on this instance",
        &[],
    ),
    def(
        JOBS_TOTAL,
        Kind::Counter,
        &["job", "status"],
        "Background jobs processed",
        &[],
    ),
    def(
        BACKFILLS_QUEUED_TOTAL,
        Kind::Counter,
        &["reason", "status"],
        "Player backfills requested, by what asked for one and what came of it",
        &[],
    ),
    def(
        REFRESH_CLAIMS_TOTAL,
        Kind::Counter,
        &["part", "outcome"],
        "Explicit ?refresh=true claims, by route part and whether the window was won",
        &[],
    ),
    def(
        LADDER_PAGES_TOTAL,
        Kind::Counter,
        &["platform", "queue"],
        "Ladder pages fetched, including the apex leagues (one page each)",
        &[],
    ),
    def(
        LADDER_ENTRIES_TOTAL,
        Kind::Counter,
        &["platform", "queue"],
        "League entries upserted by a crawl",
        &[],
    ),
    def(
        LADDER_MATCH_IDS_TOTAL,
        Kind::Counter,
        &["platform", "queue"],
        "Distinct match ids gathered by a crawl, after de-duplication",
        &[],
    ),
    def(
        LADDER_MATCHES_QUEUED_TOTAL,
        Kind::Counter,
        &["platform", "queue"],
        "Matches a crawl handed to the archive queue, having found them unarchived",
        &[],
    ),
    def(
        LADDER_CRAWL_DURATION_SECONDS,
        Kind::Histogram,
        &["platform", "queue", "status"],
        "Wall-clock time of a completed ladder crawl",
        LADDER_CRAWL_BUCKETS,
    ),
    def(
        AGGREGATE_RUNS_TOTAL,
        Kind::Counter,
        &["platform", "queue", "status"],
        "Analytics recomputes, by outcome",
        &[],
    ),
    def(
        AGGREGATE_DURATION_SECONDS,
        Kind::Histogram,
        &["platform", "queue", "step"],
        "Wall-clock time of one step of an analytics recompute",
        AGGREGATE_BUCKETS,
    ),
    def(
        AGGREGATE_ROWS,
        Kind::Gauge,
        &["platform", "queue", "table"],
        "Rows written by the last analytics recompute, by table",
        &[],
    ),
    def(
        FACTS_REEXTRACT_PROGRESS,
        Kind::Gauge,
        &[],
        "Fraction of the archive the fact re-extraction has swept, 0–1",
        &[],
    ),
    // v1's help says "Postgres archive"; v2 archives to SQLite by default.
    def(
        ARCHIVED_MATCHES_TOTAL,
        Kind::Counter,
        &[],
        "Matches upserted into the archive",
        &[],
    ),
];

/// Attach help text to every catalogue metric, and register the label-less ones at
/// zero. prom-client exported those from boot, so dashboards never saw them missing.
/// Metrics with labels appear once a series is first recorded, as they did in v1.
pub fn describe_all() {
    for m in CATALOGUE {
        let unit = if m.name.ends_with("_seconds") {
            Some(Unit::Seconds)
        } else {
            None
        };
        match (m.kind, unit) {
            (Kind::Counter, _) => describe_counter!(m.name, m.help),
            (Kind::Gauge, _) => describe_gauge!(m.name, m.help),
            (Kind::Histogram, Some(unit)) => describe_histogram!(m.name, unit, m.help),
            (Kind::Histogram, None) => describe_histogram!(m.name, m.help),
        }
        if m.labels.is_empty() {
            match m.kind {
                Kind::Counter => {
                    metrics::counter!(m.name).absolute(0);
                }
                Kind::Gauge => metrics::gauge!(m.name).set(0.0),
                Kind::Histogram => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v1 `src/metrics.ts`, `grep -o "name: 'proxy_[a-z_]*'"`, at commit c86e631.
    const V1_NAMES: &[&str] = &[
        "proxy_requests_total",
        "proxy_upstream_requests_total",
        "proxy_upstream_latency_seconds",
        "proxy_rl_wait_seconds",
        "proxy_rl_429_total",
        "proxy_cache_reads_total",
        "proxy_ws_connections",
        "proxy_jobs_total",
        "proxy_backfills_queued_total",
        "proxy_refresh_claims_total",
        "proxy_ladder_pages_total",
        "proxy_ladder_entries_total",
        "proxy_ladder_match_ids_total",
        "proxy_ladder_matches_queued_total",
        "proxy_ladder_crawl_duration_seconds",
        "proxy_aggregate_runs_total",
        "proxy_aggregate_duration_seconds",
        "proxy_aggregate_rows",
        "proxy_facts_reextract_progress",
        "proxy_archived_matches_total",
    ];

    #[test]
    fn catalogue_covers_every_v1_metric() {
        for name in V1_NAMES {
            assert!(
                CATALOGUE.iter().any(|m| m.name == *name),
                "{name} missing from CATALOGUE"
            );
        }
    }

    #[test]
    fn histograms_have_buckets_and_nothing_else_does() {
        for m in CATALOGUE {
            assert_eq!(m.kind == Kind::Histogram, !m.buckets.is_empty(), "{}", m.name);
            assert!(
                m.buckets.windows(2).all(|w| w[0] < w[1]),
                "{} buckets not ascending",
                m.name
            );
        }
    }

    #[test]
    fn metrics_md_documents_every_metric() {
        let doc = include_str!("../docs/design/metrics.md");
        for m in CATALOGUE {
            let kind = format!("{:?}", m.kind).to_lowercase();
            let labels = if m.labels.is_empty() {
                "—".to_string()
            } else {
                m.labels.join(", ")
            };
            let row = format!("| `{}` | {} | {} |", m.name, kind, labels);
            assert!(doc.contains(&row), "metrics.md is missing the row:\n{row}");
        }
    }
}

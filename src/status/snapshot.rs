//! `/status`'s `metrics` (design/14 §Metrics): flat snake_case keys read from
//! one rendering of the Prometheus recorder, so `/status` and `/metrics`
//! cannot disagree, plus L1's two gauges. Every fixed key is present from
//! boot (0 until something counts); a consumer's keys appear with its first
//! request.

use std::collections::BTreeMap;

use crate::jobs::kinds;
use crate::metrics::{CACHE_READS_TOTAL, CONSUMER_REQUESTS_TOTAL, JOBS_PENDING, UPSTREAM_REQUESTS_TOTAL};
use crate::telemetry::parse_counter;

/// `consumer_requests_total`'s groups, in design/14's order.
pub const GROUPS: [&str; 9] = [
    "riot",
    "lol",
    "analytics",
    "players",
    "admin",
    "ws",
    "ops",
    "ui",
    "other",
];

/// The job kinds `fetch_queue_depth` counts: those that fetch matches and
/// timelines.
pub const FETCH_KINDS: [&str; 2] = [kinds::ARCHIVE_MATCH, kinds::TIMELINES_BACKFILL];

/// What `metrics` is built from besides the rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cache {
    pub entries: u64,
    pub bytes: u64,
}

/// A consumer or scope name as a key suffix: lowercased, every character
/// outside `[a-z0-9]` turned into `_`.
pub fn key_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A consumer's suffix. One whose name flattens to a route group's would
/// share that group's keys, so it gets `consumer_` in front (ADR-205).
fn consumer_suffix(name: &str) -> String {
    let flat = key_name(name);
    if GROUPS.contains(&flat.as_str()) {
        format!("consumer_{flat}")
    } else {
        flat
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 {
        v.round() as u64
    } else {
        0
    }
}

/// `metrics` from `text`, one `PrometheusHandle::render()`.
pub fn metrics(text: &str, cache: Cache) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut add = |key: String, v: u64| *out.entry(key).or_insert(0) += v;

    add("requests_total".into(), 0);
    add("requests_errors_total".into(), 0);
    for g in GROUPS {
        add(format!("requests_total_{g}"), 0);
        add(format!("requests_errors_total_{g}"), 0);
    }
    for (labels, v) in parse_counter(text, CONSUMER_REQUESTS_TOTAL) {
        let n = whole(v);
        let label = |k: &str| labels.get(k).map_or("", String::as_str);
        let error = label("class") == "5xx";
        let group = match label("group") {
            g if GROUPS.contains(&g) => g,
            _ => "other",
        };
        let consumer = consumer_suffix(label("consumer"));
        let errors = if error { n } else { 0 };
        add("requests_total".into(), n);
        add("requests_errors_total".into(), errors);
        add(format!("requests_total_{group}"), n);
        add(format!("requests_errors_total_{group}"), errors);
        add(format!("requests_total_{consumer}"), n);
        add(format!("requests_errors_total_{consumer}"), errors);
    }

    let riot = [
        "riot_requests_total",
        "riot_requests_2xx_total",
        "riot_requests_4xx_total",
        "riot_requests_5xx_total",
        "riot_errors_total",
        "riot_429_total",
        "riot_403_total",
        "riot_401_total",
        "matches_fetched_total",
        "timelines_fetched_total",
    ];
    for key in riot {
        add(key.into(), 0);
    }
    for (labels, v) in parse_counter(text, UPSTREAM_REQUESTS_TOTAL) {
        let n = whole(v);
        // A status code, or `network` for a request that never completed.
        let status: Option<u16> = labels.get("status").and_then(|s| s.parse().ok());
        let ok = status.is_some_and(|s| (200..300).contains(&s));
        add("riot_requests_total".into(), n);
        match status {
            Some(200..=299) => add("riot_requests_2xx_total".into(), n),
            Some(400..=499) => add("riot_requests_4xx_total".into(), n),
            Some(500..=599) => add("riot_requests_5xx_total".into(), n),
            _ => {}
        }
        if !ok {
            add("riot_errors_total".into(), n);
        }
        match status {
            Some(429) => add("riot_429_total".into(), n),
            Some(403) => add("riot_403_total".into(), n),
            Some(401) => add("riot_401_total".into(), n),
            _ => {}
        }
        match (ok, labels.get("method").map(String::as_str)) {
            (true, Some("match.byId")) => add("matches_fetched_total".into(), n),
            (true, Some("match.timeline")) => add("timelines_fetched_total".into(), n),
            _ => {}
        }
    }

    add("cache_hits_total".into(), 0);
    add("cache_misses_total".into(), 0);
    for (labels, v) in parse_counter(text, CACHE_READS_TOTAL) {
        match labels.get("state").map(String::as_str) {
            Some("hit") => add("cache_hits_total".into(), whole(v)),
            Some("miss") => add("cache_misses_total".into(), whole(v)),
            _ => {}
        }
    }
    add("cache_entries".into(), cache.entries);
    add("cache_bytes".into(), cache.bytes);

    add("fetch_queue_depth".into(), 0);
    for (labels, v) in parse_counter(text, JOBS_PENDING) {
        if labels
            .get("kind")
            .is_some_and(|k| FETCH_KINDS.contains(&k.as_str()))
        {
            add("fetch_queue_depth".into(), whole(v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "\
# TYPE consumer_requests_total counter
consumer_requests_total{consumer=\"ninjagoldfinch-lol-prod\",group=\"players\",class=\"2xx\"} 10
consumer_requests_total{consumer=\"ninjagoldfinch-lol-prod\",group=\"riot\",class=\"5xx\"} 2
consumer_requests_total{consumer=\"anonymous\",group=\"ops\",class=\"2xx\"} 5
consumer_requests_total{consumer=\"Admin\",group=\"admin\",class=\"4xx\"} 1
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"200\"} 7
proxy_upstream_requests_total{region=\"europe\",method=\"match.byId\",status=\"200\"} 4
proxy_upstream_requests_total{region=\"europe\",method=\"match.timeline\",status=\"200\"} 3
proxy_upstream_requests_total{region=\"europe\",method=\"match.timeline\",status=\"404\"} 1
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"429\"} 2
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"403\"} 1
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"401\"} 1
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"503\"} 2
proxy_upstream_requests_total{region=\"euw1\",method=\"summoner.byPuuid\",status=\"network\"} 3
proxy_cache_reads_total{state=\"hit\"} 12
proxy_cache_reads_total{state=\"miss\"} 3
proxy_cache_reads_total{state=\"stale\"} 9
jobs_pending{kind=\"archive:match\"} 40
jobs_pending{kind=\"timelines:backfill\"} 2
jobs_pending{kind=\"ladder:walk\"} 6
";

    fn get(m: &BTreeMap<String, u64>, k: &str) -> u64 {
        *m.get(k).unwrap_or_else(|| panic!("{k} missing from {m:?}"))
    }

    #[test]
    fn keys_flatten_to_snake_case() {
        assert_eq!(key_name("ninjagoldfinch-lol-prod"), "ninjagoldfinch_lol_prod");
        assert_eq!(key_name("Bootstrap Admin.2"), "bootstrap_admin_2");
        assert_eq!(key_name("oc1"), "oc1");
        assert_eq!(key_name("é"), "_");
    }

    #[test]
    fn requests_by_consumer_and_group() {
        let m = metrics(TEXT, Cache::default());
        assert_eq!(get(&m, "requests_total"), 18);
        assert_eq!(get(&m, "requests_errors_total"), 2);
        assert_eq!(get(&m, "requests_total_ninjagoldfinch_lol_prod"), 12);
        assert_eq!(get(&m, "requests_errors_total_ninjagoldfinch_lol_prod"), 2);
        assert_eq!(get(&m, "requests_total_anonymous"), 5);
        assert_eq!(get(&m, "requests_total_players"), 10);
        assert_eq!(get(&m, "requests_errors_total_riot"), 2);
        assert_eq!(get(&m, "requests_total_ops"), 5);
        // A consumer named like a group keeps its own keys.
        assert_eq!(get(&m, "requests_total_admin"), 1);
        assert_eq!(get(&m, "requests_total_consumer_admin"), 1);
        // Every group is present before it has a request.
        assert_eq!(get(&m, "requests_total_ws"), 0);
        assert_eq!(get(&m, "requests_errors_total_other"), 0);
    }

    #[test]
    fn riot_counts_by_status() {
        let m = metrics(TEXT, Cache::default());
        assert_eq!(get(&m, "riot_requests_total"), 24);
        assert_eq!(get(&m, "riot_requests_2xx_total"), 14);
        assert_eq!(get(&m, "riot_requests_4xx_total"), 5);
        assert_eq!(get(&m, "riot_requests_5xx_total"), 2);
        // Every non-2xx, network failures included.
        assert_eq!(get(&m, "riot_errors_total"), 10);
        assert_eq!(get(&m, "riot_429_total"), 2);
        assert_eq!(get(&m, "riot_403_total"), 1);
        assert_eq!(get(&m, "riot_401_total"), 1);
        assert_eq!(get(&m, "matches_fetched_total"), 4);
        assert_eq!(get(&m, "timelines_fetched_total"), 3);
    }

    #[test]
    fn cache_and_queue() {
        let m = metrics(
            TEXT,
            Cache {
                entries: 9,
                bytes: 4096,
            },
        );
        assert_eq!(get(&m, "cache_hits_total"), 12);
        assert_eq!(get(&m, "cache_misses_total"), 3);
        assert_eq!(get(&m, "cache_entries"), 9);
        assert_eq!(get(&m, "cache_bytes"), 4096);
        assert_eq!(get(&m, "fetch_queue_depth"), 42);
    }

    /// Before any traffic every fixed key reads 0, so the shape never changes.
    #[test]
    fn an_empty_recorder_gives_every_fixed_key() {
        let m = metrics("", Cache::default());
        for k in [
            "requests_total",
            "requests_errors_total",
            "riot_requests_total",
            "riot_errors_total",
            "riot_429_total",
            "cache_hits_total",
            "cache_entries",
            "fetch_queue_depth",
            "matches_fetched_total",
            "timelines_fetched_total",
        ] {
            assert_eq!(get(&m, k), 0, "{k}");
        }
        assert_eq!(m.len(), 2 + GROUPS.len() * 2 + 10 + 4 + 1);
    }
}

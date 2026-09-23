# Metrics

`/metrics` serves Prometheus text format (`text/plain; version=0.0.4`). Every v1 name, type, label set and histogram bucket list is kept byte-identical (source: v1 `src/metrics.ts` at `c86e631`), so `ops/grafana/riot-proxy-dashboard.json` and `ops/prometheus-alerts.yml` import unchanged (design/07 §Observability).

The catalogue lives in `src/metrics.rs` (`CATALOGUE`). A unit test fails if a catalogue entry has no matching row here.

## v1 metrics (carried over)

| Name | Type | Labels | Buckets (s) | Emitted by (v2 task) |
|---|---|---|---|---|
| `proxy_requests_total` | counter | route, status, cache | — | HTTP layer (P4-04) |
| `proxy_upstream_requests_total` | counter | region, method, status | — | Riot client (P1-03) |
| `proxy_upstream_latency_seconds` | histogram | region, method | 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10 | Riot client (P1-03) |
| `proxy_rl_wait_seconds` | histogram | region, priority | 0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10 | Limiter (P2-03) |
| `proxy_rl_429_total` | counter | region, type | — | Limiter observe (P2-04) |
| `proxy_cache_reads_total` | counter | state | — | Fetcher (P3-05); `state` ∈ hit, miss, neg, stale |
| `proxy_ws_connections` | gauge | — | — | WS hub (P6-01) |
| `proxy_jobs_total` | counter | job, status | — | Scheduler (P6-03) |
| `proxy_backfills_queued_total` | counter | reason, status | — | Backfill enqueue (P6-06) |
| `proxy_refresh_claims_total` | counter | part, outcome | — | `?refresh=true` (P5-04) |
| `proxy_ladder_pages_total` | counter | platform, queue | — | Ladder (P7-02) |
| `proxy_ladder_entries_total` | counter | platform, queue | — | Ladder (P7-02) |
| `proxy_ladder_match_ids_total` | counter | platform, queue | — | Ladder collect (P7-03) |
| `proxy_ladder_matches_queued_total` | counter | platform, queue | — | Ladder archive (P7-03) |
| `proxy_ladder_crawl_duration_seconds` | histogram | platform, queue, status | 10, 60, 300, 900, 1800, 3600, 7200, 14400, 28800 | Ladder (P7-03) |
| `proxy_aggregate_runs_total` | counter | platform, queue, status | — | Analytics (P7-04) |
| `proxy_aggregate_duration_seconds` | histogram | platform, queue, step | 1, 5, 15, 30, 60, 120, 300, 600, 1200 | Analytics (P7-04) |
| `proxy_aggregate_rows` | gauge | platform, queue, table | — | Analytics (P7-04) |
| `proxy_facts_reextract_progress` | gauge | — | — | `facts:reextract` (P7-04) |
| `proxy_archived_matches_total` | counter | — | — | Archive (P5-02) |

Label-less metrics are registered at 0 on boot, as prom-client did. Labelled metrics appear once their first series is recorded, which is also v1 behaviour.

## Not carried over

| v1 | Why |
|---|---|
| `proxy_node_*` | prom-client's Node.js runtime metrics (`collectDefaultMetrics({prefix: 'proxy_node_'})`). There is no Node runtime in v2. Neither the Grafana board nor the alerts use them. |
| `proxy_cache_hit_ratio` | Only in the v1 *spec* (§13). v1 code replaced it with the `proxy_cache_reads_total` counter pair. |

## v2 additions (pending)

design/07 §Observability asks for `jobs_pending{kind}`, `limiter_bulk_waiters` and `sqlite_wal_bytes`, and P2-05 names `limiter_interactive_waiters`. The design writes these without v1's `proxy_` prefix. The prefix is decided when the first one is implemented (P2-05) and recorded here.

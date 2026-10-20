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
| `proxy_rl_429_total` | counter | region, type | — | Riot client, one per 429 received; `type` = `X-Rate-Limit-Type` or `service` (P1-03) |
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
| `proxy_rank_lookups_total` | counter | platform, queue | — | Rank lookups (DEV-29) |
| `proxy_aggregate_runs_total` | counter | platform, queue, status, kind | — | Analytics (P7-04); `kind` (`rebuild`, `delta`) since THR-03 |
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

## v2 additions

Owner decision (2026-09-24, ADR-014): these use the names **exactly as design/07 writes them, without v1's `proxy_` prefix**. Each gets a catalogue entry and a row here when its task implements it.

| Name | Type | Labels | Emitted by (v2 task) |
|---|---|---|---|
| `limiter_bulk_waiters` | gauge | — | Limiter priorities (P2-05), implemented; process-wide total |
| `limiter_interactive_waiters` | gauge | — | Limiter priorities (P2-05), implemented; process-wide total |
| `jobs_pending` | gauge | kind | Scheduler (P6-03), implemented; sampled every 15 s, a kind with nothing pending reads 0 |
| `events_published_total` | counter | name | Events (P6-02), implemented; one per event handed to the hub, whether or not anyone holds its topic |
| `sqlite_wal_bytes` | gauge | — | Disk sampler (DSK-01), implemented; the `-wal` file's size, 0 after a truncating checkpoint. Sampled with the metrics history (`METRICS_HISTORY_INTERVAL_S`, 60 s) and once at start |
| `sqlite_db_bytes` | gauge | — | Disk sampler (DSK-01), implemented; the database file's size |
| `sqlite_freelist_bytes` | gauge | — | Disk sampler (DSK-01), implemented; `freelist_count × page_size`, pages freed by deletes and not yet reused. They leave the file only with a `VACUUM` (design/07 §Reclaiming space), so a purge shows here, not in `sqlite_db_bytes` |
| `data_dir_free_bytes` | gauge | — | Disk sampler (DSK-01), implemented; free space on the data directory's file system as an unprivileged writer sees it (`statvfs` `f_bavail`, `df`'s *Avail*) |
| `cache_l2_dropped_total` | counter | — | L2 write-behind (ADM-04), implemented; entries not persisted because the 10,000-entry queue was full. L1 still holds them; a rising count means SQLite's writer is falling behind |
| `archive_offload_pending` | gauge | kind, step | `archive:offload` (OBJ-03, ADR-164), implemented, set at the end of each run; `step="pack"`: bodies not yet in a pack; `step="free"`: packed bodies past `ARCHIVE_BODY_LOCAL_DAYS` still held in SQLite. `kind` ∈ match, timeline |
| `archive_offload_bytes_total` | counter | kind | `archive:offload` (OBJ-03), implemented; bytes of packs uploaded and verified |
| `archive_packs` | gauge | — | `archive:offload` (OBJ-03), implemented, set at the end of each run; rows in `body_packs` |
| `sqlite_readers_free` | gauge | — | SQLite reader pool (INC-01), implemented; idle read connections, set on every borrow and return. A value that stays below the pool size while idle is a leak (ADR-113) |
| `sqlite_write_seconds` | histogram | — | SQLite writer thread (WR-02, ADR-180), implemented; how long each write ran on the writer, queue wait excluded. Buckets 0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10, 30, 60. A write over 1 s also logs `slow SQLite write` |
| `sqlite_write_wait_seconds` | histogram | — | SQLite writer thread (WR-02), implemented; how long each write waited in the writer's queue before it ran. Same buckets as `sqlite_write_seconds` |
| `sqlite_write_queue` | gauge | — | SQLite writer thread (WR-02), implemented; writes queued behind the running one, set on every enqueue and completion (queue size 1024) |
| `aggregate_delta_matches_total` | counter | platform, queue, kind | `aggregate:delta` (THR-03, ADR-182), implemented; matches a batch counted (`kind="added"`) or took a count back for first, after their tier moved (`kind="retracted"`) |
| `aggregate_delta_batch_seconds` | histogram | platform, queue, part | `aggregate:delta` (THR-03), implemented; one batch's write transaction, its statements (`part="sql"`, what the batch size follows) and its `COMMIT` (`part="commit"`, where a WAL checkpoint lands). Buckets 0.01, 0.025, 0.05, 0.1, 0.15, 0.25, 0.5, 1, 2, 5 |
| `aggregate_delta_batch_size` | gauge | — | `aggregate:delta` (THR-03), implemented; matches in the last batch, sized to `AGGREGATE_DELTA_TARGET_MS` within 5 and `AGGREGATE_DELTA_BATCH` |
| `aggregate_delta_pending` | gauge | — | `aggregate:delta` (THR-03), implemented; matches no rebuild or delta has fully counted (`analytics_counted.bits <> 63`), at the start of the last run |
| `timeline_backfill_fetched_total` | counter | region | `timelines:backfill` (TL-02), implemented; timelines fetched and archived by the backfill. Left to do: `GET /v1/admin/timelines/backfill` (ADR-133) |
| `timeline_backfill_not_found_total` | counter | region | `timelines:backfill` (TL-02), implemented; timelines Riot answered 404 for, marked in `timeline_gaps` and never asked for again |

Labels are provisional until the implementing task confirms them. `limiter_interactive_waiters` comes from P2-05 and `events_published_total` from the plan's P6-02; the others from design/07 §Observability.

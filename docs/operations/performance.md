# Performance

Where riot-proxy spends time, the measurements that exist, the knobs that move them, and what the host can and cannot change. Numbers are quoted from the ADRs and design documents that recorded them, with the environment each was taken in; nothing here is extrapolated without saying so.

## Known hot paths

**Analytics reads (the PERF series).** `GET /v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&patch=all&minGames=0&limit=500`, the site's call, took 3.9 s on production in October 2026 (`docs/DECISIONS.md:1734`). Three changes fixed it: every read now seeks on its key (PERF-01, ADR-167), tierless reads sum a tier-summed table `champion_stats_totals` (PERF-02, ADR-168), and the five analytics routes keep each 200's bytes and `ETag` in memory per aggregate generation (PERF-03, ADR-169). A hit, 200 or 304, runs no `Db::read` and sends `X-Cache: HIT`; a miss sends `MISS`; the cache is cleared and warmed by every rebuild (`docs/DECISIONS.md:1782-1799`; `src/routes/analytics/cache.rs:1-19`). Concurrent misses share one computation (`SingleFlight`, `:1791`). A rebuild warms 5 entries per queue with 18 reads, 93 ms on the synthetic copy (`:1799`); champion pages are not warmed and fill on first view.

**The player page composite.** `/v1/players/{puuid}/profile` fans out to four Riot reads at once (`account.byPuuid`, `summoner.byPuuid`, `league.entriesByPuuid`, `mastery.topByPuuid`; `src/routes/players.rs:285-324`), each through the limiter with the 2 s interactive budget (`CLIENT_WAIT_BUDGET_MS`, `src/config.rs:716`); the page's latency is the slowest part. `/v1/players/{puuid}/matches` fetches `match.byId` for every id not already archived (`:725-746`), so a cold player costs up to `count` Riot calls and a warm one none.

**Archive reads.** A match inside `ARCHIVE_BODY_LOCAL_DAYS` (14) is a local zstd blob; outside it the body comes from its pack by ranged GET, a few of them at 1 to 3 ms each in the same data centre (`docs/design/12-object-storage-and-hosting.md:261-267`), fetched one `get_ranges` call per pack with `ARCHIVE_OBJECT_CONCURRENCY` packs in flight (`src/archive/matches.rs:250-254`). The player page reads always fall inside the window (`:261`).

**`aggregate:analytics`.** One rebuild of one ladder took about 30 s alone on the old 2-vCPU box; four started together beside archiving had not finished after 7 minutes (`docs/design/07-deployment.md:171`). It is CPU and writer bound: three write transactions in a row (`src/jobs/analytics.rs:247-385`), capped to one at a time by `JOB_KIND_LIMITS` (`src/config.rs:111`).

**SQLite's single writer.** Every write goes through one thread and a 1024-deep queue (`src/db/mod.rs:41-42`, `:158-163`); a rebuild, `facts:reextract`, `builds:extract` and every archived match take turns on it (`docs/design/07-deployment.md:171`). `facts:reextract` decodes bodies on a blocking thread in batches of `FACTS_REEXTRACT_BATCH` with a 50 ms pause (`src/jobs/analytics.rs:404-477`); `builds:extract` does 25 timelines per batch (`:488-580`). While readers are always active the WAL does not checkpoint (`docs/design/07-deployment.md:171`), which is why `JOB_CONCURRENCY` matters more than core count on a small host.

## Benchmarks that exist

| What | Environment | Result | Source |
|---|---|---|---|
| `stat_rows` for the site's slice, before PERF-01 | synthetic copy (375,608 `champion_stats` rows), SQLite 3.46, with `ANALYZE` | 710 ms | `docs/DECISIONS.md:1737-1746` |
| same, after PERF-01 (seeking `IN` lists) | same | 208 ms; one platform + one patch 504 → 14 ms | `docs/DECISIONS.md:1755` |
| same, after PERF-02 (`champion_stats_totals`) | same, warm | 46 ms on 3.46; 38 ms on 3.51 (the bundled version) | `docs/DECISIONS.md:1767-1775` |
| `patches` for the site's slice, before → after PERF-02 | same | 93 → 30 ms (3.46); 87 → 25 ms (3.51) | `docs/DECISIONS.md:1771` |
| V0025 migration on the copy | this container | 1.9 s (3.46), 1.4 s (3.51); 138,102 rows, 13.8 MiB | `docs/DECISIONS.md:1776` |
| the site's call, p50, after PERF-03 | release build, synthetic copy, 30 requests | 41.9 ms → 0.5 ms; its 304 41.2 → 0.4 ms | `docs/DECISIONS.md:1802` |
| the same call, debug build, cold | synthetic copy | 78 ms, 40,295 B | `docs/perf/PERF-03-design.md:100-108` |
| 10 concurrent cold requests | same | 5 reads, not 50 | `docs/DECISIONS.md:1791` |
| `fio` 4 KiB writes with fsync | `ngf-prod-syd1` (6 vCPU) | 3 ms median, 9 ms p99, 326 synced writes/s; cold random reads about 10 MB/s | `docs/design/12-object-storage-and-hosting.md:781-786` |
| 8 workers on 2 vCPU | old OVH box | CPU about 1 % idle, PSI `some avg60` near 48 % | `docs/design/07-deployment.md:171` |

The synthetic database and the before/after SQL are reproducible with `python3 scripts/perf-synthetic.py /tmp/perf/s.db` (seed 1; `PERF_SQLITE=pysqlite3` runs it on SQLite 3.51), not run by CI (`scripts/perf-synthetic.py:1-24`). Production latency per route is read from the dashboard's Traffic tab, built on the recent-requests ring; there is no latency metric per route on `/metrics` (`docs/DECISIONS.md:1749`; `src/http/recent.rs:36`).

> **TODO (unverified):** the production `/champions` latency after PERF-03 is not recorded in the repository (PROGRESS.md marks PERF-03 done, `PROGRESS.md:366`, without a number).

## Tuning knobs

| Knob | Default (range) | Moves | Evidence |
|---|---|---|---|
| `JOB_CONCURRENCY` | 8 (≥ 1) | worker tasks; the lever on CPU and writer contention. 2 on a 2-vCPU host | `src/config.rs:709`; `docs/design/07-deployment.md:171` |
| `JOB_KIND_LIMITS` | `aggregate:analytics=1,archive:offload=1` | running cap per lane-less kind; `none` lifts all | `src/config.rs:111`, `:710`, `:1108-1139` |
| `JOB_YIELD_BUDGET_MS` | 1000 (0 to 900 000) | how long a job's fetch waits for the limiter before freeing its worker | `src/config.rs:711`; `src/fetcher.rs:124-140` |
| `CLIENT_WAIT_BUDGET_MS` | 2000 (≥ 0) | how long an interactive request waits for a token before `RATE_LIMITED` or a `STALE` copy | `src/config.rs:716`; `src/fetcher.rs:142-150` |
| `BULK_USAGE_CEILING` / `BULK_IDLE_CEILING` / `BULK_IDLE_AFTER_S` | 0.8 / 0.95 / 30 | bulk's share of each window while a scope is busy / after it idles / the idle threshold | `src/config.rs:717-719`, `:770-783` |
| `CACHE_L1_MAX_MB` | 128 (1 to 1 048 576) | L1 weight budget; an entry larger than the budget is not kept | `src/config.rs:713`; `src/cache/l1.rs:106-119` |
| `ANALYTICS_CACHE_ENTRIES` | 512 (0 to 4096) | analytics responses kept; about 41 kB each at most, so about 21 MB at 512 and 170 MB at 4096; 0 disables | `src/config.rs:754`; `docs/DECISIONS.md:1800` |
| `ARCHIVE_OBJECT_CONCURRENCY` | 16 (1 to 256) | ranged GETs in flight when reading offloaded bodies | `src/config.rs:735` |
| `ARCHIVE_BODY_LOCAL_DAYS` | 14 (0 to 365) | how long a packed body stays local, so which reads touch the bucket | `src/config.rs:736` |
| `ARCHIVE_PACK_TARGET_BYTES` | 64 MiB (1 MiB to 1 GiB) | pack size, so object count and when an offload is queued | `src/config.rs:737` |
| `ARCHIVE_OFFLOAD_PACE_MS` | 50 (0 to 60 000) | pause between offload turns, a brake on writer and bandwidth use | `src/config.rs:738` |
| `LADDER_BACKFILL_LIMIT` | 100 (0 to 10 000) | match ids walked per discovered player: one Riot call per 100 | `src/config.rs:748` |
| `AGGREGATE_PATCH_LIMIT` | 4 (0 to 100) | patches rebuilt per recompute; 0 is every patch | `src/config.rs:753` |
| tokio worker threads | `min(cores, 4)`, not configurable | async executors; 4 on the 6-vCPU box | `src/cli/mod.rs:125` |
| SQLite reader pool | `available_parallelism()`, fallback 4, not configurable | concurrent reads; 6 on `ngf-prod-syd1` | `src/db/mod.rs:201-202`; `PROGRESS.md:325` |
| SQLite `cache_size` / `mmap_size` | 64 MiB / 256 MiB per connection, not configurable | page cache and memory-mapped I/O | `src/db/mod.rs:31-32` |

Every env knob is read at boot; change the file, restart, and watch `/v1/admin/jobs/queue`, `sqlite_wal_bytes` and `limiter_*_waiters`.

## What the Riot key bounds

The limiter starts every scope at the personal-key limits, 20 per second and 100 per 2 minutes, until Riot's headers say otherwise (`src/riot/limiter/headers.rs:31-42`; `src/riot/limiter/mod.rs:177-184`). That is about 0.83 requests a second sustained per scope, and the request rate "is pinned by the personal key", so bandwidth and core counts above that are irrelevant to throughput (`docs/design/12-object-storage-and-hosting.md:484-486`). Derived from those figures: archiving a match with its timeline is two calls, so one region fills at most about 0.42 matches a second, roughly 36 000 a day, whatever the host. Bulk work takes at most `BULK_USAGE_CEILING` of that while anyone is active and `BULK_IDLE_CEILING` otherwise (`src/riot/limiter/mod.rs:329-344`); interactive requests always get the rest. Throughput of cached and archived reads is the host's; throughput of anything that reaches Riot is Riot's.

## Capacity on the 6-vCPU box

Measured on `ngf-prod-syd1`: the storage numbers above, a 9 min 57 s seed `rsync` at about 37 MB/s, 10 GB of page cache an hour after the move, and `/readyz` with 6 of 6 readers free after cut-over (`docs/design/12-object-storage-and-hosting.md:781-799`; `PROGRESS.md:325`). ONI-4 was chosen for the 12 GB of RAM so the whole SQLite file sits in page cache, "which is what makes the analytics rebuilds and ladder queries fast" (`:480-482`).

Extrapolated, not measured: design/07's sizing table and its 2-vCPU worker advice (`docs/design/07-deployment.md:165-171`) predate this host. With 6 cores the default 8 workers no longer leave the CPU 1 % idle by that arithmetic, and the reader pool is 6 rather than 2, but no run on this box with crawls, rebuilds and archiving together has been recorded. The single writer is still one thread, so rebuild time per ladder should be near the 30 s figure rather than six times better.

> **TODO (unverified):** a recommended `JOB_CONCURRENCY` for the 6-vCPU host; the repository records only the 2-vCPU recommendation (`docs/design/07-deployment.md:171`), and production's current value is in `/etc/riot-proxy/env`, which was not read.

Disk and memory sizing for the archive and the bucket are in `../deployment/capacity.md`.

---
**Sources:** `docs/DECISIONS.md:1732-1818`, `docs/perf/PERF-03-design.md:1-120`, `docs/design/07-deployment.md:165-173`, `docs/design/12-object-storage-and-hosting.md:261-267,480-486,781-799`, `PROGRESS.md:325,366`, `scripts/perf-synthetic.py:1-24`, `src/routes/analytics/cache.rs:1-19`, `src/routes/players.rs:285-324,725-746`, `src/archive/matches.rs:250-254`, `src/jobs/analytics.rs:247-385,404-477,488-580`, `src/db/mod.rs:31-32,41-42,158-163,201-202`, `src/cli/mod.rs:125`, `src/config.rs:111,709-719,735-738,748,753-754,770-783,1108-1139`, `src/cache/l1.rs:106-119`, `src/fetcher.rs:124-150`, `src/riot/limiter/headers.rs:31-42`, `src/riot/limiter/mod.rs:177-184,329-344`, `src/http/recent.rs:36`
**Last verified:** 2026-10-20 against commit `b7b363c`

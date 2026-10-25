# Capacity

What the 6 vCPU / 12 GB / 120 GB / 6 TB host sustains with the site sharing it: the memory budget, the CPU budget, how fast the disk fills and what moves it, the bandwidth the Riot key allows, and the signals that say it is time to free space or scale. Every number cites its source; figures marked *estimate* are arithmetic on measured inputs, not measurements.

## The box

ONI-4: 6 vCPU, 12 GB RAM, 120 GB NVMe (115 GB usable), 6 TB/month transfer, A$42.52/month (`docs/design/12-object-storage-and-hosting.md:474`; `PROGRESS.md:325`). It was chosen for the RAM: the plan is for the whole SQLite file to sit in page cache, since the Riot key pins the request rate and makes cores and bandwidth above this irrelevant (`docs/design/12-object-storage-and-hosting.md:480-487`). Its storage is replicated network storage, about 3 ms median fsync and 10 MB/s cold random reads, which the page cache hides once warm; 10 GB of RAM was cache an hour after the move (`docs/design/12-object-storage-and-hosting.md:780-786`).

## Memory

### What the proxy allocates

| Component | Size | Evidence |
|---|---|---|
| SQLite page cache | `cache_size = -65536`, 64 MiB **per connection**. `serve` opens one writer plus `available_parallelism()` readers (6 on this box), and the daily backup opens one more; worst case about 7 × 64 MiB = 448 MiB if every connection fills its cache (*estimate*) | `src/db/mod.rs:28-34,195-198`; `src/jobs/maintenance.rs:57-76`; `PROGRESS.md:325` ("6 of 6 SQLite readers") |
| SQLite `mmap_size` | 256 MiB per connection, file-backed and shared with the kernel page cache; shows in RSS as file pages, not anonymous memory | `src/db/mod.rs:32` |
| L1 response cache | `CACHE_L1_MAX_MB`, default 128 MiB, weight-bounded | `src/config.rs:726`; `src/cache/l1.rs:117-118` |
| Analytics response cache | `ANALYTICS_CACHE_ENTRIES` 512 × about 40 kB per champions list ≈ 20 MB (*estimate*) | `src/config.rs:767`; `.env.example:144-146` |
| L2 write-behind queue | up to 10 000 entries, flushed every 2 s or 500 rows | `src/cache/l2.rs:24-28` |
| Auth cache | up to 10 000 entries | `src/http/auth.rs:206-209` |
| Live activity traces | 200 events per job, 200 finished traces | `src/jobs/activity.rs:40-42` |
| Job handlers | `JOB_CONCURRENCY` tasks; an analytics rebuild and `facts:reextract` work in batches (500 matches) on blocking threads | `src/jobs/analytics.rs:539-620`; `src/config.rs:765` |

The design puts the binary's RSS at "the SQLite page cache plus L1", tunable down to about 50 MB on a Pi, and sizes an Emerald-floor crawl with timelines at 1–2 GB (`docs/design/07-deployment.md:165-173`).

> **TODO (unverified):** the proxy's observed RSS on `ngf-prod-syd1` is not recorded in the repository. `ninja-monitor` samples `mem_anon_bytes` for `riot-proxy.service` every 15 s (`docs/_meta/scratch/siblings.md:186`) and the number is on its local API, which only an SSH forward reaches.

### The host budget

| Process | Alert limit | Evidence |
|---|---|---|
| `riot-proxy.service` | 2 GiB | `docs/design/12-object-storage-and-hosting.md:832-835` |
| `ninjagoldfinch-lol.service` | 1 GiB | same |
| `cloudflared.service` | 512 MiB | same |
| `ninja-monitor.service` | `MemoryMax=64M` | `docs/_meta/scratch/siblings.md:191` |
| host | alert when available memory falls below 15 % | `docs/design/12-object-storage-and-hosting.md:835-836` |

So about 3.6 GiB is spoken for by processes at their limits, leaving roughly 8 GiB for the kernel page cache that holds the database (*estimate*). That is the whole file once the reclaim brings it under 8 GB (`deploy/README.md:267-268`); it is not the whole file at the 53.8 GB recorded on 2026-10-18 (session note), when cold reads fall back to the 10 MB/s storage.

## CPU and threads

| Pool | Size | Evidence |
|---|---|---|
| tokio runtime | `min(available_parallelism, 4)` = 4 worker threads; "the workload is I/O; four workers are plenty" | `src/cli/mod.rs:123-126` |
| SQLite readers | `available_parallelism()` = 6 read-only connections, each `read` on `spawn_blocking` | `src/db/mod.rs:195-198,285-316` |
| SQLite writer | one dedicated thread, queue depth 1024; every write is serialised through it | `src/db/mod.rs:41-42,160-172` |
| Job workers | `JOB_CONCURRENCY`, default 8; production set 4 on 2026-10-13 (session note, `reports/OBJ implementation handoff.md:20`) | `src/config.rs:722` |
| Per-kind caps | `JOB_KIND_LIMITS` default `aggregate:analytics=1,archive:offload=1`: one rebuild and one offload at a time | `src/config.rs:114` |

The 2-vCPU story in `docs/design/07-deployment.md:171` is the sizing evidence: 8 workers left the CPU about 1 % idle with PSI `some avg60` near 48 %, four rebuilds started together had not finished after 7 minutes where one alone takes about 30 s, and the WAL never checkpointed because readers were always active. The remedy was `JOB_CONCURRENCY=2` on 2 vCPU with the default kind caps. On 6 vCPU the same handoff asked for `cat /proc/pressure/cpu` to stay well under 20 on `some avg60`, lowering `JOB_CONCURRENCY` to 2–3 otherwise (`reports/OBJ implementation handoff.md:24`). Requests are served outside the job pool, so fewer workers do not lower the API's throughput (`docs/design/07-deployment.md:171`). The site's Node process competes for the same cores; nothing in either repository measures its share.

> **TODO (unverified):** CPU utilisation or PSI readings on `ngf-prod-syd1` after the move; only the pre-move 2-vCPU figures are recorded.

## Disk

### Measured

| Date | Reading | Evidence |
|---|---|---|
| 2026-10-13 | 230,715 matches hold 2.46 GB of bodies (10.7 KB each); 216,475 timelines hold 13.50 GB (62.3 KB each); 16 GB of a 20 GB file is raw bodies; facts about 1 KB per participant | `docs/design/12-object-storage-and-hosting.md:459-463`; `PROGRESS.md:325` |
| growth per match | about 12 KB per match and 60–80 KB per timeline, zstd-compressed | `docs/design/12-object-storage-and-hosting.md:13-18` |
| 2026-10-13 | first backup on the new box 21.6 GB (10.6 GB the day before: the file roughly doubled in a day of timeline backfill); about 44 GB of 115 GB used | `docs/design/12-object-storage-and-hosting.md:827-828`; `docs/DECISIONS.md:1513`; `PROGRESS.md:325` |
| 2026-10-13 | first offload run: 300 packs (44 match, 256 timeline), 551,925 bodies, 20.15 GB, freed 0 (every match inside the 14-day window) | session note (`obj-packs`, 2026-10-14) |
| 2026-10-18 | 77 % of 116 GB used; `riot-proxy.db` 53.8 GB; WAL 629 MB; `backups/` 33 GB and stale because the nightly backup is skipped by the 1.2 × guard; free list 4 KB; free-pending 0 | session note (`obj-packs`, 2026-10-18 14:52 UTC) |

From 21.6 GB on 2026-10-13 to 53.8 GB on 2026-10-18 is about 6.4 GB/day with the crawl and bulk work on and the 14-day local window (session note, 2026-10-18).

### Arithmetic

- The key allows at most 72,000 requests a day (below). With `ARCHIVE_TIMELINES=true` a match costs two requests, so at most about 36,000 matches a day can be archived even if every request went to match bodies; at 10.7 + 62.3 KB that is about 2.6 GB/day of bodies, plus about 10 KB of facts per match (*estimate*). The measured 6.4 GB/day is higher; the difference (indexes, derived tables, the L2 `cache` table, WAL) is not accounted for in any source, so plan with the measured figure.
- The 14-day window keeps every body local for 14 days after its match was archived (`src/config.rs:749`; `docs/_meta/scratch/persistence.md:353-355`). At 6.4 GB/day that is about 90 GB of local bodies, which does not fit beside the rest on 115 GB; at the 2.6 GB/day body rate it is about 36 GB (*estimate*). The session note of 2026-10-18 advised `ARCHIVE_BODY_LOCAL_DAYS=3` plus a reclaim rather than a larger disk.
- The daily backup needs 1.2 × (file + WAL) free: 65 GB at 53.8 GB, more than the 27 GB free on 2026-10-18, so it skips, and the offsite copy has nothing new to upload (`src/jobs/maintenance.rs:34,294-306`). A manual `rp backup` needs the same room (`docs/design/07-deployment.md:189`).
- Freed bodies leave their pages on the free list (`sqlite_freelist_bytes`); later writes reuse them, so the file stops growing but does not shrink until a `VACUUM INTO` copy is swapped in, which needs free space of about the live data's size (`docs/design/07-deployment.md:185-206`). After the reclaim, expect `sqlite_db_bytes` under 8 GB (`deploy/README.md:267-268`).
- The bucket: 250 GB allocated, upgradable, not downgradable; 3 M objects (`docs/design/12-object-storage-and-hosting.md:475`). From 20 GB at 2.6 GB/day the allocation lasts about 90 days (*estimate*); the design's own table puts a 1.1 M-match backlog at about 100 GB with timelines (`docs/design/12-object-storage-and-hosting.md:464-470`). Packs are never expired (`deploy/README.md:36`).

### The levers

| Lever | Effect | Evidence |
|---|---|---|
| `ARCHIVE_BODY_LOCAL_DAYS` | the main one: how long a packed body stays in SQLite; `0` frees every packed body on the next run | `src/config.rs:749`; `docs/_meta/scratch/persistence.md:358` |
| `archive:offload` | packs a kind as soon as it has 64 MiB of unpacked bodies (`ARCHIVE_PACK_TARGET_BYTES`), at boot and after each `maintenance`; frees past the window in batches of 2 000 | `docs/_meta/scratch/persistence.md:350-356` |
| `BACKUP_KEEP` | local backups kept; `1` on production | `src/config.rs:742`; `reports/OBJ implementation handoff.md:20` |
| `LADDER_CRAWL_S`, `LADDER_BACKFILL_LIMIT`, `ARCHIVE_TIMELINES` | how much the crawl archives per day; `LADDER_CRAWL_S=0` was the 2026-10-13 stop-gap for a full disk | `src/config.rs:740,754,761`; session note (`vps`, 2026-10-13) |
| the reclaim | one-off: `VACUUM INTO` copy and swap once free-pending is 0 | `deploy/README.md:222-270` |

## Bandwidth

The personal key's application limits are 100 requests per 120 s and 20 per 1 s, which is also the limiter's bootstrap (`docs/design/12-object-storage-and-hosting.md:484`; `docs/_meta/scratch/riot-and-cache.md:188`). Everything below is an *estimate* from those limits and the measured body sizes.

| Flow | Rate | Monthly |
|---|---|---|
| Riot requests | 100 / 120 s ≈ 0.83 req/s ≈ 72,000/day | ≈ 2.2 M requests |
| Riot ingress | ≤ 36,000 matches/day × (10.7 + 62.3) KB ≈ 2.6 GB/day, if wire bytes are close to the stored zstd bytes | ≈ 80 GB |
| Packs PUT to Onidel | the same bytes, once | ≈ 80 GB egress, same data centre |
| Replica, Onidel to R2 | `rclone` runs on the box, so each new pack is downloaded from Onidel and uploaded to R2 | ≈ 80 GB in + 80 GB out |
| Offsite backups to R2 | one backup file a day, uploaded once; the file is read about three times locally (hash, upload, check) at about 46 MB/s | ≈ 650 GB at 21.6 GB/file; ≈ 240 GB at 8 GB/file; nothing while the backup is skipped |
| Site traffic through the tunnel | not measured | unknown |

Onidel's egress is free up to 3 × the storage allocation, 750 GB/month at 250 GB (`docs/design/12-object-storage-and-hosting.md:475`); R2's ingress and egress are free and its writes cost about US$4.50 per million objects, a few hundred a month here (`docs/design/12-object-storage-and-hosting.md:366-369`). Adding the rows gives on the order of 1 TB/month at the worst backup size, against a 6 TB allowance: far from reached, as the design says (`docs/design/12-object-storage-and-hosting.md:484-486`).

> **TODO (unverified):** whether Onidel counts ingress as well as egress against the VPS's 6 TB, the site's own monthly traffic, and Riot's on-the-wire encoding (the 2.6 GB/day assumes the stored zstd size). None is in the repository; the 46 MB/s and "read three times" figures for the offsite job are a session note of 2026-10-13.

## Signals to free space or scale

All are on `/metrics` and in `GET /v1/admin/stats` (`docs/design/07-deployment.md:216`); `ninja-monitor` watches only the host and cgroups.

| Signal | Means | Act when | Evidence |
|---|---|---|---|
| `data_dir_free_bytes` | free space beside the database | below 1.2 × (`sqlite_db_bytes` + `sqlite_wal_bytes`): backups stop; `ninja-monitor` alerts at 70 % and 85 % disk | `src/db/disk.rs:73`; `src/jobs/maintenance.rs:34`; `docs/design/12-object-storage-and-hosting.md:836` |
| `sqlite_db_bytes` | file size | growing after the offload has caught up: shorten `ARCHIVE_BODY_LOCAL_DAYS`; above 8 GB with a large free list: reclaim | `src/db/disk.rs:70`; `deploy/README.md:267` |
| `sqlite_freelist_bytes` | pages freed but not returned | large and stable: the file can be made smaller by a reclaim | `src/db/disk.rs:72`; `docs/design/07-deployment.md:187` |
| `sqlite_wal_bytes` | WAL not checkpointing | keeps growing: readers always busy, too many workers | `src/db/disk.rs:71`; `docs/design/07-deployment.md:171` |
| `archive_offload_pending{kind,step}` | backlog to pack or free after each run | `pack` growing: the bucket is not keeping up; `free` 0 for both kinds: safe to reclaim | `src/jobs/offload.rs:204-207`; `deploy/README.md:224-226` |
| `archive_packs` | packs in the bucket | approaching the object cap (3 M) is not a concern at 64 MiB packs | `src/jobs/offload.rs:215` |
| `/readyz` `"sqlite":false` | idle writer: another process held the write lock for 1.5 s; busy writer: the running write made no progress and used no CPU for 5 s | a rebuild no longer causes it (WR-02); check for another process on the file, then treat a stalled writer as a restart signal (`../operations/troubleshooting.md` §1) | `src/routes/health.rs:21-46,148-200` |
| `limiter_interactive_waiters`, `limiter_bulk_waiters` | requests waiting for a Riot token | interactive waiters persistently above 0: bulk is taking the bucket; lower `BULK_USAGE_CEILING` | `src/riot/limiter/mod.rs:240-242`; `src/config.rs:730` |
| `jobs_pending{kind}` | queue depth per kind, sampled every 15 s | `archive:match` in the millions: the crawl outruns the key; the 2026-10-13 purge removed 1.11 M | `src/jobs/scheduler.rs:49,1226-1245`; `docs/design/12-object-storage-and-hosting.md:957-959` |
| PSI `/proc/pressure/cpu` `some avg60` | CPU contention | near 48 % was the 2-vCPU saturation; keep well under 20 | `docs/design/07-deployment.md:171`; `reports/OBJ implementation handoff.md:24` |
| `ninja-monitor` memory alerts | per-service anonymous memory | proxy above 2 GiB: look at L1, `cache_size` × connections and the job mix | `docs/design/12-object-storage-and-hosting.md:832-835` |

---
**Sources:** `docs/design/07-deployment.md:L165-L173,L185-L206,L216`, `docs/design/12-object-storage-and-hosting.md:L13-L18,L366-L369,L459-L487,L780-L786,L827-L836,L957-L959`, `deploy/README.md:L36,L222-L270`, `PROGRESS.md:L325`, `docs/DECISIONS.md:L1513`, `reports/OBJ implementation handoff.md:L20,L24`, `src/db/mod.rs:L28-L42,L160-L172,L195-L198,L285-L316`, `src/db/disk.rs:L70-L73`, `src/cli/mod.rs:L123-L126`, `src/config.rs:L114,L722,L726,L730,L740-L742,L749,L754,L761,L765,L767`, `src/cache/l1.rs:L117-L118`, `src/cache/l2.rs:L24-L28`, `src/http/auth.rs:L206-L209`, `src/jobs/activity.rs:L40-L42`, `src/jobs/maintenance.rs:L34,L57-L76,L294-L306`, `src/jobs/offload.rs:L204-L215`, `src/jobs/scheduler.rs:L49,L1226-L1245`, `src/jobs/analytics.rs:L539-L620`, `src/riot/limiter/mod.rs:L240-L242`, `src/routes/health.rs:L21-L46,L148-L200`, `.env.example:L144-L146`, `docs/_meta/scratch/persistence.md:L350-L358`, `docs/_meta/scratch/riot-and-cache.md:L188`, `docs/_meta/scratch/siblings.md:L186,L191`
**Last verified:** 2026-10-22 against commit `6eda84e`

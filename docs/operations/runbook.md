# Operations runbook

Day-two procedures for riot-proxy on `ngf-prod-syd1`: service control, logs, health, key and credential rotation, cache purges, analytics recomputes, and what to do when Riot, the object store, the disk or the job queue misbehave. Every step is one command or one checkable action; messages and limits cite the code that produces them.

Fixed facts used throughout: the unit is `riot-proxy.service` running `/usr/local/bin/riot-proxy serve` as user `riot-proxy` with `EnvironmentFile=/etc/riot-proxy/env` and `WorkingDirectory=/var/lib/riot-proxy` (`docs/design/07-deployment.md:74-95`); the database is `/var/lib/riot-proxy/data/riot-proxy.db`; the proxy listens on `127.0.0.1:8080`. Admin routes need a consumer key with the `admin` scope (`src/http/auth.rs:276-288`); on the box call them over loopback, because the WAF blocks `/v1/admin*` and `/metrics` on `api.ninjagoldfinch.lol` (`PROGRESS.md:342`). Set `ADMIN_KEY` in your shell from the owner's `admin-main` key; never paste a key into a command that lands in shell history you share.

Configuration is read once at start (`Config::load`, `src/cli/mod.rs:82-88`); there is no reload signal (`grep -rn SIGHUP src/` finds nothing), so every change to `/etc/riot-proxy/env` needs a restart.

```bash
rp() { sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
         /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data "$@"; }   # deploy/README.md:121-122
```

`rp` runs any CLI subcommand as the service user with the service's environment, so keys never appear on a command line.

## 1. Start, stop, restart, status

1. `sudo systemctl status riot-proxy` shows the unit and its last log lines.
2. `sudo systemctl restart riot-proxy` is the normal way to apply an env change or a new binary; the restart is under a second (`docs/design/07-deployment.md:229`). On SIGTERM the server logs `shutdown signal received; draining` with `grace_s=10`, finishes in-flight requests for at most 10 s (`src/app.rs:35`, `:217`, `:229`), lets running jobs finish for 10 s (`src/cli/serve.rs:418`), then writes a final limiter checkpoint and flushes L2 and logs `limiter checkpoint and L2 flushed; stopped` (`src/cli/serve.rs:404-412`). Jobs still running are left `running` and re-queued at the next boot (`src/jobs/scheduler.rs:1296`, `:963-973`).
3. `sudo systemctl stop riot-proxy` before anything that touches the database file or runs `packs restore-index` (`deploy/README.md:123-125`).
4. `sudo systemctl start riot-proxy`, then `curl -s localhost:8080/readyz` until `"ok":true`.
5. A boot that fails on configuration prints `error: Invalid environment configuration:` followed by one `  NAME: message` line each, exits 1 (`src/config.rs:118-119`, `801-805`; `src/cli/mod.rs:82-88`), and `Restart=always` / `RestartSec=2` (`docs/design/07-deployment.md:85-86`) makes it loop: fix the env file, then restart.

## 2. Reading logs

Logs are `tracing` JSON on stdout (`LOG_FORMAT` defaults to `json` when stdout is not a TTY, `src/config.rs:662-669`), one object per line with the innermost span under `span` (`src/telemetry.rs:53-66`). See `observability.md` for the full shape.

1. Follow: `sudo journalctl -u riot-proxy -f -o cat` (`-o cat` prints the raw JSON line).
2. Filter by request id (the `X-Request-Id` echoed on every response, `src/http/request_id.rs:64`): `sudo journalctl -u riot-proxy -o cat --since -1h | grep '"request_id":"01M58N5S2EJ1TCH49N26KVGW69"'`, or with jq: `… | jq -c 'select(.span.request_id=="01M58N5S2EJ1TCH49N26KVGW69")'`.
3. Only warnings and errors: `… | jq -c 'select(.level=="WARN" or .level=="ERROR")'`.
4. One job kind: `… | jq -c 'select(.target|startswith("riot_proxy::jobs"))'`.
5. Raise detail without a restart is not possible; `LOG_LEVEL` is an `EnvFilter` directive read at boot (`src/telemetry.rs:41-42`), for example `LOG_LEVEL=riot_proxy=debug,info`.
6. CLI commands run through `rp` install no log subscriber (only `serve` calls `init_tracing`, `src/cli/serve.rs:45`); their output is what they print.

## 3. Checking health

1. `curl -s localhost:8080/healthz` → `{"ok":true}`; it touches nothing (`src/routes/health.rs:59-72`).
2. `curl -s -i localhost:8080/readyz` → `{"ok":true,"sqlite":true,"limiter":true,"keyScope":"95b708b6","sqliteReaders":6,"sqliteReadersFree":6,"writerBusy":false,"writerBusyMs":0,"writerQueued":0}` (shape `src/routes/health.rs:73-97`; production reports 6 readers, `PROGRESS.md:325`). With the writer idle, `sqlite` means it executed `BEGIN IMMEDIATE; ROLLBACK;` within 2 s; with the writer busy (`writerBusy:true`, `writerBusyMs` its running time, `writerQueued` the writes behind it), that the running write made progress in the last 5 s, from SQLite's progress stamps or the writer thread's CPU clock (`src/routes/health.rs:148-200`; ADR-180). `limiter` means the checkpoint restore has run (`:123`). A 503 carries the same body (`:124-145`), so the booleans say what is wrong. A rebuild no longer makes `sqlite` false; `sqlite:false` with `writerBusy:true` is a stalled writer (see `troubleshooting.md` §1).
3. `curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/info | jq .` → version, `gitSha`, `builtAt`, `startedAt`, `uptimeSeconds`, `env`, `keyScope`, feature switches and the redacted configuration (`src/routes/admin.rs:2438-2477`). Compare `version` with `/usr/local/bin/riot-proxy --version` after an upgrade.
4. `… /v1/admin/stats | jq .` → archive counts, `archiveBodies{matches,timelines}{local,packed,offloaded}`, `dbFileBytes`, `walBytes`, `dbFreelistPages`, `diskFreeBytes` (`src/routes/admin.rs:890-924`).
5. `… /v1/admin/jobs/stats | jq .` → `kinds{kind→{pending,running,done,failed}}` and `totals` (`src/routes/admin.rs:1369-1393`). A growing `failed` total is the first thing to look at (§14).
6. `… /v1/admin/metrics | jq '{process,disk,sqlite,limiter,queues}'` for the dashboard's snapshot without opening the dashboard (`src/routes/admin.rs:2397-2399`).

## 4. Rotating the Riot key

The cache and most tables are scoped by `KeyScope`, the first 8 hex characters of `sha256(RIOT_API_KEY)` (`src/cache/keys.rs:31-37`): every L1/L2 key starts with it (`:72-90`) and `players`, `match_facts`, `ladder_entries`, `ladder_crawls`, `player_ranks`, `rank_lookups` and every analytics table carry a `key_scope` column (`docs/_meta/scratch/persistence.md` §2). The design states the consequence: rotate the key and the old rows are inert, `matches` and `timelines` (keyed by match id) are untouched, and tracked players are re-resolved by Riot ID through `POST /v1/admin/tracked-players` (`docs/design/04-data-and-cache.md:417-423`).

1. `sudo editor /etc/riot-proxy/env` and set the new `RIOT_API_KEY` (at least 8 characters, `src/config.rs:623-627`).
2. `sudo systemctl restart riot-proxy`.
3. `curl -s localhost:8080/readyz | jq .keyScope` shows the new scope; `/v1/admin/info` shows it too.
4. Expect: every passthrough read is a `MISS` until the new scope warms (L1 was lost at restart anyway; L2 rows under the old scope stay until their `hard_expires` sweep, `src/cache/l2.rs:219-223`, and never match); `GET /v1/admin/tracked-players` lists only the current scope (`src/routes/admin.rs:386-389`), so re-track each player by Riot ID; analytics reads for the new scope are empty until a rebuild under it: with `AGGREGATE_DELTA_S` > 0 boot queues one full rebuild per configured ladder for the unstamped scope (§19), otherwise `POST /v1/admin/analytics/recompute {"platform":"oc1"}`; and `/v1/admin/ladder/crawls` shows only the new scope (`src/routes/admin.rs:1848`).
5. Watch for `RIOT KEY REJECTED` (`src/riot/client.rs:305`) in the journal for a minute; none means the key is accepted.

> **TODO (unverified):** whether `match_facts` rows written under the old scope are ever re-derived under the new one. `facts:reextract` selects on `facts_version < 4` only (`src/jobs/analytics.rs:508-589`), and the ADR-1268 note says builds join facts "on the same scope" after a rotation (`docs/DECISIONS.md:1268`); no rotation procedure for facts or analytics is recorded in the repository.

## 5. Rotating or revoking consumer keys

Keys are `rpx_` + 36 characters, stored only as `sha256` (`src/consumers.rs:12`, `:95-107`); names are unique (`src/db/migrations/V0001__init.sql:5-13`), so a rotated key needs a new name.

1. Mint: `rp key create --name site-2026-10 --scopes read --quota 600` prints the key once (`src/cli/key.rs:46-57`); or `curl -s -X POST -H "Authorization: Bearer $ADMIN_KEY" -H 'Content-Type: application/json' -d '{"name":"site-2026-10","scopes":["read"]}' localhost:8080/v1/admin/consumers` (`src/routes/admin.rs:180-219`).
2. Put it in the consumer's config: for the site, `PROXY_KEY` in `/etc/ninjagoldfinch-lol/env`, then `sudo systemctl restart ninjagoldfinch-lol` (`docs/_meta/scratch/siblings.md` §1.8).
3. Check the site still answers: `curl -s http://127.0.0.1:3000/healthz`, then one real page.
4. Revoke the old key: `rp key list` to find its id, then `curl -s -X DELETE -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/consumers/<id>`. The route drops the key from the auth cache at once and closes that consumer's sockets with code 4401 (`src/routes/admin.rs:264-266`; `src/ws/socket.rs:40`). `rp key revoke <id-or-name>` works too but does not touch the running process's cache, so a found key stays valid for up to 60 s (`AUTH_CACHE_TTL`, `src/http/auth.rs:30`).
5. Verify: a request with the old key answers 401 `Missing or invalid API key` (`src/http/auth.rs:271-275`).

Rotating the owner's `admin-main` key is the same procedure with `--scopes read,admin`; update the dashboard's stored key afterwards.

## 6. Rotating object-storage keys

The proxy and `rclone` share the Onidel access key (`deploy/README.md:24-25`).

1. Create the new key in the Onidel console; keep the old one until step 5.
2. `sudo editor /etc/riot-proxy/env`: `ARCHIVE_OBJECT_ACCESS_KEY` and `ARCHIVE_OBJECT_SECRET_KEY` (both or neither, `src/config.rs:997-1000`).
3. `sudo editor /etc/riot-proxy/rclone.conf`: the `[onidel]` block (`deploy/README.md:51-62`).
4. `sudo systemctl restart riot-proxy`; the journal shows `object store configured store=s3://riot-proxy-archive at https://s3.ap-southeast-2.onidel.cloud` (`src/cli/serve.rs:101-104`; `src/archive/objects.rs:134`). A bad store fails the boot with context `ARCHIVE_OBJECT_URL` (`src/cli/serve.rs:101-105`).
5. Verify a write and a read: `rp packs snapshot` writes today's index snapshot and prints its keys (`src/cli/packs.rs:55-65`); `sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1` (`deploy/README.md:82`); and read an offloaded match as in `deploy/README.md:202`, expecting `X-Cache: ARCHIVE`. `GET /v1/admin/stats` reads SQLite only and does not prove the bucket works.
6. Delete the old key in the console.

## 7. Clearing caches

1. Dry run first: `curl -s -X POST -H "Authorization: Bearer $ADMIN_KEY" -H 'Content-Type: application/json' -d '{"pattern":"summoner.byPuuid:*","dryRun":true}' localhost:8080/v1/admin/cache/purge` → `{"ok":true,"pattern":…,"matched":n,"dryRun":true}` (`src/routes/admin.rs:838-882`). Patterns are Redis-style globs, scoped to the current key unless they already start with a scope (`src/cache/keys.rs:103-166`).
2. Without `dryRun` the call deletes from L1 and L2 (`src/routes/admin.rs:868-880`). The archive (`matches`, `timelines`) is never purged by this route (`:832-833`).
3. A restart empties L1 and re-warms it from L2 (`src/cli/serve.rs:85-89`); only the L2 endpoints (account, summoner, ladder, mastery) come back warm.
4. The analytics response cache has no purge route: it is cleared by every `aggregate:analytics` run, success or failure, and by every `aggregate:delta` run that counted something, and warmed again (`docs/DECISIONS.md:1783-1799`; `src/jobs/analytics.rs:782-787`); `POST /v1/admin/analytics/recompute` with `{"platform":"oc1"}` forces one.

## 8. A Riot outage or a suspended key

What you see: `error` lines `RIOT KEY REJECTED` on 401/403 (the full line names `RIOT_API_KEY` and says dev keys expire every 24 h, `src/riot/client.rs:305`), `warn` `upstream 5xx` (`:320`) or `network failure contacting Riot API` (`:348`). Consumers get 502 `UPSTREAM_ERROR` `Upstream authentication failed` or `Upstream request failed` (`src/riot/client.rs:176-183`), or a cached copy inside its hard TTL with `X-Cache: STALE` (`src/fetcher.rs:587-607`); archived matches keep answering `ARCHIVE`. Alert: `RiotKeyRejected` (`ops/prometheus-alerts.yml:6-14`).

1. `curl -s localhost:8080/metrics | grep proxy_upstream_requests_total` and read the `status` label: `401`/`403` is the key, `5..`/`network` is Riot or the network.
2. For 401/403: a personal key expires every 24 h (the log line says so); get a new one from the developer portal and follow §4.
3. For 5xx/network: nothing to do in the proxy; the fetcher retries twice (250 ms, 750 ms) then serves stale or fails (`src/fetcher.rs:34-35`, `:587-607`); jobs retry with backoff (§14). Confirm the box can reach Riot: `curl -sI https://oc1.api.riotgames.com/` from the host.
4. When Riot recovers, `job failed` warnings stop and `proxy_jobs_total{status="completed"}` moves again.

## 9. An accountable 429

The limiter logs `error` `accountable 429: limiter accounting is out of sync; scope frozen` with `scope`, `kind`, `retry_after_s` and freezes the whole scope for Riot's `Retry-After` (`src/riot/limiter/mod.rs:407-431`). Alert: `AccountableRateLimit429` (`ops/prometheus-alerts.yml:16-24`).

1. `curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/limits/oc1` → `{"scope","usage":[{"window":"20:1","used","limit"},{"window":"100:120",…}],"frozenMs"}` (`src/routes/admin.rs:1010-1027`); `frozenMs` counts down.
2. Wait it out: the freeze is never cleared early (`src/riot/limiter/mod.rs:315-320`), and interactive requests inside their 2 s budget get `RATE_LIMITED` with `Retry-After` meanwhile.
3. If it repeats on one scope, lower `BULK_USAGE_CEILING` (default `0.8`, `src/config.rs:730`) so bulk work leaves more headroom, restart.
4. A `service`-typed 429 is only a `warn` and is not accountable (`src/riot/limiter/mod.rs:424-428`).

## 10. An object-store outage

Reads of offloaded bodies log one `warn` ending in `reading from Riot` and fall through to Riot (`src/archive/matches.rs:295`, `:306`, `:317`; `src/archive/mod.rs:128-144`). `archive:offload` returns `Retry` on any store error (`src/jobs/offload.rs:147`) and backs off `30 s × 2^attempts`, fifth failure final (`src/jobs/scheduler.rs:40-42`, `:160-171`); `archive_offload_pending{kind,step}` grows (`src/jobs/offload.rs:204-207`). Nothing is lost: a local body is freed only after its pack is uploaded and verified (`src/archive/offload.rs:89-118`).

1. `sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1` tells the bucket from the proxy.
2. `curl -s -H "Authorization: Bearer $ADMIN_KEY" 'localhost:8080/v1/admin/jobs?kind=archive:offload&state=failed'` shows a run that gave up; retry it with `POST /v1/admin/jobs/{id}/retry` once the store is back, or wait: boot and every daily `maintenance` queue a fresh one (`src/cli/serve.rs:334-339`; `src/jobs/maintenance.rs:204-211`).
3. Reads from Riot during the outage spend Riot tokens; if the outage is long, expect `RATE_LIMITED` on cold archive reads.
4. Onidel gone for good: set `ARCHIVE_OBJECT_ENDPOINT=https://<account id>.r2.cloudflarestorage.com`, `ARCHIVE_OBJECT_REGION=auto` and the R2 keys, restart; nothing else changes (`deploy/README.md:140-142`). Then review `riot-proxy-replica.timer`, whose source is the Onidel bucket (`deploy/README.md:6`, `:15-16`).

> **TODO (unverified):** what the hourly `rclone sync` does when its Onidel source is unreachable (whether it can delete from R2); the repository does not say, so stop the timer before repointing if in doubt.

## 11. Disk full on the shared 120 GB volume

Signals: `data_dir_free_bytes` on `/metrics` (`src/db/disk.rs:73`), `diskFreeBytes` in `/v1/admin/stats`, `warn` `backup skipped: not enough disk` when free space is under 1.2 × (db + wal) (`src/jobs/maintenance.rs:34`, `:294-306`), ninja-monitor's `disk filling`. The database, its WAL, `backups/`, `ddragon/`, the site checkout and journald share the volume.

1. `df -h /var/lib/riot-proxy`; `sudo du -sh /var/lib/riot-proxy/data/backups /var/lib/riot-proxy/data/ddragon /opt/ninjagoldfinch-lol`; `sudo journalctl --disk-usage`.
2. Delete old backups: `BACKUP_KEEP` (default 14, `src/config.rs:742`) prunes to the newest N; the offsite job keeps one locally (`docs/design/12-object-storage-and-hosting.md:819-820`, `:845-846` suggests `BACKUP_KEEP=1`). Remove extra `riot-proxy-YYYY-MM-DD.db` files by hand if needed; they are plain copies.
3. Stop growth: `LADDER_CRAWL_S=0` (no scheduled crawl, `src/config.rs:754`), `BULK_USAGE_CEILING=0` **and** `BULK_IDLE_CEILING=0` (the idle ceiling is raised to 0.95 when unset, `src/config.rs:786-799`) to hold every bulk fetch while the API keeps serving; restart. Cancel a running crawl with `DELETE /v1/admin/ladder/crawls/{id}` (`src/routes/admin.rs:1870-1894`).
4. Free SQLite pages sooner: lower `ARCHIVE_BODY_LOCAL_DAYS` (default 14; `0` frees every packed body on the next offload run, `src/config.rs:749`; `docs/_meta/scratch/persistence.md` §4), restart, then `POST /v1/admin/maintenance/run` queues `archive:offload` after the maintenance steps. Freed pages go to the free list (`sqlite_freelist_bytes`), not back to the OS (`docs/design/07-deployment.md:187`).
5. Reclaim the file only with the gated swap in `deploy/README.md:222-270`: `rp backup …compact.db` in `tmux`, then `fuser`, the `SQLite format 3` header check, `quick_check`, stop, `PRAGMA wal_checkpoint(TRUNCATE)`, and remove `-wal`/`-shm` only when the WAL is empty. Never delete a non-empty `-wal`: on 2026-10-14 a swap made while the copy was still being written left an unfinished copy as `riot-proxy.db` beside the original's 1.5 GB WAL; the original and its WAL were put back together and nothing was lost (`deploy/README.md:258-265`; `docs/design/07-deployment.md:204`).
6. Where the bytes are: `sqlite3 -readonly riot-proxy.db "SELECT name, SUM(pgsize)/1048576 AS mib FROM dbstat GROUP BY name ORDER BY mib DESC LIMIT 30;"` (`docs/design/07-deployment.md:208-212`).

## 12. Bandwidth approaching 6 TB a month

> **TODO (unverified):** how outbound traffic is measured on the box. Nothing in the repository measures it (`grep -rni vnstat` over the repository finds nothing); Onidel's console or a tool such as `vnstat` installed by hand is the only source.

Levers, in order of effect: `ARCHIVE_TIMELINES=false` (a timeline averaged 62.3 KB against 10.7 KB for a match on the 2026-10-13 count, `docs/design/12-object-storage-and-hosting.md:459-460`); `LADDER_CRAWL_S` and `LADDER_BACKFILL_LIMIT` (`src/config.rs:754`, `:761`); `TIMELINE_BACKFILL_PATCHES=0` (`src/config.rs:741`). The Riot key bounds inbound volume: 100 requests per 2 minutes (`src/riot/limiter/headers.rs:31-42`) is at most 2.16 M requests a month, about 135 GB at 62.3 KB each (derived from those two numbers). The hourly replica `rclone sync` to R2 (`deploy/README.md:87-105`) re-sends only new packs.

## 13. Pausing and resuming background work

| Want | Set (then restart) | Evidence |
|---|---|---|
| No scheduled crawl | `LADDER_CRAWL_S=0` or `LADDER_PLATFORMS=` empty | `src/config.rs:754`, `:699-702` |
| Hold every bulk Riot call, keep the API | `BULK_USAGE_CEILING=0` and `BULK_IDLE_CEILING=0` | `src/config.rs:730-732`, `:786-799` |
| Fewer workers | `JOB_CONCURRENCY=2` | `src/config.rs:722` |
| One rebuild at a time (default) | `JOB_KIND_LIMITS=aggregate:analytics=1,archive:offload=1` | `src/config.rs:114`, `:723` |
| No timelines | `ARCHIVE_TIMELINES=false` | `src/config.rs:740` |
| No incremental analytics; crawls and rank lookups queue full rebuilds as before THR-03 | `AGGREGATE_DELTA_S=0` | `src/config.rs:769`; `src/jobs/analytics.rs:98-107` |

Without a restart: `DELETE /v1/admin/jobs?kind=archive:match&state=pending&dryRun=true` counts, then without `dryRun` deletes pending rows of one kind in 5 000-row chunks (`src/routes/admin.rs:1504-1536`; `src/jobs/scheduler.rs:803-842`); running rows are untouched. Resume by reverting the env and restarting; crawls re-enqueue the missing work on their next tick.

## 14. A stuck or failed job

1. `curl -s -H "Authorization: Bearer $ADMIN_KEY" 'localhost:8080/v1/admin/jobs?state=failed&limit=50' | jq '.jobs[] | {id,kind,attempts,error}'` (`src/routes/admin.rs:1334-1354`). The journal's `job failed` line carries `job`, `id`, `attempts`, `next=pending|failed`, `error` (`src/jobs/scheduler.rs:1092`); `attempts=5` is final (`MAX_ATTEMPTS`, `:40`).
2. Trace it: `GET /v1/admin/jobs/{id}/activity` (in-memory, last 200 finished traces, `src/jobs/activity.rs:40-42`).
3. Retry: `POST /v1/admin/jobs/{id}/retry` sets `pending` with `attempts=0`; refused with `VALIDATION` `An identical job is already queued` when a live duplicate holds the dedupe key (`src/jobs/scheduler.rs:723-765`; `src/routes/admin.rs:1309-1323`).
4. Cancel a pending job: `DELETE /v1/admin/jobs/{id}` → `failed` with error `cancelled` (`src/jobs/scheduler.rs:769-795`).
5. Purge many: §13. `failed` rows are never deleted automatically; `done` rows go after 7 days (`src/jobs/maintenance.rs:36`, `:237-240`).
6. Rows left `running` by a crash are reset to `pending` at boot: `re-queued jobs a previous process left running jobs=n` (`src/cli/serve.rs:283`; `src/jobs/scheduler.rs:963-973`). A job that looks `running` for a long time while the process is up is in `GET /v1/admin/jobs/queue` under `running[]` (`src/routes/admin.rs:1908-1925`).

## 15. Forcing a Data Dragon sync

`curl -s -X POST -H "Authorization: Bearer $ADMIN_KEY" -H 'Content-Type: application/json' -d '{"force":true}' localhost:8080/v1/admin/ddragon/sync` → `{"ok":true,"jobId":…}` (`src/routes/admin.rs:1634-1652`). `force` re-downloads the six data files even when the newest version is already mirrored (`src/jobs/ddragon.rs:212-229`, `:287-291`). The journal shows `syncing Data Dragon version=…` then `Data Dragon sync complete files=6 meta=1` (`src/jobs/ddragon.rs:241`, `:258`). The tick runs every `DDRAGON_SYNC_S` (3600 s) and once at boot (`src/config.rs:739`; `src/jobs/ticks.rs:195-211`).

## 16. Forcing maintenance

`curl -s -X POST -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/maintenance/run` → 202 `{"ok":true,"jobId":…,"status":"queued"|"already-queued"}` (`src/routes/admin.rs:2662-2670`). The job backs up (only if today's UTC backup is missing and the disk has room), trims `done` jobs and old `metrics_history`, sweeps L2, runs `PRAGMA optimize` and `PRAGMA wal_checkpoint(TRUNCATE)`, then queues `archive:offload` and `archive:index-snapshot` (`src/jobs/maintenance.rs:215-271`, `:204-211`). It logs `backup written` or `backup skipped: not enough disk`, then `maintenance done` with `checkpoint_busy`, `wal_frames`, `checkpointed` (`:309`, `:304`, `:263-268`). `GET /v1/admin/backups` shows `lastRun` and `diskFreeBytes` (`src/routes/admin.rs:2583-2606`).

## 17. ninja-monitor alerts

ninja-monitor watches the `riot-proxy.service` cgroup and `/var/lib/riot-proxy`; it never calls the proxy's HTTP endpoints (`docs/_meta/scratch/siblings.md` §2.2). The thresholds below are the example config's (`ninja-monitor:deploy/config.example.toml:64-99`); design/12 records the production values as 2 GiB memory, down after 30 s repeating every 30 min, host memory below 15 %, disk above 70 % and 85 %, CPU above 90 % for 20 min (`docs/design/12-object-storage-and-hosting.md:832-838`).

> **TODO (unverified):** the live `/etc/ninja-monitor/config.toml` on `ngf-prod-syd1` was not read; the two sources above disagree on several thresholds.

| Alert | Do |
|---|---|
| `riot-proxy memory high` (`mem_anon_bytes` > 2 GB for 5 m) | `GET /v1/admin/metrics` → `process.rssBytes`, `cacheStore.l1Bytes`. RSS is mostly the 64 MB SQLite page cache plus L1 (`CACHE_L1_MAX_MB`, 128) (`docs/design/07-deployment.md:173`), the analytics cache (≤ ~21 MB at 512 entries, `docs/DECISIONS.md:1800`) and whatever a rebuild or `facts:reextract` holds. Lower `CACHE_L1_MAX_MB` / `ANALYTICS_CACHE_ENTRIES`, or restart. |
| `riot-proxy down` | §1 step 5: `sudo journalctl -u riot-proxy -n 100 --no-pager` for `error: Invalid environment configuration` or `binding 127.0.0.1:8080`. |
| `disk filling` | §11. |
| `host memory low` | `systemd-cgtop -1` to see which of `riot-proxy`, `ninjagoldfinch-lol`, `cloudflared` holds it; then the row above or a restart of the site. |
| `riot-proxy OOM kill` | `sudo journalctl -k | grep -i oom`; the unit restarts itself; apply the memory row, and check `jobs/stats` for what was running. |

## 18. Prometheus alert rules

Nothing scrapes `/metrics` today (Prometheus on `ngf-mon-syd1` is planned, `docs/design/12-object-storage-and-hosting.md:846-848`); the rules in `ops/prometheus-alerts.yml` are what to apply when it is.

| Rule | Condition | Action |
|---|---|---|
| `RiotKeyRejected` (critical) | any Riot 401/403 in 10 m (`:6-14`) | §8 step 2, then §4 |
| `AccountableRateLimit429` (warning) | any `application`/`method` 429 in 10 m (`:16-24`) | §9 |
| `CacheHitRatioLow` (warning) | hit+neg+stale over all reads < 70 % for 15 m (`:26-41`) | check `CACHE_TTL_OVERRIDES` and `CACHE_L1_MAX_MB`; `troubleshooting.md` §12 |
| `UpstreamErrorRateHigh` (warning) | > 10 % of Riot calls 5xx/network for 10 m (`:43-50`) | §8 step 3 |
| `ProxyDown` (critical) | `up{job="riot-proxy"} == 0` for 2 m (`:52-57`) | §1 |

## 19. Analytics: deltas, rebuilds and Recompute now

Since THR-03 (ADR-182) the analytics tables are kept current by `aggregate:delta`: every `AGGREGATE_DELTA_S` (60 s) and after each completed crawl and finished `ranks:lookup`, it counts the matches no rebuild or delta has counted yet, for every configured ladder (`LADDER_PLATFORMS` × `LADDER_QUEUES`), in write transactions sized toward `AGGREGATE_DELTA_TARGET_MS` (100 ms) (`src/jobs/analytics.rs:741-914`). A full rebuild (`aggregate:analytics`) is now the repair, not the routine.

1. **First boot after upgrading to V0029.** No ladder is stamped yet, so boot queues one full rebuild over every patch per configured ladder, run one at a time by `JOB_KIND_LIMITS` (`aggregate:analytics=1`), and logs `queued full analytics rebuilds to stamp counted matches ladders=n` (`src/cli/serve.rs:318-331`). By the 2026-10-20 production runs that is about 58 minutes of writer time in all: oc1 532 s, na1 970 s, kr 1012 s, euw1 980 s (`docs/thr/THR-03-plan.md`). (On a synthetic 100 000-match oc1 archive the same boot rebuild took 94 s.) A ladder gets no delta until its rebuild finishes; a restart meanwhile costs only the rebuild in progress, which the next boot queues again. Plan the upgrade for a quiet hour, with crawls paused.
2. **Check it is running.** `curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/metrics | jq '.analytics.lastRuns[] | {platform,queue,kind,at,ms,games}'`: each ladder has its last `rebuild` and, once stamped, its last `delta` (`src/stats.rs:254-268`). The journal logs `analytics counted platform=… matches=… ms=…` per ladder a delta counted in. On `/metrics`, `proxy_aggregate_runs_total{kind="delta"}` and `aggregate_delta_matches_total{kind="added"}` move. On the synthetic archive above, deltas then counted about 7 000 matches in 43 s over 201 batches, each batch's statements at most 150 ms; expect `aggregate_delta_batch_seconds{part="sql"}` near the target and `part="commit"` under 100 ms apart from WAL checkpoints (0.25–1 s).
3. **`aggregate_delta_pending` rising.** The gauge is the key scope's matches with a step still uncounted, taken at the start of each delta (`src/archive/analytics/delta.rs:224-231`). Steady growth means the deltas are not keeping up or a ladder is waiting: look for a configured ladder with no recent `delta` in `lastRuns` (not stamped, or marked dirty, so a full rebuild is queued or running for it; check `GET /v1/admin/jobs/queue`), for a `facts:reextract` in progress (matches whose facts are below `FACTS_VERSION` wait for it), and at `aggregate_delta_batch_seconds{part="commit"}` for a stalled writer. The count also includes solo and flex matches of platforms or queues that are not configured ladders, which no delta counts, so a non-zero floor that does not grow is expected.
4. **When to press Recompute now** (`POST /v1/admin/analytics/recompute` with `{"platform":"oc1","queue":"RANKED_SOLO_5x5"}`, or the admin page's button; `src/routes/admin.rs:2340-2365`): after editing `matches`, `match_facts` (other than `tier`, which the `match_facts_tier_moved` trigger tracks) or `match_builds` by hand in SQLite, which bypasses the stamps; and when a ladder's numbers look wrong against what a rebuild would give. It rebuilds the newest `AGGREGATE_PATCH_LIMIT` patches (every patch for a ladder not stamped) and holds the writer for minutes on production (the timings in step 1), so press it at a quiet hour. It is not needed after running with `AGGREGATE_DELTA_S=0` for a while: each rebuild step stamps the matches it counts, so the next delta adds only what no rebuild counted.
5. **Version bumps.** A `FACTS_VERSION` or `BUILDS_VERSION` change that rewrites counted matches marks their ladders dirty; a dirty ladder gets no delta until a full rebuild that started after the mark (`src/archive/analytics/delta.rs:158-197`). `facts:reextract` queues those rebuilds when it ends, and any later delta queues them for a ladder still dirty (`src/jobs/analytics.rs:580-586`, `:769-776`).
6. **Turning it off.** `AGGREGATE_DELTA_S=0` and restart (§13): crawls and rank lookups queue rebuilds again and no tick runs the delta. The stamps stay; turning it back on resumes from them.

---
**Sources:** `deploy/README.md:6-16,24-25,51-62,82,87-105,121-125,140-142,202,222-270`, `docs/design/07-deployment.md:74-95,173,187,204,208-212,229`, `docs/design/04-data-and-cache.md:417-423`, `docs/design/12-object-storage-and-hosting.md:459-460,819-830,832-838,846-848`, `docs/DECISIONS.md:1268,1783-1800`, `ops/prometheus-alerts.yml:6-57`, `PROGRESS.md:325,342`, `src/app.rs:35,217,229`, `src/cli/serve.rs:45,57-68,85-89,101-105,283,334-339,404-412,418,420-426`, `src/cli/mod.rs:82-88`, `src/cli/key.rs:46-57`, `src/cli/packs.rs:55-65`, `src/config.rs:114,118-119,769,623-627,662-669,699-702,722-754,761,786-799,801-805,997-1000`, `src/consumers.rs:12,95-107`, `src/cache/keys.rs:31-37,72-90,103-166`, `src/cache/l2.rs:219-223`, `src/http/auth.rs:30,271-288`, `src/http/request_id.rs:64`, `src/riot/client.rs:176-183,305,320,348`, `src/riot/limiter/mod.rs:315-320,407-431`, `src/riot/limiter/headers.rs:31-42`, `src/fetcher.rs:34-35,587-607`, `src/archive/matches.rs:295,306,317`, `src/archive/mod.rs:128-144`, `src/archive/offload.rs:89-118`, `src/archive/objects.rs:134`, `src/jobs/offload.rs:147,204-207`, `src/jobs/scheduler.rs:40-42,160-171,723-795,803-842,963-973,1092,1296`, `src/jobs/maintenance.rs:34,36,204-211,215-271,294-309`, `src/jobs/ddragon.rs:212-229,241,258,287-291`, `src/jobs/ticks.rs:195-211`, `src/jobs/analytics.rs:98-107,508-589,741-914`, `src/archive/analytics/delta.rs:158-231`, `src/stats.rs:254-268`, `src/cli/serve.rs:318-331`, `docs/thr/THR-03-plan.md`, `src/jobs/activity.rs:40-42`, `src/routes/health.rs:21,59-145`, `src/routes/admin.rs:180-219,264-266,386-389,832-882,890-924,1010-1027,1309-1393,1504-1536,1634-1652,1848,1870-1894,1908-1925,2397-2399,2438-2477,2583-2606,2662-2670`, `src/ws/socket.rs:40`, `src/telemetry.rs:41-42,53-66`, `src/db/disk.rs:73`, `src/db/migrations/V0001__init.sql:5-13`, `ninja-monitor:deploy/config.example.toml:64-99`
**Last verified:** 2026-10-21 against commit `32c4672`

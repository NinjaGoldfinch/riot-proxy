# Troubleshooting

Failures an operator or integrator meets with riot-proxy, each as symptom, likely cause, the command that confirms it, and the fix. Every quoted message cites the line that emits it. Admin calls below assume `ADMIN_KEY` holds an `admin`-scoped consumer key and the proxy is on `localhost:8080`; `rp` is the helper from `runbook.md`.

## Health and readiness

### 1. `/readyz` answers 503 with `"sqlite":false` while everything else works

- **Cause:** since WR-02 (ADR-180) a long write no longer does this. While the writer thread runs a job, the probe is not queued behind it: `sqlite` is true while SQLite has stamped progress on that job in the last 5 s (`WRITER_STALL`, `src/routes/health.rs:34`, `:148-200`), every 1000 VM instructions and at every commit (`src/db/writer.rs:22`, `:214-230`); a job's own start counts for only 1.5 s, and only when it found the writer idle, so writes queued behind one another that each wait out `busy_timeout` never look fresh (`src/routes/health.rs:39`). Or, when it has not (a large sort is one VM step), while the writer thread is using CPU, read from its CPU clock 250 ms apart (`src/routes/health.rs:41-46`). So an `aggregate:analytics` rebuild that holds the writer for minutes answers `"sqlite":true,"writerBusy":true` with `writerBusyMs` climbing. `false` now means one of two things. With `"writerBusy":true`: the running write has neither stamped progress for 5 s nor used CPU, because it waits for another process's write lock (`busy_timeout` 5 s, `src/db/mod.rs:59`) or sleeps; that is a stalled writer. With `"writerBusy":false`: the idle writer's `BEGIN IMMEDIATE; ROLLBACK;` could not get the write lock within 1.5 s (`src/routes/health.rs:27`, `:155-170`), so another process holds it.
- **Diagnose:** `curl -s localhost:8080/readyz` a few times: a stall shows `writerBusyMs` growing and `writerQueued` rising behind it. The journal logs `slow SQLite write duration_ms=… queued=…` for every write over 1 s once it finishes (`src/db/writer.rs:377`); `sqlite_write_seconds` and `sqlite_write_queue` on `/metrics` show how often writes are slow and how deep the queue is. `sudo fuser -v /var/lib/riot-proxy/data/riot-proxy.db*` lists every process holding the database.
- **Fix:** stop the other process if there is one. A writer stalled with nothing else on the file is a restart signal; a restart that times out finds the limiter checkpoint stale (ADR-026), so expect windows restored full. Before WR-02 a rebuild made the probe time out behind it; on 2026-10-19 production was restarted on that signal (ADR-179).

### 2. `/readyz` answers 503 with `"limiter":false`

- **Cause:** `limiter_restored` is set after `restore_from` returns, success or failure (`src/cli/serve.rs:133-139`; `src/riot/limiter/persist.rs:224-229`), so `false` means boot has not reached step 5 yet (migrations or the L2 warm still running).
- **Diagnose:** `sudo journalctl -u riot-proxy -n 50 -o cat` for `limiter checkpoint restored rows=` or `limiter checkpoint could not be read; starting fresh` (`src/cli/serve.rs:135-137`).
- **Fix:** wait for `listening` (`src/cli/serve.rs:423`). A persistent `false` after `listening` is a bug.

## Authentication and authorisation

### 3. 401 `UNAUTHORIZED` `Missing or invalid API key`

- **Cause:** no `Authorization: Bearer` header and no `?token=`, or a key that is unknown or revoked (`src/http/auth.rs:292-300`; message `src/http/error.rs:107`). The journal shows `rejected unknown key` with the path (`src/http/auth.rs:298`). An unknown key is cached as unknown for 30 s (`NEGATIVE_AUTH_TTL`, `:32`).
- **Diagnose:** `rp key list` shows `active` or `revoked` per consumer (`src/cli/key.rs:59-79`).
- **Fix:** mint a key (`runbook.md` §5); for the site, check `PROXY_KEY` in `/etc/ninjagoldfinch-lol/env` and restart the site.

### 4. 403 `FORBIDDEN` `This key lacks the 'admin' scope`

- **Cause:** the key has `read` only (`src/http/auth.rs:301-306`).
- **Diagnose:** `rp key list` shows the scopes column.
- **Fix:** mint a key with `--scopes read,admin`; scopes cannot be changed on an existing consumer (no such route in `src/routes/admin.rs:44-82`).

### 5. 403 `FORBIDDEN` `Admin access is not permitted from this address`

- **Cause:** `ADMIN_IP_ALLOWLIST` is non-empty and the client address is not in it (`src/http/auth.rs:118-126`, `:307-313`). With `TRUST_PROXY=true` the address is the leftmost `X-Forwarded-For` entry (`:131-139`), so through the Cloudflare Tunnel every caller has their own public address and an allowlist of `127.0.0.1` refuses every tunnelled admin call; with `TRUST_PROXY=false` every tunnelled call arrives from cloudflared at `127.0.0.1` and the allowlist cannot narrow anything (`docs/design/07-deployment.md:225`). Production keeps the list empty and puts `admin.ninjagoldfinch.lol` behind Cloudflare Access instead (`PROGRESS.md:342`).
- **Diagnose:** the journal logs `admin request from disallowed IP ip=… consumer=…` (`src/http/auth.rs:308`); `GET /v1/admin/info` from loopback shows `ADMIN_IP_ALLOWLIST` and `trustProxy` (`src/routes/admin.rs:2454-2493`).
- **Fix:** leave `ADMIN_IP_ALLOWLIST` unset on the production box; an unparsable entry is warned and ignored (`:193-199`), not refused.

### 6. 429 `QUOTA_EXCEEDED` `Quota of 600/min exceeded`

- **Cause:** the consumer's own quota, a sliding 60 s window per consumer id (`src/http/quota.rs:131`, `:320`, `:397-399`), independent of Riot's limits. `Retry-After` and `x-ratelimit-reset` say when a slot frees.
- **Diagnose:** `GET /v1/admin/metrics | jq .quotas` (`src/http/quota.rs:339-391`).
- **Fix:** mint a key with a higher `--quota` (1 to 1 000 000, `src/routes/admin.rs:181-200`) and rotate it in; the site has no retry (`docs/_meta/scratch/siblings.md` §1.3).

### 7. 503 `RATE_LIMITED`

- **Cause:** two messages. `Upstream rate limited` means Riot answered 429 and the fetcher gave up (`src/riot/client.rs:184-187`, `:505-515`); `Upstream rate limit budget exceeded` means the proxy's own limiter had no token within the caller's budget, 2 s for a request (`CLIENT_WAIT_BUDGET_MS`, `src/config.rs:868`; `src/riot/limiter/mod.rs:58-80`; `src/fetcher.rs:843-849`). A cached copy inside its hard TTL is served as `STALE` instead when one exists.
- **Diagnose:** `GET /v1/admin/limits/{scope}` for `usage` and `frozenMs` (`src/routes/admin.rs:1012-1029`); `/metrics` `proxy_rl_429_total{type}` and `limiter_interactive_waiters`.
- **Fix:** for the budget message, bulk work is starving interactive calls: lower `BULK_USAGE_CEILING` or pause crawls (`runbook.md` §13); for Riot's 429, see `runbook.md` §9.

### 8. 502 `UPSTREAM_ERROR` `Upstream authentication failed`

- **Cause:** Riot answered 401 or 403; the key is expired, revoked or wrong (`src/riot/client.rs:491-504`, `:183-183`). Never retried.
- **Diagnose:** journal `error` `RIOT KEY REJECTED` with `status`, `method`, `host` (`src/riot/client.rs:502`); `/metrics` `proxy_upstream_requests_total{status="401"}` or `"403"`.
- **Fix:** `runbook.md` §4.

## Request validation

### 9. 400 `BAD_REGION` `params/platform must be equal to one of the allowed values` for `EUW1`

- **Cause:** route path parameters are matched case-sensitively against the 16 lower-case platform ids (`src/http/validate.rs:53-78`; ids `src/riot/routing.rs:49-68`). The same applies to `params/region` (`americas|europe|asia|sea`) and `querystring/platform` (`:203-207`).
- **Fix:** send `euw1`. Match ids may keep their upper-case prefix (`EUW1_…`): the prefix parser is case-insensitive (`src/riot/routing.rs:139-158`).

### 10. 400 `VALIDATION` `querystring must have required property 'platform'` on `/v1/players/{puuid}/matches`

- **Cause:** that route requires `?platform=` (`src/http/validate.rs:200-208`; `src/routes/players.rs:656`). The profile route finds the platform itself; the match page does not.
- **Fix:** take `platform` from the profile body and pass it (`docs/_meta/scratch/siblings.md` §1.5 row 2 shows how the site does it).

### 11. 404 on `/v1/static/*`: `Static data 'champion' has not been synced yet. Run the ddragon:sync job.`

- **Cause:** no patch directory with a `versions.json` exists under `DDRAGON_DIR` yet (`src/routes/statics.rs:289-292`; `:213` for `queues`). `ddragon:sync` runs hourly, and at boot when the last sync finished over an hour ago or never (`src/jobs/ticks.rs:225-403`; ADR-214), but needs the CDN; `POST /v1/admin/ddragon/sync` runs it now (runbook §15).
- **Diagnose:** `ls /var/lib/riot-proxy/data/ddragon`; journal for `syncing Data Dragon` / `Data Dragon sync complete` (`src/jobs/ddragon.rs:241`, `:258`); `GET /v1/admin/jobs?kind=ddragon:sync`.
- **Fix:** §27, then `POST /v1/admin/ddragon/sync {"force":true}`.

## Caching

### 12. `X-Cache: MISS` on every request, or hit ratio under 70 %

- **Cause:** one of: the endpoint is immutable and served `ARCHIVE` not `HIT` (`match.byId`, `match.timeline`; `src/riot/endpoints.rs:225-244`); an admin key sends `?refresh=true` and gets `BYPASS` (`src/routes/passthrough.rs:159-167`); `CACHE_TTL_OVERRIDES` set a tiny soft TTL (`src/riot/endpoints.rs:471-491`); L1 is too small so entries evict at once, and an entry larger than the whole budget is never kept (`CACHE_L1_MAX_MB`, `src/cache/l1.rs:131-144`); the L2 write-behind queue is full so nothing survives a restart (`cache_l2_dropped_total`, `warn` `L2 write queue full; entry not persisted`, `src/cache/l2.rs:163-173`).
- **Diagnose:** `GET /v1/admin/debug/cache?scope=oc1&method=summoner.byPuuid&path=/lol/summoner/v4/summoners/by-puuid/<puuid>` → `{key,present,ageSeconds,stale}` (`src/routes/admin.rs:1234-1250`); `GET /v1/admin/metrics | jq '{cache,cacheStore}'`; `/metrics` `proxy_cache_reads_total{state}` and `cache_l2_dropped_total`.
- **Fix:** remove the override, raise `CACHE_L1_MAX_MB`, or lower `JOB_CONCURRENCY` if the SQLite writer is behind (a rising `cache_l2_dropped_total` means exactly that, `docs/design/metrics.md:56`).

## Storage and maintenance

### 13. `warn` `backup skipped: not enough disk`

- **Cause:** free space on the backup directory is under 1.2 × (db + wal) (`BACKUP_FREE_FACTOR`, `src/jobs/maintenance.rs:36`, `:313-325`). The run goes on without the backup.
- **Diagnose:** `GET /v1/admin/backups` → `lastRun.report.backupSkipped`, `diskFreeBytes` (`src/routes/admin.rs:2599-2622`); `df -h /var/lib/riot-proxy`.
- **Fix:** `runbook.md` §11. After freeing space, a restart or `POST /v1/admin/maintenance/run` (runbook §16) takes the backup; until one is written, every restart runs maintenance, because the tick counts from the newest backup (AVL-13).

### 13a. `warn` `backup directory could not be read; maintenance is due now`

- **Cause:** with `BACKUP_KEEP` above 0, the boot reads the newest backup's time to schedule the `maintenance` tick (AVL-13), and could not: `$DATA_DIR/backups` is not listable by the service user, is not a directory, or its newest `riot-proxy-<date>.db` is a dangling link or cannot be `stat`ed (the error then names the file; AVL-15, `src/jobs/ticks.rs:276-298`, `:348-360`). The tick fires at once and queues the run. While the directory cannot be listed, the run fails at its retention listing before the trims (`src/jobs/maintenance.rs:239-247`): it is retried (`job failed`, five attempts, then `failed`) and nothing is trimmed, swept or checkpointed. A newest name that is a dangling link does not stop the run: the listing works and the run completes.
- **Diagnose:** `ls -la /var/lib/riot-proxy/data/backups` (owner, mode, links); `GET /v1/admin/backups` answers 500 when the listing fails (`src/routes/admin.rs:2638-2643`, `:1334-1342`); `GET /v1/admin/jobs?kind=maintenance` shows the retries and their error.
- **Fix:** restore the directory's owner and mode so the service user can list it, or remove the bad file, then restart or queue a run with `POST /v1/admin/maintenance/run` (runbook §16).

### 14. The WAL keeps growing (`sqlite_wal_bytes` in the hundreds of MB)

- **Cause:** `wal_autocheckpoint=1000` (`src/db/mod.rs:68`) cannot complete while readers hold old snapshots; with readers always active the WAL never checkpointed on the 2-vCPU host (`docs/design/07-deployment.md:175`). The daily `maintenance` runs `PRAGMA wal_checkpoint(TRUNCATE)` and logs `checkpoint_busy`, `wal_frames`, `checkpointed` (`src/jobs/maintenance.rs:269-283`).
- **Diagnose:** `/metrics` `sqlite_wal_bytes`; `ls -la /var/lib/riot-proxy/data/riot-proxy.db-wal`; the last `maintenance done` line.
- **Fix:** `POST /v1/admin/maintenance/run` in a quiet moment; lower `JOB_CONCURRENCY` so fewer readers overlap. Never delete the `-wal` file (`deploy/README.md:258-265`).

## Jobs

### 15. Jobs show `running` right after a crash or restart

- **Cause:** the previous process died with handlers mid-flight; rows stay `running` until the next boot resets them to `pending` with attempts kept (`src/jobs/scheduler.rs`, `Scheduler::recover`), logged as `re-queued jobs a previous process left running jobs=n` (`src/cli/serve.rs`). A row interrupted on its fifth attempt is `failed` with `interrupted on its last attempt…` instead (ADR-189): if one kind keeps ending that way, its handler is taking the process down.
- **Diagnose:** `GET /v1/admin/jobs?state=running` and compare `claimedAt` with `startedAt` from `/v1/admin/info`.
- **Fix:** nothing; it is recovered at boot. If rows are `running` with a `claimedAt` older than the process, the process did not restart cleanly: restart it.

### 16. `warn` `job failed … attempts=5 next=failed`

- **Cause:** the fifth attempt failed, or the handler returned `Fail` (`MAX_ATTEMPTS = 5`, `src/jobs/scheduler.rs:40`, `:1103-1108`, `:1125`). Earlier attempts back off `30 s × 2^attempts ± 20 %` (`:42`, `:160-171`). `archive:match` fails at once on a Riot 404 (`src/jobs/archive.rs:296-340`).
- **Diagnose:** `GET /v1/admin/jobs?state=failed&kind=<kind>` → `error` per row; `GET /v1/admin/jobs/{id}/activity`.
- **Fix:** `POST /v1/admin/jobs/{id}/retry` after the cause (Riot, the store, the disk) is fixed; a 404 match is final.

### 17. A ladder crawl ends `failed`

- **Cause:** a leg (`ladder:apex`, `ladder:walk`, `ladder:collect`) gave up after five attempts; when the last leg ends with `legs_failed > 0` the crawl is finished `failed` (`src/jobs/ladder.rs`, `LadderContext::end_leg`; `src/jobs/ladder/store.rs`, `end_leg`). A crawl whose last leg job ended without ending its leg (a handler panic retried to the end, an admin cancel or purge of the job, a job a crash left on its last attempt) is failed by the next `ladder:crawl` tick, logged as `ladder crawl failed: no job left to finish it` (ADR-192); before, it stayed `running` and held the ladder. The partial unique index allows one running crawl per ladder (`src/db/migrations/V0004__ladder.sql:33-34`).
- **Diagnose:** `GET /v1/admin/ladder/crawls?platform=oc1` → `status`, `legsFailed`; `GET /v1/admin/ladder/crawls/{id}` → `failed[]` (`src/routes/admin.rs:2108-2126`); the failed legs' `error` in `/v1/admin/jobs?state=failed&kind=ladder:walk`.
- **Fix:** `POST /v1/admin/ladder/crawl {"platform":"oc1"}` starts a new crawl (`src/routes/admin.rs:1702-1735`); `ladder_entries` already written are kept (`V0004__ladder.sql:7`).

## Object storage

### 18. `archive:offload` keeps failing

- **Cause:** credentials or endpoint. Boot refuses `http://` in production (`ARCHIVE_OBJECT_ENDPOINT: must be https:// when ENV=production`, `src/config.rs:1083-1095`), an `s3://` URL without an endpoint (`is required with an s3:// ARCHIVE_OBJECT_URL`, `:1093`), one key without the other (`set both or neither`, `:1086-1089`), or a production URL without keys (`:1095-1100`). A boot that passes logs `object store configured store=s3://riot-proxy-archive at <endpoint>` (`src/cli/serve.rs:167`; `src/archive/objects.rs:142`); a store that then rejects requests makes every run `Retry` (`src/jobs/offload.rs:147`).
- **Diagnose:** `GET /v1/admin/jobs?kind=archive:offload` → `error`; `rp packs snapshot` exercises a PUT with the service's credentials (`src/cli/packs.rs:55-65`); `sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1`.
- **Fix:** correct the env file and restart (`runbook.md` §6); `archive_offload_pending` falls on the next run.

### 19. `warn` lines ending in `reading from Riot`

- **Cause:** an offloaded body could not be read from its pack: `offloaded body has no location` (`src/archive/matches.rs:250`), `offloaded bodies but no ARCHIVE_OBJECT_URL` (`:258-261`, the URL was unset after bodies were freed), `pack id is not a ULID` (`:284`), `pack read failed` (`:295`), `offloaded body failed its sha256` (`:306`), `offloaded body did not decompress` (`:317`). The body is dropped and the request goes to Riot (`src/archive/mod.rs:216-235`); the re-fetched body is stored locally again (`src/archive/matches.rs:399`).
- **Diagnose:** the line's `key`/`pack_id`; `GET /v1/admin/stats | jq .archiveBodies`; `rp packs reindex --since <pack id>` lists packs that cannot be read (`src/cli/packs.rs:32-35`, `:71-83`).
- **Fix:** for a missing URL, set it back (`docs/design/12-object-storage-and-hosting.md:943`); for a store outage, `runbook.md` §10; for a damaged pack, the R2 replica holds a copy (`deploy/README.md:12-18`).

## Configuration refusals at boot

### 20. `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`

- **Cause:** `TLS=true` or `1` in the env (`src/config.rs:809-814`); built-in TLS went in ADR-067 (`docs/design/07-deployment.md:59-61`).
- **Fix:** remove `TLS`, `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT` (the last three are dropped silently, `src/config.rs:1039-1046`).

### 21. `AUTH_DISABLED cannot be enabled when ENV=production`

- **Cause:** `src/config.rs:799-803`. `AUTH_DISABLED` is for local work only; it runs every request as `dev-local` with `read,admin` (`src/http/auth.rs:43-52`).
- **Fix:** remove it from `/etc/riot-proxy/env`; mint keys instead.

### 22. `warn` `DEV_UI=true ignored: /dev is never served when ENV=production`

- **Cause:** `DEV_UI` is forced off in production (`src/config.rs:808-810`, `:918-919`; `src/cli/serve.rs:159-161`). Not an error; `/dev` and `/dev/reset` are simply absent.
- **Fix:** remove the variable to silence the warning. The dashboard (`/dashboard`, `DASHBOARD_UI`) is unaffected.

### 23. Boot fails with `binding 127.0.0.1:8080: Address already in use`

- **Cause:** another process holds the port; the bind error carries that context (`src/cli/serve.rs:419-421`) and exits 1.
- **Diagnose:** `sudo ss -ltnp 'sport = :8080'`; often a previous instance still draining (10 s grace, `src/app.rs:35`) or a stray `rp … serve`.
- **Fix:** wait or stop the other process, then `sudo systemctl restart riot-proxy`.

### 24. `warn` `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored key=match`

- **Cause:** the key is unknown or names only immutable endpoints (`match`, `timeline`), whose bodies are archived, not cached (`src/cli/serve.rs:170-172`; `src/riot/endpoints.rs:547-559`). Valid keys: `account, summoner, league, ladder, match, timeline, spectator, mastery, rotations, status` (`docs/_meta/scratch/config.md` §1.2). Malformed items are skipped silently (`src/riot/endpoints.rs:471-491`).
- **Fix:** correct the key; the rest of the list still applies.

## Other environments

### 25. The Proxmox dev VM logs `error: rejected ghcr.io/ninjagoldfinch/riot-proxy:edge (<commit>): …; rolled back to riot-proxy:previous (<commit>)`

- **Cause:** `riot-proxy-update` found the new `:edge` image unhealthy or crash-looping and restored `:previous`; it records the image in `/opt/riot-proxy/.rejected` so later runs skip it (`deploy/proxmox/files/riot-proxy-update:69-96`; `deploy/proxmox/README.md:72-85`).
- **Diagnose:** `cat /opt/riot-proxy/.rejected`; the VM's journal for the health reason.
- **Fix:** fix `main`; a newer edge image is tried automatically. To retry the same image: `sudo rm /opt/riot-proxy/.rejected && sudo riot-proxy-update` (`deploy/proxmox/README.md:80-81`). The VM is dormant today (see the writing brief); the procedure is kept for when it runs.

### 26. The container `HEALTHCHECK` fails with `error: Invalid environment configuration: RIOT_API_KEY: is required`

- **Cause:** `riot-proxy healthcheck` loads the full configuration before it GETs `/healthz` (`src/cli/mod.rs:83-89`; `src/cli/healthcheck.rs`), so it needs the same env as `serve` (`src/config.rs:743-747`). Docker runs it as `CMD ["/riot-proxy","healthcheck"]` (`Dockerfile:24`).
- **Diagnose:** `docker exec <ctr> /riot-proxy healthcheck`; `docker inspect --format '{{json .State.Health}}' <ctr>`.
- **Fix:** pass `RIOT_API_KEY` (and the rest of `.env`) into the container's environment, not only into a mounted file the health command cannot see.

### 27. `ddragon:sync` fails or stays `running`

- **Cause:** the CDN or `static.developer.riotgames.com` is unreachable: `warn` `Data Dragon file unavailable; skipping` (`src/jobs/ddragon.rs:252`) or `queue table unavailable; keeping what is on disk` (`:281`); any error makes the job `Retry` (`:302-304`). Timeouts are 15 s connect, 60 s total (`:111-112`). The job is a deduped singleton, so a second request joins the queued one (`src/jobs/ticks.rs:110-114`; `src/routes/admin.rs:1642-1652`).
- **Diagnose:** `curl -sI https://ddragon.leagueoflegends.com/api/versions.json` from the box; `GET /v1/admin/jobs?kind=ddragon:sync`; `ls -la /var/lib/riot-proxy/data/ddragon` for write permission.
- **Fix:** restore connectivity or permissions, then `POST /v1/admin/ddragon/sync {"force":true}`.

### 28. The bootstrap admin key was never shown

- **Cause:** it is printed only when the `consumers` table is empty at `serve` and `BOOTSTRAP_ADMIN_KEY` is unset (`src/consumers.rs:308-333`; `src/cli/serve.rs:115-126`, `:624-630`). With `BOOTSTRAP_ADMIN_KEY` set, that key is imported silently; with any consumer already present, nothing is created. It goes to stderr, not the log (`src/cli/serve.rs:626`).
- **Diagnose:** `rp key list` shows whether `bootstrap-admin` exists (`src/consumers.rs:16`).
- **Fix:** mint a new admin key with `rp key create --name admin-2026-10 --scopes read,admin`; a lost key cannot be recovered (only its hash is stored, `src/consumers.rs:105-107`).

## Analytics

### 29. A ladder's analytics stop moving after crawls, or `lastRuns` has no `delta` for it

- **Cause:** since THR-03 (ADR-182) a completed crawl of a stamped ladder queues `aggregate:delta`, not a rebuild, and the delta counts only ladders stamped by a full rebuild with nothing marked dirty since, of any platform (`src/jobs/analytics.rs:1132-1170`; `src/archive/analytics/delta.rs:199-222`). A ladder is unstamped after the upgrade to V0029 until its first full rebuild finishes, and dirty after a `facts:reextract` or a `BUILDS_VERSION` bump rewrote matches already counted; either way a full rebuild is queued for it and the delta skips it until then. A crawl of an unstamped ladder (a platform outside `LADDER_PLATFORMS`, started from the admin route, or one before its first full rebuild) queues that rebuild at its end, which stamps it; deltas count it from then on. Boot and each delta queue the rebuild only for configured ladders.
- **Diagnose:** `GET /v1/admin/metrics | jq '.analytics.lastRuns[] | {platform,queue,kind,at,status}'`; `GET /v1/admin/jobs/queue` for a pending or running `aggregate:analytics`; the journal's `full rebuilds queued for ladders not stamped yet queued=n` (`src/jobs/analytics.rs:1164`) or, at boot, `queued full analytics rebuilds to stamp counted matches ladders=n` (`src/cli/serve.rs:590-593`). `GET /v1/admin/info` shows `AGGREGATE_DELTA_S`; at `0` crawls queue rebuilds as before.
- **Fix:** let the queued rebuild finish (minutes per ladder on production; `runbook.md` §19 step 1). For an unconfigured platform, run a crawl of it or press Recompute now for it (`POST /v1/admin/analytics/recompute {"platform":"…"}`): its rebuild stamps it.

### 30. `aggregate_delta_pending` keeps rising

- **Cause:** the gauge counts the key scope's matches with any rebuild step still uncounted, read at the start of each delta (`src/archive/analytics/delta.rs:224-231`, `src/jobs/analytics.rs:1155-1156`). It grows when matches arrive faster than the deltas count them, when a ladder is waiting for its full rebuild (§29), or while `facts:reextract` runs (matches whose facts are below `FACTS_VERSION` are left for it, `delta.rs:233-270`). Solo and flex matches of platforms or queues that are not configured ladders are in the count and are never counted, so a floor that stays level is normal.
- **Diagnose:** `aggregate_delta_matches_total{kind="added"}` should rise with it; `aggregate_delta_batch_seconds{part="sql"}` near `AGGREGATE_DELTA_TARGET_MS` and `aggregate_delta_batch_size` show the batches; `part="commit"` of seconds is a slow writer (§1). `proxy_aggregate_runs_total{kind="delta"}` not moving means no delta is running: check `AGGREGATE_DELTA_S` and `GET /v1/admin/jobs?kind=aggregate:delta&state=failed`.
- **Fix:** none for arrival bursts; it drains between crawls. A stuck ladder: §29. A delta that fails repeatedly: retry it after the cause (`runbook.md` §14); a full rebuild of the ladder (Recompute now) is the repair.

### 31. The writer is busy for about an hour after an upgrade

- **Cause:** the first boot with V0029 and `AGGREGATE_DELTA_S` > 0 queues one full rebuild over every patch per configured ladder, one at a time; by the 2026-10-20 production runs that is about 58 minutes in all (oc1 532 s, na1 970 s, kr 1012 s, euw1 980 s; `docs/thr/THR-03-plan.md`). `/readyz` answers `"sqlite":true,"writerBusy":true` with `writerBusyMs` climbing meanwhile (§1).
- **Diagnose:** `GET /v1/admin/jobs/queue` shows `aggregate:analytics` running and the rest pending; `lastRuns` gains a `rebuild` per ladder as each finishes.
- **Fix:** nothing; pause crawls for the hour if you can. A restart costs only the rebuild in progress, which the next boot queues again.

### 32. After a restart a crawl sits in *collect* with no batch running

- **Cause:** before TL-03 the boot catch-up of missing halves (matches without a timeline, timelines without a match) queued at priority 100, a band ahead of every crawl stage, and each region's app limit then spent itself on it: on 2026-10-21 production's three crawls waited about an hour behind 938–2 029 catch-up jobs per region while `/v1/admin/metrics` showed each region's `100:120` window at 95/100. Since TL-03 (ADR-184) the catch-up queues at 20 004, after a crawl's collect (`src/jobs/mod.rs`, `priority::CATCH_UP`).
- **Diagnose:** `sqlite3 -readonly … "SELECT lane, priority, count(*) FROM jobs WHERE kind = 'archive:match' AND state = 'pending' AND priority < 20000 GROUP BY 1, 2"`; the limiter's windows in `/v1/admin/metrics` `.limiter[]`.
- **Fix:** on 2.8.0 or earlier, wait: the backlog drains at about 2 500 jobs an hour per region. From 2.9.0 (TL-03) on, collect goes first.

## Restarts

### 33. The journal says `listening` but nothing answers, and `ss` shows the listen backlog full

- **Cause:** on a release before AVL-01, the boot catch-ups ran between `listening` and the first accept, so the kernel completed connections into the listen backlog and nothing accepted them. On production from at least 2026-10-26, before AVL-06, the tier check among them found the ranked `ME1_` matches it could not stamp only after reading most of the archive in id order, which took 1.3 to 5 minutes on every boot; AVL-06 (ADR-213) stamps them `UNKNOWN` (`src/archive/tiers.rs:60-68`) and reads only past each prefix's cursor (`:279-293`). A SIGTERM in that window was not acted on until the server ran (`src/app.rs:329`), so a restart then ended in systemd's SIGKILL at 90 s. AVL-01 (ADR-209) binds and serves first (`src/cli/serve.rs:419-424`) and runs the catch-up behind the server (`:520-611`), honouring a SIGTERM during it (`:431-472`); with it, this symptom means something else holds the accept loop.
- **Diagnose:** `sudo ss -ltn 'sport = :8080'` shows `Recv-Q` at or over `Send-Q` (129 over 128 on 2026-11-08); `curl -s -m 5 localhost:8080/healthz` times out; the journal has `listening` and no `starting job workers` yet (`runbook.md` §20 step 3); `sudo cat /proc/$(pidof riot-proxy)/io` shows `read_bytes` climbing. With AVL-01, `boot catch-up done ms=…` says how long the catch-up took, and `/healthz` answers throughout it.
- **Fix:** on a release before AVL-01, wait for `starting job workers`, after which it serves; do not restart again meanwhile (`runbook.md` §20). Upgrade to a release with AVL-01, which serves before the catch-ups and removes the gap; AVL-06 bounds the tier check and ME-02 makes `me1` a platform, which remove its cost (IMPLEMENTATION.md §Post-release — AVL, ADR-208).


### 34. `/v1/lol/status/{platform}` answers `STALE` or `MISS`

- **Cause:** the status poller has not stored a fresh copy for that platform within `status`'s soft TTL (300 s): its reads were skipped or failed, the platform is not in `STATUS_POLL_PLATFORMS`, the poller is off, or `STATUS_POLL_INTERVAL_S` is above 300. `STALE` is served up to 600 s (the hard TTL); after that a read is a `MISS` and calls Riot.
- **Diagnose:** `curl -s localhost:8080/metrics | grep 'status_polls_total{platform="<platform>"'`; the `platform_status` row (`runbook.md` §21); `STATUS_POLL_PLATFORMS` and `STATUS_POLL_INTERVAL_S` in `/etc/riot-proxy/env`.
- **Fix:** list the platform, keep the interval at 240 s or less, and see §35 for skips.
- **Note:** the `platform_status` row and the route can disagree. A read that succeeds on a later attempt past the poller's 60 s deadline is stored failed (`no answer within 60 s`) while its call fills L1, so the route answers `HIT` with a fresh body. Typed 429s whose `Retry-After` waits add up past 60 s store a failure although the read was only rate limited. The next round's 200 corrects the row (design/14 §Status poller).

### 35. `status_polls_total{result="skipped"}` keeps rising

- **Cause:** the limiter had no room on that platform's scope within 30 s, round after round: a 429 freeze (`riot_retry_after_s` above 0 in `/status`), or real traffic using the bucket.
- **Diagnose:** `curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/status/ratelimits` for the scope; the journal for `scope frozen`.
- **Fix:** nothing in the poller: it reads again next round. A freeze that does not lift is §8 or a key problem.

### 36. `RIOT KEY REJECTED` names `status.platformData` on one platform

- **Cause:** since SITE-17 one status poll's 401 or 403 does not log this line or turn `riot_key` to rejected: it logs `status poll refused` at `debug`, and when that platform's reads start failing, `warn` `status poll failing` with `error=UPSTREAM_ERROR: Upstream authentication failed`. A round in which every polled platform that reached Riot refused does log it, once, ending `every status poll this round was refused`, and turns `riot_key` to `down` (owner decision of 2026-11-17): every platform refusing is the key. Any other `RIOT KEY REJECTED` for `status.platformData` comes from `/v1/lol/status/{platform}` (a consumer's call) or `/status`'s probe, so the key was refused there.
- **Diagnose:** the line's `host=`; `GET /status` → `riot_key`. One platform refusing while others answer (a platform the key cannot read) is that platform, not the key; every platform refusing is the key (`runbook.md` §8).
- **Fix:** drop a platform the key cannot read from `STATUS_POLL_PLATFORMS`: the `RiotKeyRejected` alert counts every 401 or 403 in `proxy_upstream_requests_total`, a poll's included, so such a platform keeps it firing every round. Replace an expired key (`runbook.md` §4).

---
**Sources:** `src/routes/health.rs:21-46,148-200`, `src/db/writer.rs:22,158-206,214-230,326-380`, `src/db/mod.rs:59,68,71-72,241-254,313-341,595-601`, `src/jobs/analytics.rs:533-780,624,1132-1170`, `src/archive/analytics/delta.rs:199-270`, `src/cli/serve.rs:570-583,115-126,133-139,159-161,165-172,208-219,529-599,611,615-629`, `src/archive/tiers.rs:60-68,129-294`, `src/riot/limiter/persist.rs:224-229`, `src/http/auth.rs:32,43-52,118-139,196-202,292-313`, `src/http/error.rs:107`, `src/http/quota.rs:131,320,339-399`, `src/http/validate.rs:53-78,194-208`, `src/riot/routing.rs:49-68,139-158`, `src/riot/client.rs:183-188,491-515`, `src/riot/limiter/mod.rs:58-80`, `src/fetcher.rs:843-849`, `src/riot/endpoints.rs:225-244,471-491,547-559`, `src/routes/passthrough.rs:159-167`, `src/routes/players.rs:656`, `src/routes/statics.rs:213,289-292`, `src/cache/l1.rs:131-144`, `src/cache/l2.rs:163-173`, `src/jobs/maintenance.rs:36,239-247,270-284,310-322`, `src/jobs/scheduler.rs:40-42,160-171,996-1006,1103-1125`, `src/jobs/archive.rs:296-340`, `src/jobs/ladder.rs:861-909`, `src/jobs/ladder/store.rs:363-418`, `src/jobs/ddragon.rs:111-112,241-304`, `src/jobs/offload.rs:147`, `src/jobs/ticks.rs:110-114,225-403`, `src/archive/matches.rs:250-317,399`, `src/archive/mod.rs:216-235`, `src/archive/objects.rs:142`, `src/config.rs:743-747,799-814,868,928-929,1039-1046,1080-1092,1147-1161`, `src/consumers.rs:16,105-107,308-333`, `src/cli/mod.rs:83-89`, `src/cli/key.rs:59-79`, `src/cli/packs.rs:32-35,55-83`, `src/routes/admin.rs:44-82,181-200,1012-1029,1234-1250,1702-1735,1924-1941,2108-2126,2454-2493,2599-2622,1334-1342,2638-2643`, `src/db/migrations/V0004__ladder.sql:7,33-34`, `src/app.rs:35,329`, `Dockerfile:24`, `deploy/README.md:12-18,258-265`, `deploy/proxmox/README.md:72-85`, `deploy/proxmox/files/riot-proxy-update:69-96`, `docs/design/07-deployment.md:59-61,175,225`, `docs/design/12-object-storage-and-hosting.md:943`, `docs/design/metrics.md:56`, `PROGRESS.md:342`, `docs/thr/THR-03-plan.md`
**Last verified:** 2026-11-18 against commit `101740c`

# Troubleshooting

Failures an operator or integrator meets with riot-proxy, each as symptom, likely cause, the command that confirms it, and the fix. Every quoted message cites the line that emits it. Admin calls below assume `ADMIN_KEY` holds an `admin`-scoped consumer key and the proxy is on `localhost:8080`; `rp` is the helper from `runbook.md`.

## Health and readiness

### 1. `/readyz` answers 503 with `"sqlite":false` while everything else works

- **Cause:** since WR-02 (ADR-180) a long write no longer does this. While the writer thread runs a job, the probe is not queued behind it: `sqlite` is true while SQLite has stamped progress on that job in the last 5 s (`WRITER_STALL`, `src/routes/health.rs:34`, `:148-200`), every 1000 VM instructions and at every commit (`src/db/writer.rs:22`, `:214-230`); a job's own start counts for only 1.5 s, and only when it found the writer idle, so writes queued behind one another that each wait out `busy_timeout` never look fresh (`src/routes/health.rs:39`). Or, when it has not (a large sort is one VM step), while the writer thread is using CPU, read from its CPU clock 250 ms apart (`src/routes/health.rs:41-46`). So an `aggregate:analytics` rebuild that holds the writer for minutes answers `"sqlite":true,"writerBusy":true` with `writerBusyMs` climbing. `false` now means one of two things. With `"writerBusy":true`: the running write has neither stamped progress for 5 s nor used CPU, because it waits for another process's write lock (`busy_timeout` 5 s, `src/db/mod.rs:29`) or sleeps; that is a stalled writer. With `"writerBusy":false`: the idle writer's `BEGIN IMMEDIATE; ROLLBACK;` could not get the write lock within 1.5 s (`src/routes/health.rs:27`, `:155-170`), so another process holds it.
- **Diagnose:** `curl -s localhost:8080/readyz` a few times: a stall shows `writerBusyMs` growing and `writerQueued` rising behind it. The journal logs `slow SQLite write duration_ms=… queued=…` for every write over 1 s once it finishes (`src/db/writer.rs:272`); `sqlite_write_seconds` and `sqlite_write_queue` on `/metrics` show how often writes are slow and how deep the queue is. `sudo fuser -v /var/lib/riot-proxy/data/riot-proxy.db*` lists every process holding the database.
- **Fix:** stop the other process if there is one. A writer stalled with nothing else on the file is a restart signal; a restart that times out finds the limiter checkpoint stale (ADR-026), so expect windows restored full. Before WR-02 a rebuild made the probe time out behind it; on 2026-10-19 production was restarted on that signal (ADR-179).

### 2. `/readyz` answers 503 with `"limiter":false`

- **Cause:** `limiter_restored` is set after `restore_from` returns, success or failure (`src/cli/serve.rs:75-81`; `src/riot/limiter/persist.rs:224-229`), so `false` means boot has not reached step 5 yet (migrations or the L2 warm still running).
- **Diagnose:** `sudo journalctl -u riot-proxy -n 50 -o cat` for `limiter checkpoint restored rows=` or `limiter checkpoint could not be read; starting fresh` (`src/cli/serve.rs:77-79`).
- **Fix:** wait for `listening` (`src/cli/serve.rs:148`). A persistent `false` after `listening` is a bug.

## Authentication and authorisation

### 3. 401 `UNAUTHORIZED` `Missing or invalid API key`

- **Cause:** no `Authorization: Bearer` header and no `?token=`, or a key that is unknown or revoked (`src/http/auth.rs:267-275`; message `src/http/error.rs:107`). The journal shows `rejected unknown key` with the path (`src/http/auth.rs:273`). An unknown key is cached as unknown for 30 s (`NEGATIVE_AUTH_TTL`, `:32`).
- **Diagnose:** `rp key list` shows `active` or `revoked` per consumer (`src/cli/key.rs:59-79`).
- **Fix:** mint a key (`runbook.md` §5); for the site, check `PROXY_KEY` in `/etc/ninjagoldfinch-lol/env` and restart the site.

### 4. 403 `FORBIDDEN` `This key lacks the 'admin' scope`

- **Cause:** the key has `read` only (`src/http/auth.rs:276-281`).
- **Diagnose:** `rp key list` shows the scopes column.
- **Fix:** mint a key with `--scopes read,admin`; scopes cannot be changed on an existing consumer (no such route in `src/routes/admin.rs:44-82`).

### 5. 403 `FORBIDDEN` `Admin access is not permitted from this address`

- **Cause:** `ADMIN_IP_ALLOWLIST` is non-empty and the client address is not in it (`src/http/auth.rs:118-126`, `:282-288`). With `TRUST_PROXY=true` the address is the leftmost `X-Forwarded-For` entry (`:131-139`), so through the Cloudflare Tunnel every caller has their own public address and an allowlist of `127.0.0.1` refuses every tunnelled admin call; with `TRUST_PROXY=false` every tunnelled call arrives from cloudflared at `127.0.0.1` and the allowlist cannot narrow anything (`docs/design/07-deployment.md:221`). Production keeps the list empty and puts `admin.ninjagoldfinch.lol` behind Cloudflare Access instead (`PROGRESS.md:342`).
- **Diagnose:** the journal logs `admin request from disallowed IP ip=… consumer=…` (`src/http/auth.rs:283`); `GET /v1/admin/info` from loopback shows `ADMIN_IP_ALLOWLIST` and `trustProxy` (`src/routes/admin.rs:2438-2477`).
- **Fix:** leave `ADMIN_IP_ALLOWLIST` unset on the production box; an unparsable entry is warned and ignored (`:193-199`), not refused.

### 6. 429 `QUOTA_EXCEEDED` `Quota of 600/min exceeded`

- **Cause:** the consumer's own quota, a sliding 60 s window per consumer id (`src/http/quota.rs:131`, `:320`, `:397-399`), independent of Riot's limits. `Retry-After` and `x-ratelimit-reset` say when a slot frees.
- **Diagnose:** `GET /v1/admin/metrics | jq .quotas` (`src/http/quota.rs:339-391`).
- **Fix:** mint a key with a higher `--quota` (1 to 1 000 000, `src/routes/admin.rs:181-200`) and rotate it in; the site has no retry (`docs/_meta/scratch/siblings.md` §1.3).

### 7. 503 `RATE_LIMITED`

- **Cause:** two messages. `Upstream rate limited` means Riot answered 429 and the fetcher gave up (`src/riot/client.rs:179-182`, `:308-318`); `Upstream rate limit budget exceeded` means the proxy's own limiter had no token within the caller's budget, 2 s for a request (`CLIENT_WAIT_BUDGET_MS`, `src/config.rs:729`; `src/riot/limiter/mod.rs:58-80`; `src/fetcher.rs:460-466`). A cached copy inside its hard TTL is served as `STALE` instead when one exists.
- **Diagnose:** `GET /v1/admin/limits/{scope}` for `usage` and `frozenMs` (`src/routes/admin.rs:1010-1027`); `/metrics` `proxy_rl_429_total{type}` and `limiter_interactive_waiters`.
- **Fix:** for the budget message, bulk work is starving interactive calls: lower `BULK_USAGE_CEILING` or pause crawls (`runbook.md` §13); for Riot's 429, see `runbook.md` §9.

### 8. 502 `UPSTREAM_ERROR` `Upstream authentication failed`

- **Cause:** Riot answered 401 or 403; the key is expired, revoked or wrong (`src/riot/client.rs:304-307`, `:176-178`). Never retried.
- **Diagnose:** journal `error` `RIOT KEY REJECTED` with `status`, `method`, `host` (`src/riot/client.rs:305`); `/metrics` `proxy_upstream_requests_total{status="401"}` or `"403"`.
- **Fix:** `runbook.md` §4.

## Request validation

### 9. 400 `BAD_REGION` `params/platform must be equal to one of the allowed values` for `EUW1`

- **Cause:** route path parameters are matched case-sensitively against the 16 lower-case platform ids (`src/http/validate.rs:53-78`; ids `src/riot/routing.rs:46-63`). The same applies to `params/region` (`americas|europe|asia|sea`) and `querystring/platform` (`:194-198`).
- **Fix:** send `euw1`. Match ids may keep their upper-case prefix (`EUW1_…`): the prefix parser is case-insensitive (`src/riot/routing.rs:132-143`).

### 10. 400 `VALIDATION` `querystring must have required property 'platform'` on `/v1/players/{puuid}/matches`

- **Cause:** that route requires `?platform=` (`src/http/validate.rs:200-208`; `src/routes/players.rs:635`). The profile route finds the platform itself; the match page does not.
- **Fix:** take `platform` from the profile body and pass it (`docs/_meta/scratch/siblings.md` §1.5 row 2 shows how the site does it).

### 11. 404 on `/v1/static/*`: `Static data 'champion' has not been synced yet. Run the ddragon:sync job.`

- **Cause:** no patch directory with a `versions.json` exists under `DDRAGON_DIR` yet (`src/routes/statics.rs:289-292`; `:213` for `queues`). `ddragon:sync` runs at boot and hourly (`src/jobs/ticks.rs:195-211`) but needs the CDN.
- **Diagnose:** `ls /var/lib/riot-proxy/data/ddragon`; journal for `syncing Data Dragon` / `Data Dragon sync complete` (`src/jobs/ddragon.rs:241`, `:258`); `GET /v1/admin/jobs?kind=ddragon:sync`.
- **Fix:** §27, then `POST /v1/admin/ddragon/sync {"force":true}`.

## Caching

### 12. `X-Cache: MISS` on every request, or hit ratio under 70 %

- **Cause:** one of: the endpoint is immutable and served `ARCHIVE` not `HIT` (`match.byId`, `match.timeline`; `src/riot/endpoints.rs:185-204`); an admin key sends `?refresh=true` and gets `BYPASS` (`src/routes/passthrough.rs:159-167`); `CACHE_TTL_OVERRIDES` set a tiny soft TTL (`src/riot/endpoints.rs:424-444`); L1 is too small so entries evict at once, and an entry larger than the whole budget is never kept (`CACHE_L1_MAX_MB`, `src/cache/l1.rs:106-119`); the L2 write-behind queue is full so nothing survives a restart (`cache_l2_dropped_total`, `warn` `L2 write queue full; entry not persisted`, `src/cache/l2.rs:102-112`).
- **Diagnose:** `GET /v1/admin/debug/cache?scope=oc1&method=summoner.byPuuid&path=/lol/summoner/v4/summoners/by-puuid/<puuid>` → `{key,present,ageSeconds,stale}` (`src/routes/admin.rs:1232-1248`); `GET /v1/admin/metrics | jq '{cache,cacheStore}'`; `/metrics` `proxy_cache_reads_total{state}` and `cache_l2_dropped_total`.
- **Fix:** remove the override, raise `CACHE_L1_MAX_MB`, or lower `JOB_CONCURRENCY` if the SQLite writer is behind (a rising `cache_l2_dropped_total` means exactly that, `docs/design/metrics.md:56`).

## Storage and maintenance

### 13. `warn` `backup skipped: not enough disk`

- **Cause:** free space on the backup directory is under 1.2 × (db + wal) (`BACKUP_FREE_FACTOR`, `src/jobs/maintenance.rs:34`, `:294-306`). The run goes on without the backup.
- **Diagnose:** `GET /v1/admin/backups` → `lastRun.report.backupSkipped`, `diskFreeBytes` (`src/routes/admin.rs:2583-2606`); `df -h /var/lib/riot-proxy`.
- **Fix:** `runbook.md` §11.

### 14. The WAL keeps growing (`sqlite_wal_bytes` in the hundreds of MB)

- **Cause:** `wal_autocheckpoint=1000` (`src/db/mod.rs:38`) cannot complete while readers hold old snapshots; with readers always active the WAL never checkpointed on the 2-vCPU host (`docs/design/07-deployment.md:171`). The daily `maintenance` runs `PRAGMA wal_checkpoint(TRUNCATE)` and logs `checkpoint_busy`, `wal_frames`, `checkpointed` (`src/jobs/maintenance.rs:254-268`).
- **Diagnose:** `/metrics` `sqlite_wal_bytes`; `ls -la /var/lib/riot-proxy/data/riot-proxy.db-wal`; the last `maintenance done` line.
- **Fix:** `POST /v1/admin/maintenance/run` in a quiet moment; lower `JOB_CONCURRENCY` so fewer readers overlap. Never delete the `-wal` file (`deploy/README.md:258-265`).

## Jobs

### 15. Jobs show `running` right after a crash or restart

- **Cause:** the previous process died with handlers mid-flight; rows stay `running` until the next boot resets them to `pending` with attempts kept (`src/jobs/scheduler.rs:963-973`), logged as `re-queued jobs a previous process left running jobs=n` (`src/cli/serve.rs:284`).
- **Diagnose:** `GET /v1/admin/jobs?state=running` and compare `claimedAt` with `startedAt` from `/v1/admin/info`.
- **Fix:** nothing; it is recovered at boot. If rows are `running` with a `claimedAt` older than the process, the process did not restart cleanly: restart it.

### 16. `warn` `job failed … attempts=5 next=failed`

- **Cause:** the fifth attempt failed, or the handler returned `Fail` (`MAX_ATTEMPTS = 5`, `src/jobs/scheduler.rs:40`, `:1070-1075`, `:1092`). Earlier attempts back off `30 s × 2^attempts ± 20 %` (`:42`, `:160-171`). `archive:match` fails at once on a Riot 404 (`src/jobs/archive.rs:263-307`).
- **Diagnose:** `GET /v1/admin/jobs?state=failed&kind=<kind>` → `error` per row; `GET /v1/admin/jobs/{id}/activity`.
- **Fix:** `POST /v1/admin/jobs/{id}/retry` after the cause (Riot, the store, the disk) is fixed; a 404 match is final.

### 17. A ladder crawl ends `failed`

- **Cause:** a leg (`ladder:apex`, `ladder:walk`, `ladder:collect`) gave up after five attempts; when the last leg ends with `legs_failed > 0` the crawl is finished `failed` (`src/jobs/ladder.rs:784-832`; `src/jobs/ladder/store.rs:351-406`). The partial unique index allows one running crawl per ladder (`src/db/migrations/V0004__ladder.sql:33-34`).
- **Diagnose:** `GET /v1/admin/ladder/crawls?platform=oc1` → `status`, `legsFailed`; `GET /v1/admin/ladder/crawls/{id}` → `failed[]` (`src/routes/admin.rs:2092-2110`); the failed legs' `error` in `/v1/admin/jobs?state=failed&kind=ladder:walk`.
- **Fix:** `POST /v1/admin/ladder/crawl {"platform":"oc1"}` starts a new crawl (`src/routes/admin.rs:1700-1733`); `ladder_entries` already written are kept (`V0004__ladder.sql:7`).

## Object storage

### 18. `archive:offload` keeps failing

- **Cause:** credentials or endpoint. Boot refuses `http://` in production (`ARCHIVE_OBJECT_ENDPOINT: must be https:// when ENV=production`, `src/config.rs:930-942`), an `s3://` URL without an endpoint (`is required with an s3:// ARCHIVE_OBJECT_URL`, `:1004`), one key without the other (`set both or neither`, `:997-1000`), or a production URL without keys (`:1006-1011`). A boot that passes logs `object store configured store=s3://riot-proxy-archive at <endpoint>` (`src/cli/serve.rs:103`; `src/archive/objects.rs:134`); a store that then rejects requests makes every run `Retry` (`src/jobs/offload.rs:147`).
- **Diagnose:** `GET /v1/admin/jobs?kind=archive:offload` → `error`; `rp packs snapshot` exercises a PUT with the service's credentials (`src/cli/packs.rs:55-65`); `sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1`.
- **Fix:** correct the env file and restart (`runbook.md` §6); `archive_offload_pending` falls on the next run.

### 19. `warn` lines ending in `reading from Riot`

- **Cause:** an offloaded body could not be read from its pack: `offloaded body has no location` (`src/archive/matches.rs:250`), `offloaded bodies but no ARCHIVE_OBJECT_URL` (`:258-261`, the URL was unset after bodies were freed), `pack id is not a ULID` (`:284`), `pack read failed` (`:295`), `offloaded body failed its sha256` (`:306`), `offloaded body did not decompress` (`:317`). The body is dropped and the request goes to Riot (`src/archive/mod.rs:128-144`); the re-fetched body is stored locally again (`src/archive/matches.rs:399`).
- **Diagnose:** the line's `key`/`pack_id`; `GET /v1/admin/stats | jq .archiveBodies`; `rp packs reindex --since <pack id>` lists packs that cannot be read (`src/cli/packs.rs:32-35`, `:71-83`).
- **Fix:** for a missing URL, set it back (`docs/design/12-object-storage-and-hosting.md:943`); for a store outage, `runbook.md` §10; for a damaged pack, the R2 replica holds a copy (`deploy/README.md:12-18`).

## Configuration refusals at boot

### 20. `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`

- **Cause:** `TLS=true` or `1` in the env (`src/config.rs:681-686`); built-in TLS went in ADR-067 (`docs/design/07-deployment.md:59-61`).
- **Fix:** remove `TLS`, `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT` (the last three are dropped silently, `src/config.rs:889-896`).

### 21. `AUTH_DISABLED cannot be enabled when ENV=production`

- **Cause:** `src/config.rs:671-675`. `AUTH_DISABLED` is for local work only; it runs every request as `dev-local` with `read,admin` (`src/http/auth.rs:43-52`).
- **Fix:** remove it from `/etc/riot-proxy/env`; mint keys instead.

### 22. `warn` `DEV_UI=true ignored: /dev is never served when ENV=production`

- **Cause:** `DEV_UI` is forced off in production (`src/config.rs:677-679`, `:778-779`; `src/cli/serve.rs:95-97`). Not an error; `/dev` and `/dev/reset` are simply absent.
- **Fix:** remove the variable to silence the warning. The dashboard (`/dashboard`, `DASHBOARD_UI`) is unaffected.

### 23. Boot fails with `binding 127.0.0.1:8080: Address already in use`

- **Cause:** another process holds the port; the bind error carries that context (`src/cli/serve.rs:143-146`) and exits 1.
- **Diagnose:** `sudo ss -ltnp 'sport = :8080'`; often a previous instance still draining (10 s grace, `src/app.rs:35`) or a stray `rp … serve`.
- **Fix:** wait or stop the other process, then `sudo systemctl restart riot-proxy`.

### 24. `warn` `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored key=match`

- **Cause:** the key is unknown or names only immutable endpoints (`match`, `timeline`), whose bodies are archived, not cached (`src/cli/serve.rs:106-108`; `src/riot/endpoints.rs:500-512`). Valid keys: `account, summoner, league, ladder, match, timeline, spectator, mastery, rotations, status` (`docs/_meta/scratch/config.md` §1.2). Malformed items are skipped silently (`src/riot/endpoints.rs:424-444`).
- **Fix:** correct the key; the rest of the list still applies.

## Other environments

### 25. The Proxmox dev VM logs `error: rejected ghcr.io/ninjagoldfinch/riot-proxy:edge (<commit>): …; rolled back to riot-proxy:previous (<commit>)`

- **Cause:** `riot-proxy-update` found the new `:edge` image unhealthy or crash-looping and restored `:previous`; it records the image in `/opt/riot-proxy/.rejected` so later runs skip it (`deploy/proxmox/files/riot-proxy-update:69-96`; `deploy/proxmox/README.md:72-85`).
- **Diagnose:** `cat /opt/riot-proxy/.rejected`; the VM's journal for the health reason.
- **Fix:** fix `main`; a newer edge image is tried automatically. To retry the same image: `sudo rm /opt/riot-proxy/.rejected && sudo riot-proxy-update` (`deploy/proxmox/README.md:80-81`). The VM is dormant today (see the writing brief); the procedure is kept for when it runs.

### 26. The container `HEALTHCHECK` fails with `error: Invalid environment configuration: RIOT_API_KEY: is required`

- **Cause:** `riot-proxy healthcheck` loads the full configuration before it GETs `/healthz` (`src/cli/mod.rs:82-88`; `src/cli/healthcheck.rs`), so it needs the same env as `serve` (`src/config.rs:623-627`). Docker runs it as `CMD ["/riot-proxy","healthcheck"]` (`Dockerfile:24`).
- **Diagnose:** `docker exec <ctr> /riot-proxy healthcheck`; `docker inspect --format '{{json .State.Health}}' <ctr>`.
- **Fix:** pass `RIOT_API_KEY` (and the rest of `.env`) into the container's environment, not only into a mounted file the health command cannot see.

### 27. `ddragon:sync` fails or stays `running`

- **Cause:** the CDN or `static.developer.riotgames.com` is unreachable: `warn` `Data Dragon file unavailable; skipping` (`src/jobs/ddragon.rs:252`) or `queue table unavailable; keeping what is on disk` (`:281`); any error makes the job `Retry` (`:302-304`). Timeouts are 15 s connect, 60 s total (`:111-112`). The job is a deduped singleton, so a second request joins the queued one (`src/jobs/ticks.rs:76-80`; `src/routes/admin.rs:1640-1650`).
- **Diagnose:** `curl -sI https://ddragon.leagueoflegends.com/api/versions.json` from the box; `GET /v1/admin/jobs?kind=ddragon:sync`; `ls -la /var/lib/riot-proxy/data/ddragon` for write permission.
- **Fix:** restore connectivity or permissions, then `POST /v1/admin/ddragon/sync {"force":true}`.

### 28. The bootstrap admin key was never shown

- **Cause:** it is printed only when the `consumers` table is empty at `serve` and `BOOTSTRAP_ADMIN_KEY` is unset (`src/consumers.rs:308-333`; `src/cli/serve.rs:57-68`, `:421-427`). With `BOOTSTRAP_ADMIN_KEY` set, that key is imported silently; with any consumer already present, nothing is created. It goes to stderr, not the log (`src/cli/serve.rs:423`).
- **Diagnose:** `rp key list` shows whether `bootstrap-admin` exists (`src/consumers.rs:16`).
- **Fix:** mint a new admin key with `rp key create --name admin-2026-10 --scopes read,admin`; a lost key cannot be recovered (only its hash is stored, `src/consumers.rs:105-107`).

## Analytics

### 29. A ladder's analytics stop moving after crawls, or `lastRuns` has no `delta` for it

- **Cause:** since THR-03 (ADR-182) a completed crawl of a stamped ladder queues `aggregate:delta`, not a rebuild, and the delta counts only ladders stamped by a full rebuild with nothing marked dirty since, of any platform (`src/jobs/analytics.rs:779-811`; `src/archive/analytics/delta.rs:199-222`). A ladder is unstamped after the upgrade to V0029 until its first full rebuild finishes, and dirty after a `facts:reextract` or a `BUILDS_VERSION` bump rewrote matches already counted; either way a full rebuild is queued for it and the delta skips it until then. A crawl of an unstamped ladder (a platform outside `LADDER_PLATFORMS`, started from the admin route, or one before its first full rebuild) queues that rebuild at its end, which stamps it; deltas count it from then on. Boot and each delta queue the rebuild only for configured ladders.
- **Diagnose:** `GET /v1/admin/metrics | jq '.analytics.lastRuns[] | {platform,queue,kind,at,status}'`; `GET /v1/admin/jobs/queue` for a pending or running `aggregate:analytics`; the journal's `full rebuilds queued for ladders not stamped yet queued=n` (`src/jobs/analytics.rs:805`) or, at boot, `queued full analytics rebuilds to stamp counted matches ladders=n` (`src/cli/serve.rs:327-330`). `GET /v1/admin/info` shows `AGGREGATE_DELTA_S`; at `0` crawls queue rebuilds as before.
- **Fix:** let the queued rebuild finish (minutes per ladder on production; `runbook.md` §19 step 1). For an unconfigured platform, run a crawl of it or press Recompute now for it (`POST /v1/admin/analytics/recompute {"platform":"…"}`): its rebuild stamps it.

### 30. `aggregate_delta_pending` keeps rising

- **Cause:** the gauge counts the key scope's matches with any rebuild step still uncounted, read at the start of each delta (`src/archive/analytics/delta.rs:224-231`, `src/jobs/analytics.rs:796-797`). It grows when matches arrive faster than the deltas count them, when a ladder is waiting for its full rebuild (§29), or while `facts:reextract` runs (matches whose facts are below `FACTS_VERSION` are left for it, `delta.rs:233-270`). Solo and flex matches of platforms or queues that are not configured ladders are in the count and are never counted, so a floor that stays level is normal.
- **Diagnose:** `aggregate_delta_matches_total{kind="added"}` should rise with it; `aggregate_delta_batch_seconds{part="sql"}` near `AGGREGATE_DELTA_TARGET_MS` and `aggregate_delta_batch_size` show the batches; `part="commit"` of seconds is a slow writer (§1). `proxy_aggregate_runs_total{kind="delta"}` not moving means no delta is running: check `AGGREGATE_DELTA_S` and `GET /v1/admin/jobs?kind=aggregate:delta&state=failed`.
- **Fix:** none for arrival bursts; it drains between crawls. A stuck ladder: §29. A delta that fails repeatedly: retry it after the cause (`runbook.md` §14); a full rebuild of the ladder (Recompute now) is the repair.

### 31. The writer is busy for about an hour after an upgrade

- **Cause:** the first boot with V0029 and `AGGREGATE_DELTA_S` > 0 queues one full rebuild over every patch per configured ladder, one at a time; by the 2026-10-20 production runs that is about 58 minutes in all (oc1 532 s, na1 970 s, kr 1012 s, euw1 980 s; `docs/thr/THR-03-plan.md`). `/readyz` answers `"sqlite":true,"writerBusy":true` with `writerBusyMs` climbing meanwhile (§1).
- **Diagnose:** `GET /v1/admin/jobs/queue` shows `aggregate:analytics` running and the rest pending; `lastRuns` gains a `rebuild` per ladder as each finishes.
- **Fix:** nothing; pause crawls for the hour if you can. A restart costs only the rebuild in progress, which the next boot queues again.

### 32. After a restart a crawl sits in *collect* with no batch running

- **Cause:** before TL-03 the boot catch-up of missing halves (matches without a timeline, timelines without a match) queued at priority 100, a band ahead of every crawl stage, and each region's app limit then spent itself on it: on 2026-10-21 production's three crawls waited about an hour behind 938–2 029 catch-up jobs per region while `/v1/admin/metrics` showed each region's `100:120` window at 95/100. Since TL-03 (ADR-184) the catch-up queues at 20 004, after a crawl's collect (`src/jobs/mod.rs`, `priority::CATCH_UP`).
- **Diagnose:** `sqlite3 -readonly … "SELECT lane, priority, count(*) FROM jobs WHERE kind = 'archive:match' AND state = 'pending' AND priority < 20000 GROUP BY 1, 2"`; the limiter's windows in `/v1/admin/metrics` `.limiter[]`.
- **Fix:** on 2.8.0 or earlier, wait: the backlog drains at about 2 500 jobs an hour per region. From TL-03 on, collect goes first.

---
**Sources:** `src/routes/health.rs:21-46,148-200`, `src/db/writer.rs:22,158-206,214-230,244-275`, `src/db/mod.rs:29,38,41-42,160-172,229-257,367-373`, `src/jobs/analytics.rs:354-520,410,779-811`, `src/archive/analytics/delta.rs:199-270`, `src/cli/serve.rs:319-332,57-68,75-81,95-97,101-108,143-148,284,421-427`, `src/riot/limiter/persist.rs:224-229`, `src/http/auth.rs:32,43-52,118-139,193-199,267-288`, `src/http/error.rs:107`, `src/http/quota.rs:131,320,339-399`, `src/http/validate.rs:53-78,194-208`, `src/riot/routing.rs:46-63,132-143`, `src/riot/client.rs:176-183,304-318`, `src/riot/limiter/mod.rs:58-80`, `src/fetcher.rs:460-466`, `src/riot/endpoints.rs:185-204,424-444,500-512`, `src/routes/passthrough.rs:159-167`, `src/routes/players.rs:635`, `src/routes/statics.rs:213,289-292`, `src/cache/l1.rs:106-119`, `src/cache/l2.rs:102-112`, `src/jobs/maintenance.rs:34,254-268,294-306`, `src/jobs/scheduler.rs:40-42,160-171,963-973,1070-1092`, `src/jobs/archive.rs:263-307`, `src/jobs/ladder.rs:784-832`, `src/jobs/ladder/store.rs:351-406`, `src/jobs/ddragon.rs:111-112,241-304`, `src/jobs/offload.rs:147`, `src/jobs/ticks.rs:195-211`, `src/archive/matches.rs:250-317,399`, `src/archive/mod.rs:128-144`, `src/archive/objects.rs:134`, `src/config.rs:623-627,671-686,729,778-779,889-896,930-942,997-1011`, `src/consumers.rs:16,105-107,308-333`, `src/cli/mod.rs:82-88`, `src/cli/key.rs:59-79`, `src/cli/packs.rs:32-35,55-83`, `src/routes/admin.rs:44-82,181-200,1010-1027,1232-1248,1700-1733,1908-1925,2092-2110,2438-2477,2583-2606`, `src/db/migrations/V0004__ladder.sql:7,33-34`, `src/app.rs:35`, `Dockerfile:24`, `deploy/README.md:12-18,258-265`, `deploy/proxmox/README.md:72-85`, `deploy/proxmox/files/riot-proxy-update:69-96`, `docs/design/07-deployment.md:59-61,171,221`, `docs/design/12-object-storage-and-hosting.md:943`, `docs/design/metrics.md:56`, `PROGRESS.md:342`, `docs/thr/THR-03-plan.md`
**Last verified:** 2026-10-26 against commit `220cdef`

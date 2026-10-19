# Jobs, scheduler, realtime, metrics, logging — code inventory

Evidence inventory at HEAD 7491ff6 (branch `docs/DOC-01-documentation-set`). Every claim cites `file:line`. Config defaults cite the `Config` literal in `src/config.rs::from_sources` (lines 695–756); the ±1 risk on those lines is noted once here rather than per row.

---

## 1. Boot sequence of `riot-proxy serve`

`src/cli/serve.rs::serve_with` (lines 43–394), in order:

| # | Step | Where | Log line (exact) |
|---|---|---|---|
| 1 | Install tracing (JSON or pretty per `LOG_FORMAT`, filter `LOG_LEVEL`) | serve.rs:44–46 → telemetry.rs:481–491 | — |
| 2 | Install Prometheus recorder (v1 buckets, describe catalogue), spawn histogram upkeep every 5 s | serve.rs:48–49 → telemetry.rs:512–527, 467, 531–539 | — |
| 3 | Open SQLite: create parent dir, writer pragmas (WAL, synchronous=NORMAL, wal_autocheckpoint=1000), connection pragmas, **run embedded refinery migrations grouped in one tx**, `PRAGMA optimize`, open reader pool (`default_readers()` = CPU count, fallback 4, db/mod.rs:190–193), start `sqlite-writer` thread (write queue 1024) | serve.rs:51–55 → db/mod.rs:135–172, 312–316, 31–38 | `database ready` (path=) serve.rs:55 |
| 4 | Bootstrap admin consumer (`BOOTSTRAP_ADMIN_KEY` imported if set, else a key is minted and printed **to stderr, not the log**) | serve.rs:57–68, 399–405 | `bootstrap admin consumer created` (id=) serve.rs:63; stderr `bootstrap admin key (shown once): …` 401–403 |
| 5 | Build limiter (`BULK_USAGE_CEILING`, idle ceiling/after), restore checkpoint from `limiter_state`, set `limiter_restored`, spawn checkpoint task every 10 s | serve.rs:71–82 → limiter/persist.rs:25, 232–234 | `limiter checkpoint restored` (rows=) :77 / warn `limiter checkpoint could not be read; starting fresh` :79 |
| 6 | L1 cache from config; warm L1 from L2; sweep expired L2 rows; `ResponseCache` with `L2Writer` (flush every 2 s, batch 500, queue 10 000) | serve.rs:85–93 → cache/l2.rs:24–28 | `L1 warmed from L2` (entries=) :87 / warn `L2 warm failed; starting with a cold cache` :88 / warn `L2 sweep failed` :91 |
| 7 | TTL policy; warn if `DEV_UI=true` under production; attach object store if `ARCHIVE_OBJECT_URL`; warn on ineffective `CACHE_TTL_OVERRIDES` keys | serve.rs:94–108 | `DEV_UI=true ignored: /dev is never served when ENV=production` :96; `object store configured` (store=) :103; `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored` :107 |
| 8 | Build `Queue` with `JOB_KIND_LIMITS`; `offload::Pending` trigger only with a bucket | serve.rs:111–117 | — |
| 9 | Build `Fetcher` with `SqliteArchive` (queues missing halves at priority 0, late-stamp days, counts unpacked bytes), `CLIENT_WAIT_BUDGET_MS` interactive budget, `JOB_YIELD_BUDGET_MS` job budget, SWR | serve.rs:118–139 | — |
| 10 | Install SIGTERM/SIGINT handlers **before** bind; bind `HOST:PORT` | serve.rs:143–148 → app.rs:238–248 | `listening` (addr=) :148 |
| 11 | Hub, Data Dragon `Mirror`+`Cdn`, every job context (`PollContext`, `ArchiveContext`, `LadderContext`, `NamesBackfill`, `Maintenance` (+bucket jobs), `Analytics` cache, `AnalyticsContext`, `TimelinesContext`), `Activity`, handler registry; `archive:offload` + `archive:index-snapshot` registered **only with a bucket** | serve.rs:152–270 → jobs/mod.rs:89–162 | — |
| 12 | `Scheduler` with limiter + activity; `recover()` resets `running`→`pending` | serve.rs:271–278 → scheduler.rs:963–973 | `re-queued jobs a previous process left running` (jobs=) :276 |
| 13 | `assign_lanes()` for pre-V0007 pending rows | serve.rs:279–283 → scheduler.rs:1029–1054 | `gave queued jobs their rate-limit lane` :281 |
| 14 | TL-01 catch-up: queue `archive:match` for stored matches lacking a timeline / timelines lacking a match, priority 100 | serve.rs:284–288 → jobs/archive.rs:169–214 | `queued the missing halves of archived matches` (matches=) :286 |
| 15 | `facts:reextract` if any match has `facts_version < 4` | serve.rs:290–294 → analytics.rs:145–150 | `queued facts:reextract for matches an older facts version derived` :291 |
| 16 | `tiers:backfill` if any ranked match has an unstamped fact | serve.rs:297–301 → analytics.rs:102–111 | `queued tiers:backfill for matches archived without tier stamps` :298 |
| 17 | `aggregate:analytics` per ladder still on `analytics_unsplit` | serve.rs:304–311 → analytics.rs:116–129 | `queued analytics rebuilds to split rows by tier and side` (ladders=) :306 |
| 18 | With a bucket: queue one `archive:offload` | serve.rs:313–318 | `queued archive:offload` :315 |
| 19 | Start `JOB_CONCURRENCY` workers + `jobs_pending` sampler (15 s) | serve.rs:319–324 → scheduler.rs:1172–1247 | `starting job workers` (workers=, kind_limits=) :319–323 |
| 20 | Start ticks (`running_schedule`, ladders = `LADDER_PLATFORMS × LADDER_QUEUES`). **Every tick's first firing is immediate** (ticks.rs:170, 198–202), so at boot: poll fan-outs, `ddragon:sync`, `maintenance`, `names:backfill` (and `ladder:crawl`, `aggregate:analytics`, `timelines:backfill` if enabled) are all enqueued at once | serve.rs:325–330 → ticks.rs:276–323, 332–372 | debug `tick` (tick=, queued=) ticks.rs:365 / warn `tick could not enqueue` :366 |
| 21 | `Stats`; `metrics` WS topic ticker every `METRICS_INTERVAL_S` (5 s); history sampler (fast 15 s, history point every `METRICS_HISTORY_INTERVAL_S` = 60 s) | serve.rs:335–358 → ws/metrics.rs:533–542, stats.rs:1005–1036 | — |
| 22 | `Auth`, `AppState`, `app::serve` with graceful shutdown (grace 10 s, app.rs:35) | serve.rs:360–378 → app.rs:199–233 | on signal: `SIGTERM`/`SIGINT` app.rs:244–245; `shutdown signal received; draining` (grace_s=) app.rs:217; warn `drain exceeded the grace period; exiting with requests in flight` app.rs:229 |
| 23 | **Shutdown order**: ticks stop → metrics ticker abort → history sampler abort → `workers.shutdown(10 s)` (running jobs finish or are aborted; rows stay `running` for next boot's `recover`) → `hub.shutdown()` (close 1001) → checkpoint task abort → final limiter checkpoint → L2 flush | serve.rs:383–392, 397; scheduler.rs:1291–1300; ws/hub.rs:410–414 | warn `jobs still running at shutdown; they resume on the next boot` scheduler.rs:1296; `limiter checkpoint and L2 flushed; stopped` serve.rs:391 |

`ROLE`: SQLite allows only `all` (config.rs:640–646); api/worker split needs Postgres (stub).

---

## 2. The scheduler

### 2.1 `jobs` table

DDL `src/db/migrations/V0001__init.sql:33–45`: `id TEXT PK` (ULID, monotonic generator scheduler.rs:190–198), `kind`, `dedupe_key`, `priority INTEGER` (lower first), `payload TEXT` (JSON), `state TEXT DEFAULT 'pending'`, `attempts INTEGER DEFAULT 0`, `run_after INTEGER` (unix ms), `claimed_at`, `finished_at`, `error`. Indexes: `jobs_dedupe UNIQUE(kind, dedupe_key) WHERE state IN ('pending','running')` (:44), `jobs_claim` (:45). V0007 (`V0007__job_lane.sql:5–9`) adds `lane TEXT`, `method TEXT`, `jobs_lane_claim(lane, priority, run_after, id) WHERE state='pending'`, `jobs_lane_running(lane) WHERE state='running'`. V0022 adds covering `jobs_state_kind(state, kind, run_after, finished_at)` for every count.

States: `pending | running | done | failed` (scheduler.rs:506). Columns read back: `COLUMNS` :173, `ROW_COLUMNS` :535–536.

### 2.2 Priority bands (`src/jobs/mod.rs:71–86`)

| Const | Value | Used by |
|---|---|---|
| `INTERACTIVE` | 0 | `archive:match` for a just-finished game (poll.rs:360–366), a stored match/timeline's missing half (archive/mod.rs:101), admin recompute (analytics.rs:79–83) |
| `ARCHIVE_DEPTH` | 100 (+ depth/10) | `archive:match` by depth (poll.rs:364, archive.rs:112–114); boot TL-01 catch-up at exactly 100 (archive.rs:202) |
| `POLL` | 10 000 | `poll:*` |
| `BACKFILL` | 20 000 | `backfill:player`; ladder order: crawl 20 000, apex 20 001, walk 20 002, collect 20 003, archive 20 004, crawl-found `archive:match` 20 005, ranks:lookup 20 006 (ladder.rs:48–61) |
| `MAINTENANCE` | 30 000 | `aggregate:analytics`, `facts:reextract`, `builds:extract`, `tiers:backfill`, `maintenance`, `ddragon:sync`, `names:backfill`, `archive:offload`, `archive:index-snapshot` |
| `TIMELINE_BACKFILL` | 40 000 | `timelines:backfill` |

Claim band key `CLAIM_BAND`: `<100 → 0`, `<10 000 → 100`, else `priority - priority % 10000` (scheduler.rs:599–600). Order inside a lane `CLAIM_ORDER = priority ASC, run_after ASC, id ASC` (:594).

### 2.3 Enqueue and dedupe

- `enqueue_on`: `INSERT OR IGNORE` with lane/method derived by `lanes::of(kind, payload)`; if ignored, returns the existing pending/running row's id with `created=false` (scheduler.rs:225–254). `Enqueued::status()` → `"queued"` / `"already-queued"` (:88–92).
- `enqueue_or_promote_on`: same, but a **pending** duplicate is lifted to `MIN(priority)` and `MIN(run_after)`; a running one is left alone (:259–269). Used by admin recompute (analytics.rs:79–83) and TL-01 missing halves (archive.rs:151, which also forces `fetchTimeline=true` on the pending row :153–157).
- `Queue::enqueue` wakes one worker (`notify_one`), `enqueue_all` wakes all (:418–472).
- Dedupe keys per kind: see §3 table.

### 2.4 Lanes (`src/jobs/lanes.rs`)

`of()` :425–445 maps Riot-calling kinds to `(lane, method)`: `poll:live`→(platform, `spectator.activeGame`); `poll:rank`/`ranks:lookup`→(platform, `league.entriesByPuuid`); `poll:matches`/`backfill:player`/`ladder:collect`→(region of platform, `match.idsByPuuid`); `archive:match`→(region from match id, `match.byId`); `ladder:walk`→(platform, `league.entriesByTier`); `ladder:apex`→(platform, `league.master|grandmaster|challenger`); `timelines:backfill`→(region, `match.timeline`). Everything else has no lane (`LANED` :448–459). All lanes = 16 platforms + 4 regions (:463–468; routing.rs:46, 146).

### 2.5 Claim loop

`Scheduler::claim` (scheduler.rs:977–991): asks the limiter `bulk_blocked()` → `ClaimFilter` of open lanes and blocked `"lane method"` pairs, plus `kind_limits`; runs `claim_sql` (db/store.rs:98–133): one `UPDATE jobs SET state='running', claimed_at=$1, attempts=attempts+1 WHERE id = (…) RETURNING …`. The subquery takes each open lane's best ready row (skipping blocked methods and capped kinds), plus the best lane-less row, then orders heads by band, then by running count on that lane (spread), then `priority, run_after, id`.

Worker loop (:1177–1225): `control.enter()` (halt check) → claim → run → `leave()`. If nothing claimable: sleep `idle_for()` = min(next delayed `run_after`, earliest blocked lane/method free time), else `IDLE_POLL` 1 s, floor `MIN_IDLE` 10 ms (:996–1025); woken early by `Notify`. Claim error → `error!("job claim failed")` + 1 s sleep (:1218–1221).

`run` (:1126–1158): handler spawned in its own task with the job as task-local `activity::Current`; a panic → `Retry("handler panicked")`; an abort (shutdown/halt) records `aborted` and no outcome. Missing handler → `Fail("no handler for job kind '…'")`. Outcome recorded via `finish` (:1060–1121).

### 2.6 Retries, backoff, yield, failure

- `MAX_ATTEMPTS = 5` (:40), `BACKOFF_BASE = 30 s` (:42), `backoff(attempts, jitter) = 30 s × 2^min(attempts,20) × jitter∈[0.8,1.2]` (:160–171).
- `Retry(e)` with `job.attempts < 5` → `pending`, `run_after = now + backoff(job.attempts)`, `error` kept (:1070–1074, 1108–1111). **Note:** `job.attempts` already includes the current run (claim increments it, store.rs:104), so the first retry waits ~60 s, not 30 s — design/06 §Backoff says `2^attempts × 30 s` without saying which count.
- 5th failure, or any `Fail(e)` → `failed`, `finished_at`, `error` (:1075, 1112–1115). Log: warn `job failed` (job=, id=, attempts=, next=pending|failed, error=) :1092; metric `proxy_jobs_total{job,status="failed"}` :1086 (counted on every backoff too).
- `Ok` → `done`, `finished_at`, `error=NULL`; `proxy_jobs_total{status="completed"}` (:1078–1081, 1098–1101).
- `Yield{retry_at, payload}` (`JOB_YIELD_BUDGET_MS`): `pending`, `run_after=retry_at`, `attempts = max(attempts-1, 0)`, `payload = coalesce(new, old)`; **not** counted in `jobs_total`; debug `job yielded to the rate limiter` (:1082–1084, 1102–1107). Source: a job fetch (`FetchOptions::JOB` fetcher.rs:128–139) whose limiter wait would exceed `JOB_YIELD_BUDGET_MS` (default 1000, config.rs:711) fails with `limited_until` (limiter/mod.rs:288–290) → `JobError::from_fetch` (:134–142). `take_turns()` = yield with `retry_at = now` (:145–150).
- `recover()` at boot: `running`→`pending`, `claimed_at=NULL`, attempts kept (:963–973).
- Admin: `retry(id)` (`failed`→`pending`, attempts=0; refused if a live duplicate holds the dedupe key) :723–765; `cancel(id)` (`pending`→`failed`, error `cancelled`) :769–795; `purge{kind,state∈pending|failed,min_priority,dry_run}` in chunks of 5 000 with 20 ms pauses :510–514, 803–842; `cancel_pending_on` for a crawl's legs :204–221. Routes: `GET /v1/admin/jobs`, `/jobs/stats`, `POST /jobs/{id}/retry`, `DELETE /jobs/{id}`, `DELETE /jobs`, `GET /jobs/queue`, `/jobs/activity`, `/jobs/{id}/activity` (routes/admin.rs:1326, 1396, 1419, 1448, 1491, 1928, 1967, 2000).
- `done` rows are deleted by `maintenance` after 7 days (`JOB_RETENTION_MS` maintenance.rs:36, 237–240).

### 2.7 Concurrency knobs

| Var | Default | Effect | Where |
|---|---|---|---|
| `JOB_CONCURRENCY` | 8 (min 1) | worker tasks | config.rs:709; serve.rs:324; scheduler.rs:1172–1177 |
| `JOB_KIND_LIMITS` | `aggregate:analytics=1,archive:offload=1` | per-kind running cap applied inside the claim (`$4`); only lane-less kinds accepted | config.rs:111, 710; store.rs:106–109; scheduler.rs:368–379 |
| `JOB_YIELD_BUDGET_MS` | 1000 (0–900 000) | max limiter wait per job fetch before yield | config.rs:711; serve.rs:137 |
| `Queue::halt(wait)` (DEV-22) | — | holds workers, aborts running handlers (dev reset only) | scheduler.rs:387–415 |

### 2.8 Live activity (DEV-19, `src/jobs/activity.rs`)

In-memory only: per-job trace (worker, attempt, start, `now` line, ≤200 events `EVENTS_PER_JOB` :40, 200 finished traces kept `FINISHED_KEPT` :42). Event kinds `job|step|riot|wait|error` (:52–55). Handlers call `activity::step/event/now` (:364–378); the fetcher logs each Riot call with X-Cache outcome and ms (fetcher.rs:284–296), limiter waits (:446, 473), 429 backoffs and 5xx retries (:576, 592).

### 2.9 Ticks (`src/jobs/ticks.rs`)

`every()` :193–209 = `tokio::interval` with `MissedTickBehavior::Skip`; first tick immediate. `schedule()` :276–304:

| Tick | Cadence | Config var (default) | Enqueues |
|---|---|---|---|
| `poll:live` | `TRACK_POLL_LIVE_S` | 60 s (min 10) config.rs:723 | `fan_out`: one job per tracked player (`players.tracked=1`, players.rs:111–119), priority 10 000, dedupe puuid, payload `{puuid, platform}` (ticks.rs:213–241) |
| `poll:rank` | `TRACK_POLL_RANK_S` | 600 s (min 30) :724 | same fan-out |
| `poll:matches` | `TRACK_POLL_MATCH_S` | 300 s (min 30) :725 | same fan-out |
| `ddragon:sync` | `DDRAGON_SYNC_S` | 3600 s (min 60) :726 | `singleton` priority 30 000, dedupe `ddragon:sync`, payload `{}` (ticks.rs:243–247) |
| `maintenance` | fixed 86 400 s | — ticks.rs:284 | singleton |
| `names:backfill` | fixed 86 400 s | — :287 | singleton |
| `ladder:crawl` | `LADDER_CRAWL_S` | **0 = off** (max 604 800) config.rs:741 | one `ladder:crawl` per (`LADDER_PLATFORMS` × `LADDER_QUEUES`), dedupe `platform:queue` (ticks.rs:249–257, 306–318; ladder.rs:321–334). `LADDER_PLATFORMS` default empty → no crawl even if `LADDER_CRAWL_S` set (config.rs:337, 685–688) |
| `aggregate:analytics` | `AGGREGATE_INTERVAL_S` | **0 = off** :755 | one per ladder, dedupe `platform:queue` (ticks.rs:259–270) |
| `timelines:backfill` | fixed 3600 s (`TIMELINE_BACKFILL_EVERY` :273) | only if `TIMELINE_BACKFILL_PATCHES` > 0 (default 0) :728 | one job per match-v5 region (4), dedupe region (timelines.rs:253–271) |

Ticks persist nothing; `Ticks::shutdown` flips a watch and joins (:374–377).

---

## 3. Job kinds

All 21 kinds: `src/jobs/mod.rs:20–68`. Every Riot call from a job goes through `Fetcher::fetch(req, FetchOptions::JOB)` (bulk priority, yield budget).

| Kind | Enqueued by | Payload | Priority / dedupe | What it does | Riot endpoints | Tables written | Events | Retry/notes | Config |
|---|---|---|---|---|---|---|---|---|---|
| `poll:live` | tick (ticks.rs:352–354) | `{puuid, platform}` | 10 000 / puuid | Load player (`players.get`); GET spectator; 404 = not in game. New `gameId` ≠ stored `in_game_id` → set `InGame(Some)`, publish `game.started`. Stored game, none now → set `InGame(None)`, publish `game.ended`, enqueue `poll:matches` with `run_after = now+60 s` (`MATCH_NUDGE` poll.rs:35), dedupe puuid (poll.rs:102–184) | `spectator.activeGame` (platform) | `players.in_game_id` (via `set_poll_state`) | `game.started`, `game.ended` | 404 handled; other fetch errors → Retry/Yield; unknown player → Ok | `TRACK_POLL_LIVE_S` |
| `poll:rank` | tick | `{puuid, platform}` | 10 000 / puuid | GET league entries; store snapshot `{queue→{tier,rank,lp}}` in `players.last_rank`; first observation = baseline; each changed queue → `rank.changed{before,after}` (poll.rs:197–257). The fetcher's archive `observe` also records `player_ranks` + late tier stamps (archive/mod.rs:180–201) | `league.entriesByPuuid` (platform) | `players.last_rank`; `player_ranks`, `rank_lookups`, `match_facts.tier` (late stamp) | `rank.changed` | fetch error → Retry/Yield | `TRACK_POLL_RANK_S`, `TIER_LATE_STAMP_DAYS` |
| `poll:matches` | tick; `poll:live` nudge | `{puuid, platform}` | 10 000 / puuid | Page `match.idsByPuuid` from `start=0`: first page `count=5` (`POLL_PAGE`), later `count=100` (`CATCHUP_PAGE`), until the stored cursor `last_seen_match_id`, an empty/short page, or `start ≥ TRACK_CATCHUP_LIMIT` (then warn `match poll fell behind its catch-up limit; queuing a backfill` and enqueue `backfill:player{limit: LOOKUP_BACKFILL_LIMIT, reason:"catchup"}`). `filter_unarchived` → `archive:match` per id at `archive_priority(depth)` (0 for depth<5, else 100+depth/10), dedupe matchId, payload `{matchId, puuid, fetchTimeline: ARCHIVE_TIMELINES}`; cursor = newest id seen (poll.rs:264–354) | `match.idsByPuuid` (region) | `players.last_seen_match_id`; `jobs` | — | — | `TRACK_POLL_MATCH_S`, `TRACK_CATCHUP_LIMIT` (500, config.rs:740), `LOOKUP_BACKFILL_LIMIT`, `ARCHIVE_TIMELINES` |
| `archive:match` | `poll:matches`, `backfill:player`, `ladder:archive` (20 005), archive missing-half hook (0, archive/mod.rs:88–110), boot TL-01 catch-up (100, archive.rs:169–214) | `{matchId, puuid?, fetchTimeline?}` | 0 / 100+depth/10 / 20 005; dedupe matchId | Region from match id; GET `match.byId` (the fetcher's `SqliteArchive` stores body + facts + tier stamps and bumps `proxy_archived_matches_total`, archive/matches.rs:395–404); if `x_cache != ARCHIVE` publish `match.archived` **before** the timeline; if `fetchTimeline` (default `ARCHIVE_TIMELINES`) GET `match.timeline`: a limiter yield is propagated, any other timeline error is only a warn `timeline fetch failed` (archive.rs:245–289) | `match.byId`, `match.timeline` (region) | `matches`, `match_facts`, `match_bans`, `timelines` (via archive), `body_*` counters | `match.archived` | 404 → `Fail("match … not found")`; else Retry/Yield | `ARCHIVE_TIMELINES` (true, config.rs:727) |
| `backfill:player` | first lookup (`reason:"lookup"`, routes/players.rs:575–590), tracking (`"track"`, admin.rs:527–536), `POST /v1/admin/backfill` (`"admin"`, admin.rs:1604–1615), `poll:matches` (`"catchup"`) via `enqueue_backfill` (archive.rs:218–225, counts `proxy_backfills_queued_total{reason,status}`) | `{puuid, platform, limit (default 500), fetchTimeline?, queueId?, reason?}` | 20 000 / puuid | Upsert player; resume from `players.backfill_state.cursor` if same limit; page `match.idsByPuuid` 100 at a time (`BACKFILL_PAGE`) up to `limit`, optional `queue`; enqueue `archive:match` at `100+pos/10` per unarchived id; save cursor after every page; complete when history ran out (unfiltered) or `limit ≥ LOOKUP_BACKFILL_LIMIT` → `doneAt` (archive.rs:309–419). Log `backfill finished` (puuid, queued, depth, complete, reason) :410 | `match.idsByPuuid` | `players.backfill_state`, `jobs` | — | fetch error → Retry (cursor resumes) | `LOOKUP_BACKFILL_LIMIT` (default `u32::MAX`, config.rs:739) |
| `ddragon:sync` | tick; `POST /v1/admin/ddragon/sync` (dedupe `ddragon:sync:force` when `force`, admin.rs:1634–1654) | `{force?}` | 30 000 / `ddragon:sync` | Under a mutex: GET `versions.json`; refresh `meta/queues.json` always; if newest version not mirrored (or `force`): download the 6 `DATA_FILES` (`champion,item,runesReforged,summoner,profileicon,map`, static/mod.rs:28–35) for `DDRAGON_LOCALE` into `DDRAGON_DIR/<ver>/`, write `versions` last, set current; publish `patch.new` only when changed (ddragon.rs:445–497, 532–546). Logs: `syncing Data Dragon`, `Data Dragon sync complete`, warn `Data Dragon file unavailable; skipping`, warn `queue table unavailable; keeping what is on disk` | none (CDN, no limiter, 15 s connect / 60 s timeout :260–261) | filesystem only | `patch.new` | any error → Retry | `DDRAGON_SYNC_S`, `DDRAGON_DIR`, `DDRAGON_LOCALE` (en_US) |
| `ladder:crawl` | tick; `POST /v1/admin/ladder/crawl` (admin.rs:1700–1730, calls `start_crawl` directly) | `{platform, queue, tierFloor?}` | 20 000 / `platform:queue` | `start_crawl`: validate; insert `ladder_crawls` row (partial unique index = one running crawl per ladder); add `crawl_legs` and enqueue one `ladder:apex` per apex tier ≥ floor and one `ladder:walk` per (paged tier ≥ floor × 4 divisions), all in one tx (ladder.rs:237–318). Log `ladder crawl started` / `ladder crawl already running; returning the live one` | none | `ladder_crawls`, `crawl_legs`, `jobs` | — | invalid → Fail | `LADDER_TIER_FLOOR` (MASTER, config.rs:747), `LADDER_PLATFORMS`, `LADDER_QUEUES` (RANKED_SOLO_5x5) |
| `ladder:apex` | `ladder:crawl` | `{crawlId, platform, queue, tier}` | 20 001 / `{crawlId}:ladder:apex:{TIER}` | Skip if crawl not running; one GET of the apex league; `write_page` (entries, players as untracked, counters, late stamps; ≥10 000 entries marks `apex_capped`); `settle` → `end_leg` (ladder.rs:424–450, 515–571) | `league.master|grandmaster|challenger` | `ladder_entries`, `players`, `ladder_crawls` counters/`apex_capped`, `match_facts.tier`, `crawl_legs` | `crawl.phase` when it ends a stage | final failure ends the leg failed → crawl ends `failed` | — |
| `ladder:walk` | `ladder:crawl` | `{crawlId, platform, queue, tier, division}` | 20 002 / `{crawlId}:ladder:walk:{TIER}:{DIV}` | Resume at `crawl_legs.cursor` (page); GET `league.entriesByTier?page=n` until an empty page; `write_page` stores cursor = page+1; every 10 pages (`CANCEL_CHECK_PAGES`) yields `take_turns()`; checks crawl still running at the start of each turn (ladder.rs:456–510). Log `ladder walk stopping; crawl is not running` | `league.entriesByTier` | as apex | as apex | as apex | — |
| `ladder:collect` | `advance()` at end of enumerate (ladder.rs:871–918) | `{crawlId, platform, queue, puuids[≤25], offset}` | 20 003 / `{crawlId}:ladder:collect:{offset}` | Per player: `mark_walk_started`; page `match.idsByPuuid?queue=<420|440>` 100 at a time up to `LADDER_BACKFILL_LIMIT`; `INSERT OR IGNORE crawl_match_ids`; 404 skips the player (warn `ladder collect skipping a player match-v5 does not know`); if limit ≥ `LOOKUP_BACKFILL_LIMIT` `mark_walk_complete`; on yield re-queues with the remaining puuids as the new payload; bumps `match_ids_seen` (ladder.rs:579–703) | `match.idsByPuuid` | `players.backfill_state`, `crawl_match_ids`, `ladder_crawls` | `crawl.phase` | as apex | `LADDER_BACKFILL_LIMIT` (100, config.rs:748), `LOOKUP_BACKFILL_LIMIT` |
| `ladder:archive` | `advance()` at end of collect (or straight from enumerate when nobody to walk) | `{crawlId, platform, queue}` | 20 004 / `{crawlId}:ladder:archive` | Loop: peek 100 ids (`ARCHIVE_BATCH`), `filter_unarchived`, enqueue `archive:match` at 20 005 with `fetchTimeline: ARCHIVE_TIMELINES`, drop ids, bump `matches_queued`, one tx per batch (ladder.rs:711–778). Log `ladder matches handed to the archive queue` | none | `jobs`, `crawl_match_ids`, `ladder_crawls` | `crawl.phase`, `ladder.crawl.completed` | — | `ARCHIVE_TIMELINES` |
| `ranks:lookup` | `end_leg` of a **completed** crawl, if `RANK_LOOKUP_LIMIT>0` (ladder.rs:804–806) | `{platform, queue, planned}` | 20 006 / `platform:queue` | First turn: carry on a leftover `rank_lookup_queue` or `plan()` up to `RANK_LOOKUP_LIMIT` archived players the ladder lacks and not looked up within `RANK_LOOKUP_RECHECK_S`, most games first; each turn `take()` 25 players (`TURN`), GET league entries (archive `observe` records the rank), 404 → `stamp`; limiter yield → `put_back` + yield with `planned:true`; empty list → enqueue `aggregate:analytics` (waits 5 s yield loops if one is running) (ranks.rs:61–171). Logs `rank lookups planned`, `rank lookups done; analytics queued` | `league.entriesByPuuid` | `rank_lookup_queue`, `player_ranks`, `rank_lookups`, `match_facts.tier` | — | `RANK_LOOKUP_LIMIT=0` → no-op | `RANK_LOOKUP_LIMIT` (50 000, config.rs:749), `RANK_LOOKUP_RECHECK_S` (604 800) |
| `names:backfill` | tick daily; crawl end (completed or failed, ladder.rs:797); `POST /v1/admin/players/names/backfill` (admin.rs:2282) | `{}` | 30 000 / `names:backfill` | Up to 50 000 nameless players (`TARGET_LIMIT`), 500 per chunk, read their 3 most recent archived matches' bodies, fill `game_name/tag_line` only where NULL (names.rs:94–185). Log `player names backfilled from archive` (named, unnamed, ms) | none | `players` | — | store error → Retry | — |
| `aggregate:analytics` | crawl completed (ladder.rs:800–802); `ranks:lookup` end; tick (`AGGREGATE_INTERVAL_S`); boot split check; `POST /v1/admin/analytics/recompute` (priority 0, promote; admin.rs:2358) | `{platform, queue}` | 30 000 (0 admin) / `platform:queue` | `PRAGMA optimize`; run `extract_builds` and `backfill_tiers` inline; then three write steps each in its own tx: `rebuild_champions` (slices, champion_stats, bans, totals, per-floor totals), `rebuild_matchups`, `rebuild_builds` (items, runes, spells, set builds + parts, matchup builds) over the newest `AGGREGATE_PATCH_LIMIT` patches (or all when the ladder is `unsplit`); record `analytics_runs`; clear + warm the analytics response cache; publish `analytics.updated` (analytics.rs:247–385; archive/analytics.rs:135, 352, 379). Log `analytics recomputed` | none | `analytics_slices`, `champion_stats(_totals)`, `champion_bans(_totals)`, `analytics_match_totals`, floor totals, `champion_matchups`, `champion_items/runes/spells`, `champion_builds(+parts)`, `champion_matchup_builds(+parts)`, `analytics_runs`, `match_builds`, `match_facts.tier` | `analytics.updated` | store error → Retry; cap 1 via `JOB_KIND_LIMITS` | `AGGREGATE_PATCH_LIMIT` (4, config.rs:753), `AGGREGATE_INTERVAL_S`, `ANALYTICS_CACHE_ENTRIES` |
| `facts:reextract` | boot when stale; `POST /v1/admin/analytics/reextract` (admin.rs:2380) | `{}` | 30 000 / kind | Select matches with `facts_version < 4` (`FACTS_VERSION` facts.rs:19) in id order, `FACTS_REEXTRACT_BATCH` at a time, decode bodies, re-derive facts/bans/remake on a blocking thread, write; 50 ms pause per batch; `proxy_facts_reextract_progress` gauge (analytics.rs:404–477). Log `facts re-extracted` | none | `match_facts`, `match_bans`, `matches.facts_version/remake` | — | — | `FACTS_REEXTRACT_BATCH` (500, config.rs:752) |
| `builds:extract` | `timelines:backfill` turn that stored any; inline by `aggregate:analytics` | `{}` | 30 000 / kind | With the mirrored `item.json`: timelines whose match has facts and no `match_builds` at `BUILDS_VERSION` 3 (builds.rs:24), 25 per batch, derive starter/boots/items/skill order, write; 50 ms pause (analytics.rs:488–580). Logs `builds extracted`, `no item.json mirrored yet; builds not extracted` | none | `match_builds` | — | — | — |
| `tiers:backfill` | boot when unstamped; inline by aggregate | `{}` | 30 000 / kind | Ranked matches with a NULL-tier fact, 500 per batch (`TIERS_BATCH`), `tiers::stamp` from `ladder_entries`/`player_ranks` else `UNKNOWN` (analytics.rs:590–626; tiers.rs:33–55). Log `match tiers stamped` | none | `match_facts.tier` | — | — | — |
| `maintenance` | tick daily (fires at every boot); `POST /v1/admin/maintenance/run` (admin.rs:2662–2670) | `{}` | 30 000 / `maintenance` | In order: (1) backup `VACUUM INTO $DATA_DIR/backups/riot-proxy-<UTC date>.db` via `.partial`+rename, once per day, skipped if free < 1.2×(db+wal) (`BACKUP_FREE_FACTOR`) or `BACKUP_KEEP=0`; prune to newest `BACKUP_KEEP`; (2) `DELETE jobs WHERE state='done' AND finished_at < now-7d`; `DELETE metrics_history WHERE at < now-24h`; (3) L2 sweep; (4) `PRAGMA optimize`; (5) `PRAGMA wal_checkpoint(TRUNCATE)`; then with a bucket enqueue `archive:offload` + `archive:index-snapshot` (maintenance.rs:192–272, 278–323). Logs `backup written`, warn `backup skipped: not enough disk`, warn `backup failed; maintenance goes on`, `maintenance done` (report, checkpoint_busy, wal_frames, checkpointed) | none | `jobs`, `metrics_history`, `cache`, filesystem | — | step error → Retry | `BACKUP_KEEP` (14, config.rs:729) |
| `timelines:backfill` | hourly tick per region (only if `TIMELINE_BACKFILL_PATCHES>0`) | `{region}` | 40 000 / region | Each turn: newest N patches with ranked (420/440) matches; up to 25 (`BATCH`) matches of this region's platforms lacking a timeline and not in `timeline_gaps`, newest patch then newest game first; GET `match.timeline`; 404 → `timeline_gaps(reason='not_found')`; other error → from_fetch; counters by region; if any fetched enqueue `builds:extract`; returns `take_turns()` until nothing left (timelines.rs:480–561). Logs `timeline backfill: nothing left`, `TIMELINE_BACKFILL_PATCHES is 0; nothing to backfill` | `match.timeline` | `timelines`, `timeline_gaps`, `jobs` | — | — | `TIMELINE_BACKFILL_PATCHES` (0, config.rs:728) |
| `archive:offload` | `Pending::add` when a kind's unpacked bytes ≥ `ARCHIVE_PACK_TARGET_BYTES` (offload.rs:423–443, re-asks at most every 60 s when absorbed); boot (serve.rs:313–318); after each `maintenance` | `{}` | 30 000 / kind | For match then timeline: while `next_pack` is due, `write_pack` to the bucket (log `pack written`), bump `archive_offload_bytes_total`, pace sleep; then `free` packed bodies older than `ARCHIVE_BODY_LOCAL_DAYS` in batches of 2 000 (`FREE_BATCH` archive/offload.rs:173); then re-measure → `archive_offload_pending{kind,step}` and `archive_packs` gauges (offload.rs:478–549). Log `offload done` | none | `body_packs`, `body_locations`, `matches`/`timelines` body columns | — | error → Retry; cap 1 via `JOB_KIND_LIMITS`; registered only with `ARCHIVE_OBJECT_URL` | `ARCHIVE_PACK_TARGET_BYTES` (64 MiB), `ARCHIVE_BODY_LOCAL_DAYS` (14), `ARCHIVE_OFFLOAD_PACE_MS` (50) config.rs:736–738 |
| `archive:index-snapshot` | after each `maintenance` (with bucket) | `{}` | 30 000 / kind | `recovery::write_snapshot`: `body_packs` + `body_locations` → bucket `index/…-<date>` (index_snapshot.rs:592–607). Log `index snapshot written` (date, packs, locations, last_pack) | none | bucket only | — | error → Retry | — |

Handlers are registered in `jobs::handlers` (mod.rs:89–162); the two bucket kinds in serve.rs:249–270.

---

## 4. Ladder crawl

Phases (`ladder_crawls.phase`): `enumerate → collect → archive → status completed` (`next_stage` ladder.rs:858–865; `advance` :871–923). The crawl row starts `status='running'`, `phase='enumerate'` by column default (V0004__ladder.sql:20–21; `insert_crawl` store.rs:85–106); the partial unique index `ladder_crawls_live` (V0004:33–34) refuses a second running crawl per ladder.

1. **Enumerate.** Tier list = `tiers_at_or_above(floor)` over `IRON…DIAMOND` (paged, 7) + `MASTER, GRANDMASTER, CHALLENGER` (apex, 3) (riot/ladder.rs:9–13, 40–42). Legs: 1 `ladder:apex` per apex tier ≥ floor; 1 `ladder:walk` per (paged tier ≥ floor) × `I,II,III,IV` (ladder.rs:262–290; `enumerate_legs` :218–231). Default floor `MASTER` → 3 legs, 3 requests total; floor `IRON` → 3 + 28 = 31 legs.
   - Requests per tier: apex = **1** request (≤10 000 entries, `RIOT_APEX_LIST_CAP`; ≥10 000 marks `apex_capped`, store.rs:221–224, 297–323); paged = **1 request per page of 205** (`ENTRIES_PER_PAGE` riot/ladder.rs:26) per division until an empty page, so ~ceil(players/205)+1 per division.
   - Each page is one tx: upsert `ladder_entries`, upsert `players` as untracked, bump `pages_fetched/entries_seen/players_discovered`, store the walk cursor, late tier stamps (store.rs:214–295). Metrics `proxy_ladder_pages_total`, `proxy_ladder_entries_total` (ladder.rs:540–545).
2. **Collect.** When the last enumerate leg ends: candidates = `ladder_entries` seen by this crawl, excluding players whose `backfill_state.startedAt ≥ crawl.started_at`, LP desc (store.rs:439–459); chunked into `ladder:collect` jobs of 25 (`COLLECT_BATCH`), `backfills_enqueued += players`; log `ladder crawl collecting match ids`. Nobody to walk → straight to archive (:888–891). `LADDER_BACKFILL_LIMIT=0` → crawl completes after enumeration (:860).
   - Per player: `match.idsByPuuid?queue=<id>&start&count=min(100, limit-start)` up to `LADDER_BACKFILL_LIMIT` (default 100 → 1 request/player). Ids go into `crawl_match_ids` (`INSERT OR IGNORE`), so a match shared by ten ladder players is one id. Metric `proxy_ladder_match_ids_total`.
3. **Archive.** One `ladder:archive` leg: batches of 100 ids → `filter_unarchived` → `archive:match` at priority 20 005 (dedupe matchId, `fetchTimeline: ARCHIVE_TIMELINES`), ids dropped and `matches_queued` bumped in the same tx. Metric `proxy_ladder_matches_queued_total`.
4. **Completion trigger** (`end_leg` ladder.rs:781–818; store.rs:351–406): the leg deletes its `crawl_legs` row; if it was the last and `legs_failed>0` the crawl ends `failed`; else `advance`. Archive stage ending → `finish(…,'completed')` drops `crawl_legs`/`crawl_match_ids`. In the **same transaction**: enqueue `names:backfill` (completed or failed), and if `completed`: `aggregate:analytics{platform,queue}` (30 000, dedupe `platform:queue`) and, if `RANK_LOOKUP_LIMIT>0`, `ranks:lookup`. Then `wake_all`, publish `crawl.phase` (every transition), `ladder.crawl.completed` (completed only), record `proxy_ladder_crawl_duration_seconds{status}`; logs `ladder crawl moved on` / `ladder crawl finished` (:820–853).
5. **Cancel**: `DELETE /v1/admin/ladder/crawls/{id}` → status `cancelled`, working state dropped, pending `ladder:*` jobs with this `crawlId` set `failed/'cancelled'`; running legs stop at their next `running()` check (store.rs:615–651; scheduler.rs:204–221).
6. **Platforms**: whatever `LADDER_PLATFORMS` lists (validated against `Platform::ALL`, 16 platforms, routing.rs:46), × `LADDER_QUEUES` ⊂ {`RANKED_SOLO_5x5`, `RANKED_FLEX_SR`} (riot/ladder.rs:7). Admin route `POST /v1/admin/ladder/crawl` can crawl any platform regardless of the tick list (admin.rs:1700–1730).

---

## 5. Events and WebSocket topics

Topics (`src/ws/protocol.rs:210–218`): `firehose`, `metrics`, `ladder` (admin-only, `is_admin` :247–250), `patch`, `player:<puuid>`. Anything else is refused at parse (:225–232). Limits: heartbeat ping every 30 s, drop after 2 unanswered (:201–204, socket.rs:121–130), 200 topics per socket, 200-char topic max (:205–208).

Hub (`src/ws/hub.rs`): one `broadcast` channel per topic, capacity 256 (:229), created on first subscribe, dropped when empty (:328–347). `publish` sends to the topic **and** `firehose` (:312–325); a socket holding `firehose` skips duplicates from its other topics (socket.rs:106–109). Lagged receiver → `{"op":"resync","topic","dropped"}` (socket.rs:112–115). Close codes: 1001 shutdown, 4401 revoked (socket.rs:36–41). `proxy_ws_connections` gauge set on register/drop (hub.rs:78, 180 of the file; `register`/`Drop`).

Frame (`src/events.rs:158–187`): `{"op":"event","event":<name>,"topic":<topic>,"at":<unix ms>,"data":{…}}`. `publish` increments `events_published_total{name}` and debug-logs `event published` (:191–206).

| Event | Topic | Data fields | Published by |
|---|---|---|---|
| `game.started` | `player:<puuid>` | `puuid, platform, gameId, queueId?, championId?` | poll.rs:146–156 |
| `game.ended` | `player:<puuid>` | `puuid, platform?, gameId, queueId?, championId?` | poll.rs:159–168 |
| `rank.changed` | `player:<puuid>` | `puuid, queue, before{tier,rank,lp}|null, after` | poll.rs:246–254 |
| `match.archived` | `player:<puuid>` if `puuid` set, else `firehose` only | `puuid?, matchId, patch?, participants[]` | archive.rs:268–270, 423–448 (only when the fetch was not served from the archive) |
| `patch.new` | `patch` | `version` | ddragon.rs:537–544 (only when a patch was downloaded) |
| `crawl.phase` | `ladder` | `crawlId, platform, queue, phase (collect|archive|completed|failed|cancelled…), stats{pagesFetched,entriesSeen,playersDiscovered,backfillsEnqueued,matchIdsSeen,matchesQueued}` | ladder.rs:826, 836, 946–959 |
| `ladder.crawl.completed` | `ladder` | `crawlId, platform, queue, entries, players, durationS` | ladder.rs:838–850 |
| `analytics.updated` | `ladder` | `platform, queue, durationS, tables{name→rows}` | analytics.rs:302–310 |
| `metrics.snapshot` | `metrics` | the `stats::Snapshot` (stats.rs:295–331, `v:1`) | ws/metrics.rs:515–530, every `METRICS_INTERVAL_S` (5 s) **only while someone holds `metrics`** |

Names list: events.rs:114–124 (9). Event topics: :143–156.

---

## 6. Metrics

Catalogue `src/metrics.rs:89–346`; names are v1's byte-for-byte for the `proxy_*` set, v2 additions unprefixed (ADR-014). Label-less metrics are registered at 0 on boot (`describe_all` :351–374). Served at `GET /metrics` (`text/plain; version=0.0.4; charset=utf-8`, telemetry.rs:464, 542–561).

| Name | Type | Labels | Meaning | Recorded at |
|---|---|---|---|---|
| `proxy_requests_total` | counter | route, status, cache | downstream requests; `cache` from `X-Cache` (hit/miss/neg/stale/archive/bypass/none); route in `:param` form, `unmatched` fallback | app.rs:126–171 |
| `proxy_upstream_requests_total` | counter | region, method, status | Riot calls; `status` = HTTP code or `network` | riot/client.rs:359–361, 348 |
| `proxy_upstream_latency_seconds` | histogram | region, method | Riot RTT; buckets 0.025…10 | client.rs:362 |
| `proxy_rl_wait_seconds` | histogram | region, priority | limiter wait before dispatch; buckets 0.001…10 | limiter/mod.rs:283 |
| `proxy_rl_429_total` | counter | region, type | 429s; `type` = `X-Rate-Limit-Type` or `service` | client.rs:308–313 |
| `proxy_cache_reads_total` | counter | state (hit/miss/neg/stale) | cacheable reads | fetcher.rs:401–403; routes/players.rs:733 (archive hits) |
| `proxy_ws_connections` | gauge | — | open sockets | ws/hub.rs `register`/`Registration::drop` |
| `proxy_jobs_total` | counter | job, status (completed/failed) | job outcomes (yields excluded) | scheduler.rs:1079, 1086 |
| `proxy_backfills_queued_total` | counter | reason (lookup/track/catchup/admin), status (queued/already-queued) | backfill requests | jobs/archive.rs:223 |
| `proxy_refresh_claims_total` | counter | part, outcome (claimed/coalesced) | `?refresh=true` windows | routes/players/refresh.rs:53, 61 |
| `proxy_ladder_pages_total` | counter | platform, queue | ladder pages incl. apex | ladder.rs:540 |
| `proxy_ladder_entries_total` | counter | platform, queue | entries upserted | ladder.rs:543 |
| `proxy_ladder_match_ids_total` | counter | platform, queue | distinct ids collected | ladder.rs:635 |
| `proxy_ladder_matches_queued_total` | counter | platform, queue | unarchived ids handed to `archive:match` | ladder.rs:769 |
| `proxy_ladder_crawl_duration_seconds` | histogram | platform, queue, status | crawl wall time; buckets 10…28 800 | ladder.rs:833 |
| `proxy_rank_lookups_total` | counter | platform, queue | `ranks:lookup` lookups | ladder/ranks.rs:135 |
| `proxy_aggregate_runs_total` | counter | platform, queue, status | recomputes | analytics.rs:275 |
| `proxy_aggregate_duration_seconds` | histogram | platform, queue, step | per step; buckets 1…1200 | analytics.rs:365 |
| `proxy_aggregate_rows` | gauge | platform, queue, table | rows written by last recompute | analytics.rs:371 |
| `proxy_facts_reextract_progress` | gauge | — | 0–1 | analytics.rs:470, 474 |
| `proxy_archived_matches_total` | counter | — | matches upserted | archive/matches.rs:404 |
| `limiter_interactive_waiters` | gauge | — | interactive acquires waiting | limiter/mod.rs:240 |
| `limiter_bulk_waiters` | gauge | — | bulk acquires waiting | limiter/mod.rs:242 |
| `events_published_total` | counter | name | events handed to the hub | events.rs:198 |
| `jobs_pending` | gauge | kind | pending rows; sampled every 15 s (`PENDING_SAMPLE`), 0 for kinds that drained | scheduler.rs:49, 1226–1245 |
| `sqlite_readers_free` | gauge | — | idle reader connections | db/mod.rs:119 |
| `timeline_backfill_fetched_total` | counter | region | timelines fetched | timelines.rs:365 |
| `timeline_backfill_not_found_total` | counter | region | 404s marked | timelines.rs:366 |
| `sqlite_db_bytes` | gauge | — | db file size | db/disk.rs:70 (via `stats.sample_slow` stats.rs:828, i.e. once at boot then every history interval) |
| `sqlite_wal_bytes` | gauge | — | `-wal` size | db/disk.rs:71 |
| `sqlite_freelist_bytes` | gauge | — | freelist pages × page size | db/disk.rs:72 |
| `data_dir_free_bytes` | gauge | — | `statvfs` avail on data dir | db/disk.rs:73 |
| `cache_l2_dropped_total` | counter | — | L2 writes dropped on a full 10 000 queue | cache/l2.rs:109 |
| `archive_offload_pending` | gauge | kind (match/timeline), step (pack/free) | backlog after each offload run | offload.rs:204–207 |
| `archive_offload_bytes_total` | counter | kind | pack bytes uploaded | offload.rs:164 |
| `archive_packs` | gauge | — | rows in `body_packs` | offload.rs:215 |

A unit test keeps `docs/design/metrics.md` rows in sync with the catalogue (metrics.rs:427–439).

### Metrics-history recorder (`src/stats.rs`)

- `spawn_sampler(stats, FAST_SAMPLE=15 s, METRICS_HISTORY_INTERVAL_S=60 s)` :1005–1036: samples both sets once at start; fast set (`totals`, `queues` by v1's six queue names, `ladder`, `analytics`) every 15 s; every 60 s `record_point` samples both sets (slow = archive byte stats, disk usage → disk gauges, L2 row count) and writes a `HistoryPoint` (:353–368: `t, totals, queues{active,pending,failed}, analytics{rows,ageSeconds}, cache{hit,miss,neg,stale}, disk, requests, upstream`) with `INSERT OR REPLACE INTO metrics_history(at, point)` then trims to the newest 1440 rows (`HISTORY_MAX_POINTS` :43) (:960–982). `maintenance` additionally deletes rows older than 24 h (maintenance.rs:38, 241–244). Read by `GET /v1/admin/metrics/history` (admin.rs:2418) and `/v1/admin/metrics` snapshot (:2392). Table DDL V0001:47–50. Warn lines `stats sample failed`, `metrics history point failed` (stats.rs:1008, 1011, 1023, 1030).
- Queue counts read only the `jobs_state_kind` index (`QUEUE_COUNTS_SQL` :456–460): `waiting` = pending & due, `delayed` = pending & future, `failed` = last 24 h, `completed` = last hour (:586–617).

### Alert rules (`ops/prometheus-alerts.yml`)

| Alert | expr | for | severity |
|---|---|---|---|
| `RiotKeyRejected` | `increase(proxy_upstream_requests_total{status=~"401\|403"}[10m]) > 0` | 0m | critical (:6–14) |
| `AccountableRateLimit429` | `increase(proxy_rl_429_total{type=~"application\|method"}[10m]) > 0` | 0m | warning (:16–24) |
| `CacheHitRatioLow` | `sum(rate(proxy_cache_reads_total{state=~"hit\|neg\|stale"}[5m])) / sum(rate(proxy_cache_reads_total[5m])) < 0.7` | 15m | warning (:26–41) |
| `UpstreamErrorRateHigh` | `sum(rate(proxy_upstream_requests_total{status=~"5..\|network"}[5m])) / sum(rate(proxy_upstream_requests_total[5m])) > 0.1` | 10m | warning (:43–50) |
| `ProxyDown` | `up{job="riot-proxy"} == 0` | 2m | critical (:52–57) |

### Grafana dashboard (`ops/grafana/riot-proxy-dashboard.json`, uid/title `riot-proxy`)

12 panels, no rows: stat `Cache hit ratio (5m)`, `Accountable 429s (5m)`, `WebSocket connections`, `Upstream auth failures (15m)`; timeseries `Requests by cache state`, `Requests by status`, `Upstream requests by method`, `Upstream latency p50 / p95`, `Rate limiter wait p95 by priority (§9.3)`, `429s by type`, `Background jobs` (`sum by (job,status) rate(proxy_jobs_total)`), `Matches archived`. Metrics referenced: `proxy_cache_reads_total`, `proxy_rl_429_total`, `proxy_ws_connections`, `proxy_upstream_requests_total`, `proxy_requests_total`, `proxy_upstream_latency_seconds_bucket`, `proxy_rl_wait_seconds_bucket`, `proxy_jobs_total`, `proxy_archived_matches_total`. No panel uses any v2-added metric.

---

## 7. Logging

- Setup `telemetry::init_tracing` (telemetry.rs:481–491): `EnvFilter` parsed from `LOG_LEVEL` (default `"info"`, config.rs:701; invalid → `TelemetryError::Filter`), format from `LOG_FORMAT`: default `pretty` when stdout is a TTY else `json` (config.rs:648–655). JSON layer: one object per event, event fields flattened to top level, the **innermost span's fields under `span`**, no span list (telemetry.rs:495–506; test :139–155 shows `{"message","level","upstream_ms","span":{"request_id"}}`).
- HTTP spans: outermost `request_id` middleware wraps the request in span `request{request_id}` and echoes `X-Request-Id` (http/request_id.rs:50–66); `TraceLayer` creates span `http{request_id, method, path, consumer=Empty}` (app.rs:108–116); `consumer` is recorded after auth (http/auth.rs:325). `on_response` is tower-http's `DefaultOnResponse` at INFO (app.rs:115) — it logs status and latency; **no `x_cache`/`upstream_ms` field is attached to the response line** (design/07:218 claims both; only the `x_cache` header and a `upstream_ms` struct field exist — see §9).
- `ApiError` responses log `request failed` (code=, status=, message=) at error for 5xx, warn otherwise (http/error.rs:178–180).

Notable fixed-text lines operators should know (beyond §1):

| Level | Text | Where |
|---|---|---|
| error | `RIOT KEY REJECTED — check RIOT_API_KEY (dev keys expire every 24h)` (status, method, host) | riot/client.rs:305 on 401/403 |
| warn | `upstream 5xx` / `unexpected upstream status` / `network failure contacting Riot API` | client.rs:320, 325, 348 |
| error | `accountable 429: limiter accounting is out of sync; scope frozen` (scope, kind, retry_after_s) | limiter/mod.rs:417–422 |
| warn | `service 429 with Retry-After; scope frozen` | limiter/mod.rs:424–428 |
| warn | `unknown X-Rate-Limit-Type` | limiter/headers.rs:62 |
| info/warn | `service 429, trying the next cluster` / `service 429, backing off` / `upstream unavailable, retrying` / `serving stale on upstream failure` / `serving stale while rate limited` | fetcher.rs:558, 569, 590, 602, 462 |
| warn | `job failed` (job, id, attempts, next, error) | scheduler.rs:1092 |
| error | `could not record job outcome` / `job claim failed` | scheduler.rs:1156, 1219 |
| warn | `jobs still running at shutdown; they resume on the next boot` | scheduler.rs:1296 |
| warn | `limiter checkpoint failed`, `unreadable limiter checkpoint row skipped`, `unknown limiter checkpoint row skipped` | limiter/persist.rs:220, 150, 171 |
| warn | `L2 write queue full; entry not persisted` / `L2 writer task failed` / `L2 flush failed; entries stay in L1 only` | cache/l2.rs:110, 121, 154 |
| error | `a SQLite write closure panicked` | db/mod.rs:323 |
| warn | `rejected unknown key` / `admin request from disallowed IP` / `ADMIN_IP_ALLOWLIST entry is not an IP or CIDR; ignored` | http/auth.rs:273, 283, 197 |
| warn | `archive write failed` / `player rank write failed` / `could not queue a match's missing half` | archive/mod.rs:174, 199, 109 |
| warn | `match facts not extracted`; offload read fallbacks `… reading from Riot` | archive/matches.rs:117, 235–310 |
| info | `ladder crawl started`, `ladder crawl moved on`, `ladder crawl finished`, `ladder crawl collecting match ids`, `ladder matches handed to the archive queue` | ladder.rs:312, 825, 830, 917, 776 |
| info | `analytics recomputed`, `facts re-extracted`, `builds extracted`, `match tiers stamped` | analytics.rs:301, 475, 578, 623 |
| info | `backup written`, `maintenance done`; warn `backup skipped: not enough disk`, `backup failed; maintenance goes on` | maintenance.rs:309, 263, 304, 315 |
| info | `pack written`, `offload done`, `index snapshot written` | offload.rs:162, 192; index_snapshot.rs:598 |
| info | `queued backfill on first lookup`, `tracked player upserted`, `backfill finished` | routes/players.rs:584, admin.rs:521, archive.rs:410 |
| debug | `tick`, `event published`, `websocket lagged`, `websocket closed`, `job yielded to the rate limiter` | ticks.rs:365, events.rs:204, socket.rs:113, 138, scheduler.rs:1083 |

Migrations: refinery runs grouped (db/mod.rs:314–315); there is **no dedicated "migrations applied" log line** — only `database ready` after `open_async` succeeds, and `DbError::Migration("migration failed: …")` on failure (db/mod.rs:44–45).

---

## 8. Health

`src/routes/health.rs` — both public, no key, no quota.

- `GET /healthz` → `200 {"ok":true}`; touches nothing (:32–45).
- `GET /readyz` → `Ready{ok, sqlite, limiter, keyScope, sqliteReaders, sqliteReadersFree}` (camelCase, :51–63). `sqlite` = the writer thread executes `BEGIN IMMEDIATE; ROLLBACK;` within 2 s (`READY_TIMEOUT` :19, :83–87); `limiter` = `limiter_restored` flag (:88); `ok = sqlite && limiter`; **503 carries the same body** (:90–105). `sqliteReadersFree` is informational only.
- `riot-proxy healthcheck [--timeout]` (cli/healthcheck.rs:113–119, cli/mod.rs:44–50, 105): plain-HTTP GET `http://<loopback or HOST>:<PORT>/healthz`, exit 0 on 2xx; `0.0.0.0`→`127.0.0.1`, `::`→`[::1]` (:131–140); no CA store needed (scratch image).

---

## 9. Design-doc deviations and `TODO (unverified)`

- **Backoff off-by-one vs design/06 §Backoff**: code uses `job.attempts` which includes the current run → first retry ≈ 60 s ± 20 %, second 120 s, …, fifth run final (scheduler.rs:1070–1075; store.rs:104). Design text "2^attempts × 30 s" is ambiguous.
- **design/06 catalogue `ladder:crawl`**: "fan out `ladder:apex` × 3 + `ladder:walk` × (tier, division)" — code fans out only tiers at or above `tierFloor` (ladder.rs:262–280).
- **design/06 `ladder:archive` row** says it enqueues `ranks:lookup`; code gates that on `RANK_LOOKUP_LIMIT > 0` (ladder.rs:804–806) and queues `names:backfill` on failed crawls too (:797).
- **design/06 §Scheduler diagram** lists 7 ticks; code also has `names:backfill` (daily) and `timelines:backfill` (hourly, gated) ticks (ticks.rs:287, 299–302).
- **design/06 `backfill:player`** "up to `LOOKUP_BACKFILL_LIMIT`": config default is `u32::MAX` (config.rs:739); the payload default is 500 (archive.rs:79–81). Tracking/lookup pass `LOOKUP_BACKFILL_LIMIT` explicitly (admin.rs:526, players.rs:578).
- **design/07:218** "every request carries `request_id`, `consumer`, `x_cache`, `upstream_ms`": only `request_id`, `method`, `path`, `consumer` are span fields (app.rs:112; auth.rs:325). `TODO (unverified)`: whether any per-request INFO line carries `x_cache`/`upstream_ms` — no `tracing::info!` with those fields was found outside telemetry.rs's test (:164).
- **design/06 §Protocol** error example ✓: `socket.rs::handle` answers `{"op":"error","error":{"code":"FORBIDDEN","message":"Topic '<t>' requires the admin scope"}}` for an admin topic on a non-admin socket (socket.rs:165–171) and `VALIDATION` / `Unknown topic '<raw>'` for an unparseable one (:161–163); the `subscribed` ack lists only what is held (:202–206). Beyond the cap of 200 topics further subscribes are silently dropped (:176–178).
- **metrics.md `proxy_ws_connections` "WS hub (P6-01)"** ✓; `jobs_pending` "sampled every 15 s" ✓ (scheduler.rs:49).
- Crawl row defaults ✓: `insert_crawl` inserts only `id, key_scope, platform, queue, tier_floor, started_at` with `ON CONFLICT DO NOTHING RETURNING` (store.rs:85–106); `status DEFAULT 'running'`, `phase DEFAULT 'enumerate'`, counters default 0, and the partial unique index `ladder_crawls_live(key_scope, platform, queue) WHERE status='running'` come from V0004 (`V0004__ladder.sql:14–34`).
- Reader pool ✓: `Db::default_readers()` = `available_parallelism()` (fallback 4) (db/mod.rs:190–193); `readers()` / `readers_free()` back `/readyz` and `sqlite_readers_free` (:199–212).
- `TODO (unverified)`: exact tower-http `DefaultOnResponse` message text (`finished processing request`) and its field names (`latency`, `status`) — library default, not in repo.
- `TODO (unverified)`: config.rs line numbers 695–756 for defaults are computed from the 640–790 listing anchored on `BULK_IDLE_CEILING` at :718; treat as ±1.
- Not in design/06 catalogue but in code: the `ddragon:sync:force` dedupe key (admin.rs:1640–1649), `enqueue_or_promote` lifting TL-01 halves to priority 0 and forcing `fetchTimeline` (archive.rs:122–160), `ranks:lookup` waiting out a running recompute with 5 s yields (ranks.rs:31, 145–171), and `Pending`'s 60 s `RETRY_ABSORBED` re-ask (offload.rs:385).

# Progress

Legend: [ ] todo · [~] in progress (branch name) · [x] merged (#PR)

## P0 — Foundations
- [x] P0-00 bootstrap (direct to `main`, no PR — see notes)
- [x] P0-01 workspace, toolchain, CI (#1)
- [x] P0-02 config (#2)
- [x] P0-03 logging + metrics + request ids (#3)
- [x] P0-04 SQLite layer (#4)
- [x] P0-05 HTTP skeleton + health (#5)
- [x] P0-06 CLI (#6)
- [x] P0-07 dev tooling (#7)
Exit check: **passed** (2026-09-23, `main` @ `6d7f2f8`)
```
$ rm -rf data && RIOT_API_KEY=<placeholder> cargo run -- serve
  migrated V1__init → "database ready" path=./data/riot-proxy.db
  "bootstrap admin consumer created"
  bootstrap admin key (shown once): rpx_<redacted>          ← stderr, once
  "listening" addr=0.0.0.0:8080
$ curl localhost:8080/healthz        → {"ok":true} [200]
$ curl localhost:8080/metrics        → [200] # TYPE proxy_archived_matches_total counter …
$ kill -TERM <pid>                   → "SIGTERM" → "draining" → "stopped", exit 0
$ cargo run -- key create --name test → Consumer created … API KEY rpx_<redacted> (36 chars)
CI run 35857621010 on main: fmt ✓ clippy ✓ test ✓ build-musl ✓
  target/x86_64-unknown-linux-musl/release/riot-proxy: 14,517,536 bytes, "static-pie linked", stripped  (< 20 MB)
  docker image riot-proxy:ci: 14.6 MB; container healthcheck ✓, /healthz ✓, /readyz ✓, SIGTERM exit 0; docker compose up → /healthz ✓
```
The musl binary was checked in CI, not on the dev box (no `musl-gcc` there; ADR-006/013).

## P1 — Riot client
- [x] P1-01 routing (#10)
- [x] P1-02 endpoint registry (#11)
- [x] P1-03 HTTP client (#12)
- [x] P1-04 rate-limit header parsing (#13)
- [x] P1-05 dev subcommand (#14)
- [x] P1-06 exhaustive host-resolution test for the exit check (#15)
- [x] P1-07 fix: `.env` parsing compatible with v1/node dotenv (#16)
Exit check: **passed** (2026-09-24, `main` @ `ddf9853`, real development key from `.env`)
```
$ just riot account/by-riot-id europe Faker KR1
  → /riot/account/v1/accounts/by-riot-id/Faker/KR1 → NotFound (404)
    The key is accepted (no 401/403), but the Riot ID in the plan's example does not exist.
$ just riot account/by-riot-id europe 'Hide on bush' KR1
  200 /riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1 (461 ms)
  {"puuid":"NkQRxdiN…","gameName":"Hide on bush","tagLine":"KR1"}        ← Riot's raw JSON
$ just riot account/by-riot-id sea 'Hide on bush' KR1
  200 … (sea → asia account host, live)
$ cargo test every_endpoint_group_resolves_to_the_right_host
  ok: 16 endpoints / nine groups × na1, euw1, kr, oc1, vn2 → correct host
```
Found and fixed on the way: `.env` parsing rejected v1-style unquoted values (#16, ADR-020).

## P2 — Rate limiter
- [x] P2-01 port v1 limiter tests first (#19)
- [x] P2-02 windows and scopes (#20)
- [x] P2-03 acquire (single priority) (#21)
- [x] P2-04 observe + freeze (#22)
- [x] P2-05 priorities (#23)
- [x] P2-06 checkpoint/restore (#24)
- [x] P2-07 property test + soak (#25)
Exit check: **passed** (2026-09-25, `main` @ `98f4599`)
```
$ cargo test limiter
  ok. 70 passed; 0 failed; 0 ignored   (lib: ported v1 suite, bucket, headers, persist, proptest)
  ok. 1 passed                          (tests/limiter_restart.rs … restart never over-commits, etc.)
  riot::limiter::proptest::never_over_commits_a_window ... ok   (1 000 cases; fails on a planted off-by-one)
$ cargo test --release --test limiter_soak -- --ignored --nocapture
  admitted 100 in 60s; worst 1 s = 20/20, worst 120 s = 100/100, worst tight 2 s = 6/7   (50 tasks)
docs/design/05-rate-limiter.md: "As built (P2)" section lists every deviation with its ADR.
```

## P3 — Cache, single-flight, fetcher
- [x] P3-01 cache keys + key_scope (#27)
- [x] P3-02 L1 (moka) (#28)
- [x] P3-03 L2 (SQLite write-behind + warm) (#29)
- [x] P3-04 single-flight (#30)
- [x] P3-05 fetcher (#31)
- [x] P3-06 replay harness (#32)
Exit check: **passed** (2026-09-25, `main` @ `9af51a0`)
```
$ cargo test --test fetcher_states        → 17 passed
  every X-Cache value asserted against wiremock:
  HIT ×4, MISS ×4, STALE ×3, HIT-NEG ×1, ARCHIVE ×2, BYPASS ×1   (six values: ADR-022, ADR-031)
  incl. stale-on-5xx, SWR refresh at bulk priority, typed/service 429 retries, RATE_LIMITED hint
$ CI=true cargo test --test replay        → 2 passed (snapshots unchanged)
  cold_summoner_lookup   (10 real exchanges, 2 passes)
  typed_application_429  (synthetic, freeze ≈1 s then MISS)
```
The plan's exit check says `NEG`; per ADR-022 the value is `HIT-NEG`.

## P4 — Public surface
- [x] P4-01 auth (#34)
- [x] P4-02 consumer quota (#35)
- [x] P4-03 OpenAPI scaffolding (#36)
- [x] P4-04 `/v1/riot/*` passthrough (#37)
- [x] P4-05 `/v1/lol/*` typed routes (#38)
- [x] P4-06 dev UI + dashboard shells `[parallel-ok]` (#39)
Exit check (2026-10-04): **passed.** The `spec` and `compare-openapi.py` half passed at P4 (zero missing `/v1/riot/*` and `/v1/lol/*` operation ids, analytics deferred to P7-04 per ADR-037). The owner confirmed in a browser that `/docs` renders Scalar on `v2.0.0-rc.1`, with the info header, response-header table, bearer auth and every tag group (players, riot, lol, static, ws, ops, admin, models).

## P5 — Archive and composites
- [x] P5-01 archive schema (#40)
- [x] P5-02 match archive (#41)
- [x] P5-03 facts extraction (#42)
- [x] P5-04 players + composites (#43; snapshots reviewed by owner)
- [x] P5-05 admin routes (data) (#44)

Exit check (2026-09-30): **passed.**
The real binary ran against Riot with a fresh `DATA_DIR`, making one live call.
```
GET /v1/lol/matches/asia/KR_8393343196   → 200, X-Cache: MISS     (78939 bytes)
SIGTERM, restart serve (same DATA_DIR)
GET /v1/lol/matches/asia/KR_8393343196   → 200, X-Cache: ARCHIVE  (78939 bytes)
cmp: byte-identical (sha256 71b146e59a3b2818…); no RGAPI- in the server log
```
The response shape of `/v1/players/by-riot-id/{gameName}/{tagLine}/profile` matches v1's `ProfileBody` (snapshot `players_routes__players_profile.snap`), reviewed and approved by the owner on #43. The plan's path `/v1/lol/match/{id}` is served as v1's `/v1/lol/matches/{region}/{matchId}`.

## P6 — Scheduler, jobs, realtime
- [x] P6-01 WebSocket hub spike (may run any time after P0) (#46)
- [x] P6-02 events (#47)
- [x] P6-03 durable jobs core (#48)
- [x] P6-04 ticks (#49)
- [x] P6-05 poll handlers (#50)
- [x] P6-06 archive + backfill handlers (#51)
- [x] P6-07 WS auth + wiring (#52)
- [x] P6-08 admin routes (jobs) (#53)

Exit check (2026-09-30, `main` @ `bc15e98` + the exit-check test): **passed.**
`tests/p6_exit.rs` runs the real `serve` pipeline in-process against wiremock (`ServeOptions::riot_base_url`); "kill" aborts the task, so nothing shuts down gracefully. Run with `cargo test --test p6_exit -- --ignored --nocapture`.
```
1. game.started on /v1/ws 9.9 s after the spectator flip: {"championId":134,"gameId":77,"platform":"kr","puuid":"NkQRx…","queueId":420}
2. killed serve: backfill cursor 100 of 150, backfill row still running (1), 100 archive:match rows
3. restarted: backfill resumed at 100 and completed (depth 150); page one read 1×; archive:match rows 150, distinct 150, done 150; matches archived 150
```
The player was tracked through `POST /v1/admin/tracked-players`, which queued the walk, and `game.started` was read on a socket subscribed to `player:<puuid>` with an admin key. It arrived within one `TRACK_POLL_LIVE_S` (10 s) tick. The restart's `recover()` re-queued the running backfill, and the walk resumed from its saved cursor.

## P7 — Data Dragon, ladder, analytics, dashboard
- [x] P7-01 Data Dragon sync + static serving (#55)
- [x] P7-02 ladder enumerate (#56; **+ v1's ladder schema, `POST /v1/admin/ladder/crawl` and `/options`, owner decisions, ADR-054**)
- [x] P7-03 ladder collect + archive (#57; **+ `GET /v1/admin/ladder/crawls`, `DELETE …/crawls/{id}`, and v1's names job, tick and `POST /v1/admin/players/names/backfill`, owner decisions at P7-02**)
- [x] P7-04 analytics (#58; **+ v1's three `/v1/lol/analytics/*` routes, deferred from P4-05 by the owner, ADR-037; v1's analytics schema and remakes kept apart, owner decisions, ADR-056**)
- [x] P7-05 maintenance (#59)
- [x] P7-06 dashboard wiring (#60; screenshots in `docs/img/p7-06-*.png`)

Exit check (2026-10-03, `main` @ `0a3e4bf` + the exit-check test): **passed.**
`tests/p7_exit.rs` runs the real `serve` in-process against a wiremock ladder of 30 players (`LADDER_QUEUES=RANKED_SOLO_5x5 LADDER_TIER_FLOOR=MASTER`), 12 matches each shared by 10 of them. The crawl is started with `POST /v1/admin/ladder/crawl` and watched on `/v1/ws` (`ladder` and `metrics` topics). Run with `cargo test --test p7_exit -- --ignored --nocapture`.
```
crawl: "completed" in 78.8ms
ladder events: crawl.phase collect → archive → completed, ladder.crawl.completed, analytics.updated
match fetches: [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]
counters: entries 30 players 30 walked 30 match ids 12 queued 12
dashboard: archivedMatches 12 knownPlayers 30 ladder.entries 30 lastCompleted "completed" analytics "completed" topChampions 5 snapshots 1
```
Each match was fetched once from Riot, although every one of them is reachable from ten players' histories. The live `metrics.snapshot` on `/v1/ws` and `GET /v1/admin/metrics` both show the finished crawl, and `/dashboard` answers 200. Screenshots of the dashboard rendering live data in a browser are in `docs/img/p7-06-*.png` (#60).

## P8 — Contract, migration, packaging
- [x] P8-01 port acceptance suite (#62; mock Riot, live opt-in, required `acceptance` CI job; 29/29, ADR-059)
- [x] P8-02 `migrate-v1` subcommand (#63; 1 837 matches/s in release, ADR-060)
- [x] P8-03 built-in TLS (#64; rustls-acme TLS-ALPN-01, 308 redirect, private ops endpoints, ADR-061)
- [x] P8-04 Postgres feature flag, compile-only (#65; `Store` seam, claim routed through `SqliteStore`, ADR-062)
- [x] P8-05 release pipeline (#66; three binaries + GHCR image on `v*`, README for v2, ADR-063; `v2.0.0-rc.0` dry run published: three binaries + sha256sums + image digest)
- [x] P8-06 cut-over runbook (#67; `docs/CUTOVER.md`, ADR-064; owner sign-off pending)
- [x] P8 exit prep (#68; 2.0.0-rc.1, `/docs` in CI's compose check, the release job attaches binaries only)

Exit check (2026-10-04, `main` @ `031acff`, CI run 37127324010 and release run 37127332578): **passed.**
- **Acceptance suite green against v2 with a mock upstream:** CI job `acceptance` (v1's suite, mock Riot, `serve` from this commit):
  ```
  Test Files  5 passed (5)
       Tests  29 passed (29)
  ```
- **`migrate-v1` imports a v1 dump fixture:** `tests/migrate_v1.rs` in CI's `test` job:
  ```
  test the_v1_archive_and_players_arrive_and_consumers_do_not ... ok
  test the_cli_imports_the_text_form_and_the_custom_dump_when_pg_restore_exists ... ok
  ```
  Locally (Postgres 17), a plain `pg_dump --data-only -t matches -t players` of the fixture imports as `imported 19 matches, 1 timelines, 12 players`, one malformed body skipped (ADR-064).
- **`docker compose up` on a clean VM serves `/docs`:** CI's `build-musl` job, on a fresh GitHub-hosted runner, builds the image from `docker-compose.yml`, waits for `/healthz`, then `curl -fsS localhost:8080/docs | grep -qi "<html"` and `/openapi.json` carries `"openapi"`. Both passed.
- **GitHub release `v2.0.0-rc.1`** (pre-release), https://github.com/NinjaGoldfinch/riot-proxy/releases/tag/v2.0.0-rc.1:
  ```
  riot-proxy-darwin-arm64  15225824  5197abf25d782d1484baf9de40ba07319ea783ae4643bc405df584f686ac806a
  riot-proxy-linux-amd64   17954912  237da9a7034c8413a45ccf059ed740dcd7482e2392deca1f2411632930a03af3  (static-pie, < 20 MiB)
  riot-proxy-linux-arm64   15238512  46dce345af9d03c8d7d982575528da6418002f8faeffd3030565a39607a3c78a
  sha256sums
  image ghcr.io/ninjagoldfinch/riot-proxy:2.0.0-rc.1
        @sha256:9309b10b0dd2553ca7552881d71c85ac378c6e7540aa121f0ed9c054f7678980  (amd64 + arm64, anonymous pull 200)
  ```
  Each binary's `--version` was checked against the tag in the run; the amd64 image passed its own healthcheck. `:2` is not pushed for a pre-release (404), as designed (ADR-063).

Owner items still open from P8: P8-06 runbook sign-off. (P8-03's real-domain TLS check was dropped with the feature in RC-03.)

## RC — fixes before 2.0.0 (owner requests after rc.1)
- [x] RC-01 no default platform (#70; player + admin routes require it, analytics sums every platform without it, ADR-065)
- [x] RC-02 account-v1 without a region (#71; region-less `/v1/riot/accounts/*` routes, every internal lookup picks asia → americas → europe by limiter room and 429s, one shared cache entry, ADR-066)
- [x] RC release `v2.0.0-rc.2` (#73; RC-01 + RC-02)
- [x] RC-03 remove built-in TLS (#74; plain HTTP only; `TLS=true` refuses to boot; musl binary 17.9 → 16.6 MB, ADR-067)
- [x] RC-04 `TRUST_PROXY` (#75; default off: the admin allowlist uses the TCP peer, `X-Forwarded-For` only behind a trusted proxy, ADR-068)
- [x] RC release `v2.0.0-rc.3` (#76; RC-03 + RC-04)

## OPS — dev environment on Proxmox (owner request 2026-10-05)
- [x] OPS-01 `:edge` image on every push to `main` (#79, ADR-069)
- [x] OPS-02 Proxmox dev VM (#80; `deploy/proxmox/`, CI job `ops`, ADR-070)
- [x] OPS-03 `create-vm.sh --generate-key`: a unique login keypair per VM (#82, ADR-072)
- [x] OPS-04 faster CI: `build-musl` packages its own binary, the source `Dockerfile` builds in a cached `docker` job, caches saved from `main` only (#93, ADR-079)
- [ ] OPS-05 each image's GHCR description lists the commits since the image before it (`:edge` and releases); release notes list the PRs (ADR-107)

## DEV — dev explorer (owner request 2026-10-05)
- [x] DEV-01 dev explorer at `/dev` (#81; replaces v1's dev UI, forms from the OpenAPI document, never in production, design/10, ADR-071)
- [x] DEV-02 player tab: closable windows, matches paged 10/25/50 with queue/type filters, page summary, inline scoreboard, history and backfill card (#84, ADR-073)
- [x] DEV-03 per-player archive endpoints (`/v1/admin/players/{puuid}/archive[/matches]`), exact backfill counts and an "Archive (all stored)" match source in `/dev`, jsdom page tests in CI (#86, ADR-074)
- [x] DEV-04 scoreboard layout: match header row (patch, length, X-Cache, raw, ×), aligned team tables, right-aligned numbers, side and team totals; match list shows the result, CS and length cleanly (#87, ADR-075)
- [x] DEV-05 Data Dragon images mirrored on first request at `/ddragon/{v}/img/{kind}/{file}`: champion, profile icon, item, spell (#89, ADR-076)
- [x] DEV-06 `/dev/showcase`: example frontend shell, Showcase in the page bar, read-route coverage guard, home view (ladder, status, rotation, top champions) (#95, design/11, ADR-080)
- [x] DEV-07 showcase player view: profile header, rank cards, live-game banner, match history with queue tabs and load more, champion pool, mastery (#98, design/11, ADR-082)
- [x] DEV-08 showcase match detail (scoreboard, gold-difference graph) and champion view (rates by tier, builds, matchups) (#99, design/11, ADR-083)
- [x] DEV-09 Reset tab in `/dev`: `GET`/`POST /dev/reset` (admin, dev only) wipes all fetched data and keeps consumers and the limiter checkpoint (#91, ADR-077)
- [x] DEV-10 page bar across `/dashboard` and `/dev` (Dashboard · Dev explorer · API docs · Metrics), rendered from config so it links only to pages that exist (#92, ADR-078)
- [x] DEV-11 lookup backfill walks the whole history: `LOOKUP_BACKFILL_LIMIT` defaults to and caps at `u32::MAX` (#94, ADR-081)
- [x] DEV-12 showcase layout tested in headless Chromium (playwright-core) at three widths with real Data Dragon-sized images; icons fixed to their CSS size, match cards, scoreboards, headers and chips fixed on narrow screens (#100, design/11, ADR-084)
- [x] DEV-13 dashboard crawl activity: `GET /v1/admin/jobs/queue` (running, up next in claim order, ready/delayed) and `GET /v1/admin/ladder/crawls/{id}` (stage progress with pace and ETA, legs in flight, running/next/failed jobs, jobs ahead, platform downloads); history rows open into it; dashboard jsdom and Chromium layout tests (#101, design/06, ADR-087)
- [x] DEV-14 rune icons mirrored at `/ddragon/{v}/img/perk-images/…` from runesReforged.json; showcase scoreboard and match cards show keystone + secondary style and the champion level as a badge on the portrait; champion builds show rune icons (#103, design/07, design/11, ADR-086)
- [x] DEV-15 fetched timelines are archived whatever `ARCHIVE_TIMELINES` says; the flag only makes archive jobs fetch them (#102, ADR-085)
- [x] DEV-16 dashboard Recompute now: queue `aggregate:analytics` for a picked ladder from the Analytics recompute panel (#104, design/06, ADR-088)
- [x] DEV-17 dashboard Ladder tab cleanup: one card per running crawl (stage bars and totals, the rest in a details fold), past crawls only below and paged by ten, job queue and analytics recompute folded, start form folded unless idle (#105, design/06, ADR-091)
- [x] DEV-18 manual analytics recompute runs first: the route queues `aggregate:analytics` at priority 0 and lifts a rebuild already queued for the ladder, so the next free worker runs it on the matches archived so far (#106, design/06, ADR-090)
- [x] DEV-19 `/dev/jobs`: live job activity (in-memory traces of what each worker's job does: steps, Riot calls, rate-limit waits, outcome) through `GET /v1/admin/jobs/activity` and `/v1/admin/jobs/{id}/activity`, and a page with the workers, the queue, and a closable tab per job that follows it (#108, design/06, design/10, ADR-092)
- [x] DEV-20 dashboard job queue groups alike jobs: one row per kind and platform with its count (a collect row gives the player range), up next reads the next 100 ready jobs (#112, design/06, ADR-093)
- [x] DEV-21 analytics over every patch: `patch=all` sums every aggregated patch on the three analytics routes, `GET /v1/lol/analytics/patches` lists a ladder's patches with their games, and the showcase reads every patch by default with a patch picker (Top champions, champion page) and a region picker (champion page); the API default stays the newest patch (#113, design/11, ADR-094)
- [x] DEV-22 the dev reset stops running jobs first: `Queue::halt` holds the workers and aborts what they run, the reset wipes (jobs included) and lets them go; the response adds `stoppedJobs` (#115, design/06, design/10, ADR-096)
- [x] DEV-23 timelines on by default: `ARCHIVE_TIMELINES` defaults to `true`, so ladder crawls, polls and backfills fetch each archived match's timeline; `false` turns it off (#118, design/04, design/07, ADR-098)
- [x] DEV-24 the showcase and the `/dev` explorer revalidate every call (`cache: 'no-cache'`), so a recompute shows at once instead of after the analytics routes' `max-age=300` (#119, design/10, design/11, ADR-099)
- [x] DEV-25 the champion page's patch picker counts that champion's games: `GET /v1/lol/analytics/patches` takes `championId` and then lists only the patches it was played on, with its games (#120, design/11, ADR-100)
- [x] DEV-26 the matchups rebuild reads each laned fact once (lane head count from a window), so SQLite no longer loops over the whole ladder for every fact: an oc1 recompute's matchups step took 43.8 s (#121, ADR-101)
- [x] DEV-27 the writer keeps the planner's statistics fresh: `PRAGMA optimize` at open, before each analytics rebuild and in the daily maintenance (#123, design/04, design/06, ADR-102)
- [x] DEV-28 analytics count every archived game: every participant of the platform's matches, at the newer of their ladder tier and their last league lookup (new `player_ranks`, recorded from every `league.entriesByPuuid` read), else `UNKNOWN`; matchups from both sides; the showcase shows UNKNOWN last (#127, design/04, design/11, ADR-105)

## SCH — scheduler fairness and an elastic pool (owner requests 2026-10-08/09)
- [x] SCH-01 rate-limit-aware job claims: one lane per limiter scope plus the job's main endpoint, claims skip a region only when its app limit is full (a capped endpoint blocks only jobs that use it), spread over free regions, rate-limited jobs yield their worker and re-queue without using an attempt (IMPLEMENTATION.md §Post-release — SCH) (#107, ADR-089)
- [ ] SCH-02 elastic worker pool: base `JOB_CONCURRENCY` workers, extra workers spun up while all are busy up to `JOB_MAX_WORKERS`, idle extras exit, `JOB_MAX_PER_GROUP` caps the workers one crawl or one player can hold (IMPLEMENTATION.md §Post-release — SCH)

## THR — crawl and analytics throughput (owner plan 2026-10-09)
Baseline before THR-06a (one kr crawl to MASTER on the dev VM): not yet recorded. Since DEV-23 a crawl fetches timelines by default (ADR-098); record which setting the baseline used.
- [ ] THR-06a idle bulk ceiling: `BULK_IDLE_CEILING` replaces `BULK_USAGE_CEILING` for a scope with no interactive request in `BULK_IDLE_AFTER_S` (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-01 batch fetching: `archive:batch` fetches a crawl's ids `ARCHIVE_BATCH_CONCURRENCY` at a time, so the limiter paces them, not the worker count (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-02 stored tiers: `match_tiers` stamped when a ranked match is archived, `tiers:backfill`, analytics join it in place of `ladder_entries` (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-03 incremental analytics: `aggregate:delta` adds uncounted matches in short transactions; the rebuild stays as a repair tool (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-04 collect cursor: skip players whose wins + losses are unchanged, `startTime` for the rest (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-05 wider coverage: LAD-02's discover ordered by match count, run alongside archive, tiers stamped for discovered players (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-06b timeline sampling for crawl matches (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-06c several matches per write transaction, if measured writer wait justifies it (IMPLEMENTATION.md §Post-release — THR)
- [ ] THR-06d zstd dictionary for match bodies, if an offline trial shows a quarter or more saved (IMPLEMENTATION.md §Post-release — THR)

## LAD — Master players past Riot's 10,000 cap (owner request 2026-10-09)
- [x] LAD-01 say when an apex league is cut off at Riot's cap: `apex_capped` on the crawl (V0008, set in the apex leg's write transaction for a list of `RIOT_APEX_LIST_CAP` or more), `apexCapped` on the crawl routes and the stats snapshot, "Master: top 10,000 only (Riot API limit)" next to the player count on the dashboard crawl card and under the showcase ladder (#117, design/06, design/11, ADR-097)
- [ ] LAD-02 find the Master players the cap leaves out: a `discover` stage that looks up the ranks of archived match participants not on the ladder (IMPLEMENTATION.md §Post-release — LAD)
- [x] LAD-03 re-run the cap checks from `/dev`: `POST /v1/admin/ladder/probe` checks `masterleagues` against `RIOT_APEX_LIST_CAP`, pages league-exp-v4 (`league.expEntries`, new in the registry) and compares the two, and checks that the paged route refuses MASTER. Each check is confirmed / not-seen / changed / error, shown on a new Ladder tab on `/dev` (#114, design/10, ADR-095)

## SITE — requests from ninjagoldfinch.lol (owner request 2026-10-09)
The site's backfill question (item 3) needs no task: the unbounded lookup backfill stays (ADR-081, owner confirmed 2026-10-09); SITE-04 documents it.
- [x] SITE-01 report when each part was last fetched from Riot: `cache.fetched_at` (V0009) beside `content_at`, `fetchedAgeSeconds` on the profile, `matchIdsFetchedAgeSeconds` on the match page, `X-Cache-Fetched-Age` on passthroughs and composites (none from the archive) (#125, design/04, ADR-103)
- [x] SITE-02 `champion` filter on the match page: Riot's newest 20 ids archived first, then the archive paged for that champion (`archive::player::champion_match_ids`), exact `hasMore`, `archive.complete` from the backfill's `doneAt`, `type` refused with it; the showcase's pool rows filter the history (#126, design/11, ADR-104)
- [x] SITE-03 `gameVersion` described as the game build, plus `ddragonVersion`: the newest Data Dragon version in the mirrored `versions.json` with the build's `major.minor` (`Mirror::versions`, `static::ddragon_version_for`), absent when there is none (#PR, ADR-108)
- [ ] SITE-04 declare response headers in the OpenAPI document; document the image mirror and the backfill limit (IMPLEMENTATION.md §Post-release — SITE)
- [ ] SITE-05 partial schemas for Riot's account, summoner, league and mastery payloads (IMPLEMENTATION.md §Post-release — SITE)
- [x] SITE-06 `roleBoundItem` in `PlayerSummary`, next to `item6`: the role quest slot (a bot laner's boots, another role's quest reward), verbatim, absent when Riot didn't send it; the showcase shows it after the inventory (#130, design/11, ADR-106)

## BLD — set builds on the champion page (owner request 2026-10-09)
- [ ] BLD-01 per-player build facts from the timeline: `match_builds` (purchase order minus undos, finished items, boots, starter, skill order) filled by `builds:extract` before each recompute (IMPLEMENTATION.md §Post-release — BLD)
- [ ] BLD-02 aggregate set builds: `champion_builds` keyed by the first two finished items, with `champion_build_parts` for the 3rd–5th items, starter, boots, skill order, runes and spells (IMPLEMENTATION.md §Post-release — BLD)
- [ ] BLD-03 `GET /v1/lol/analytics/champions/{championId}/builds` and the showcase's build tabs, with today's lists as the fallback (IMPLEMENTATION.md §Post-release — BLD)

## MU — matchups worth reading (owner request 2026-10-09)
- [ ] MU-01 show how much a matchup's win rate can be trusted: `winRateLow` (Wilson 95% lower bound), `sort=confidence`, small samples dimmed in the showcase (IMPLEMENTATION.md §Post-release — MU)
- [ ] MU-02 win rate against what the two champions would be expected to do: `delta` from each champion's lane baseline, a *vs expected* column in the showcase; after DEV-28 (IMPLEMENTATION.md §Post-release — MU)

## Owner review at the P0 gate — resolved 2026-09-24 (ADR-014)
- CORS deferred (off, as v1). License MIT. New metrics use design names without the `proxy_` prefix. Bootstrap-to-stderr and the `NODE_ENV` fallback are confirmed.

## Notes for the next task
- **Names backfill:** `names:backfill` and `POST /v1/admin/players/names/backfill` (v1's job that fills Riot IDs from archived matches) landed with P7-03 (#57): `src/jobs/names.rs`, a daily tick, queued when a crawl ends, tests in `tests/ladder.rs`.
- **Events** keep v1's names plus `crawl.phase` (ADR-045): P7-04 publishes `analytics.updated`; P7-02/03 publish `crawl.phase` and `ladder.crawl.completed`; the dashboard's names already match.
- Routes: follow `src/routes/riot.rs`, which uses `http::validate::*` for v1 rules and `routes::passthrough::{respond, options}`. Add read routers to `routes::docs::api_router`'s `read` group so they get auth+quota. `tests/common::app_with(env, wiremock_uri)` gives a full app. v1's `/v1/lol` bodies are raw passthrough too (`PassthroughResponse`), so P4-05's "typed" part is request validation.
- OpenAPI: add routes to `routes::docs::api_router()` as `OpenApiRouter`s with `#[utoipa::path]` handlers. `just`/CI check: `cargo run -- spec > /tmp/spec.json && scripts/compare-openapi.py docs/contract/v1-openapi.json /tmp/spec.json --prefix /v1/riot/ --prefix /v1/lol/` (16 missing at P4-03). The `tests/snapshots/openapi__openapi_document.snap` changes with every documented route; review it.
- Disk: `target/` reached ~26 GB with debug, release and feature builds and hit the session's disk allowance. `rm -rf target/release target/debug/incremental` frees ~10 GB.
- Auth: protect routers with `.route_layer(axum::middleware::from_fn_with_state(state.clone(), http::auth::require_read))` (or `require_admin`); handlers take `Extension<Arc<http::auth::Consumer>>` (has `quota_per_min` for P4-02). `AppState.auth: Arc<Auth>`; admin revoke calls `auth.invalidate(hash)` (done in P5-05).
- Replay: `tests/replay.rs` + `tests/fixtures/replay/` (README documents re-recording). Since P5-02, pass-2 matches in `replay__replay_cold_summoner_lookup.snap` are `ARCHIVE`.
- Fetcher: `state.fetcher.fetch(RiotRequest, FetchOptions{priority, bypass}) -> Result<FetchResult{body, x_cache, cache_age}, FetchError{api, x_cache}>`. Routes (P4-04) must set `X-Cache`/`X-Cache-Age`, including on HIT-NEG errors. `?refresh=true` → `bypass`, admin only (P4). The `Archive` trait's SQLite implementation is `archive::SqliteArchive` (P5-02); `archive::matches::{get, put, filter_unarchived, get_timeline, put_timeline}` for jobs.
- Single-flight: `singleflight::SingleFlight<K,T,E>::run(key, || async {..}) -> Flight{value, did_work}`; `E: From<WorkFailed> + Clone`. The work is spawned, so put the cache write *inside* the work closure.
- Cache: `cache::ResponseCache::new(L1, Some(L2Writer::spawn(db)))` with `get`, `put(key, ep, body, &ttls)`, `put_negative(key, ep, ttl)`, `shutdown()`. **P3-05 must wire into `serve`:** `l2::warm` + `l2::sweep` at boot, `cache.shutdown()` after the drain. `crate::clock::Clock` converts Instant↔unix ms.
- L1: `cache::l1::{L1::from_config, get → Lookup::{Fresh,Stale,Miss}, put(key, body, &ttls), put_negative, insert_entry (for L2 warm), invalidate_where}`. Entries hold tokio `Instant`s; P3-03 must convert to and from unix ms, e.g. with `riot::limiter::persist::Clock`.
- Cache keys: `cache::keys::{KeyScope::from_key(&cfg.riot_api_key), cache_key(&scope, &req), derived_key, scoped_purge_pattern}`, in design 04's readable shape (owner, ADR-027). `RiotRequest.params` holds the encoded path params. No `neg:` prefix: L1 entries carry their status.
- `AppState` now has `limiter: Arc<Limiter>` and `limiter_restored`; `serve` restores, checkpoints every 10 s and on shutdown (`riot::limiter::persist`). `/readyz` body is `{ok, sqlite, limiter}`.
- Limiter: **sliding-log windows** (owner, ADR-023), not design/05's fixed counters. The API is in `src/riot/limiter/mod.rs` (todo!() bodies). `tests.rs` holds 24 ported cases, `#[ignore = "P2-0x"]` by task; un-ignore them as each task lands. Use `tokio::time::Instant` everywhere so `start_paused` tests work. `bucket::Window` (sliding log: `try_take`, `rollback`, `next_free`, `sync`, `prune`), `ScopeState::reconfigure`, `ScopeEntry {app, app_known, methods, frozen_until}`; `Limiter::lock()` is a std `Mutex` and must never be held across an await.
- Dev CLI: `just riot account/by-riot-id europe 'Hide on bush' KR1`. A real dev key is in `./.env` (gitignored; dev keys expire every 24 h).
- Headers: `riot::limiter::headers::{RateLimitHeaders::from_headers, parse_limits, parse_counts, RateLimitType, BOOTSTRAP_APP_LIMITS}`. The client still reports the 429 type as a raw string; P2-04 can convert with `RateLimitType::parse`.
- Service-429 backoff (owner, ADR-021): use **v1's numbers**, 500 ms × 2ⁿ capped at 8 s, ±20 %, 3 tries, implemented in the fetcher (P3-05), not the client.
- Client: `RiotClient::new(&cfg)` / `with_base_url(&cfg, mock_uri)`; `RiotRequest::new(ep, target, &params)?.query(k, Some(v))?`; `client.send(&req) -> Result<RiotResponse, RiotError>` (errors carry `headers`). v1's retry policy lives in the fetcher (ADR-031).
- Endpoints: `riot::endpoints::{ENDPOINTS, Endpoint::by_id, Endpoint::path(&[..]), target_for_platform/region, TtlPolicy::from_config(&cfg).ttls(ep)}`. When the fetcher is wired (P3-05), `serve` should log `ineffective_overrides()` at warn.
- `X-Cache` for negative hits is **`HIT-NEG`** (owner, ADR-022), not design/03's `NEG`. That includes the P3 exit check list.
- Routing: `riot::routing::{Platform, Region}` with `region()`, `account_region()` (sea→asia), `host()`, `parse()` → `BAD_REGION`, and `Platform::from_match_id`. Config's `default_platform` and `ladder_platforms` are typed.
- v1 reference is `NinjaGoldfinch/riot-proxy-deprecated` (cloned at `../riot-proxy-v1`, commit `c86e631`), not `ninja-recorder-deprecated` as §1 of the plan says.
- Repo is **public** (owner decision), not private.
- Toolchain pinned to 1.98.1 (`rust-toolchain.toml`); CI installs it with `rustup toolchain install`.
- rusqlite is held at **0.39** for refinery 0.9 compatibility (ADR-005). Don't bump it without checking refinery's range.
- reqwest 0.13: the feature is `rustls`, not `rustls-tls`. Every reqwest client must use `tls_certs_only(...)` (webpki roots, or empty for plain HTTP), or it fails to build in `FROM scratch` (ADR-007/013).
- `metrics-exporter-prometheus` has default features off (no built-in HTTP listener); P0-03 renders `/metrics` from our own axum route.
- Config: `riot_proxy::config::Config::load(ConfigArgs)`; `ConfigArgs` is a `clap::Args` to `#[command(flatten)]` into `serve` in P0-06. `LOG_LEVEL` is a tracing filter string and `log_format` is already resolved (tty → pretty) for P0-03. Platforms are typed and validated at boot (P1-01). `LADDER_QUEUES`/`LADDER_TIER_FLOOR` are validated at boot since P7-02 (ADR-054).
- Telemetry: `telemetry::init_tracing(&config)`, `telemetry::metrics_handle()` (idempotent), `telemetry::spawn_upkeep(handle)` and `telemetry::metrics_router(handle)` for P0-05 to merge. `http::request_id::request_id` goes on as the **outermost** `axum::middleware::from_fn` layer. Metric names are constants in `src/metrics.rs`; use those, not string literals.
- DB: `db::Db::open(path, readers)` (blocking) or `Db::open_async`; `db.write(|c: &mut Connection| …)` and `db.read(|c: &Connection| …)`, generic over the error type (`E: From<DbError>`). Migrations are `src/db/migrations/V000N__name.sql` (refinery naming, ADR-010); P5-01 adds `V0002__archive.sql`. `Config.database` gives the path (`Database::Sqlite(path)`).
- Error envelope (owner decision): `{error:{code,message,requestId,retryAfter?}}`. Return `http::ApiError` from handlers; `requestId` is filled in automatically (ADR-011).
- App: `app::router(AppState{config,db}, metrics_handle)` and `app::serve(listener, router, app::shutdown_signal()?)`. The CLI lives in `src/cli/` (`serve`, `migrate`, `key create|list|revoke`, `healthcheck`, `spec`); `main.rs` just calls `cli::run()`. Consumer storage is `src/consumers.rs` (`create`, `list`, `revoke`, `bootstrap_admin`, `hash_key`), which P4-01 auth should reuse. Integration-test fixtures are in `tests/common/mod.rs`; `tests/cli.rs` drives the real binary.
- Logs go to **stdout** (JSON). The bootstrap key banner goes to **stderr**.
- The crate is lib + bin (`src/lib.rs`), so `tests/*.rs` can import modules.
- The musl build needs `musl-gcc`, which isn't on the dev box (no sudo), so it's verified in CI only.
- CI (owner decision): required status checks are the job names `fmt`, `clippy`, `test`, `build-musl` (not the workflow name `ci`, which GitHub never reports as a check). Since P0-07, `build-musl` also builds and smoke-tests the Docker image and `docker compose up` (ADR-013).
- No docker on the dev box; image behaviour is verified in CI only. `just` is installed at `~/.local/bin/just`.
- `acceptance/` is v1's suite verbatim (plus its `vitest.acceptance.config.ts`); it hits the real Riot API and is ported in P8-01.

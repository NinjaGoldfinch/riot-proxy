# Tests, tooling, deploy kit, docs map, ADR index and git history — inventory

Scratch for DOC-01. Branch `docs/DOC-01-documentation-set`, HEAD `7491ff6` (= `origin/main` at the time of reading). Evidence only; every claim cites `file:line` as read on 2026-10-19. Nothing was built or run (`cargo build`/`cargo test` not invoked; `.env` not read).

---

## 1. Tests

### 1.1 Shape of the suite

- The crate is lib + bin (`PROGRESS.md:420` "The crate is lib + bin (`src/lib.rs`), so `tests/*.rs` can import modules"), so every file in `tests/` is its own cargo integration-test binary. There is **no `tests/it.rs` and no `[[test]]`/`autotests = false` in `Cargo.toml`** on this HEAD (`Cargo.toml:1–83` has `[package]`, `[dependencies]`, `[features]`, `[dev-dependencies]`, `[lints.clippy]`, `[profile.release]`, `[profile.release.package."*"]` only). The single-binary consolidation is task **CI-04**, still unticked (`PROGRESS.md:373`), specified at `docs/IMPLEMENTATION.md:581` ("`autotests = false` and one `[[test]]` target, `tests/it.rs`, with every `tests/*.rs` as a module… Snapshots keep their files, renamed for the new module path").
- Testing strategy (the authority): `docs/IMPLEMENTATION.md:88–99` §0.6, a six-layer table — unit (`src/<module>/tests.rs`), integration (wiremock + tempfile SQLite in `tests/*.rs`), snapshot (insta, `tests/snapshots/`), replay (`tests/fixtures/replay/`), acceptance (`acceptance/`, vitest), property (`proptest`, limiter only). Coverage: "measured with `cargo llvm-cov` in CI and reported, not gated — but any module under 70% needs a written justification in the PR" (`docs/IMPLEMENTATION.md:99`).
- Dev-dependencies (`Cargo.toml:56–66`): `async-trait`, `futures-util`, `insta` 1.48 (`json` feature), `proptest` 1.11, `tempfile` 3.27, `tokio-test`, `tokio-tungstenite` 0.29, `ulid`, `wiremock` 0.6.
- Lint policy relevant to tests: `clippy.toml:2–4` allows `unwrap`/`expect`/`panic` in tests; `Cargo.toml:68–71` warns on them elsewhere.
- Counts (from ADR-174, measured 2026-10-19): "938 tests" total under nextest; "the lib's 562 tests" and "about 45 test binaries" (`docs/DECISIONS.md:1896–1905`). My own grep of `#[tokio::test]`/`#[test]` attributes in `tests/*.rs` gives about 381 integration test fns across 41 files (approximate; attribute grep).

### 1.2 Integration test binaries (`tests/*.rs`) — what each covers

Line counts and attribute-grep test counts are from the read; the "covers" column is the file's `//!` header, cited at its line 1.

| File | Lines | Tests | Covers (from the module doc) |
|---|---|---|---|
| `tests/admin_jobs.rs` | 474 | 8 | `/v1/admin/jobs*` and `/v1/admin/backfill` (P6-08): list filters, stats, retry failed only, cancel pending only, admin scope (`:1–3`) |
| `tests/admin_routes.rs` | 1125 | 19 | `/v1/admin/*` data subset (P5-05) through the full app against wiremock Riot; admin scope on every route; v1 request rules and messages (`:1–3`) |
| `tests/analytics.rs` | 2292 | 31 | Analytics end to end (P7-04): two recorded matches archived for real, KR ladder, `aggregate:analytics`, the three `/v1/lol/analytics/*` routes (ETag, 304, `?remakes=`), `facts:reextract`, admin routes (`:1–4`) |
| `tests/archive_jobs.rs` | 524 | 12 | `archive:match` and `backfill:player` against wiremock (P6-06): 250-id backfill over three pages, resume from cursor, `match.archived` only for new matches (`:1–4`) |
| `tests/archive.rs` | 388 | 10 | Match archive through the full app (P5-02): fetched once then `X-Cache: ARCHIVE` byte-identical; match + timeline archived together (TL-01) (`:1–4`) |
| `tests/auth.rs` | 355 | 13 | Auth through a real router: keys, revocation, scopes, admin allowlist, `AUTH_DISABLED`, v1's 401/403 bodies as snapshots (`:1–2`) |
| `tests/builds.rs` | 268 | 7 | `builds:extract` end to end (BLD-01): a recorded ranked game + timeline, mirrored item.json, `match_builds` read back (`:1–3`) |
| `tests/cache_l2.rs` | 296 | 8 | L2 against a real SQLite file: write-behind, restart, warm, sweep (`:1`) |
| `tests/cli.rs` | 602 | 13 | The real binary: subcommands, key storage, bootstrap-once, healthcheck (`:1`) |
| `tests/cors.rs` | 158 | 3 | ADM-07 opt-in CORS for the admin API (`:1–4`) |
| `tests/dashboard.rs` | 564 | 5 | Dashboard backend (P7-06): `GET /v1/admin/metrics` builds v1's whole `MetricsSnapshot`; history sampler keeps newest 1440 points (`:1–3`) |
| `tests/ddragon.rs` | 984 | 28 | Data Dragon (P7-01): `ddragon:sync` against a wiremock CDN (ported from v1 `test/ddragon-mirror.test.ts`), `/v1/static/*`, `/ddragon/*` images filled on first request (DEV-05), `POST /v1/admin/ddragon/sync` (`:1–4`) |
| `tests/dev_reset.rs` | 359 | 8 | `/dev/reset` (DEV-09, ADR-077): admin only, dev-only mount, `{"confirm":"reset"}`, wipes fetched tables + L1, keeps consumers and limiter checkpoint, stops running jobs (DEV-22) (`:1–4`) |
| `tests/fetcher_states.rs` | 545 | 17 | Fetcher against wiremock: every `X-Cache` state (HIT, MISS, STALE, HIT-NEG, ARCHIVE, BYPASS), stale-on-failure, bulk-priority SWR, retries, error mapping; P3-05 / P3 exit check; real time, short TTLs (`:1–3`) |
| `tests/http.rs` | 145 | 9 | HTTP skeleton: health routes, error envelope on unrouted requests, layers (`:1`) |
| `tests/job_activity.rs` | 205 | 1 | DEV-19 live worker activity via `/v1/admin/jobs/activity` and `/v1/admin/jobs/{id}/activity` (`:1–3`) |
| `tests/jobs.rs` | 354 | 6 | Durable job queue with real workers (P6-03): exactly-once, restart resumes, panics/unknown kinds contained, enqueue wakes an idle worker (`:1–4`) |
| `tests/ladder_probe.rs` | 208 | 3 | `POST /v1/admin/ladder/probe` (LAD-03) against a wiremock league-v4 shaped like kr on 2026-10-09 (10,000 cap, league-exp paging, MASTER refused on the paged route) (`:1–4`) |
| `tests/ladder.rs` | 2030 | 19 | Ladder enumeration (P7-02): apex leagues and every (tier, division) walked, each page fetched once, stage flips once, resume from cursor, a leg that gives up fails the crawl; `POST /v1/admin/ladder/crawl` and `/options` (`:1–5`) |
| `tests/lanes.rs` | 412 | 4 | Rate-limit-aware job claims (SCH-01): yields, side-by-side platforms, walks take turns (`:1–5`) |
| `tests/limiter_restart.rs` | 123 | 3 | Kill-and-restart against a real SQLite file: a restored limiter never over-admits across the restart (P2-06) (`:1–2`) |
| `tests/limiter_soak.rs` | 99 | 1 | 60 s soak (P2-07, P2 exit check), 50 tasks on one scope. **Ignored by default**: `cargo test --release --test limiter_soak -- --ignored --nocapture` (`:1–3`) |
| `tests/lol_routes.rs` | 241 | 4 | `/v1/lol/*` through the full app: exact upstream request per route (v1 defaults), v1 validation, contract coverage (`:1–2`) |
| `tests/maintenance.rs` | 288 | 8 | `maintenance` and `riot-proxy backup` (P7-05): daily backup, `BACKUP_KEEP`, trims, L2 sweep, WAL truncate; a backup with no room never stops the rest (DSK-02) (`:1–5`) |
| `tests/migrate_v1.rs` | 233 | 3 | `riot-proxy migrate-v1` (P8-02) on a real v1 dump: 20 matches, 1 timeline, 12 players, 2 consumers from Postgres 17 (`tests/fixtures/v1-dump/v1.dump`, `v1-data.sql`) (`:1–4`) |
| `tests/offload.rs` | 206 | 2 | `archive:offload` end to end (OBJ-03): packs into a local-directory "bucket", frees past the window, every body reads back byte for byte (`:1–4`) |
| `tests/openapi.rs` | 325 | 7 | The OpenAPI document and docs routes (P4-03), and the compare script (`:1`) |
| `tests/p6_exit.rs` | 334 | 1 | P6 exit check end to end through `serve` (track → `game.started` on `/v1/ws`; kill mid-backfill, restart, resume with no duplicates). **Ignored by default** (~10 s): `cargo test --test p6_exit -- --ignored --nocapture` (`:1–11`) |
| `tests/p7_exit.rs` | 320 | 1 | P7 exit check through `serve`: a crawl against a wiremock ladder of 30 players completes enumerate → collect → archive → done, each match fetched once. **Ignored by default**: `cargo test --test p7_exit -- --ignored --nocapture` (`:1–8`) |
| `tests/players_routes.rs` | 1004 | 35 | `/v1/players/*` through the full app (P5-04), ported from v1 `test/players-composite.test.ts` and `player-champions.test.ts`; Riot's side is the real recording `tests/fixtures/replay/cold-lookup` (`:1–3`) |
| `tests/poll.rs` | 385 | 8 | Poll handlers (P6-05): every transition publishes exactly one event; match cursor; deep gap becomes a backfill (`:1–3`) |
| `tests/replay.rs` | 209 | 2 | Replay (P3-06): recorded exchanges served by wiremock through the real fetcher, cache and limiter; event log is an insta snapshot (`:1–4`) |
| `tests/riot_client.rs` | 258 | 10 | Riot client against wiremock: headers, bytes passed through, status classification, no retries, metrics (`:1–2`) |
| `tests/riot_routes.rs` | 449 | 14 | `/v1/riot/*` through the full app: auth, passthrough, cache headers, validation, admin-only refresh, request metric, v1 contract coverage (`:1–2`) |
| `tests/shutdown.rs` | 97 | 2 | Real SIGTERM lets an in-flight request finish. Its own binary because it installs a process-wide SIGTERM handler (`:1–2`) |
| `tests/telemetry.rs` | 136 | 7 | `/metrics` exposition and the request-id middleware (`:1`) |
| `tests/timelines.rs` | 448 | 7 | `timelines:backfill` end to end (TL-02) with wiremock, real fetcher/limiter/archive, tempfile SQLite (`:1–3`) |
| `tests/ui.rs` | 511 | 27 | `/dev`, `/dev/showcase`, `/dashboard` (P4-06, DEV-01, DEV-06): served without a key when enabled, 404 when not, config documents; also runs the three node helper suites (`:1–2`, `:42–70`, `:248–249`, `:399–400`) and the showcase coverage rule (CLAUDE.md "Keep `/dev/showcase` current") |
| `tests/ws_hub.rs` | 345 | 7 | WebSocket hub spike (P6-01) over real sockets: v1 protocol + v2 additions (ADR-045), topic isolation, lag → resync, heartbeat, server closes (`:1–4`) |
| `tests/ws_route.rs` | 302 | 7 | `/v1/ws` through the full app over real TCP (P6-07): handshake auth (bearer or `?token=`), 4401, admin topics gated, quota, revocation, `metrics` topic ticks only while held (`:1–4`) |
| `tests/ws_soak.rs` | 138 | 1 | 1 000 sockets held 20 s, RSS must not grow. **Ignored by default**: `cargo test --release --test ws_soak -- --ignored --nocapture` (`:1–4`) |

Shared fixtures: `tests/common/mod.rs` (162 lines). `TEST_KEY = "RGAPI-test-key-not-real"` (`:13`); `config(extra)` builds a `Config` from an env vec (`:15–23`); `app()` / `app_with(env, upstream)` return `(TempDir, AppState, Router)` over a fresh SQLite file, a `Cdn` pointed at the wiremock URI, a `Limiter::new(0.8)`, and a `Queue` with the config's kind limits (`:26–45`); `Reply` + `send(router, req)` + `get(router, path)` drive the router with `tower::ServiceExt` (`:109–140`); `fetcher(...)` at `:141`. It is `#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (`:2`).

### 1.3 Unit tests in `src/`

- 23 `tests.rs` modules: `src/archive/{analytics,builds,facts,matches,objects,offload,pack,player,pool,recovery,tiers}/tests.rs`, `src/config/tests.rs`, `src/db/tests.rs`, `src/events/tests.rs`, `src/jobs/{activity,ladder,offload,scheduler,ticks,timelines}/tests.rs`, `src/riot/limiter/tests.rs`, `src/routes/players/summary/tests.rs`, `src/static/tests.rs`; 66 files carry `#[cfg(test)]` (grep).
- Limiter: `src/riot/limiter/` holds `bucket.rs`, `headers.rs`, `mod.rs`, `persist.rs`, `proptest.rs`, `tests.rs`; `tests.rs` is the ported v1 suite ("24 ported cases", `PROGRESS.md:401`); `proptest.rs:161` is the `proptest!` block (1000 cases + 60 s soak landed in #25, `git log` 2026-09-25). A second `proptest::proptest!` lives in `src/archive/facts/tests.rs:142`.
- `proptest-regressions/riot/limiter/proptest.txt` holds two persisted failure seeds (`cc 5e7d33…` "shrinks to ops = [Acquire ×4]" and `cc 35d459…` "Sync{app_short:0, app_long:10}, Freeze{ms:654}, …") — checked in as proptest recommends (file header lines 1–6).
- insta in unit tests: `src/archive/builds/tests.rs`, `src/archive/facts/tests.rs` (3 uses), `src/events/tests.rs` (grep).

### 1.4 Snapshots (insta)

`tests/snapshots/` — 20 `.snap` files, 19 814 lines total; named `<binary>__<test>.snap`:

`admin_routes__admin_tracked_player`, `analytics__analytics_champion_builds`, `analytics__analytics_champion_detail`, `analytics__analytics_champions`, `auth__auth_401_missing_key`, `auth__auth_403_admin_ip`, `auth__auth_403_missing_scope`, `auth__quota_429`, `ddragon__ddragon_file_headers`, `ddragon__ddragon_missing_headers`, `http__unknown_route_404`, `lol_routes__lol_routes_upstream_calls`, `openapi__openapi_document`, `players_routes__players_champion_pool`, `players_routes__players_match_page`, `players_routes__players_profile`, `replay__replay_cold_summoner_lookup`, `replay__replay_typed_application_429`, `riot_client__riot_error_bodies`, `ws_hub__ws_session`.

- Header format is insta's (`source: tests/replay.rs` / `expression: log`, `replay__replay_cold_summoner_lookup.snap:1–4`).
- Update policy: "Review snapshot diffs deliberately; never `--accept` blindly" (`docs/IMPLEMENTATION.md:93`); `tests/replay.rs:3` "review changes deliberately"; `tests/fixtures/replay/README.md:22` "Re-recording changes the snapshot in `tests/snapshots/`; review it deliberately"; `PROGRESS.md:388` "The `tests/snapshots/openapi__openapi_document.snap` changes with every documented route; review it." The PR template checks "public API changes reflected in `/openapi.json` snapshot test" (`.github/PULL_REQUEST_TEMPLATE.md:21`).
- **No `cargo insta` / `INSTA_UPDATE` mention anywhere in the repo** (grep of docs, README, justfile, tests). Updating is the standard insta workflow, undocumented here — see TODOs.
- No custom `insta::Settings` / `snapshot_path` in `tests/` or `src/` (grep).

### 1.5 Fixtures and mocking

- **Mock upstream:** `wiremock` 0.6 for Riot and the Data Dragon CDN (`tests/common/mod.rs:31–38`; every "against a wiremock" header above). The replay and players suites serve real recordings through wiremock.
- **Database:** `tempfile::tempdir()` + `Db::open(path, 2)` per test (`tests/common/mod.rs:33–34`).
- `tests/fixtures/` (sizes: builds 2.9 M, matches 300 K, replay 600 K, v1-dump 2.0 M):
  - `replay/README.md` (26 lines): each exchange is `NN-<method>.json` (request, status, rate-limit headers) + `NN-<method>.body` (verbatim bytes) (`:4`). "The API key is never recorded: `riot record` redacts it and refuses to keep anything key-shaped, and CI greps for `RGAPI-`" (`:4`). `cold-lookup` is a real recording of 2026-09-24 (`Hide on bush#KR1`: account, summoner, league, mastery top-3, 5 match ids, 5 matches; 10 exchanges `01`–`10`), re-recorded with `cargo run -q --features dev-cli -- riot record --out $OUT …` (`:8–20`). `429-typed-application` is synthetic, built from `cold-lookup/02` with `"synthetic": true`; a real application 429 "is never provoked (owner decision)" (`:24–26`).
  - `matches/README.md` (17 lines): `ranked-solo.json` (= replay 06), `arena.json` (= replay 09, queue 1750, 18 players), `remake.json` (derived from replay 07 by script: 185 s, early surrender, holes in `teamPosition`/`perks`/`summoner2Id`) for `src/archive/facts/tests.rs` (`:3–15`).
  - `builds/README.md` (21 lines): three real OC1 ranked games with timelines (`OC1_711969250` patch 16.19, `OC1_712417978` 16.20 for the Viego rule BLD-05, `OC1_706102889` 16.14 for the Udyr rule BLD-06), cut-down `item-16.{14,19,20}.1.json`, and `*.expected.json` golden files derived by an independent script; puuids replaced with `bld-fixture-puuid-NN` (`:5–21`). Read by `src/archive/builds/tests.rs`, `tests/builds.rs` and `tests/showcase.mjs` (`:3`).
  - `v1-dump/v1.dump` + `v1-data.sql`: a Postgres 17 `pg_dump` of v1's own migrations (`tests/migrate_v1.rs:1–4`).
  - `v1.env.example` (115 lines): v1's `.env.example` at `c86e631` verbatim with the key redacted (`:1–4`), used by the `.env.example` parity test (`docs/DECISIONS.md:177`).
  - `.gitkeep`.
- **Secret hygiene:** CI's `test` job runs `! grep -rE "RGAPI-[0-9a-f-]{20,}" tests/ src/ docs/` (`.github/workflows/ci.yml:52–53`).

### 1.6 Node suites

Three layers, all without a build step:

1. **Pure-helper unit tests, run from `cargo test`** (`tests/ui.rs:42–70`, `:248–249`, `:399–400` call `node --test <file>`; skipped locally if `node` isn't runnable, a panic in CI `:53–56`):
   - `tests/dev_ui.mjs` (186 lines, 16 tests): dev explorer helpers cut out of a marked block in `src/ui/dev-ui.html` (`:1–5`).
   - `tests/admin_ui.mjs` (302 lines, 31 tests): dashboard helpers and theme tokens from `src/ui/admin.html` (`:1–6`).
   - `tests/showcase.mjs` (527 lines, 50 tests): showcase helpers (DEV-06/07/08, BLD-04) from `src/ui/showcase.html` (`:1–4`).
2. **jsdom page tests** in `tests/dom/` (package `riot-proxy-dom-tests`, `node --test`, devDeps `jsdom ^25.0.1`, `playwright-core 1.64.0`; `tests/dom/package.json:1–13`). Harnesses: `admin-harness.mjs` (89 lines; loads `src/ui/admin.html` against `fake-admin.mjs` with a fake WebSocket and switchable `document.hidden`, `:1–4`), `fake-admin.mjs` (434 lines; shapes are the OpenAPI document's, `:1–4`), `fake-api.mjs` (222 lines; the read API for both showcase suites, `:1–5`). Suites (attribute-grep counts): `admin-about` 2, `admin-archive` 8, `admin-cache` 4, `admin-consumers` 5, `admin-events` 5, `admin-jobs` 8, `admin-keyboard` 6, `admin-ladder` 10, `admin-limiter` 5, `admin-overview` 8, `admin-shell` 14, `admin-traffic` 3, `dev-ui` 8, `showcase` 63 (155 `test()`s across the 15 `*.test.mjs` files by grep). Each file's header names its ADM/DEV task.
3. **Headless Chromium layout tests** (playwright-core, `chromium-headless-shell`): `showcase.browser.test.mjs` (270 lines, 5 tests; desktop/tablet/phone widths, real PNGs at Data Dragon sizes, `:1–13`) and `admin.browser.test.mjs` (151 lines, 1 test; both themes, no sideways scroll, every tab reachable, `:1–7`). Screenshots go to `tests/dom/screenshots/` (git-ignored, `.gitignore:10`; 15 PNGs present locally: `{home,player,match,champion,dashboard-ladder}-{desktop,tablet,phone}.png`). "Skips without a browser, except under CI" (`admin.browser.test.mjs:6–7`). CI uploads the screenshots as the `showcase-screenshots` artefact (`.github/workflows/ci.yml:73–79`).

### 1.7 Acceptance suite (`acceptance/`)

- Package `riot-proxy-acceptance`: "v1's black-box acceptance suite, run against riot-proxy v2 and a mock Riot (plan P8-01)"; `npm test` = `vitest run --config vitest.acceptance.config.ts`; devDeps vitest ^3.2.7, ws, dotenv, typescript (`acceptance/package.json:1–17`). No README in the directory (checked); the docs are the helper headers and ADR-059.
- `vitest.acceptance.config.ts`: a `PhaseOrder` sequencer sorts files by name so phases run 1 → 7 every time (`:4–13`); `globalSetup: ['helpers/setup.ts']`, `testTimeout` 15 min, `hookTimeout` 2 min, `fileParallelism: false`, no shuffle, verbose reporter (`:20–33`) — "Shared rate-limit buckets and one archive make parallel phases lie to each other" (`:27–28`).
- Two modes (`helpers/env.ts:10–20`, ADR-059): **mock** (default; `just acceptance` and CI) starts `helpers/mock-riot.ts` and `riot-proxy serve` pointed at it with a known admin key; **live** (`ACCEPTANCE_LIVE=1`) is v1's mode against the real Riot API, run by hand only. Env knobs read in `env.ts`: `ACCEPTANCE_LIVE`, `ACCEPTANCE_PORT` (18980), `ACCEPTANCE_MOCK_PORT` (18981), `ACCEPTANCE_BASE_URL`, `ACCEPTANCE_PHASE2_REQUESTS` (60), `ACCEPTANCE_BACKFILL_LIMIT` (40), `RIOT_API_KEY`, `ACCEPTANCE_RIOT_ID` (e.g. `NinjaGoldfinch#OCENZ`), `ACCEPTANCE_PLATFORM` (oc1), `ACCEPTANCE_API_KEY`, `ACCEPTANCE_LIVE_GAME`, `ACCEPTANCE_LADDER` (`:6`, `:83–142`). Mock mode "must not read" the repo's `.env` (`:4–5`).
- `helpers/setup.ts`: spawns `RIOT_PROXY_BIN` or `target/debug/riot-proxy` with `serve` on a scratch `DATA_DIR`, waits for `/readyz` up to 60 s (`:24–56`); prints `acceptance: SKIPPED — <reason>` when not enabled (`:58–61`).
- `helpers/mock-riot.ts` (324 lines): one address stands in for every Riot host via `x-riot-host`; the world is `Acceptance#MOCK` with ranked games, `Other#MOCK`, a MASTER ladder, spectator state flipped by `POST /__mock/state`, Data Dragon and the queue table; Riot's rate-limit model with accountable 429s (`:5–22`).
- `helpers/harness.ts` (305 lines): HTTP/WS helpers. `fixtures/match.json` (79 KB).
- Phases (it() counts): `phase1-lookups` 7, `phase2-rate-limits` 1 (zero *accountable* 429s, p95 bucket math, `:5–23`), `phase5-archive` 2, `phase6-events` 2 (real chain poll → spectator diff → event → socket in mock mode, `:16–24`), `phase7-ladder` 14 (enumeration cheap, discovery expensive, `:16–25`). There is no phase 3 or 4 file. P8 exit check recorded "Tests 29 passed (29)" (`PROGRESS.md:166–170`).

### 1.8 How to run each suite

| Suite | Command | Source |
|---|---|---|
| Everything Rust (serial binaries) | `just test` → `cargo test` | `justfile:15–17` |
| Everything Rust, as CI (parallel binaries, each test its own process) | `just nextest` → `cargo nextest run --profile ci --all-features` (needs cargo-nextest) | `justfile:19–21`; profile in `.config/nextest.toml:5–13` (`fail-fast=false`, `retries=0`, `slow-timeout 60s ×3`, `failure-output immediate-final`) |
| Ignored soaks / exit checks | `cargo test --release --test limiter_soak -- --ignored --nocapture`; `--test ws_soak`; `cargo test --test p6_exit -- --ignored --nocapture`; `--test p7_exit` | the files' headers (§1.2) |
| Node helper tests alone | `node --test tests/dev_ui.mjs` (also `admin_ui.mjs`, `showcase.mjs`) | `tests/dev_ui.mjs:4–5` |
| jsdom + Chromium | `just ui-test` → `cd tests/dom && npm ci --no-audit --no-fund && npx playwright-core install chromium-headless-shell && npm test` | `justfile:23–25` |
| Acceptance (mock) | `just acceptance` → `cargo build` then `cd acceptance && npm ci && npm test`; `ACCEPTANCE_LIVE=1` for real Riot | `justfile:49–52` |
| Lint | `just lint` → `cargo fmt --all --check` + `cargo clippy --all-targets --all-features -- -D warnings` | `justfile:27–30` |
| Coverage | `just cov` → `cargo llvm-cov --all-features --workspace --summary-only` (needs cargo-llvm-cov) | `justfile:32–34` |
| Postgres stub | `cargo check --all-targets --features postgres` and `cargo test --lib --features postgres db::store` (CI only) | `.github/workflows/ci.yml:36–38` |

### 1.9 CI jobs (what runs where) — `.github/workflows/ci.yml` (219 lines)

Triggers: push to `main`, every PR (`:3–6`). `RUSTFLAGS=-D warnings`, `GIT_SHA=${{ github.sha }}` for `build.rs` (`:8–12`). Job ids are the required checks: `fmt`, `clippy`, `test`, `build-musl`, `acceptance`, "and `ui` once the owner adds it" (`:14–16`, ADR-006/175). Rust caches saved from `main` only (`:18`, ADR-079).

| Job | Does | Lines |
|---|---|---|
| `fmt` | `cargo fmt --all --check` | 20–25 |
| `clippy` | clippy `-D warnings`; postgres feature check + `db::store` tests | 27–38 |
| `test` | `cargo nextest run --profile ci --all-features --workspace`; the `RGAPI-` leak grep | 42–53 |
| `ui` | Node 22, `npm ci` in `tests/dom`, Chromium headless shell `--with-deps`, `npm test`, screenshot artefact | 58–79 |
| `coverage` | `cargo llvm-cov nextest --profile ci --all-features --workspace --summary-only` — reported, not required | 83–94 |
| `acceptance` | `cargo build` + acceptance `npm test`, mock mode | 98–115 |
| `build-musl` | static musl build; asserts static, **< 24 MiB**, carries `GIT_SHA`; packages into `Dockerfile.release`, runs the image, healthcheck, `/healthz`, `/readyz`, bootstrap banner, clean SIGTERM | 117–157 |
| `docker` | the source `Dockerfile` with GHA layer cache, only when `scripts/docker-paths.sh` says a PR can change it; `docker compose up` serves `/healthz`, `/docs`, `/openapi.json` | 164–205 |
| `ops` | shellcheck/python3-yaml/cloud-init/rclone; runs `deploy/proxmox/test.sh`, `deploy/test.sh`, `scripts/image-description.test.sh`, `scripts/edge-tags.test.sh`, `scripts/docker-paths.test.sh` — not required | 210–219 |

PR template `.github/PULL_REQUEST_TEMPLATE.md:1–21` (Task / What changed / How it was tested / Design conformance / Checklist).

---

## 2. Tooling and scripts

### 2.1 Build/dev configuration

| File | Purpose | Evidence |
|---|---|---|
| `Cargo.toml` | package `riot-proxy` 2.7.0, edition 2024, `rust-version = "1.98"`, MIT, `publish = false` (`:1–8`); deps pinned to current minor (`:10–50`); features `dev-cli` (never in release) and `postgres` (compile-only stub, ADR-062) (`:52–56`); `[lints.clippy]` unwrap/expect/panic = warn (`:68–71`); `[profile.release]` `lto = "fat"`, `codegen-units = 1`, `strip = true` "the static binary must stay under 24 MiB (CI; 20 MB until ADR-162)" (`:73–77`); dependencies at `opt-level = "s"` (22.2 → 17.9 MB, ~6 % off migrate-v1 throughput, ADR-061) (`:79–83`). No `[profile.dev]` yet (CI-05 pending). |
| `rust-toolchain.toml` | `channel = "1.98.1"` pinned at P0-01, components rustfmt + clippy, target `x86_64-unknown-linux-musl` (`:1–5`). Dockerignored, so `Dockerfile:4` pins `rust:1.98.1-alpine` separately ("bump both together", `Dockerfile:3`). |
| `rustfmt.toml` | edition 2024, `max_width = 110`, `use_field_init_shorthand` (`:1–3`) |
| `clippy.toml` | `too-many-arguments-threshold = 8`; unwrap/expect/panic allowed in tests (`:1–4`) |
| `.config/nextest.toml` | the `ci` profile (CI-02, ADR-174) (`:1–13`) |
| `build.rs` | compile-time `GIT_SHA` (env → `git rev-parse` → `unknown`, 12 chars), `RUSTC_VERSION`, `BUILD_UNIX_MS` (`SOURCE_DATE_EPOCH` or now) for `GET /v1/admin/info` (ADM-03); "Nothing here can fail a build" (`:1–10`); watches `.git/HEAD`, `packed-refs` and the branch ref so a local build after a commit reruns (`:49–68`) |
| `justfile` | `dev`, `test`, `nextest`, `ui-test`, `lint`, `cov`, `musl`, `docker`, `riot` (dev-cli raw call with `./.env`), `acceptance`; `set dotenv-load := false` because the binary reads `.env` itself (`:3`); `just --list` default (`:8–9`) |
| `.env.example` | 9.7 KB documented config; "Precedence: CLI flag > environment > this file > built-in default. An empty value (FOO=) means use the default" (`:1–3`) |
| `.gitignore` | `/target`, `/data`, `.env`, `*.db*`, `/backups`, `node_modules`, `/.claude`, `/tests/dom/screenshots/` |
| `.dockerignore` | `/target /data /backups .env .git **/node_modules rust-toolchain.toml` |
| `.claude/settings.local.json` | local Claude Code settings, git-ignored (359 bytes; not read) |
| `data/` | local dev DB + WAL, `backups/riot-proxy-2026-10-03.db`, `ddragon/` — git-ignored |

### 2.2 Docker

| File | Purpose | Evidence |
|---|---|---|
| `Dockerfile` | design/07 §Option A: `rust:1.98.1-alpine` builder, dependency layer cached by building a stub `main.rs`, `ARG GIT_SHA` (no `.git` in context), `FROM scratch`, `VOLUME /data`, `EXPOSE 8080`, `ENV DATA_DIR=/data`, `HEALTHCHECK … /riot-proxy healthcheck`, `ENTRYPOINT ["/riot-proxy"] CMD ["serve"]` (`:1–26`) |
| `Dockerfile.release` | the release/edge image (ADR-063): `FROM scratch`, copies `dist/riot-proxy-linux-${TARGETARCH}` built by the workflow; "Not for local builds" (`:1–12`) |
| `docker-compose.yml` | `ghcr.io/ninjagoldfinch/riot-proxy:2`, `build:` with `GIT_SHA` arg, `env_file: .env`, `8080:8080`, `./data:/data` (`:1–11`) |

### 2.3 Scripts (`scripts/`)

| Script | What it does | Who runs it | Evidence |
|---|---|---|---|
| `compare-openapi.py` (55 lines) | compares operations ("METHOD /path") of two OpenAPI documents; exit 1 if any OLD op under `--prefix` is missing from NEW; written for the P4 exit check | by hand / `tests/openapi.rs` ("and the compare script", `tests/openapi.rs:1`); recipe in `PROGRESS.md:388` (`cargo run -- spec > /tmp/spec.json && scripts/compare-openapi.py docs/contract/v1-openapi.json /tmp/spec.json --prefix /v1/riot/ --prefix /v1/lol/`) | `:1–11` |
| `perf-synthetic.py` (253 lines) | builds a synthetic analytics SQLite from `src/db/migrations` up to V0024, 375,608 `champion_stats` rows, times PERF-01/02 reads cold/warm with and without ANALYZE; `PERF_SQLITE=pysqlite3` option; "Not run by CI; run by hand to re-measure" (ADR-166/167/168) | by hand | `:1–25` |
| `image-description.sh` (48 lines) | prints the ≤512-char GHCR description: title + commits since the previous image (OPS-05, ADR-107) | `edge.yml:83`, `release.yml:114` | `:1–17` |
| `check-image-description.sh` (31 lines) | fails unless an image index (registry ref or OCI tarball) carries the `org.opencontainers.image.description` annotation | `edge.yml:124`, `release.yml:156` | `:1–30` |
| `edge-tags.sh` (41 lines) | picks `sha-<short>` always and `edge` only while the commit is still head of `main` (INC-02, ADR-114); fetches `origin/main` first; fails if it can't | `edge.yml:64` | `:1–28` |
| `docker-paths.sh` (38 lines) | reads changed paths on stdin, prints `build=true|false` for the `docker` job (CI-01, ADR-173) | `ci.yml:175` | `:1–27` |
| `*.test.sh` (`docker-paths`, `edge-tags`, `image-description`) | bash self-tests with shellcheck, throwaway git repos / OCI tarballs | CI `ops` job; locally `scripts/<name>.test.sh` | each file's `:1–5` |

---

## 3. Deploy kit

### 3.1 `deploy/` (production, Onidel `ngf-prod-syd1`, run by the owner)

`deploy/README.md` (270 lines): index table (`:3–7`); "Everything below is for production … and is run by the owner. The design is docs/design/12 (§Replica and backups); this is the how" (`:9–10`). Sections: Archive bucket and replica (`:12–100`: Onidel `riot-proxy-archive` + R2 replica, lifecycle rules table `:31–36`, `rclone.conf` in `/etc/riot-proxy/` 0600 with `no_check_bucket`/`no_head` `:50–85`, the hourly unit install `:87–100`); Index snapshots and recovery (`:102–145`: `packs restore-index` / `reindex [--since]` / `snapshot`, the `rp()` systemd-run helper, which to use when); Restore drill (quarterly) (`:146–210`); Reclaiming the file after the offload (`:212–270`: three gated steps in tmux, "never rm a -wal", the 2026-10-14 incident recounted `:262–267`).

- `riot-proxy-replica.service` (33 lines): `Type=oneshot`, `rclone sync onidel:riot-proxy-archive r2:riot-proxy-archive --metadata --transfers 16`, `LoadCredential=rclone.conf:/etc/riot-proxy/rclone.conf`, `DynamicUser=yes`, hardened (`:5–33`).
- `riot-proxy-replica.timer`: `OnCalendar=*-*-* *:17:00`, `RandomizedDelaySec=2min`, `Persistent=true` (`:34–44` of the cat; file lines 1–11).
- `test.sh` (85 lines): shellcheck, `systemd-analyze verify`, greps the unit contract, then runs the unit's own rclone command between two temp directories (dry run changes nothing, real run mirrors with deletes) — CI `ops` job (`:1–5`).

### 3.2 `deploy/proxmox/` (dev VM that follows `:edge`, OPS-01/02/03, INC-03)

- `README.md` (134 lines): Debian 13 VM on Proxmox VE 8+, Docker, timer pulls `:edge` every 2 min (`:1–11`); Create the VM (`:13–53`), Updating (`:55–92`; rollback behaviour, `.rejected`, `RIOT_PROXY_HEALTH_TIMEOUT`, `RIOT_PROXY_TAG` pin), Files table (`:94–104`), Updating an existing VM via `qm guest exec` (`:106–118`), Rebuild (`:120–128`), Tests (`:130–134`).
- `create-vm.sh` (253 lines). Options (`:33–57`): `--ssh-keys FILE | --generate-key` (required, one or both), `--key-dir` (default `/root/.ssh/riot-proxy`), `--image-dir` (`/var/tmp/riot-proxy-images`), `--vmid` (next free id), `--name` (`riot-proxy-dev`), `--storage` (`local-lvm`), `--snippets` (`local`), `--bridge` (`vmbr0`), `--cores` 2, `--memory` 2048, `--disk` 16G, `--ip CIDR|dhcp`, `--gw`, `--user` (`riot`), `--no-start`, `--dry-run`, `--print-vendor-data`. Flow: validate names (`:87–88`), build cloud-init vendor-data embedding `files/*` as base64 (`:93–125`), download/refresh the Debian 13 genericcloud qcow2 and verify against `SHA512SUMS` (`:168–184`), optional ed25519 keypair per VM after the download (`:189–204`), `qm create/set/resize/start` (`:206–220`), print login + next steps (`:232–253`).
- `files/compose.yaml` (11 lines): `image: ${RIOT_PROXY_IMAGE:-ghcr.io/ninjagoldfinch/riot-proxy:${RIOT_PROXY_TAG:-edge}}` (`:7`).
- `files/env.example` (6 lines): `RIOT_API_KEY=`, `ENV=development`, `LOG_LEVEL=info`, `RIOT_PROXY_TAG=edge`.
- `files/riot-proxy-update` (97 lines): tags the running image `riot-proxy:previous`, `compose pull`, skips `.rejected`, `compose up`, waits for `healthy` (timeout `RIOT_PROXY_HEALTH_TIMEOUT` 120 s, poll 2 s), rolls back and records `.rejected` otherwise (INC-03, ADR-115) (`:1–97`); logs at journal priority `<3>` when stderr is the journal (`:16–19`).
- `files/riot-proxy-update.service` / `.timer`: oneshot after `docker.service`; `OnBootSec=30s`, `OnUnitActiveSec=2min`.
- `test.sh` (222 lines): shellcheck, vendor-data byte-for-byte check in Python/yaml, `cloud-init schema`, dry-run `qm` assertions, generated-key cases, a "real run" against fake `qm/pvesh/pvesm/curl`, and the updater against a fake `docker` (healthy, rollback, rejected left alone, crash loop, unhealthy, first install), plus `docker compose config` if available (`:1–222`).

---

## 4. Ops files (`ops/`)

- `ops/prometheus-alerts.yml` (57 lines): group `riot-proxy` with `RiotKeyRejected` (401/403 from Riot, critical), `AccountableRateLimit429` (`proxy_rl_429_total{type=~"application|method"}`, warning), `CacheHitRatioLow` (< 0.7 for 15 m; NaN-safe when idle), `UpstreamErrorRateHigh` (> 10 % 5xx/network for 10 m), `ProxyDown` (`up{job="riot-proxy"} == 0` 2 m) (`:1–57`). Header cites "§13" of the (v1-era) spec and "Lua accounting" (`:1–2`, `:18–19`) — v1 wording survives.
- `ops/grafana/riot-proxy-dashboard.json` (179 lines): dashboard "riot-proxy", uid `riot-proxy`, 12 panels: Cache hit ratio (5m), Accountable 429s (5m), Upstream auth failures (15m), Requests by status, Requests by cache state, Upstream requests by method, Upstream latency p50/p95, 429s by type, Rate limiter wait p95 by priority (§9.3), Background jobs, Matches archived, WebSocket connections. design/13 says it "keeps importing unchanged" (`docs/design/13-admin-dashboard.md:9`).

---

## 5. Existing docs map

| File | Purpose | Keep / supersede note |
|---|---|---|
| `README.md` (167 lines) | user-facing: what it is, Install (binary / Docker / source), Dev VM on Proxmox, First run, Configuration, Operations (HTTPS, systemd, health/metrics, backups, migrating from v1), Development (`:1–167`, headings at `:7,27,60,90,114–144,154`) | keep as the front door; the Proxmox section duplicates `deploy/proxmox/README.md` — consider linking instead |
| `CLAUDE.md` (2.8 KB) | the agent's non-negotiables, layout, commands, conventions, phase gates | keep; it is the short form of IMPLEMENTATION.md §0 |
| `docs/IMPLEMENTATION.md` (745 lines) | the plan: §0 working rules (`:9–113`: rules never relaxed, task lifecycle, branch/commit conventions, PR template, testing strategy, DoD), §1 bootstrap (`:115–156`), §2 phases P0–P8 then post-release series SCH, THR, LAD, SITE, BLD, MU, INC, TL, FLT, PAIR, DSK, HOST, ME, ADM, OBJ, PERF, CI (`:157–583`), §3 parallelism (`:584–593`), §4 bootstrap file contents (`:595–729`), §5 kick-off prompt, §6 owner checklist (`:738–745`). Task rows carry Files / Tests / Accept / Design columns (`:159–166`) | keep as the task authority; §1 and §4 are historical (bootstrap already done) — mark as such |
| `PROGRESS.md` (423 lines) | tick list per phase/series with PR numbers and ADRs; exit-check transcripts pasted under each phase; `## Notes for the next task` (`:384–423`) is a long accreted API cheat-sheet | keep; see inconsistencies in §8 TODO |
| `docs/DECISIONS.md` (1912 lines) | ADR-001 … ADR-175, one `## ADR-nnn — title (date)` each | keep; the only ADR store (`docs/adr/` is empty) |
| `docs/adr/` | **empty directory** | remove or populate; nothing references it (TODO) |
| `docs/CUTOVER.md` (189 lines) | v1 → v2 runbook (ADR-064): 0 prerequisites, 1 deploy v2 beside v1, 2 import v1's archive (`migrate-v1`), 3 verify, 4 move consumers one at a time, 5 watch v1 reach zero, 6 switch v1 off (keep 14 days), 7 decommission; Rollback; Sign-off table (`:1–189`) | historical: owner retired v1 2026-10-04 (memory, ADR-065 "The owner no longer uses v1"); sign-off never filled (`:180–189`); `IMPLEMENTATION.md:745` still lists the sign-off as open |
| `docs/contract/v1-openapi.json` (361 KB), `v1-endpoints.txt` (16 method ids @ c86e631) | v1 parity artefacts used by `scripts/compare-openapi.py` and `endpoints::parity` (`docs/DECISIONS.md:102`) | keep while the parity tests exist; parity itself was dropped by the owner 2026-10-04 (ADR-065/068/081/123…) |
| `docs/design/README.md` (62 lines) | the redesign's cover: one-sentence thesis (`:12–16`), headline v1→v2 table (`:18–32`), document map 01–13 (`:41–58`) | keep; status still says "Proposal / design draft" (`:5`) — stale |
| `docs/design/01-current-state.md` … `09-resources.md` | the original v2 design (dated 2026-09-16); 03/04/05/06/07/09 updated through Oct (file mtimes) | keep; the "authority" per CLAUDE.md |
| `docs/design/10-dev-explorer.md` (164 lines) | `/dev`: one self-contained page, never in production (`DEV_UI=true` ignored in production), forms built from the OpenAPI document, five tabs, Player/Ladder/Reset tabs, page bar, the Jobs page retired to the dashboard (`:1–164`) | keep (Accepted, ADR-071) |
| `docs/design/11-showcase.md` (131 lines) | `/dev/showcase`: an example consumer frontend on read routes only, gated like `/dev`, one search box, every card names its calls; §Builds notes for a frontend; §The coverage rule that `tests/ui.rs` enforces (`:1–131`) | keep (Accepted, ADR-080) |
| `docs/design/12-object-storage-and-hosting.md` (59 KB) | Onidel move as executed, packs in S3, offload job, replica/backups, ADM-01 runbook | keep |
| `docs/design/13-admin-dashboard.md` (464 lines) | `/dashboard` rebuilt in ADM: API-first, one embedded ≤160 KB file, only the active tab renders/polls, sampled aggregates, additive JSON, every write confirms; reach through Cloudflare Access; layout A at 1280/390 px; per-tab specs ADM-08..16; task order (`:1–464`) | keep (Accepted per `PROGRESS.md:358`; design/README still calls it "(proposal)" at `:57`) |
| `docs/design/metrics.md`, `docs/design/img/*.svg`, `docs/img/p7-06-*.png` | metric catalogue; two SVG diagrams; two v1-dashboard screenshots from P7-06 | `docs/img` PNGs show the retired v1 dashboard (ADR-161) — candidates to drop |
| `docs/flt/FLT-03-plan.md` (689), `FLT-04-plan.md` (629), `FLT-05-plan.md` (300) + PNG folders | per-task plans written against a named `origin/main` commit with preflight (migration/ADR numbers), before/after screenshots at 390/820/1280 px, read-path/rebuild/schema diagrams | historical working docs; `PROGRESS.md:309–311` links them |
| `docs/perf/PERF-03-design.md` (404) + `PERF-03/*.mmd,*.svg` | design "written from the merged code" because the Phase A doc was never saved (`:5`); Mermaid sources rendered with mermaid-cli (`:7`) | historical |
| `deploy/README.md`, `deploy/proxmox/README.md` | see §3 | keep |
| `reports/` (untracked, 3.4 M) | `Backup plan for Onidel hosting.md` (2026-10-13), `FLT-03 handoff.md`, `FLT-03 plan.md` (686 lines; the committed `docs/flt/FLT-03-plan.md` is its 689-line successor), `FLT-04 handoff.md`, `OBJ implementation handoff.md`, `PERF-02 handoff.md`, `PERF-03 design.md` (the one the committed copy says "the handoff calls it"), `PERF-03 handoff.md`, `R2 alternatives by cost region reliability.md`; folders `FLT-03/` (screenshots incl. `observed/`), `PERF-03/` (mmd+svg) | session handoffs; not in git; duplicates of `docs/flt`/`docs/perf` content |
| `research_notes/R2 alternatives by cost region reliability/` (untracked, 232 K) | `backup_use_case.md`, `budget_s3_providers.md`, `r2_and_hyperscalers.md`, `reliability_and_regions.md`, `video_and_media_use_case.md` (all "as of 2026-10-13") | research inputs to the backup plan report; not in git |

---

## 6. ADR index (complete, from `docs/DECISIONS.md`)

Line = heading line in `docs/DECISIONS.md`. **Bold** = notable design decision for the documentation set; `(S)` = superseded.

| ADR | Line | Title (date) |
|---|---|---|
| **001** | 3 | **Single process, single binary** (2026-09-16) — "A Riot key's rate limit bounds throughput; multiple processes only add coordination" |
| **002** | 6 | **SQLite (WAL) as the only default store** (2026-09-16) — Postgres behind a feature flag for > ~50 GB |
| **003** | 9 | **Rust / axum / tokio** (2026-09-16) — Go the runner-up |
| **004** | 12 | **rusqlite (bundled) over sqlx** (2026-09-23) |
| **005** | 15 | **refinery for migrations; rusqlite held at 0.39** (2026-09-23) |
| **006** | 19 | **CI required checks and toolchain pin** (2026-09-23) — checks are job names; 1.98.1; repo public |
| 007 | 26 | reqwest 0.13 TLS features; embedded roots via `tls_certs_only` |
| 008 | 30 | Config layering and interpretation choices |
| 009 | 41 | Request ids, log shape, metrics exposition |
| 010 | 49 | SQLite layer shape |
| 011 | 57 | Error envelope, health bodies, HTTP layers |
| **012** | 68 | **CLI and consumer keys** (2026-09-23) |
| 013 | 79 | Dev tooling and container |
| 014 | 87 | Owner decisions at the P0 gate (CORS deferred, MIT, metric names) |
| 015 | 94 | Routing |
| 016 | 100 | Endpoint registry (v1's `endpoints.ts` is the source of truth) |
| 017 | 109 | Riot HTTP client |
| 018 | 117 | Rate-limit header parsing |
| 019 | 123 | Dev CLI |
| 020 | 129 | `.env` parsing follows node dotenv, not `dotenvy` |
| 021 | 132 | Service-429 backoff uses v1's numbers |
| 022 | 135 | Negative-cache hits are `X-Cache: HIT-NEG` |
| **023** | 138 | **Limiter windows are sliding logs, as in v1** (2026-09-25) |
| 024 | 141 | Observe and freeze details |
| 025 | 148 | Priorities |
| 026 | 155 | Limiter checkpoint format and restore |
| 027 | 162 | Cache key format |
| 028 | 170 | L1 cache |
| 029 | 179 | L2 write-behind |
| 030 | 186 | Single-flight |
| 031 | 194 | Fetcher |
| 032 | 204 | Replay fixtures |
| **033** | 212 | **Authentication** (consumer keys: Bearer or `?token=`, 60 s cache, scopes) |
| 034 | 222 | Consumer quota |
| 035 | 230 | OpenAPI document and docs routes |
| 036 | 237 | `/v1/riot/*` and shared route behaviour |
| 037 | 247 | `/v1/lol/*` routes |
| 038 | 256 | Dev UI and dashboard shells (v1 pages embedded; later replaced by 071/152) |
| 039 | 259 | Archive schema |
| 040 | 264 | Match archive |
| 041 | 273 | Match facts |
| 042 | 282 | `/v1/players/*` composites |
| 043 | 293 | CS and game length in the archive |
| 044 | 300 | Admin routes, data subset |
| 045 | 325 | WebSocket protocol and hub |
| 046 | 343 | Event catalogue |
| **047** | 349 | **Durable job queue** (SQLite-backed) |
| 048 | 360 | Ticks |
| 049 | 369 | Poll handlers |
| 050 | 382 | Archive and backfill jobs, and jobs in `serve` |
| 051 | 401 | `/v1/ws` |
| 052 | 410 | Admin routes for jobs |
| 053 | 420 | Data Dragon mirror |
| 054 | 449 | Ladder schema and crawl enumeration |
| 055 | 496 | Crawl collect and archive, crawl list and cancel, names backfill |
| 056 | 535 | Analytics in v1's shape, with remakes kept apart |
| 057 | 588 | Maintenance and backups |
| 058 | 608 | The dashboard's snapshot and history |
| **059** | 639 | **The acceptance suite against v2** (mock Riot default, live opt-in, required CI job) |
| 060 | 669 | `migrate-v1` |
| 061 (S) | 695 | Built-in TLS (superseded by 067) |
| **062** | 714 | **Postgres feature flag, compile-only** (`Store` trait seam) |
| **063** | 731 | **Release pipeline** (`v*` tag → 3 binaries + GHCR + GitHub release) |
| 064 | 750 | Cut-over runbook |
| **065** | 760 | **No default platform** — "The owner no longer uses v1, so v1 parity does not bind this change" (RC-01; the point where parity was dropped) |
| 066 | 773 | account-v1 picks its cluster |
| **067** | 792 | **Built-in TLS removed** (RC-03) — "The proxy is never the public edge"; `TLS=true` refuses to boot |
| 068 | 800 | `TRUST_PROXY`, off by default (break from v1) |
| **069** | 807 | **`:edge` image on every push to `main`** |
| **070** | 814 | **Proxmox dev VM that follows `:edge`** |
| **071** | 822 | **Dev explorer replaces v1's dev UI; never in production** |
| 072 | 849 | Per-VM login keys for the Proxmox dev VM |
| 073 | 859 | Dev explorer player tab |
| 074 | 866 | Per-player archive endpoints; the dev page reads exact counts (jsdom tests in CI) |
| 075 | 880 | Scoreboard layout in `/dev` |
| 076 | 886 | Data Dragon images are mirrored on first request |
| 077 | 899 | Dev reset |
| 078 | 908 | One page bar across `/dashboard` and `/dev` |
| **079** | 916 | **Faster CI: `build-musl` packages its own binary, the source `Dockerfile` builds in its own cached job** |
| **080** | 925 | **`/dev/showcase`: an example frontend that must cover every read route** |
| 081 | 935 | A lookup backfill walks the whole history by default |
| 082 | 943 | Showcase player view |
| 083 | 952 | Showcase match detail and champion view |
| **084** | 960 | **The showcase's layout is tested in headless Chromium** |
| **085** | 968 | **Fetched timelines are always archived** |
| 086 | 974 | Rune icons are mirrored |
| 087 | 983 | Dashboard crawl activity |
| 088 | 993 | Recompute analytics from the dashboard |
| **089** | 1001 | **Rate-limit-aware job claims: lanes, yields, turns** (SCH-01) |
| 090 | 1014 | A manual analytics recompute runs ahead of the queue |
| 091 | 1022 | Ladder tab: one card per running crawl |
| 092 | 1033 | Live job activity and the /dev/jobs page |
| 093 | 1044 | The dashboard job queue groups alike jobs |
| 094 | 1052 | Analytics over every patch, and a patch list |
| 095 | 1062 | Re-running the apex cap checks from /dev (league-exp joins the registry) |
| 096 | 1073 | The dev reset stops running jobs first |
| 097 | 1084 | Saying when an apex league is cut off at Riot's cap |
| **098** | 1094 | **Archive jobs fetch timelines by default** (`ARCHIVE_TIMELINES` defaults to `true`) |
| 099 | 1102 | The showcase and explorer revalidate every call |
| 100 | 1110 | A champion's own patch list |
| 101 | 1118 | The matchups rebuild reads each laned fact once |
| 102 | 1126 | The writer keeps the planner's statistics fresh |
| 103 | 1138 | Report when a part was last read from Riot (`X-Cache-Fetched-Age`, a v2 header) |
| 104 | 1147 | The match page's champion filter reads the archive |
| 105 | 1156 | Analytics count every archived game |
| 106 | 1169 | The match summary carries the role quest slot |
| **107** | 1176 | **Each image says what changed** (GHCR descriptions) |
| 108 | 1185 | A match summary names its Data Dragon version |
| 109 | 1192 | Responses declare their headers; images documented |
| 110 | 1199 | Partial schemas for Riot's payloads |
| 111 | 1207 | Look up the ranks of archived players off the ladder |
| 112 | 1219 | The showcase closes up empty item slots |
| 113 | 1226 | A cancelled read keeps its reader connection (INC-01) |
| **114** | 1234 | **`:edge` only moves forwards** (INC-02) |
| **115** | 1244 | **The dev VM's updater rolls back an unhealthy image** (INC-03) |
| 116 | 1257 | Trinket and boots in fixed cells |
| 117 | 1265 | Per-player build facts from the archived timelines |
| **118** | 1274 | **A match and its timeline are archived together** (TL-01) |
| 119 | 1284 | The image routes are declared, and any listed patch is served |
| 120 | 1294 | Set builds aggregated by first two finished items |
| 121 | 1304 | Set builds served per role |
| 122 | 1314 | One player's build is worked out in the browser |
| 123 | 1324 | Without a tier, a champion is one row over every tier (**breaking**; parity dropped 2026-10-04, `:1331`) |
| 124 | 1335 | A player's platform comes from their account |
| 125 | 1345 | The showcase finds a player's platform and searches champions |
| **126** | 1352 | **Bulk may use 95 % of a bucket while nobody is using the app** (THR-06a) |
| 127 | 1362 | A participant's tier is stamped when their match is archived |
| 128 | 1380 | The API serves every row; a games floor is the caller's |
| 129 | 1391 | Per-participant analytics rows carry tier and side |
| 130 | 1404 | Every analytics route filters by tier and side |
| 131 | 1416 | Viego's possession isn't counted as his skill points |
| 132 | 1426 | Udyr's skill order ranks R with Q, W and E |
| **133** | 1443 | **A bounded, lowest-priority backfill for missing timelines** (TL-02) |
| 134 | 1453 | A cap per job kind; analytics rebuilds one at a time |
| 135 | 1459 | Disk usage is measured and exported |
| 136 | 1466 | Backups are configurable and never fill the disk |
| 137 | 1474 | Pending jobs can be purged in bulk; no in-memory mirror |
| 138 | 1483 | A participant's tier lives on their fact row |
| 139 | 1490 | Rune pages stored as a positional array |
| 140 | 1498 | Derived tables keyed by their whole row are WITHOUT ROWID |
| 141 | 1507 | `match_facts` is WITHOUT ROWID and loses `facts_champ` |
| **142** | 1516 | **Production moves from OVH to Onidel Sydney** (HOST-01; ONI-4, `ngf-prod-syd1`) |
| 143 | 1524 | `ME1_` match ids are served by europe |
| **144** | 1534 | **The admin dashboard is API-first, embedded, and reached through Cloudflare Access** (ADM-00) |
| 145 | 1544 | The dashboard's aggregates are sampled in the background |
| **146** | 1555 | **Raw bodies go to object storage in packs** (OBJ-00) |
| 147 | 1563 | Build identity and `GET /v1/admin/info` |
| 148 | 1573 | Runtime gauges in the dashboard's snapshot |
| 149 | 1583 | Recent requests and errors, kept in memory |
| 150 | 1591 | Backup status and maintenance on demand |
| 151 | 1600 | Opt-in CORS for the admin API |
| **152** | 1609 | **The new dashboard shell replaces v1's page** (v1 parity retired, `:1602`) |
| 153 | 1618 | Dashboard charts and the Traffic tab |
| 154 | 1627 | Limiter and Cache tabs; dry run for the purge |
| 155 | 1635 | The dashboard's Jobs tab folds in /dev/jobs |
| 156 | 1642 | The Ladder tab returns |
| 157 | 1650 | Archive & Storage tab |
| 158 | 1660 | Consumers & Players tab |
| 159 | 1666 | Events and About tabs |
| 160 | 1675 | Dashboard keyboard, command palette, accessibility checks |
| 161 | 1682 | The old dashboard and jobs paths are retired |
| **162** | 1696 | **The object store client and its configuration** (OBJ-01; the binary cap moved 20 MB → 24 MiB here, `Cargo.toml:74`) |
| 163 | 1709 | Pack format, body locations and reads by range |
| 164 | 1722 | The offload job |
| **165** | 1732 | **Index snapshots, reindex, restore and the bucket replica** (OBJ-04) |
| **166** | 1751 | **Analytics reads: seek, sum over tier once, cache per generation** (PERF-00; the 3.9 s `/champions` story) |
| 167 | 1759 | Analytics reads seek on their keys |
| 168 | 1780 | Tierless stats reads sum a tier-summed table |
| 169 | 1819 | Analytics responses cached per aggregate generation |
| 170 | 1858 | Tier ranges count each match once per floor (FLT-03) |
| 171 | 1876 | Set builds against a lane opponent (FLT-04) |
| 172 | 1888 | The *vs* picker on the champion page (FLT-05) |
| **173** | 1888 | **The source `Dockerfile` builds on `main`, and on PRs that can change it** (CI-01) |
| **174** | 1896 | **Tests run in parallel under nextest; coverage moves to its own job** (CI-02) |
| **175** | 1906 | **The DOM and browser suites run in their own `ui` job** (CI-03) |

(The three CI entries are dated 2026-10-19.) No ADR covers the binary-size cap as its own decision: it appears inside ADR-006 (20 MB) and ADR-162 (24 MiB) per `Cargo.toml:74` and `ci.yml:127–132`. "v1 parity dropped" is likewise not an ADR of its own — it is stated in ADR-065 (`:761`) and reused in 068, 081, 094, 103, 123, 152.

---

## 7. Git history highlights

- **218 commits** on `HEAD`; authors: Samuel Goldfinch 217, Claude Dev 1 (`git shortlog -sn`). First commit `f124118 2026-09-23 chore: bootstrap riot-proxy v2`; then `P0-01 — Workspace, toolchain, CI (#1)` the same day. Squash-merge discipline: every non-bootstrap commit carries a `(#N)` PR number; latest is #223.
- **Tags** (creatordate): `phase-P0` 09-24, `phase-P1` 09-24, `phase-P2` 09-25, `phase-P3` 09-25, `phase-P5` 09-30, `phase-P6` 09-30, `phase-P7` 10-03, `v2.0.0-rc.0` 10-04, `v2.0.0-rc.1` 10-04, `phase-P8` 10-04, **`phase-P4` 10-04** (tagged out of order after the owner's browser check of `/docs`, #72), `v2.0.0-rc.2` 10-04, `v2.0.0-rc.3` 10-05, `v2.0.0` 10-10, `v2.1.0` 10-11, `v2.2.0` 10-13, `v2.3.0` 10-13, `v2.4.0` 10-14, `v2.4.1` 10-14, `v2.5.0` 10-14, `v2.6.0` 10-15, `v2.7.0` 10-19.
- **Release cadence:** P0 → P8 in 12 days (09-23 → 10-04); four RCs over two days; then a minor release roughly every 1–2 days from 10-10 to 10-15 (2.0.0, 2.1.0, 2.2.0, 2.3.0, 2.4.0, 2.4.1, 2.5.0, 2.6.0), and 2.7.0 four days later. Release commits are `chore(release): X.Y.Z (#N)`; 2.2.0 was cut from a `release/2.2.0` branch (`4538420`, `PROGRESS.md:200`).
- **Commit style drift:** P-phase and most series PRs are titled `<TASK-ID> — <title> (#N)` (e.g. `OBJ-02 — pack format… (#197)`), while others use Conventional Commits with the task in the body (e.g. `feat(analytics): tier ranges on every analytics route (FLT-03) (#217)`); the CI series uses `ci(scope): … (CI-0n) (#N)`.
- **25 most recent subjects** (newest first): CI-03 #223, CI-02 #222, CI-01 #221, release 2.7.0 #220, FLT-05 #219, FLT-04 #218, progress ticks #216, FLT-03 #217, fix(ui) dropdowns #215, release 2.6.0 #214, PERF-03 #213, PERF-02 #212, docs(deploy) OBJ-04 reclaim #211, release 2.5.0 #210, PERF-01 #208, PERF-00 #207, fix(deploy) replica metadata #209, OBJ-04 #206, release 2.4.1 #205, docs(hosting) ADM-01 #204, fix(ui) key sheet 403 #203, fix(ui) chart table #201, fix(jobs) offload trigger #202, release 2.4.0 #200, OBJ-03 #198.
- **Series in history** (commits whose subject/body mention `SERIES-nn`, first→last date): P0 7 (09-23), P1 6 (09-24), P2 7 (09-25), P3 6 (09-25), P4 7 (09-23→09-26, exit 10-04), P5 7 (09-26→10-05), P6 9 (09-30), P7 9 (09-26→10-05), P8 7 (10-03→10-05), RC 6 (10-04→10-05), OPS 10 (10-06→10-16), DEV 33 (10-06→10-11), SCH 3 (10-09), INC 4 (10-09), LAD 4 (10-09), SITE 13 (10-09→10-10), BLD 9 (10-09→10-11), TL 3 (10-10→10-11), FLT 8 (10-10→10-19), DSK 10 (10-13), HOST 2 (10-13), ADM 21 (10-13→10-14), OBJ 12 (10-13→10-15), PERF 6 (10-14→10-15), CI 3 (10-19). THR, MU, ME, PAIR exist as series ids in PROGRESS.md; THR-02/THR-06a and ME-01 landed under `feat(...)` subjects (`fc8aa1a`, `76c4e9e`, `3fe3a83`).
- **Densest day:** 2026-10-13 — DSK-01..07, HOST-01, ME-01, v2.2.0 and v2.3.0, ADM-00..17 (#164–#195), OBJ-00 (#179): about 32 PRs in one day.
- **Notable one-offs:** `04de307 RC-03 — remove built-in TLS (#74)` (two days after `3670e34 feat(tls)` #64 added it); `6abd3c6 chore: ignore .claude/ (#78)`; `0279dd3 ci(edge): :edge only moves forwards (#137)`; `41567bd HOST-01 — production moved to Onidel Sydney (#172)`.

---

## 8. TODO (unverified)

- TODO (unverified): whether `tests/it.rs` (CI-04) and `[profile.dev]` (CI-05) exist on PRs #224/#225 — memory says they do; not on `origin/main` as fetched here (HEAD `7491ff6`). The ADR numbers 176/177 are therefore also unconfirmed.
- Verified by attribute grep (not by running): 381 `#[test]`/`#[tokio::test]` fns in `tests/*.rs` (5 `#[ignore]`), 562 in `src/` (matches ADR-174's "lib's 562 tests"; 381 + 562 = 943 ≈ ADR-174's 938, the difference being proptest/ignored accounting), 97 node helper tests, 155 jsdom/browser `test()`s, 26 acceptance `it()`s (P8 exit check reported 29 — three may be generated at runtime; TODO (unverified)).
- TODO (unverified): how snapshots are updated in practice — no `cargo insta` or `INSTA_UPDATE` is documented anywhere in the repo; presumably `cargo insta review` or `INSTA_UPDATE=always` is used locally.
- Verified: `docs/adr/` is tracked and contains only `.gitkeep` (`git ls-files docs/adr`); nothing references it. Candidate for removal.
- TODO (unverified): PROGRESS.md inconsistencies seen: (a) `:206` "Release `v2.7.0`" is **unticked** although tag `v2.7.0` and `10cf81d chore(release): 2.7.0 (#220)` exist; (b) `:310` FLT-04 is **unticked** although `c1d13e0 feat(analytics): … (FLT-04) (#218)` is merged (memory: "tick PR #226" pending); (c) the OBJ-00..06 rows (`:331–337`) sit under the `## ME` heading — there is no `## OBJ` heading; (d) `:189` says the P8-06 cut-over sign-off is still an open owner item.
- TODO (unverified): `docs/design/README.md:5` status "Proposal / design draft" and `:57` "(proposal)" for design/13 are stale against `PROGRESS.md:358` (design/13 Accepted).
- `ops/prometheus-alerts.yml:1–2,18–19` still speaks of "§13", "Lua accounting" and `rl:*` keys (v1 Redis wording) — the prose is stale, but the three metric names it uses (`proxy_rl_429_total`, `proxy_cache_reads_total`, `proxy_upstream_requests_total`) do appear in `src/metrics.rs` (6 grep hits). TODO (unverified): the label values (`type`, `state`, `status="network"`).
- TODO (unverified): whether the Grafana dashboard JSON's queries match the current metric set after DSK-01/ADM-04 gauges were added.
- Verified: no `.md` file references `docs/img/p7-06-dashboard-*.png` (grep); the two PNGs show the v1 dashboard retired in ADR-161 and are orphaned.
- Verified: `README.md:116–118` §HTTPS already reflects ADR-067 ("The proxy serves plain HTTP only… `TLS=true`… refuses to start"). TODO (unverified): the rest of `README.md:60–167` (First run, Configuration, systemd, Backups, Migrating from v1) was only skimmed by heading.
- TODO (unverified): the `data/` directory (dev DB from 2026-10-04) and `tests/dom/screenshots/` (15 PNGs from 2026-10-09) are local artefacts only; confirm nothing in the docs assumes they exist.
- TODO (unverified): the owner checklist `docs/IMPLEMENTATION.md:740–745` still has three open boxes (v1 repo URL, dev key in `.env`, CI secret) that are long resolved per `PROGRESS.md:413–414`.
- TODO (unverified): during this inventory the harness temp filesystem (`/tmp/claude-1000/...`) filled to 0 MB — likely the background build linking; later Bash output could not be captured. Nothing in the repo was touched except this file.

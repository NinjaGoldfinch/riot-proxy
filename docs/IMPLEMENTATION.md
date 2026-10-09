# riot-proxy v2 — Implementation Plan

> **Audience:** Claude Code (Opus 5.5) working autonomously in a fresh repository.
> **Owner:** NinjaGoldfinch.
> **Design authority:** `docs/design/*.md` (copied from the v2 redesign docs). Where this plan and the design docs disagree, the design docs win. Where the design docs and the v1 repo's *observed behaviour* disagree, v1 wins — ask before choosing.

---

## 0. How to work in this repo

Read this section fully before starting any task. `CLAUDE.md` restates the hard rules in short form and is loaded automatically.

### 0.1 Rules that are never relaxed

1. **One task = one branch = one PR.** Never batch tasks. Never push to `main`.
2. **Every PR has tests.** A PR that adds behaviour without a test for it is not done. A PR that changes behaviour without updating the test is a bug.
3. **CI must be green** (`fmt`, `clippy -D warnings`, `test`, `musl build`) before you mark a task complete.
4. **Never invent Riot API semantics.** Header names, routing values, status codes, endpoint paths and TTL rationale come from `docs/design/`, the v1 repo, or the Riot developer portal — never from memory. If none of those answers the question, stop and ask.
5. **The v1 repo is read-only reference material.** Clone it, read it, port its *tests* and *contract artefacts*, but do not translate its source files line by line. Its structure is shaped around Redis/BullMQ; v2's is not.
6. **No secrets in the repo.** `RIOT_API_KEY` lives in `.env` (gitignored) and in CI secrets only. Fixture files must have the key redacted.
7. **Stop and ask** when: a design doc is ambiguous, a Riot behaviour is unknown, a task's acceptance criteria can't be met as written, or a dependency's API differs materially from what the plan assumes. Do not silently pick an interpretation.
8. **Record every non-trivial decision** in `docs/DECISIONS.md` (ADR format, one entry per decision, dated).
9. **Update `PROGRESS.md`** at the end of every task: tick the task, note the PR number, note anything the next task needs to know.
10. **Keep the showcase current.** A PR that adds a read route, or changes one's response shape, updates `src/ui/showcase.html` (or its `notShowcased` list, with a reason) and `docs/design/11-showcase.md`. `tests/ui.rs` enforces the first half (design/11 §The coverage rule).

### 0.2 Task lifecycle

```
pick next unchecked task in PROGRESS.md (strictly in order unless marked [parallel-ok])
  → git switch -c <branch>          (see 0.3)
  → write the failing test(s) first where the task is logic-heavy
  → implement
  → cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
  → update docs touched by the change (design doc, DECISIONS.md, PROGRESS.md)
  → commit(s) using Conventional Commits (see 0.4)
  → gh pr create --fill --base main   (title = task ID + summary; body = PR template)
  → wait for CI; fix until green
  → self-review against the checklist in .github/PULL_REQUEST_TEMPLATE.md
  → gh pr merge --squash --delete-branch
  → git switch main && git pull
```

If a task turns out to need more than ~600 lines of diff, split it: finish the first coherent slice, PR it, add the remainder as a new task in `PROGRESS.md` with the next free ID in that phase.

### 0.3 Branch naming

`<type>/<task-id>-<short-slug>` where type ∈ `feat | fix | test | chore | docs | ci`, e.g. `feat/P2-03-limiter-observe`.

### 0.4 Commit messages

Conventional Commits, scoped to the module:

```
feat(limiter): observe X-App-Rate-Limit headers and reconfigure windows
test(limiter): port v1 multi-window rollback cases
chore(ci): add musl build job
docs(design): record ADR-004 rusqlite over sqlx
```

Body explains *why* when it isn't obvious. Reference the task ID in the footer: `Task: P2-03`.

### 0.5 PR template (`.github/PULL_REQUEST_TEMPLATE.md`)

```markdown
## Task
P?-?? — <title from PROGRESS.md>

## What changed
-

## How it was tested
- [ ] unit tests added/updated: `<paths>`
- [ ] integration tests (wiremock) added/updated: `<paths>`
- [ ] ran `cargo test` locally — all green
- [ ] manual check (if applicable): `<what and result>`

## Design conformance
- [ ] matches `docs/design/<doc>#<section>`; deviations recorded in `docs/DECISIONS.md` as ADR-___
- [ ] no Riot semantics invented; sources: ___

## Checklist
- [ ] `cargo fmt` / `clippy -D warnings` clean
- [ ] no secrets, no un-redacted fixtures
- [ ] `PROGRESS.md` updated
- [ ] public API changes reflected in `/openapi.json` snapshot test
```

### 0.6 Testing strategy (applies to every phase)

| Layer | Tool | Where | Rule |
|---|---|---|---|
| Unit | `cargo test`, `tokio::test`, `tokio::time::pause()` | `src/<module>/tests.rs` or `#[cfg(test)] mod tests` | Every pure-logic module (limiter, cache keys, routing, priority, jobs claim, facts extraction) has table-driven unit tests |
| Integration | `wiremock` mock of Riot + real SQLite in `tempfile` | `tests/*.rs` | Exercises the full fetcher path and HTTP routes against a mocked upstream. No network. |
| Snapshot | `insta` | `tests/snapshots/` | `/openapi.json`, error bodies, WS frames. Review snapshot diffs deliberately; never `--accept` blindly |
| Replay | recorded Riot responses (key redacted) served by wiremock | `tests/fixtures/replay/` | Asserts exact sequence of limiter acquires + `X-Cache` states for a scripted scenario (P3-06) |
| Acceptance | v1's black-box suite, ported (TypeScript, vitest — it doesn't matter what language a black box is in) | `acceptance/` | Runs against a live `riot-proxy serve` with `AUTH_DISABLED=true` in dev and a mock upstream. Gate for P8 |
| Property | `proptest` (limiter only) | `src/riot/limiter/proptest.rs` | Never over-commits a window under random acquire/observe interleavings |

Coverage is measured with `cargo llvm-cov` in CI and reported, not gated — but any module under 70% needs a written justification in the PR.

### 0.7 Definition of Done — task

- Acceptance criteria in the task entry are all met.
- Tests from 0.6 exist for the layers the task names.
- CI green; PR merged; `PROGRESS.md` ticked with PR number.

### 0.8 Definition of Done — phase

- Every task in the phase merged.
- The phase's **Exit check** (below) passes and its result is pasted into `PROGRESS.md`.
- A git tag `phase-P<n>` is pushed.

---

## 1. Bootstrap (do this once, before P0)

These steps create the repository. They are a task like any other (`P0-00`) but have no PR because there is no repo yet.

```bash
# 1. New repo. The old one has been renamed; do not reuse it.
gh repo create NinjaGoldfinch/riot-proxy --private --clone \
  --description "Lightweight single-binary proxy for the Riot Games API (v2, Rust)"
cd riot-proxy
# If the name is taken, STOP and ask — do not pick another name.

# 2. Reference clone of v1, outside the new repo, read-only.
git clone https://github.com/NinjaGoldfinch/ninja-recorder-deprecated ../riot-proxy-v1
#   ^ path given by the owner. If this repo does not contain the v1 riot-proxy
#     (Fastify/BullMQ/Drizzle, ~16k lines TS), STOP and ask for the right URL.

# 3. Scaffold
cargo init --name riot-proxy
mkdir -p docs/design docs/adr .github/workflows tests/fixtures acceptance
cp <the ten v2 design docs + img/> docs/design/          # provided by owner
cp ../riot-proxy-v1/openapi.json docs/contract/v1-openapi.json
cp -r ../riot-proxy-v1/acceptance acceptance/            # port later (P8-01)
cp ../riot-proxy-v1/ops/grafana/*.json ops/grafana/
cp ../riot-proxy-v1/ops/prometheus-alerts.yml ops/

# 4. Initial commit on main, then protect it
git add -A && git commit -m "chore: bootstrap riot-proxy v2"
git push -u origin main
gh api -X PUT repos/NinjaGoldfinch/riot-proxy/branches/main/protection \
  -f required_status_checks[strict]=true \
  -f 'required_status_checks[contexts][]=ci' \
  -f enforce_admins=false \
  -f required_pull_request_reviews=null \
  -f restrictions=null
```

Files created in bootstrap (contents in §4):

- `CLAUDE.md`, `PROGRESS.md`, `docs/DECISIONS.md` (with ADR-001..003 pre-filled), `.github/PULL_REQUEST_TEMPLATE.md`, `.github/workflows/ci.yml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `.gitignore`, `.env.example`, `LICENSE` (same as v1).

---

## 2. Phases and tasks

Conventions in task entries:

- **Files** — primary files touched (guidance, not a contract).
- **Tests** — the minimum tests that must exist when the PR merges.
- **Accept** — objective acceptance criteria.
- **Design** — the section of `docs/design/` that governs the task.
- `[parallel-ok]` — may be done out of order relative to its neighbours.

Estimated diff sizes are for splitting decisions only.

### Phase P0 — Foundations

Exit check: `cargo run -- serve` on an empty `data/` dir boots, migrates, prints a bootstrap admin key, serves `/healthz` 200 and `/metrics`; `cargo run -- key create --name test` prints an `rpx_` key; CI runs all four jobs green; `ls -la target/x86_64-unknown-linux-musl/release/riot-proxy` is a static binary under 20 MB.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P0-01 | **Workspace, toolchain, CI.** `Cargo.toml` with deps pinned to current minor versions (`axum`, `tokio`, `tower-http`, `reqwest`, `rusqlite` bundled, `refinery`, `moka`, `dashmap`, `serde`, `serde_json`, `zstd`, `metrics`, `metrics-exporter-prometheus`, `tracing`, `tracing-subscriber`, `clap`, `figment`, `dotenvy`, `sha2`, `hex`, `ulid`, `jiff`, `thiserror`, `anyhow`; dev: `wiremock`, `insta`, `proptest`, `tempfile`, `tokio-test`). `ci.yml` jobs: `fmt`, `clippy`, `test` (with llvm-cov summary), `build-musl`. | `Cargo.toml`, `.github/workflows/ci.yml`, `rust-toolchain.toml` | — | CI green on an empty `fn main`. Record ADR-004 (rusqlite vs sqlx) and ADR-005 (refinery for migrations). | 02, 09 |
| P0-02 | **Config.** Typed `Config` from env + `.env` + CLI flags via `figment`. All v1 variable names that survive (design 07 table). `ENV=production` + `AUTH_DISABLED=true` → refuse to boot with a clear error. `DATA_DIR` default `./data`, created if missing. | `src/config.rs`, `.env.example` | unit: precedence (flag > env > .env), production refusal, defaults | `cargo test config` green; `.env.example` lists every variable with its default and a one-line comment | 07 §Configuration |
| P0-03 | **Logging + metrics + request ids.** `tracing` JSON (pretty on tty), `metrics` recorder with Prometheus exporter on `/metrics`, `ulid` request id middleware, `X-Request-Id` echoed. Metric names copied from v1 `src/metrics.ts` — write them into `docs/design/metrics.md` as you go. | `src/telemetry.rs`, `src/http/request_id.rs`, `docs/design/metrics.md` | integration: `/metrics` returns text/plain with at least one registered counter | Every v1 metric name appears in `metrics.md` with its type and labels | 07 §Observability |
| P0-04 | **SQLite layer.** Open with pragmas from design 04; single writer thread with `mpsc<Box<dyn FnOnce(&Connection)+Send>>`; reader pool; `Db::write(|c| …)` and `Db::read(|c| …)` async helpers; `refinery` embedded migrations; `0001_init.sql` with **only** `consumers`, `limiter_state`, `cache`, `jobs`, `metrics_history` (archive tables come in P5). | `src/db/mod.rs`, `src/db/migrations/0001_init.sql` | unit: pragma values after open; writer serialises concurrent writes; migration applies twice idempotently (tempfile) | `PRAGMA journal_mode` returns `wal`; 100 concurrent `write()` calls complete with no `SQLITE_BUSY` | 04 §SQLite configuration, §Schema |
| P0-05 | **HTTP skeleton + health.** `axum` router, `tower-http` trace/compression/cors, graceful shutdown on SIGTERM/SIGINT with drain, `/healthz`, `/readyz` (db writable), `ApiError` enum → JSON body `{error:{code,message,requestId}}` with v1's code strings (copy from v1 `src/http/errors.ts`). | `src/app.rs`, `src/http/error.rs`, `src/routes/health.rs`, `src/main.rs` | integration: `/healthz` 200; unknown route → 404 body matches snapshot; SIGTERM completes in-flight request | `insta` snapshot of the 404 body committed | 03 §Module layout |
| P0-06 | **CLI.** `clap` subcommands: `serve`, `migrate`, `key create --name --scopes --quota`, `key revoke`, `key list`, `healthcheck` (GET /healthz, exit code), `spec` (print OpenAPI — stub until P4). Bootstrap admin key printed once on first `serve` when `consumers` is empty. | `src/main.rs`, `src/cli/*.rs` | integration: `key create` inserts a row with sha256 hash; plaintext never persisted (grep the db file) | All subcommands have `--help` text; bootstrap key printed exactly once | 07 §First run |
| P0-07 | **Dev tooling.** `justfile` (or `Makefile`) with `dev`, `test`, `lint`, `cov`, `musl`, `docker`; `Dockerfile` from design 07 (`FROM scratch`); `docker-compose.yml` (5 lines). | `justfile`, `Dockerfile`, `docker-compose.yml`, `.dockerignore` | CI `build-musl` job also builds the image and runs `healthcheck` in it | `docker compose up` → `/healthz` 200 | 07 §Option A |

### Phase P1 — Riot client

Exit check: `cargo run -- riot get account/by-riot-id europe Faker KR1` (dev-only subcommand) prints Riot's raw JSON with a real key from `.env`; all nine endpoint groups resolve to the correct host in unit tests.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P1-01 | **Routing.** `Platform` and `Region` enums, platform→region map, `sea→asia` rule for account-v1, host formatting. Copy the table from v1 `src/riot/routing.ts` and verify against the Riot portal. | `src/riot/routing.rs` | unit: every platform maps; sea special-case; unknown platform → `ApiError::BadRequest` | Table-driven test covers all platforms listed in v1 | 03, v1 spec §5 |
| P1-02 | **Endpoint registry.** `Endpoint` struct: `id`, `path_template`, `scope` (platform/region), `method_scope_key`, `soft_ttl`, `hard_ttl`, `persist_l2`, `immutable`, `negative_ttl`. Populate all nine groups from v1 `src/riot/endpoints.ts`. TTLs from design 04 table. `CACHE_TTL_OVERRIDES` parsing. | `src/riot/endpoints.rs` | unit: every endpoint id from v1 present (assert against a checked-in list); override parsing | `cargo test endpoints::parity` compares ids against `docs/contract/v1-endpoints.txt` | 04 §TTLs |
| P1-03 | **HTTP client.** `reqwest` with pool, timeouts, `User-Agent`, `X-Riot-Token` injection, gzip. `RiotResponse { status, headers, body: Bytes }`. Error policy: 401/403 → `UpstreamAuth` (log at error), 404 → `NotFound`, 5xx → `UpstreamUnavailable`, network → `UpstreamUnavailable`, 429 → `RateLimited{typed, retry_after}`. No retries here (limiter/fetcher own that). | `src/riot/client.rs` | integration (wiremock): each status maps to the right error; token header present; body passed through byte-identical | Snapshot of error bodies | 03, v1 spec §9.4 |
| P1-04 | **Rate-limit header parsing.** `parse_limits("20:1,100:120")`, `parse_counts`, `RateLimitType`. Tolerant of missing/malformed headers (return `None`, log at warn). | `src/riot/limiter/headers.rs` | unit: valid, empty, malformed, out-of-order windows | — | 05 §Observe |
| P1-05 | **Dev subcommand.** `riot get <endpoint-id> <platform|region> <params…>` (only compiled with `--features dev-cli`). | `src/cli/riot.rs` | — | Works against real API with `.env` | — |

### Phase P2 — Rate limiter

Exit check: `cargo test limiter` green including proptest; `docs/design/05-rate-limiter.md` updated with any deviation; a 60 s soak test (`tests/limiter_soak.rs`, `#[ignore]`) never exceeds a configured window under 50 concurrent tasks.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P2-01 | **Port v1 limiter tests first.** Read `../riot-proxy-v1/test/limiter*.test.ts`; translate each case into a Rust test against a not-yet-existing `Limiter` API (compile with `todo!()` bodies). This PR is tests + stubs only. | `src/riot/limiter/tests.rs`, `src/riot/limiter/mod.rs` (stubs) | the ported suite (all `#[ignore]` until P2-04) | Every v1 test name appears as a Rust test with a comment citing the original | 05 |
| P2-02 | **Windows and scopes.** `Window { limit, seconds, count, reset_at }` with `try_take`, `rollback`, `expired`; `ScopeState { windows, frozen_until }`; `Scope` key type. | `src/riot/limiter/bucket.rs` | unit: take/rollback; reset on expiry; reconfigure keeps counts for matching `seconds` | — | 05 §Scopes |
| P2-03 | **Acquire (single priority).** `acquire(app, method, budget)` per the flowchart, mutex held only for check-and-take, all-or-nothing across app ∪ method windows, sleep-until-reset within budget, `RateLimited{retry_at}` beyond budget. | `src/riot/limiter/mod.rs` | unit under `tokio::time::pause()`: rollback on partial; budget exceeded; wait-then-succeed | Ported v1 acquire tests un-ignored and green | 05 §Acquire |
| P2-04 | **Observe + freeze.** Reconfigure on limit headers; `count = max(count, riot_count)`; typed 429 → freeze scope until `Retry-After` and log error; untyped 429 → no bucket change (caller backs off). | `src/riot/limiter/mod.rs` | unit: sync never lowers; reconfigure; freeze blocks acquire; remaining ported tests green | All of P2-01's tests un-ignored | 05 §Observe |
| P2-05 | **Priorities.** `Priority::{Interactive,Bulk}`; interactive waiters woken first; bulk parked when interactive waiting or any window ≥ `BULK_USAGE_CEILING`; gauges `limiter_bulk_waiters`, `limiter_interactive_waiters`. | `src/riot/limiter/priority.rs` | unit: bulk starves while interactive present; bulk resumes when ceiling clears | — | 05 §Priorities |
| P2-06 | **Checkpoint/restore.** `checkpoint()` → rows; `restore(rows)` with the conservative rule (stale > 120 s ⇒ windows full until reset); background loop every 10 s + on shutdown. | `src/riot/limiter/persist.rs` | unit: round-trip; stale checkpoint is conservative; integration: restart a `Db` and restore | Kill-and-restart test never over-commits | 05 §Persistence |
| P2-07 | **Property test + soak.** `proptest` over random acquire/observe/tick sequences: invariant `count ≤ limit` per window at all times. Ignored soak test. | `src/riot/limiter/proptest.rs`, `tests/limiter_soak.rs` | as described | 1 000 proptest cases pass in CI | 05 |

### Phase P3 — Cache, single-flight, fetcher

Exit check: `tests/fetcher_states.rs` reaches every `X-Cache` value (`HIT`, `MISS`, `STALE`, `NEG`, `ARCHIVE`, `BYPASS`) against wiremock; replay scenario in P3-06 passes.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P3-01 | **Cache keys + key_scope.** `key_scope()` from `RIOT_API_KEY`; canonical key builder; query-hash rule. Copy the algorithm from v1 `src/cache/keys.ts` and its tests. | `src/cache/keys.rs` | unit: ported v1 key tests; key_scope stable for same key, differs across keys | — | 04 §Canonical key, §key_scope |
| P3-02 | **L1 (moka).** `CacheEntry { status, body: Bytes, content_at, soft_expires, hard_expires }`; `get` returns `Fresh/Stale/Miss`; negative entries; weight-bounded (`CACHE_L1_MAX_MB`); `content_at` preserved when a refresh yields byte-identical body. | `src/cache/l1.rs` | unit: soft/hard transitions under paused time; byte-identical keeps `content_at`; eviction by weight | — | 04 §Cache tiers |
| P3-03 | **L2 (SQLite write-behind + warm).** `persist_l2` entries batched via mpsc (2 s / 500); boot warm; expired sweep. | `src/cache/l2.rs` | integration: put → restart `Db` → warm → `HIT` | Crash between put and flush loses ≤ 2 s (documented, not tested) | 04 §Cache tiers |
| P3-04 | **Single-flight.** `DashMap<Key, Shared<BoxFuture<Result<Arc<Entry>>>>>`; joiners share the result; errors are not cached in the map. | `src/singleflight.rs` | unit: 100 concurrent misses → 1 upstream call; error propagates to all joiners | — | 03 |
| P3-05 | **Fetcher.** The funnel from design 03 sequence diagram (archive step stubbed until P5): negative → L1 → SWR spawn (bulk) → single-flight → limiter → client → observe → put → return `FetchResult { body, status, x_cache, cache_age }`. Serve stale on upstream 5xx within hard TTL. `?refresh=true` → `BYPASS` (admin scope only, enforced in P4). | `src/fetcher.rs` | integration (wiremock): every `X-Cache` state; stale-on-5xx; SWR refresh happens at bulk priority | `tests/fetcher_states.rs` | 03 §Request lifecycle |
| P3-06 | **Replay harness.** Record ~200 real responses with the dev CLI (`riot record --out tests/fixtures/replay/<scenario>/`), redact the key, and a scripted scenario asserting the exact limiter/cache sequence (`insta` snapshot of the event log). Start with two scenarios: "cold summoner lookup" and "429 typed application". | `src/cli/record.rs`, `tests/replay.rs`, `tests/fixtures/replay/` | as described | Fixtures contain no `RGAPI-` string (CI grep) | 08 §Contract tests |

### Phase P4 — Public surface

Exit check: `cargo run -- spec > openapi.json` and `scripts/compare-openapi.py docs/contract/v1-openapi.json openapi.json` reports zero missing operation ids for `/v1/riot/*` and `/v1/lol/*`; `/docs` renders Scalar in a browser.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P4-01 | **Auth.** Bearer `rpx_…` → sha256 → `consumers` (moka-cached 60 s); scopes `read`/`admin`; `AUTH_DISABLED` dev bypass; `ADMIN_IP_ALLOWLIST`; 401/403 bodies from v1. | `src/http/auth.rs` | integration: valid/invalid/revoked key; scope enforcement; allowlist | Snapshot of 401/403 bodies | 03 |
| P4-02 | **Consumer quota.** Sliding window per consumer (`quota_per_min`), `X-RateLimit-Limit/Remaining/Reset` headers, `QUOTA_EXCEEDED` 429 (distinct from upstream `RATE_LIMITED`). | `src/http/quota.rs` | unit: window math; integration: 429 after N | Header names byte-identical to v1 | 03 |
| P4-03 | **OpenAPI scaffolding.** `utoipa` + `utoipa-axum`; `/openapi.json`; `/docs` via `utoipa-scalar`; `spec` subcommand; `scripts/compare-openapi.py`. Snapshot test of the document. | `src/routes/docs.rs`, `scripts/compare-openapi.py` | snapshot | — | 03 |
| P4-04 | **`/v1/riot/*` passthrough.** One handler per endpoint id via a macro or registry loop; path params validated; headers `X-Cache`, `X-Cache-Age`, `X-Request-Id`, `Cache-Control`. Riot bytes forwarded untouched. | `src/routes/riot.rs` | integration: one route per group; passthrough is byte-identical to wiremock body | OpenAPI compare: `/v1/riot/*` complete | 03 |
| P4-05 | **`/v1/lol/*` typed routes.** Typed `serde` structs for the nine groups (derive from v1 TypeBox schemas), validation errors → 400 with v1 codes. | `src/routes/lol.rs`, `src/riot/types/*.rs` | integration + snapshot of one response per route | OpenAPI compare: `/v1/lol/*` complete | 03 |
| P4-06 | **Dev UI + dashboard shells.** `/dev` and `/dashboard` HTML from v1 `public/`, embedded with `include_str!`, gated by `DEV_UI`/`DASHBOARD_UI`. Dashboard data wiring lands in P7. `[parallel-ok]` | `src/routes/ui.rs`, `src/ui/*.html` | integration: 200 when enabled, 404 when disabled | — | 03 |

### Phase P5 — Archive and composites

Exit check: archive a match via `/v1/lol/match/{id}`, restart, fetch again → `X-Cache: ARCHIVE` and body byte-identical; `/v1/players/{riotId}` matches v1's response shape (snapshot diff reviewed by owner).

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P5-01 | **Archive schema.** Migration `0002_archive.sql`: `players`, `matches`, `timelines`, `match_facts`, `champion_*`, `ladder_*`, `crawl_match_ids` exactly as design 04. | `src/db/migrations/0002_archive.sql` | migration test | — | 04 §Schema |
| P5-02 | **Match archive.** `archive::matches::{get, put, filter_unarchived}`; zstd level 3; `patch`, `queue_id`, `game_end_ms` extracted on insert; `ARCHIVE_TIMELINES` flag. Wire into fetcher (`immutable` endpoints check archive first, write after miss). | `src/archive/matches.rs`, `src/fetcher.rs` | unit: round-trip byte-identical; `filter_unarchived`; integration: `X-Cache: ARCHIVE` | Compression ratio logged at debug | 04 |
| P5-03 | **Facts extraction.** `match_facts` rows from a match body; `facts_version` constant; pure function with fixture-based tests from three real matches (redacted). | `src/archive/facts.rs`, `tests/fixtures/matches/` | unit against fixtures | Handles remakes and missing positions without panicking | 04, 06 |
| P5-04 | **Players + composites.** `players` upsert on lookup; `/v1/players/{riotId}` and `/v1/players/{puuid}/*` fan-out with `join_all` over fetcher calls; partial-failure semantics copied from v1 `src/routes/players.ts`. | `src/routes/players.rs` | integration: happy path; one upstream 5xx → partial response per v1 | Snapshot reviewed by owner | 03 |
| P5-05 | **Admin routes (data).** `/v1/admin/consumers`, `/v1/admin/tracked-players` (add/remove/re-resolve by Riot ID), `/v1/admin/cache/purge`, `/v1/admin/archive/stats`. | `src/routes/admin.rs` | integration per route, admin scope enforced | OpenAPI compare: `/v1/admin/*` (data subset) complete | 03 |

### Phase P6 — Scheduler, jobs, realtime

Exit check: track a player, start `serve`, observe `game.started` on `/v1/ws` from a wiremock spectator flip; kill `serve` mid-backfill, restart, backfill resumes from the `jobs` table with no duplicate `archive:match` rows.

**Do P6-01 first, before anything else in this phase — it is the highest-risk piece of Rust in the project.**

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P6-01 | **WebSocket hub spike.** `Hub` with `broadcast::Sender` per topic; socket task `select!`ing over N receivers + inbound frames; `Lagged` → `resync`; subscribe/unsubscribe/ping protocol from design 06. Standalone, no auth yet. | `src/ws/hub.rs`, `src/ws/protocol.rs` | integration: two clients, topic isolation, lag → resync frame; snapshot of frames | Works with 1 000 idle sockets in an ignored soak test without growing memory | 06 §Realtime |
| P6-02 | **Events.** `Event` enum (`#[serde(tag="name")]`), `publish()`, `events_published_total{name}` metric. | `src/events.rs` | unit: serialisation snapshot per variant | — | 06 §Events |
| P6-03 | **Durable jobs core.** `enqueue(kind, dedupe_key, priority, payload)` with `INSERT OR IGNORE`; claim loop (`UPDATE … RETURNING`) × `JOB_CONCURRENCY`; `Notify` wake; backoff `2^attempts × 30 s ± 20 %`, `failed` after 5; handler trait. | `src/jobs/scheduler.rs` | unit: dedupe; claim ordering by priority; backoff; integration: restart resumes | No job runs twice concurrently (assert in test with a slow handler) | 06 §Scheduler |
| P6-04 | **Ticks.** `tokio::interval` loops for `poll:live`, `poll:rank`, `poll:matches`, `ddragon:sync`, `maintenance`, each fanning out per tracked player. | `src/jobs/ticks.rs` | unit under paused time: tick enqueues expected rows | — | 06 |
| P6-05 | **Poll handlers.** `poll:live` (spectator diff → `game.started/ended`), `poll:rank` (league diff → `rank.changed`), `poll:matches` (cursor from `last_seen_match_id`, enqueue `archive:match` with depth-block priority, gap → `backfill:player`). | `src/jobs/poll.rs` | integration (wiremock): each transition emits exactly one event; cursor advances | — | 06 §Job catalogue |
| P6-06 | **Archive + backfill handlers.** `archive:match` (fetch at bulk, archive, facts, `match.archived`), `backfill:player` (page ids up to `LOOKUP_BACKFILL_LIMIT`, state in `players.backfill_state`). First lookup of an untracked player enqueues backfill (v1 behaviour). | `src/jobs/archive.rs` | integration: backfill of 250 ids across 3 pages; resume after restart | Exit-check scenario passes | 06 |
| P6-07 | **WS auth + wiring.** Bearer or `?key=`; admin topics need admin scope; quota applies; `/v1/ws` route; `metrics` topic ticks only with subscribers. | `src/routes/ws.rs` | integration: unauth rejected; admin topic gated | — | 06 |
| P6-08 | **Admin routes (jobs).** `/v1/admin/jobs` list/retry/cancel; `/v1/admin/jobs/stats`. | `src/routes/admin.rs` | integration | — | 06 |

### Phase P7 — Data Dragon, ladder, analytics, dashboard

Exit check: `LADDER_QUEUES=RANKED_SOLO_5x5 LADDER_TIER_FLOOR=MASTER` crawl against a wiremock ladder of 30 players completes enumerate → collect → archive → done, each match fetched exactly once (assert on wiremock request count); `/dashboard` shows live numbers.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P7-01 | **Data Dragon sync + static serving.** `versions.json` → mirror new patch into `$DATA_DIR/ddragon`; `patch.new` event; `tower-http::ServeDir` at `/ddragon` with immutable cache headers; `champions.rs` id↔name loaded from mirror. | `src/jobs/ddragon.rs`, `src/static/champions.rs` | integration (wiremock CDN): sync once, second run no-op; static route headers snapshot | — | 07 §Option B |
| P7-02 | **Ladder enumerate.** `ladder:crawl` creates crawl row + fans out `ladder:apex` × 3 and `ladder:walk` × (tier, division) down to `LADDER_TIER_FLOOR`; counters on the crawl row; last job flips phase (decrement inside the same write txn). | `src/jobs/ladder.rs` | integration: phase flips exactly once under concurrency | — | 06 |
| P7-03 | **Ladder collect + archive.** `ladder:collect` (25 players/job → `crawl_match_ids`), `ladder:archive` (`filter_unarchived` → enqueue), `crawl.phase` events, enqueue `aggregate:analytics` on done. | `src/jobs/ladder.rs` | exit-check scenario | Each match fetched once (wiremock count) | 06 |
| P7-04 | **Analytics.** `aggregate:analytics` rebuilds `champion_stats/matchups/builds` for last `AGGREGATE_PATCH_LIMIT` patches in one txn per table; `facts:reextract` batches. `/v1/lol/analytics/*` routes copied from v1. | `src/jobs/analytics.rs`, `src/routes/analytics.rs` | unit on fixtures; integration for routes | Results match a hand-computed fixture | 06 |
| P7-05 | **Maintenance.** Trim `jobs`/`metrics_history`, L2 sweep, `wal_checkpoint(TRUNCATE)`, `VACUUM INTO` daily backup keep 14; `backup` subcommand. | `src/jobs/maintenance.rs`, `src/cli/backup.rs` | integration: backup file opens and has the rows | — | 07 §Backups |
| P7-06 | **Dashboard wiring.** `metrics_history` sampler (60 s), `/v1/admin/metrics/history`, dashboard subscribes to `metrics` + `firehose`; crawl progress panel. | `src/routes/admin.rs`, `src/ui/dashboard.html` | integration for the history endpoint | Manual: dashboard renders in browser (screenshot in PR) | 07 |

### Phase P8 — Contract, migration, packaging

Exit check: acceptance suite green against v2 with mock upstream; `migrate-v1` imports a v1 dump fixture; `docker compose up` on a clean VM serves `/docs`; GitHub release `v2.0.0-rc.1` with linux-amd64, linux-arm64 and darwin-arm64 binaries and the image on GHCR.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| P8-01 | **Port acceptance suite.** Make `acceptance/` (vitest) run against `riot-proxy serve` + wiremock via a `just acceptance` recipe; fix v2 where v1 is right; record intentional differences in `docs/DECISIONS.md`. | `acceptance/`, `justfile`, `ci.yml` (new job) | the suite | Zero failures; every skipped test has an ADR reference | 08 §Contract tests |
| P8-02 | **`migrate-v1` subcommand.** Streams `matches`, `timelines`, `players` from a `pg_dump -Fc` (via `pg_restore --data-only -f -` piped, or COPY text) → zstd → SQLite; re-derives facts; consumers not migrated. Fixture: a 20-match dump checked into `tests/fixtures/v1-dump/`. | `src/cli/migrate_v1.rs` | integration on fixture | ≥ 1 000 matches/s on the fixture loop | 08 §Data migration |
| P8-03 | **Built-in TLS.** `--tls --domain --acme-email` via `rustls-acme`; 80→443 redirect; `/metrics` and `/readyz` private-range only. `[parallel-ok]` | `src/tls.rs` | integration with a self-signed cert (ACME not tested) | Manual on a real domain (owner) | 07 §Option B |
| P8-04 | **Postgres feature flag (compile-only).** `Store` trait, `SqliteStore` impl; `PgStore` stubbed behind `--features postgres` so the door stays open. No behaviour. `[parallel-ok]` | `src/db/store.rs` | `cargo check --features postgres` in CI | — | 04 §Postgres compatibility |
| P8-05 | **Release pipeline.** `release.yml`: on tag `v*` build musl amd64/arm64 + darwin-arm64, push image to GHCR (`:2`, `:2.0.0`), attach binaries + `sha256sums`. `README.md` written for v2 (install, first run, config table, ops). | `.github/workflows/release.yml`, `README.md` | dry-run on `v2.0.0-rc.0` | Release page has three binaries + image digest | 07 |
| P8-06 | **Cut-over runbook.** `docs/CUTOVER.md` from design 08: deploy, migrate, switch consumers one at a time, watch v1 to zero, decommission. | `docs/CUTOVER.md` | — | Owner sign-off | 08 §Cut-over |

### Post-release — SCH: scheduler fairness and an elastic pool (owner requests 2026-10-08/09)

Found while running three ladder crawls at once (na1, euw1, oc1). There are two problems:

1. **Claims ignore rate limits.** Workers take jobs in `priority, run_after, id` order. All of na1's `ladder:walk` rows were queued first, so they are all claimed before any euw1 or oc1 walk. The 8 workers then share na1's app limit while euw1's and oc1's buckets sit unused, and the crawls run one platform after another.
2. **A rate-limited job keeps its worker.** Bulk fetches wait up to `BULK_BUDGET` (15 min) inside `Limiter::acquire`. A worker that is waiting on na1's limit can't pick up a euw1 job that could run right now, or a poll (priority 10 000) that is ready.

The owner's requirement: **fetch from as many regions at once as the rate limits allow. When a job hits a limit, its worker moves on to a job for a region that still has room.** v1 did neither: BullMQ ran ladder jobs on a fixed 3 workers and ignored regions (`src/worker.ts` `CONCURRENCY`).

A second request: when every worker is busy and new work arrives (a crawl running while a player is looked up), the pool should grow, up to a cap, and no single crawl or player may take too many workers (SCH-02).

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| SCH-01 | **Rate-limit-aware job claims.** (a) **Lane and method:** two nullable columns, set at enqueue by every kind that calls Riot. `lane` is the limiter scope the job's requests hit (`Target::scope()`: platform for league/summoner, region for match-v5/account). `method` is the endpoint id it mainly calls (`league.entriesByTier` for a walk, `match.byId` for `archive:match`, …). Kinds that don't call Riot (`maintenance`, `aggregate:analytics`, …) leave both `NULL` and can always be claimed. (b) **Skip blocked work, not whole regions:** before claiming, the worker asks the limiter what has no bulk room right now (this needs a new `Limiter` method). A **lane** is blocked only when its *app* limit is: frozen after an app 429, at `BULK_USAGE_CEILING`, an interactive caller waiting, or out of app tokens. A capped **method** blocks only the `(lane, method)` pair (owner, 2026-10-09): other jobs in that region using other endpoints can still be claimed. The claim excludes blocked lanes and blocked pairs. It moves to another region only when no job left for this region can run with the current rate limits. It may then take a job from a lower priority band on a free lane over a higher-band job on a blocked one: an idle worker is the waste this task removes. If nothing is claimable, the worker sleeps until the earliest blocked lane frees up (or `enqueue` wakes it), not for a fixed 1 s. (c) **Spread across lanes:** among claimable rows in the best band, pick the lane with the fewest `running` jobs (ties by `run_after, id`), so N workers cover N regions before doubling up on one. (d) **Yield instead of waiting:** jobs fetch with a short bulk budget (`JOB_YIELD_BUDGET_MS`, default 1 000). If a request would wait longer, whether for the app limit or one method's limit, the handler returns a new `JobError::Yield { retry_at }`. The scheduler puts the row back to `pending` with `run_after = retry_at`. This does **not** count as an attempt, is not logged as a failure and does not bump `jobs_total{status="failed"}`. Every yielding handler must resume where it stopped: `ladder:walk` already has a page cursor; `ladder:collect` has to persist the players still to do, or re-queue itself with only those; `backfill:player` resumes from `backfill_state`. (e) **Take turns:** a walk also re-queues itself every `CANCEL_CHECK_PAGES` pages, so higher-priority work queued behind it gets a worker. This takes the place of a per-kind concurrency cap. (f) The claim stays one `UPDATE … RETURNING`. Update the SQLite `claim_sql`; the Postgres claim is still a stub (P8-04) and stays one. Interactive requests are unchanged (they keep `interactive_budget`). **Scope (owner, 2026-10-09):** single process only (`ROLE=all`), where the limiter and the workers share memory. Coordinating limiter state across separate `ROLE=worker` processes is a later task. | `src/db/migrations/V00NN__job_lane.sql`, `src/jobs/scheduler.rs`, `src/db/store.rs`, `src/riot/limiter/mod.rs`, `src/fetcher.rs`, `src/jobs/ladder.rs`, `src/jobs/ladder/store.rs`, `src/jobs/archive.rs`, `src/jobs/poll.rs`, `docs/design/06-jobs-and-realtime.md`, `docs/design/05-rate-limiter.md` | unit (paused time): na1's lane blocked and euw1 free → the next claim is a euw1 job even though na1's rows are older; na1's `league.entriesByTier` capped but its app limit free → the next claim is a na1 job using another method (e.g. a collect), and it moves to euw1 only when na1 has no such job left; a free lane in band 20 000 beats a blocked lane in band 10 000, and a free 10 000 still beats a free 20 000; with 3 crawls queued (na1 first), the first claims cover all three platforms; a yield leaves `attempts` unchanged, sets `run_after` to the limiter's `retry_at` and doesn't touch `error`; the walk resumes at the next page; a collect batch that yields doesn't re-fetch players it already finished; when every lane is blocked the worker sleeps until the earliest one frees. Integration (wiremock, 3 platforms, small app limits): three crawls finish in about the time of the slowest one, not the sum of all three; a poll queued behind 30 walks runs within one walk's turn. The P6-03 "no job runs twice" test and the `claim_sql` engine test still pass | Running 3 crawls on a dev key: every platform records pages within the first minute; `proxy_rl_wait_seconds{priority="bulk"}` never goes above the yield budget; no worker waits in `acquire` while another lane has room | 06 §Claiming, §Priority; 05 §Priorities; new ADR |
| SCH-02 | **Elastic worker pool with a per-group cap.** Depends on SCH-01. Today `Scheduler::start` spawns a fixed `JOB_CONCURRENCY` workers. The owner wants extra workers spun up when new work arrives while every worker is busy (e.g. a ladder crawl and a player lookup's archive jobs at the same time; the lookup's HTTP response never waits for a worker, but its `backfill:player` / `archive:match` jobs do). (a) **Grow:** `JOB_CONCURRENCY` (default 8) becomes the base pool, which is always running. When every worker is busy and a claimable row exists (using SCH-01's lane/method rules), spawn one more worker, up to `JOB_MAX_WORKERS` (new, default 32, validated `≥ JOB_CONCURRENCY` at boot). Trigger the check from `enqueue`/`notify` and whenever a worker finishes, not on a poll. (b) **Shrink:** a worker above the base exits after `JOB_WORKER_IDLE_S` (new, default 30) with nothing to claim; base workers never exit. (c) **Per-group cap:** a nullable `group_key` column naming the piece of work a job belongs to: the crawl id for `ladder:*` and the `archive:match` jobs a crawl queues, the puuid for `backfill:player` and the `archive:match` jobs a lookup or poll queues, `NULL` (no cap) for one-off kinds (`maintenance`, `ddragon:sync`, …). The claim skips rows whose group already has `JOB_MAX_PER_GROUP` (new, default 8, validated `≤ JOB_MAX_WORKERS`) jobs `running`. One crawl can then never hold more than 8 workers, and the pool grows for everyone else. The claim stays one `UPDATE … RETURNING`. (d) **Shutdown and recovery** cover the extra workers (they live in the same `JoinSet`, or one owned by `Workers`). (e) **Visibility:** the `/v1/admin/stats` worker section and the dashboard show workers busy / running / base / max. A new gauge `job_workers{state="busy"\|"idle"}` follows the v2 naming rule (no `proxy_` prefix, P0 owner review); no existing metric name changes. | `src/jobs/scheduler.rs`, `src/config.rs`, `src/db/migrations/V00NN__job_group.sql`, `src/jobs/ladder.rs`, `src/jobs/archive.rs`, `src/jobs/poll.rs`, `src/stats.rs`, `src/metrics.rs`, `src/ui/dashboard.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/07-deployment.md` (config table) | unit (paused time): 8 busy workers + a new claimable row → a 9th worker starts and claims it; never more than `JOB_MAX_WORKERS`; an extra worker exits after `JOB_WORKER_IDLE_S` idle, a base worker doesn't; a crawl with 50 ready walks holds at most `JOB_MAX_PER_GROUP` workers while a lookup's `archive:match` for another group is claimed right away; `NULL` groups aren't capped; shutdown drains and aborts extra workers like base ones; config rejects `JOB_MAX_WORKERS < JOB_CONCURRENCY` and `JOB_MAX_PER_GROUP > JOB_MAX_WORKERS`. Integration (wiremock): a running crawl doesn't delay a player lookup's first `archive:match` by more than one claim cycle. The P6-03 "no job runs twice" test still passes | A dev-key run of 3 crawls plus a player lookup: no crawl holds more than 8 workers, the lookup's archive jobs start within a second, and the pool shrinks back to the base 30 s after the queue drains. **Confirm the three defaults with the owner before merging** | 06 §Scheduler, §Claiming; 07 config table; new ADR |

### Post-release — THR: crawl and analytics throughput (owner plan 2026-10-09)

From the owner's plan "riot-proxy: crawl and analytics throughput plan" (2026-10-09). The plan was written by reading the code at `8e022ad`, without running it. Every throughput figure in it is arithmetic from the code and Riot's standard limits, so **each task starts by measuring the number it says it will move**. A task that moves nothing is dropped. The four crawl stages (enumerate, collect, archive, fetch), the single SQLite file, the v1 HTTP contract, error codes, headers and existing metric names don't change. Every new field, config variable and metric is a v2 addition.

What limits the pipeline today:

| Limit | Where | Consequence |
|---|---|---|
| 8 workers for all jobs | `Scheduler::start`, `JOB_CONCURRENCY` | Each `archive:match` holds a worker for one round trip, so round-trip time caps the fetch rate, not Riot's limit (about 32/s across all regions at 250 ms, against about 50/s per region on a standard production key) |
| Rebuild runs inside `db.write` | `AnalyticsContext::rebuild` | The single writer is held, so archiving and job claims wait |
| Tier read from the current ladder | `ladder_facts()`, `rebuild_matchups` in `src/archive/analytics.rs` | Totals can't be added to; a game moves tier when its player does |
| Every player collected every crawl | `collect_candidates`, `collect_one` | One request per player even when wins and losses haven't changed; at most `LADDER_BACKFILL_LIMIT` (100) games |
| Only listed players are crawled | `ladder:crawl` fan-out | Master past 10,000 (LAD) and anyone below the floor are never collected |
| Bulk work stops at 80% of each bucket | `BULK_USAGE_CEILING` | A fifth of the limit sits idle when nobody is looking anything up |

**Order of work.** Step 0 is a baseline: one kr crawl to MASTER on the dev VM. Record the wall time of each stage, matches archived per second, app-bucket usage per region, `proxy_aggregate_duration_seconds` per step, requests sent per endpoint, and the database file size and match count. Paste the numbers into `PROGRESS.md` under this group. Then: THR-06a → THR-01 → THR-02 → THR-03 → THR-04 → LAD-01, then LAD-02 with THR-05's changes → SCH-02 → THR-06b/c/d (only if their own measurement justifies them). Each is its own branch and PR. Migrations take the next free `V00NN` when they merge, since LAD-01/LAD-02 add some too.

**Ask the owner before starting the task that depends on each answer:**

1. The dev VM's round-trip time to each Riot regional host (THR-01's gain depends on it; 250 ms is an assumption).
2. The production key's actual app and `match.byId` limits (the plan assumes 500 per 10 s and 30,000 per 10 min).
3. THR-02 changes what a tier means in analytics, from "where the player is now" to "where the player was when the match was archived". Is that the intended meaning?
4. Matches archived by a lookup before their players were on the ladder get no tier and stay out of analytics. Accept that, or add a late-stamp pass after each enumerate?
5. THR-05: carry the retract step, or rely on a weekly full rebuild for late stamps?
6. Defaults: `ARCHIVE_BATCH_CONCURRENCY` 16, `AGGREGATE_DELTA_BATCH` 500, `LADDER_COLLECT_OVERLAP_S` 7 200, `BULK_IDLE_CEILING` 0.95, `BULK_IDLE_AFTER_S` 30.
7. How far down `LADDER_TIER_FLOOR` should go once THR-01 and THR-04 are in. Storage grows with it; the baseline's bytes per match gives the estimate.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| THR-01 | **Match fetch concurrency set by the rate limiter, not the worker count.** Depends on SCH-01 (#107). **Measure first:** during a crawl's archive stage, compare each region's app-bucket usage (`Limiter::usage`) with the bulk ceiling. If usage already sits at the ceiling, the limit is the bottleneck and this task is skipped. (a) New job kind `archive:batch`, payload `{crawlId, matchIds, fetchTimeline}`. `ladder:archive` already reads the set `ARCHIVE_BATCH` (100) ids at a time; each read queues one `archive:batch` in place of 100 `archive:match` rows. (b) The handler fetches its ids with `futures::stream::iter(ids).map(fetch).buffer_unordered(n)`, `n` = new `ARCHIVE_BATCH_CONCURRENCY` (proposed 16; with 8 workers, 128 requests in flight). (c) Each fetch uses `FetchOptions::JOB`, as now. When one returns `limited_until`, start no new fetches, let those in flight finish, and return `JobError::Yield` with a payload holding only the ids not yet done (the `collect_batch` pattern). (d) Lane and method are the region and `match.byId`, so SCH-01's claim rules apply unchanged. (e) `archive:match` stays for lookups, polls and backfills: its per-match depth priority is why a looked-up player's newest games arrive first. (f) Crawl batches lose per-id dedupe. That costs nothing at Riot: the fetcher answers an archived id from the archive (`X-Cache: Archive`). (g) A 404 on one id is logged and dropped; it doesn't fail the batch. (h) The jobs page shows a label for the new kind. With this task a crawl needs few workers, so SCH-02 only has to keep lookups from waiting behind a crawl. | `src/jobs/archive.rs`, `src/jobs/ladder.rs`, `src/jobs/lanes.rs`, `src/jobs/mod.rs`, `src/config.rs`, `src/ui/jobs.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/07-deployment.md` | unit (paused time): a batch of 100 at concurrency 16 never has more than 16 fetches in flight; a yield after 40 ids re-queues a payload of exactly the other 60 and leaves `attempts` unchanged; an id archived between queueing and fetching makes no upstream call; a 404 id is dropped and the rest of the batch archives; `match.archived` is still published once per new match. Integration (wiremock, 200 ms latency, generous limits): 1,000 matches archive in under a quarter of the time the per-match path takes | On the dev VM with the production key, a single-region crawl's archive stage holds that region's app bucket at 90% or more of the bulk ceiling, and `proxy_rl_wait_seconds{priority="bulk"}` stays under the yield budget | 06 §Job catalogue, §Claiming; 07 config table; new ADR |
| THR-02 | **Store each participant's tier when the match is archived.** Analytics then read the stored tier, so a player promoted from Diamond to Master no longer takes their earlier games with them, and totals can be added to (THR-03). (a) New table `match_tiers (match_id TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE, key_scope, puuid, platform` (from the match id prefix)`, queue` (`RANKED_SOLO_5x5`\|`RANKED_FLEX_SR`)`, tier, stamped_at INTEGER, PRIMARY KEY (match_id, puuid))` and index `match_tiers_ladder (key_scope, platform, queue, tier)`. It is kept apart from `match_facts`, so facts stay a pure derivation of the body and `facts:reextract` never touches it. (b) `matches::put`, after `write_derived` in the same transaction, inserts one row per participant that `ladder_entries` holds for the match's platform and queue, with `INSERT OR IGNORE`: a re-archive must not restamp. Matches in other queues write nothing. Participants the ladder doesn't hold get no row, which keeps today's rule that they are left out of analytics. (c) One-off job `tiers:backfill` stamps every existing ranked match from the current `ladder_entries`, a batch at a time, paced like `facts:reextract`. Today's numbers use the current ladder, so this reproduces them exactly. (d) `ladder_facts()` and `rebuild_matchups` join `match_tiers` in place of `ladder_entries`, taking platform, queue and tier from it. The rebuild still works and gives the same output straight after the backfill. (e) The `/dev` reset clears the table. **Owner question 3 before starting.** | `src/db/migrations/V00NN__match_tiers.sql`, `src/archive/matches.rs`, `src/archive/analytics.rs`, `src/jobs/analytics.rs`, `src/jobs/mod.rs`, `src/routes/dev.rs`, `docs/design/04-data-and-cache.md` | unit: archiving a solo-queue match with 3 ladder players writes 3 rows with their tiers; a re-archive after one of them is promoted keeps the first tier; an ARAM match writes none; `facts:reextract` leaves `match_tiers` untouched. Snapshot: `tests/analytics.rs` output is byte-identical before and after the join change, once the backfill has run. Integration: after a promotion and a second crawl, the old games stay in the old tier and the new games count in the new one | On a copy of the dev VM database, `tiers:backfill` followed by a recompute gives the same `champion_stats` row count and game totals as before the change | 04 §Archive; 06 §Job catalogue; new ADR |
| THR-03 | **Incremental analytics.** Depends on THR-02: totals are only additive once a fact's tier never changes. A new `aggregate:delta` job adds only the matches not yet counted, in short transactions. It replaces delete-and-rebuild as the normal path; the rebuild stays as a repair tool. It runs as a batch job, not inside the archive transaction: one match touches about 100 counter rows, and doing that per match would put the work on the writer at fetch rate. **Measure first:** record `proxy_aggregate_duration_seconds` per step for a recompute on the current archive (the writer stall this removes, and the baseline for Accept). (a) Migration: `matches.aggregated_at INTEGER` and `CREATE INDEX matches_unaggregated ON matches (match_id) WHERE aggregated_at IS NULL AND queue_id IN (420, 440)`. (b) `aggregate:delta` (no payload, deduped to one, no lane) loops: read up to `AGGREGATE_DELTA_BATCH` (proposed 500) ids from that index whose facts are at the current `facts_version`; in **one** write transaction run each existing `INSERT … SELECT` restricted to those ids with `ON CONFLICT (…) DO UPDATE SET games = games + excluded.games, wins = wins + excluded.wins`, and the same for every summed column, then stamp `aggregated_at` on the batch; sleep briefly between batches, as `facts:reextract` does; stop when the index is empty. (c) Every aggregate in the current SQL is additive over disjoint sets of matches, including the `count(DISTINCT match_id)` columns (`matches`, `matches_picked`, `bans`), because a match is in exactly one batch. (d) Platform, queue and tier come from `match_tiers`, so one job serves every ladder: group by `mt.platform, mt.queue` where the rebuild binds them. (e) Triggers: `archive:batch` and `archive:match` enqueue it (deduped) when they archive something new, and `AGGREGATE_INTERVAL_S` enqueues it on a tick. A crawl's completion no longer queues a full rebuild. (f) The full rebuild (`aggregate:analytics`, dashboard "Recompute now") gives the same output but also stamps `aggregated_at` on every match it counted, or the next delta counts them twice. (g) Anything that changes facts forces a rebuild: `facts:reextract` clears `aggregated_at` for the matches it rewrites and queues `aggregate:analytics` when it ends. (h) `analytics.updated` is published once per delta run, not per batch. (i) **Keep the `ETag` fresh:** the analytics routes build their weak `ETag` from the rows' `computed_at` (DEV-21's `patch=all` uses `max(computed_at)`). Every delta upsert sets `computed_at = excluded.computed_at` on each row it touches, on insert and on conflict, and so does THR-05's retract step. Otherwise a client holding an old `ETag` keeps getting `304` for totals that have moved. **Gaps found when checking the plan against `916b098` (settle in the ADR):** (j) *Switch-over:* after the migration every match has `aggregated_at IS NULL`, but the analytics tables already count them, so the first delta would double-count everything. The first aggregation after the migration must be a full rebuild over every patch (ignoring `AGGREGATE_PATCH_LIMIT`) that stamps what it counts. The delta doesn't run until that rebuild has finished. (k) *The rebuild is five transactions* (champions; matchups; items, runes and spells), but there is one `aggregated_at`. Fix the match set when the rebuild starts (for example `archived_at ≤` its start time) so every step counts the same matches, and stamp them in the last step. The delta never runs while a rebuild is running or after one stopped part-way, until a rebuild completes. A rebuild limited to the newest `AGGREGATE_PATCH_LIMIT` patches stamps only those patches' matches; older ones were stamped by (j). (l) *Key scopes:* `matches` has no `key_scope`, but facts and analytics do. If more than one key scope can hold facts for one match, track the stamp per `(match_id, key_scope)` in a side table instead of a column. | `src/db/migrations/V00NN__aggregated_at.sql`, `src/archive/analytics.rs`, `src/jobs/analytics.rs`, `src/jobs/archive.rs`, `src/jobs/ladder.rs`, `src/jobs/ticks.rs`, `src/config.rs`, `src/metrics.rs`, `src/ui/dashboard.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/metrics.md` | property test: for a random set of matches split into random batches, delta output equals rebuild output, table by table. unit: a crash between two batches loses nothing and double-counts nothing (stamp and counters commit together); a rebuild then a delta changes no row; a re-archived match (same id) isn't counted again; a match with one participant in Master and one in Grandmaster adds 1 to `analytics_slices.matches` in both tiers; the first aggregation after the migration is a full rebuild and leaves totals unchanged; a delta queued while a rebuild runs waits for it; paused time: a delta over 10,000 matches never holds the writer for more than one batch. Integration: a read, then a delta that adds one game to a row in that slice, then the same read with `If-None-Match` → `200` with a new `ETag`; a delta that touches no row in the slice → still `304` | On the dev VM, analytics for a running crawl are current within one tick, the longest write transaction during the crawl is under 250 ms, and a full recompute afterwards changes no totals | 06 §Job catalogue; metrics; new ADR |
| THR-04 | **Collect only what is new.** Collect skips players whose wins plus losses haven't changed since their last collect, and asks the rest only for games since then. **Check first:** confirm on the Riot developer portal whether `startTime` on `match.idsByPuuid` filters on game start or game end. The overlap in (d) assumes game start, the safe reading (never invent Riot semantics). The saving grows with crawl frequency and in lower tiers, where many accounts are idle. The `startTime` half is worth doing regardless: it removes the 100-game ceiling. (a) Migration: nullable `collected_games INTEGER` and `collected_at INTEGER` (unix ms) on `ladder_entries`. (b) `collect_candidates` adds `AND (le.collected_games IS NULL OR le.collected_games <> le.wins + le.losses)`. `<>` (not `<`) means a season reset, where totals drop, still collects. (c) The candidate query returns `(puuid, wins + losses, collected_at)`, and `CollectJob` carries those per player in place of bare puuids. (d) `collect_one` with a `collected_at`: `startTime = collected_at / 1000 - LADDER_COLLECT_OVERLAP_S` (proposed 7 200), paged 100 at a time until a short page, no depth limit. The overlap covers games in progress at the last collect; ids seen twice are already dropped by `crawl_match_ids` and `filter_unarchived`. (e) Without one (first sight of a player): unchanged, `start=0` up to `LADDER_BACKFILL_LIMIT`. (f) On success, set `collected_games` to the total the candidate query returned and `collected_at` to the time the first id request was sent. Games played between enumerate and collect leave the stored total low, so the next crawl collects again: an extra request, never a missed game. (g) Remakes don't change wins or losses; a remake is picked up at the player's next real game, whose `startTime` reaches back past it. (h) `players_skipped` on `ladder_crawls`, shown on the dashboard crawl card beside `backfills_enqueued`. | `src/db/migrations/V00NN__collect_cursor.sql`, `src/jobs/ladder.rs`, `src/jobs/ladder/store.rs`, `src/routes/admin.rs`, `src/ui/dashboard.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/07-deployment.md` | unit/integration (wiremock): a second crawl over an unchanged ladder queues no collect jobs and goes straight to archive; a player whose total rose by 3 gets one request carrying `startTime`, and the 3 new ids reach the set; a player with 250 new games is paged three times and all 250 ids are collected; a total that dropped is collected; a collect that yields part-way stamps only the players it finished; a 404 player isn't stamped | Two crawls of one ladder an hour apart on the dev VM: the second sends fewer `match.idsByPuuid` requests than it has players, and archives every match the first one's players finished in between (spot-check 20 players against their match lists) | 06 §Job catalogue; 07 config table; new ADR |
| THR-05 | **Wider coverage from match participants.** Builds on LAD-01 and LAD-02 and changes three things about LAD-02; do it as part of LAD-02. Diamond and below need no new code: `ladder:walk` already pages `league.entriesByTier` down to `LADDER_TIER_FLOOR` at about 205 players per request, so lowering the floor is a config change, made after THR-01 and THR-04 (owner question 7). **Changes to LAD-02:** (a) *Order candidates by value:* rank unlisted participants by how many matches this crawl archived with them in it, most first, so `LADDER_DISCOVER_LIMIT` cuts the least useful lookups. (b) *Run alongside archive:* `league.entriesByPuuid` is on the platform host and `match.byId` on the regional host, separate limiter scopes, so `discover` starts once the first archive batches have landed instead of after the archive stage. (c) *Collect discovered players in the next crawl* (answers LAD-02 (e)): with THR-04 their first collect is one request, and the crawl keeps four stages. **Stamping tiers:** (d) when discover upserts a player at Master or above, it inserts `match_tiers` rows for that player's matches archived by this crawl (played in the last few days, so the tier just read is right). (e) Those matches may already be counted by `aggregate:delta` without that player. Retract step in THR-03's SQL: the same `INSERT … SELECT` with `games = games - excluded.games` (and `computed_at` set) for the affected ids, run before the new tier rows are inserted, then clear `aggregated_at`; the next delta counts the match again with the full set of players. (f) The alternative to (e) is a weekly scheduled full rebuild that picks up late stamps. **Owner question 5 before starting.** | As LAD-02, plus `src/archive/analytics.rs`, `src/jobs/analytics.rs` | Beyond LAD-02's: candidates come back ordered by match count, and a limit of 10 takes the top 10; discover and archive legs of one crawl run at the same time on different lanes; retract then delta over a match gives the same totals as a rebuild that knew every player from the start; a discovered Diamond player gets no `match_tiers` rows | A kr crawl on the dev VM ends with more than 10,000 Master players stored, total crawl time within 10% of a crawl with discover off, and a full recompute afterwards changes no totals | as LAD-02 |
| THR-06a | **Idle bulk ceiling.** Do now. `BULK_IDLE_CEILING` (proposed 0.95): `Limiter` uses it in place of `BULK_USAGE_CEILING` for a scope that has had no interactive request in the last `BULK_IDLE_AFTER_S` (proposed 30), and drops back the moment one arrives. It stays below 1.0: the local windows and Riot's are never exactly in step, and the gap is what prevents 429s. | `src/riot/limiter/mod.rs`, `src/config.rs`, `docs/design/05-rate-limiter.md`, `docs/design/07-deployment.md` | the existing ceiling proptest with both values; an interactive request at 94% usage is admitted at once and bulk is held from then on; bulk returns to the idle ceiling `BULK_IDLE_AFTER_S` after the last interactive request; config rejects `BULK_IDLE_CEILING` below `BULK_USAGE_CEILING` or at/above 1.0 | A 30-minute crawl with no 429s and app-bucket usage above 90% | 05 §Priorities; 07 config table; new ADR |
| THR-06b | **Timeline sampling for crawl matches.** Before turning `ARCHIVE_TIMELINES` on for crawls: a timeline is a second request per match, so fetching all of them halves match throughput. `ARCHIVE_TIMELINE_SAMPLE` (0.0–1.0, default 0) for crawl batches, chosen by a hash of the match id so a re-run picks the same matches. Lookups keep their own `fetchTimeline` flag. Depends on THR-01. | `src/jobs/archive.rs`, `src/config.rs`, `docs/design/07-deployment.md` | at 0.1 over 10,000 ids, between 900 and 1,100 are chosen, and the same ones twice | Owner check on the dev VM | 07 config table |
| THR-06c | **Several matches per write transaction.** Only if writer queue time shows up during a crawl after THR-01. `matches::put` commits once per match; WAL with `synchronous=NORMAL` already makes a commit cheap. Measure the writer channel's wait time during a crawl first. If it matters, `archive:batch` hands `put_many` up to 16 prepared matches for one transaction. | `src/archive/matches.rs`, `src/jobs/archive.rs` | `put_many` writes the same rows as `put` called once per match; a failure in one match rolls back only its own group | Writer wait time during a crawl drops against the measurement | 04 §Archive |
| THR-06d | **zstd dictionary for match bodies.** Only if an offline trial shows the archive shrinking by a quarter or more: export 2,000 bodies, `zstd --train`, compare sizes with and without (match bodies are about 100 KB, where the gain is uncertain). Needs a `dict_id` column on `matches` and the dictionary stored in the database, so old rows stay readable. | `src/db/migrations/V00NN__zstd_dict.sql`, `src/archive/matches.rs` | a body written with the dictionary reads back byte-identical; a row without `dict_id` still reads | The trial's numbers, then the archive size on the dev VM | 04 §Archive; new ADR |

### Post-release — LAD: Master players past Riot's 10,000 cap (owner request 2026-10-09)

Found while running kr, euw1 and na1 crawls down to MASTER: each one enumerated exactly **11,000** players (300 Challenger + 700 Grandmaster + 10,000 Master), but those shards have about 30K, 22K and 12K Master players (dpm.lol, 2026-10-08). The crawl doesn't drop anyone; Riot returns no more:

- `GET /lol/league/v4/masterleagues/by-queue/RANKED_SOLO_5x5` returns exactly 10,000 entries on kr, euw1 and na1. The lowest LP in the list is 313 on kr, 412 on euw1 and 31 on na1, so the players missing are the bottom of Master.
- `GET /lol/league-exp/v4/entries/RANKED_SOLO_5x5/MASTER/I?page=N` pages the same list: 48 pages of 205 and 160 on page 49 (= 10,000), then empty pages. Compared player by player on euw1 and kr, it has nobody `masterleagues` lacks (9,999 and 9,998 overlap, because the ladder moves while it is paged).
- `GET /lol/league/v4/entries/{queue}/MASTER/I` answers 400: that route only takes DIAMOND and below.

All checked with the production key on 2026-10-09. The cap isn't in Riot's documentation we have, so the 10,000 is an observed value: the tasks name it in one constant and check it against what Riot returns, not assume it.

| ID | Task | Files | Tests | Accept | Design |
|---|---|---|---|---|---|
| LAD-01 | **Say when an apex league is cut off at Riot's cap.** (a) `RIOT_APEX_LIST_CAP = 10_000` next to `APEX_TIERS`, with a comment giving the evidence above. (b) When an apex leg stores a list of at least that many entries, record it on the crawl: a nullable `apex_capped` column (JSON list of capped tiers, e.g. `["MASTER"]`) on `ladder_crawls`, written in the same transaction as the page. (c) `GET /v1/admin/ladder/crawls/{id}` and the crawl list carry `apexCapped` (`[]` when none). (d) The dashboard crawl card (DEV-17) shows, next to the player count, "Master: top 10,000 only (Riot API limit)" for each capped tier. (e) The showcase home ladder (`#/`, design 11) shows the same note under a Master list that has reached the cap; `GET /v1/lol/league/apex/...` stays a passthrough and is not changed. (f) Design 06 §Job catalogue records the cap and the evidence. Field names are new v2 admin fields; no v1 contract changes. | `src/riot/ladder.rs`, `src/jobs/ladder.rs`, `src/jobs/ladder/store.rs`, `src/db/migrations/V00NN__apex_capped.sql`, `src/routes/admin.rs`, `src/ui/dashboard.html`, `src/ui/showcase.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/11-showcase.md` | unit: an apex list of 10,000 sets `apex_capped = ["MASTER"]`, 9,999 leaves it `[]`, Challenger/GM never set it; integration (wiremock): a crawl whose Master list has 10,000 entries reports `apexCapped: ["MASTER"]` on the crawl route; jsdom: the crawl card and the showcase ladder show the note only when capped | A kr crawl on the dev VM shows the Master note on its card; an oc1 crawl (Master under 10,000) doesn't | 06 §Job catalogue; 11 §Page map; new ADR |
| LAD-02 | **Find the Master players the cap leaves out, from archived matches.** Depends on LAD-01. Riot only lists the top 10,000; the rest still play ranked against the players we archive, so they show up as `match_facts.puuid`. (a) **When:** after `archive`, a crawl whose `apex_capped` includes MASTER gets a fourth stage, `discover`. Crawls that aren't capped skip it (unchanged). (b) **Who:** puuids in `match_facts` for matches this crawl archived (`crawl_match_ids`) with no `ladder_entries` row for this (scope, platform, queue), and not checked within `LADDER_DISCOVER_RECHECK_S`. (c) **How:** batches of 25 puuids (as collect), one `league.entriesByPuuid` call per puuid on the crawl's platform. An entry for this queue at MASTER or above (and at or above the crawl's floor) is upserted into `ladder_entries` with `first_seen_crawl_id`/`last_seen_crawl_id` set to this crawl, and the player is added to the analytics set like any enumerated player. Anyone else (Diamond and below, unranked, other queues) is only stamped as checked in a new `ladder_discover_checked (key_scope, platform, queue, puuid, checked_at)` table, so the next crawl doesn't look them up again too soon. (d) **Bounds:** at most `LADDER_DISCOVER_LIMIT` lookups per crawl; 0 turns the stage off. Jobs use SCH-01's lane (`platform`, `league.entriesByPuuid`), yield and resume like collect batches, and take the bulk priority band of the crawl. (e) **Whether discovered players get their match ids collected** in the same crawl is an open question: see the owner questions below. (f) **Visibility:** the crawl card gets a `discover` stage bar (lookups done / to do, Master found), `ladder_discovered_total{platform,queue}` counter. New config goes in design 07's table. **Ask the owner before starting:** the defaults for `LADDER_DISCOVER_LIMIT` (proposed 50 000) and `LADDER_DISCOVER_RECHECK_S` (proposed 604 800, 7 days), and whether discovered players are collected and archived in the same crawl or the next one. | `src/jobs/ladder.rs`, `src/jobs/ladder/store.rs`, `src/jobs/lanes.rs`, `src/db/migrations/V00NN__ladder_discover.sql`, `src/config.rs`, `src/metrics.rs`, `src/routes/admin.rs`, `src/ui/dashboard.html`, `docs/design/06-jobs-and-realtime.md`, `docs/design/07-deployment.md`, `docs/design/metrics.md` | unit: an uncapped crawl goes archive → complete; a capped one goes archive → discover → complete; a puuid already in `ladder_entries` or checked within the recheck window isn't looked up; a MASTER answer is upserted with this crawl's ids, a DIAMOND or unranked answer only stamps `checked_at`; `LADDER_DISCOVER_LIMIT=0` skips the stage; a yielding batch resumes without repeating finished puuids. Integration (wiremock): a crawl with a capped 10,000-entry Master list and archived matches whose participants include lower-Master players ends with those players in `ladder_entries` and on the crawl's counts | A kr crawl on the dev VM ends with more than 10,000 Master players stored, without pushing `proxy_rl_wait_seconds{priority="bulk"}` past the yield budget | 06 §Job catalogue; 07 config table; new ADR |
| LAD-03 | **Re-run the cap checks from `/dev`** (owner request 2026-10-09: "tests that I can easily run through the dev portal"). Runs before LAD-01, which reuses its constant. (a) `RIOT_APEX_LIST_CAP = 10_000` next to `APEX_TIERS` (LAD-01 (a)). (b) `league.expEntries` (`/lol/league-exp/v4/entries/{queue}/{tier}/{division}?page=`) joins the endpoint registry as v2's first method past v1's list: own limiter bucket, `ladder` TTL key, not persisted. (c) `POST /v1/admin/ladder/probe {platform, queue?}` asks Riot now, skipping the cache read, at interactive priority: the three apex leagues, league-exp MASTER/I ten pages at a time until an empty page (at most 100), and `entries/{queue}/MASTER/I?page=1`. Each of three checks answers `confirmed`, `not-seen`, `changed` or `error`: `master-capped` (`masterleagues` lists exactly the cap), `exp-same-list` (league-exp has no more than 10 players `masterleagues` lacks; the ladder moves while it is paged), `paged-refuses-apex` (the paged route refuses MASTER). It stores nothing. (d) A **Ladder** tab on `/dev` (before Reset) with platform and queue, Run, a summary, the three verdicts, the apex lists (players, lowest LP, at the cap) and the league-exp overlap. | `src/riot/ladder.rs`, `src/riot/endpoints.rs`, `src/routes/admin/ladder_probe.rs`, `src/routes/admin.rs`, `src/ui/dev-ui.html`, `docs/design/10-dev-explorer.md` | unit: the verdicts for a capped list, a short list, more than the cap, extra league-exp players, a page limit hit, a paged MASTER that answers, and failed calls; integration (wiremock, kr-shaped: 10,000 Master, 48 × 205 + 160 league-exp pages, 400 for MASTER): all three confirmed, 50 league-exp calls, a second probe asks Riot again; an oc1-shaped short list is `not-seen`; admin only, body validated before any Riot call. Node and jsdom: verdict classes, list rows, the tab runs only on Run and shows each verdict | Probing kr, euw1 and na1 on the dev VM confirms all three checks; oc1 shows `not-seen` | 10 §Page map; new ADR |

---

## 3. Parallelism and ordering

Strict order within a phase unless `[parallel-ok]`. Phases are sequential except:

- **P6-01 (WS hub spike) may be done any time after P0** and is recommended immediately after P0 to de-risk.
- P4-06, P8-03, P8-04 may be done whenever there is a gap.

Never start a phase's second task while its first PR is unmerged; rebase pain is not worth it in a single-agent repo.

---

## 4. Bootstrap file contents

### 4.1 `CLAUDE.md` — see separate file in this folder (copy verbatim to repo root).

### 4.2 `PROGRESS.md`

```markdown
# Progress

Legend: [ ] todo · [~] in progress (branch name) · [x] merged (#PR)

## P0 — Foundations
- [ ] P0-00 bootstrap
- [ ] P0-01 workspace, toolchain, CI
- [ ] P0-02 config
- [ ] P0-03 logging + metrics + request ids
- [ ] P0-04 SQLite layer
- [ ] P0-05 HTTP skeleton + health
- [ ] P0-06 CLI
- [ ] P0-07 dev tooling
Exit check: _pending_

## P1 — Riot client
- [ ] P1-01 … P1-05
Exit check: _pending_

(… one section per phase, every task listed …)

## Notes for the next task
-
```

### 4.3 `docs/DECISIONS.md` seed

```markdown
# Architecture Decision Records

## ADR-001 — Single process, single binary (2026-09-16)
Accepted. A Riot key's rate limit bounds throughput; multiple processes only add coordination. See docs/design/03.

## ADR-002 — SQLite (WAL) as the only default store (2026-09-16)
Accepted. Postgres behind a feature flag for > ~50 GB archives. See docs/design/02, 04.

## ADR-003 — Rust / axum / tokio (2026-09-16)
Accepted. Go was the runner-up. See docs/design/02.

## ADR-004 — rusqlite vs sqlx
Pending — decide in P0-01. Default recommendation: rusqlite (bundled) + refinery; sqlx if compile-time query checking proves worth the async complexity.

## ADR-005 — Migrations tool
Pending — decide in P0-01.
```

### 4.4 `.github/workflows/ci.yml`

```yaml
name: ci
on: { push: { branches: [main] }, pull_request: {} }
env: { CARGO_TERM_COLOR: always, RUSTFLAGS: "-D warnings" }
jobs:
  fmt:
    runs-on: ubuntu-latest
    steps: [ {uses: actions/checkout@v4}, {uses: dtolnay/rust-toolchain@stable, with: {components: rustfmt}}, {run: cargo fmt --all --check} ]
  clippy:
    runs-on: ubuntu-latest
    steps: [ {uses: actions/checkout@v4}, {uses: dtolnay/rust-toolchain@stable, with: {components: clippy}}, {uses: Swatinem/rust-cache@v2}, {run: cargo clippy --all-targets --all-features -- -D warnings} ]
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@cargo-llvm-cov
      - run: cargo llvm-cov --all-features --workspace --summary-only
      - run: '! grep -rE "RGAPI-[0-9a-f-]{20,}" tests/ src/ docs/'   # no leaked keys
  build-musl:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: x86_64-unknown-linux-musl }
      - uses: Swatinem/rust-cache@v2
      - run: sudo apt-get install -y musl-tools && cargo build --release --target x86_64-unknown-linux-musl
      - run: ls -la target/x86_64-unknown-linux-musl/release/riot-proxy && file target/x86_64-unknown-linux-musl/release/riot-proxy | grep -q "statically linked"
      - run: docker build -t riot-proxy:ci . && docker run --rm riot-proxy:ci --version
```

Pin action versions to current majors when writing the real file; the `ci` status check name used by branch protection is the workflow name.

### 4.5 `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`

```toml
# rust-toolchain.toml
[toolchain]
channel = "stable"          # pin to the exact version CI resolves on P0-01 and record it here
components = ["rustfmt", "clippy"]
targets = ["x86_64-unknown-linux-musl"]
```

```toml
# rustfmt.toml
edition = "2024"
max_width = 110
use_field_init_shorthand = true
```

```toml
# clippy.toml
too-many-arguments-threshold = 8
```

Add to `Cargo.toml`:

```toml
[lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
```

### 4.6 `.gitignore`

```
/target
/data
.env
*.db
*.db-wal
*.db-shm
/backups
node_modules
```

---

## 5. Kick-off prompt for Claude Code

Paste this as the first message in the new repo's Claude Code session after bootstrap:

> Read `CLAUDE.md`, then `docs/IMPLEMENTATION.md` in full, then skim `docs/design/README.md` and `docs/design/03-architecture.md`. Confirm you have the v1 reference clone at `../riot-proxy-v1` and that it is the Fastify/BullMQ riot-proxy. Then start with task **P0-01** exactly as described: one branch, tests where the task lists them, CI green, PR via `gh`, squash-merge, update `PROGRESS.md`. Work through tasks strictly in order. Stop and ask me whenever §0.1 rule 7 applies. After each phase's exit check, paste the result in `PROGRESS.md`, tag `phase-P<n>`, and pause for my review before starting the next phase.

---

## 6. Owner checklist (things only you can do)

- [ ] Confirm the v1 reference repo URL (`ninja-recorder-deprecated` is what you gave me — double-check it isn't the recorder repo).
- [ ] Put a **development** Riot key in `.env` for P1-05 / P3-06 recording; never the production key.
- [ ] Add `RIOT_API_KEY` (dev) as a GitHub Actions secret only if you want the ignored live tests in CI — not required.
- [x] ~~Review the P5-04 composite snapshot and the P8-01 skipped-test ADRs personally.~~ The P5-04 snapshots were reviewed by the owner (#43). P8-01 skipped no test: all 29 run in mock mode (ADR-059).
- [x] ~~Test P8-03 built-in TLS on a real domain.~~ Not needed: built-in TLS was removed (RC-03, ADR-067).
- [ ] Sign off `docs/CUTOVER.md` before decommissioning v1.

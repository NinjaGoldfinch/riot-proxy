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
- [ ] Review the P5-04 composite snapshot and the P8-01 skipped-test ADRs personally.
- [ ] Test P8-03 built-in TLS on a real domain.
- [ ] Sign off `docs/CUTOVER.md` before decommissioning v1.

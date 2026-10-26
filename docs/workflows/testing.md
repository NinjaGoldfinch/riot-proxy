# Testing

The test layers of riot-proxy, how to run each one (all of it, one file, one test, the way CI runs it), how Riot is mocked, the fixtures, the snapshot policy, coverage, the key-leak check and the browser suite's prerequisites. The strategy itself is `docs/IMPLEMENTATION.md` §0.6 (`:88-99`).

## Layout

The crate is lib plus bin (`src/lib.rs`), so every `tests/*.rs` can import the modules (`PROGRESS.md:419`). Since CI-04 (ADR-176) `Cargo.toml` sets `autotests = false` and declares one `[[test]]` target `it` at `tests/it.rs` whose modules are the other test files, so `tests/common` compiles once and one binary links; `tests/telemetry.rs` keeps its own target because it owns the process-global metrics recorder. A guard test in `tests/it.rs` fails when a new `tests/*.rs` file is neither a module of it nor a `[[test]]` path. Snapshot files are named `it__<module>__<test>.snap`.

| Layer | Where | Tool | Count |
|---|---|---|---|
| Unit | `src/<module>/tests.rs` or `#[cfg(test)] mod tests` beside the code | `cargo test --lib`, `tokio::test`, `tokio::time::pause()` | 23 `tests.rs` modules; the lib binary holds 562 tests (ADR-174, `docs/DECISIONS.md:1898`) |
| Integration | `tests/*.rs`, shared fixtures in `tests/common/mod.rs` | wiremock for Riot and the CDN, a tempfile SQLite per test | 41 files, about 380 test functions; one file per concern (`tests/riot_routes.rs`, `tests/jobs.rs`, …) |
| Snapshot | `tests/snapshots/<binary>__<test>.snap`, plus insta in `src/archive/{builds,facts}/tests.rs` and `src/events/tests.rs` | insta 1.48 (`json` feature) | 20 files, 19 814 lines |
| Replay | `tests/fixtures/replay/`, driven by `tests/replay.rs` and `tests/players_routes.rs` | recorded exchanges served by wiremock | 2 scenarios |
| Property | `src/riot/limiter/proptest.rs:161` (1 000 cases) and `src/archive/facts/tests.rs:142` | proptest 1.11; seeds in `proptest-regressions/` | 2 blocks |
| Node helpers | `tests/dev_ui.mjs`, `tests/admin_ui.mjs`, `tests/showcase.mjs` | `node --test`, run **from `cargo test`** by `tests/ui.rs:42-71` | 16, 31, 50 tests |
| jsdom pages | `tests/dom/*.test.mjs` with `admin-harness.mjs`, `fake-admin.mjs`, `fake-api.mjs` | `node --test`, jsdom 25 | 15 files, 155 tests |
| Browser layout | `tests/dom/showcase.browser.test.mjs`, `admin.browser.test.mjs` | playwright-core 1.64 and `chromium-headless-shell` | 5 + 1 tests |
| Acceptance | `acceptance/` (v1's black-box suite, vitest) | `riot-proxy serve` against `helpers/mock-riot.ts` | 26 `it()`s across five phases; the P8 run recorded 29 (`PROGRESS.md:166-170`) |

Test files may `unwrap`, `expect` and `panic`: `clippy.toml:2-4` allows it in tests and every file opens with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (`tests/riot_routes.rs:4`).

## Running

| What | Command | Source |
|---|---|---|
| Everything Rust | `just test` (= `cargo test`) | `justfile:15-17` |
| Everything Rust as CI runs it: binaries in parallel, each test its own process | `just nextest` (= `cargo nextest run --profile ci --all-features`) | `justfile:19-21`; profile `.config/nextest.toml:5-13`: `fail-fast = false`, `retries = 0`, slow timeout 60 s with a kill after 3 periods, `failure-output = "immediate-final"` |
| One integration module | `cargo test --test it riot_routes::` | every `tests/*.rs` is a module of the `it` binary (`tests/it.rs`, CI-04) |
| One test by name | `cargo test --test it riot_routes::requires_a_key`, or `cargo test requires_a_key` across binaries, or `cargo nextest run --profile ci --all-features requires_a_key` | cargo and nextest name filters |
| The lib's unit tests only | `cargo test --lib`, or one module: `cargo test --lib riot::endpoints` | |
| With output | append `-- --nocapture` (`cargo test`) or `--no-capture` (nextest) | |
| Ignored soaks and exit checks | `cargo test --release --test it limiter_soak:: -- --ignored --nocapture` (60 s, P2 exit check); `cargo test --release --test it ws_soak:: -- --ignored --nocapture` (1 000 sockets, 20 s); `cargo test --test it p6_exit:: -- --ignored --nocapture` (about 10 s, through `serve`); `cargo test --test it p7_exit:: -- --ignored --nocapture` (a crawl through `serve`) | the files' headers (`tests/limiter_soak.rs:1-3`, `tests/ws_soak.rs:1-4`, `tests/p6_exit.rs:1-11`, `tests/p7_exit.rs:1-8`); these are the 5 ignored |
| Node helper suites alone | `node --test tests/dev_ui.mjs` (also `admin_ui.mjs`, `showcase.mjs`) | `tests/dev_ui.mjs:4-5`; also run inside `cargo test --test it ui::` |
| jsdom and Chromium | `just ui-test` (= `cd tests/dom && npm ci --no-audit --no-fund && npx playwright-core install chromium-headless-shell && npm test`) | `justfile:23-25` |
| Acceptance, mock Riot | `just acceptance` (= `cargo build`, then `cd acceptance && npm ci && npm test`); `ACCEPTANCE_LIVE=1` for the real Riot API, by hand only | `justfile:49-52`; `acceptance/helpers/env.ts:19-25` |
| Lint | `just lint` (= `cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings`) | `justfile:27-30` |
| Coverage | `just cov` (= `cargo llvm-cov --all-features --workspace --summary-only`) | `justfile:32-34` |
| Postgres stub | `cargo check --all-targets --features postgres` and `cargo test --lib --features postgres db::store` | CI `clippy` job (`.github/workflows/ci.yml:36-38`) |

What CI runs in which job, and the local equivalent of each, is in [ci.md](./ci.md#running-the-same-checks-locally). `cargo test` stops at the first failing binary; nextest reports every failure, which is why CI uses it (ADR-174, `docs/DECISIONS.md:1899`).

## How Riot is mocked

- **wiremock** stands in for Riot and the Data Dragon CDN in every integration test. `common::app_with(env, upstream)` builds the whole app over a fresh `tempfile` SQLite (`Db::open(path, 2)`), a `Cdn` pointed at the wiremock URI, a `Limiter::new(0.8)`, a `Queue` with the config's kind limits, the archive, auth, hub, mirror, stats, maintenance and the analytics cache, then returns `(TempDir, AppState, Router)`; keep the `TempDir` alive for the test (`tests/common/mod.rs:25-107`). `common::app()` is the same over a dead upstream (`:26-28`). `send(router, request)` and `get(router, path)` drive it with `tower::ServiceExt::oneshot` and return a `Reply { status, headers, body }` with `.json()` (`:109-138`). `fetcher(config, upstream, limiter, archive)` builds a fetcher alone (`:140-162`).
- **The key** is `TEST_KEY = "RGAPI-test-key-not-real"` (`tests/common/mod.rs:13`), which is not key-shaped by the CI grep's definition (no 20 hex characters after `RGAPI-`).
- **Which host a request was meant for**: the client sends `x-riot-host` (`MOCK_HOST_HEADER`, `src/riot/client.rs:196`), so a test matches on it to pin a cluster: `Mock::given(path(…)).and(header("x-riot-host", "asia.api.riotgames.com"))` (`tests/riot_routes.rs:243-245`; `tests/riot_client.rs:241-248`). The acceptance mock routes every Riot host the same way (`acceptance/helpers/mock-riot.ts:7`, `297`).
- **A whole `serve` against a mock** uses `RIOT_BASE_URL` and `DDRAGON_BASE_URL`, refused in production (`src/config.rs:693-702`); the acceptance suite starts `target/debug/riot-proxy serve` that way on port 18980 with the mock on 18981 and waits up to 60 s for `/readyz` (`acceptance/helpers/setup.ts:23-54`; `acceptance/helpers/env.ts:244-275`).
- **Replay fixtures** are real exchanges, recorded with `riot record` and redacted (`tests/fixtures/replay/README.md:1-4`), loaded as `NN-<method>.json` + `.body` pairs and mounted in order so an earlier exchange wins (`tests/replay.rs:25-60`). Recording is described in [adding-a-riot-call.md](./adding-a-riot-call.md#recording-a-replay-fixture).
- **Real time, short TTLs** rather than paused time where the fetcher's SWR behaviour is under test (`tests/fetcher_states.rs:1-3`); the limiter's unit tests use `tokio::time::Instant` so `start_paused` works (`PROGRESS.md:399`).

## Fixtures

| Path | Contents | Used by |
|---|---|---|
| `tests/fixtures/replay/cold-lookup/` | a real first lookup of `Hide on bush#KR1` recorded 2026-09-24: account, summoner, league, top-3 mastery, 5 match ids, 5 matches (10 exchanges) | `tests/replay.rs`, `tests/players_routes.rs` |
| `tests/fixtures/replay/429-typed-application/` | synthetic: `cold-lookup/02` with an application-typed 429 and `Retry-After: 1`, then the 200 (`"synthetic": true`); a real application 429 is never provoked | `tests/replay.rs` |
| `tests/fixtures/matches/` | `ranked-solo.json`, `arena.json` (queue 1750), `remake.json` (derived by script) | `src/archive/facts/tests.rs` |
| `tests/fixtures/builds/` | three real OC1 ranked games with timelines, cut-down `item-16.{14,19,20}.1.json`, `*.expected.json` goldens; PUUIDs replaced by `bld-fixture-puuid-NN` | `src/archive/builds/tests.rs`, `tests/builds.rs`, `tests/showcase.mjs` |
| `tests/fixtures/v1-dump/` | a Postgres 17 `pg_dump` of v1's schema with 20 matches, 1 timeline, 12 players, 2 consumers | `tests/migrate_v1.rs` (needs `pg_restore` on `PATH`) |
| `tests/fixtures/v1.env.example` | v1's `.env.example` at `c86e631`, key redacted | the `.env.example` parity test |
| `acceptance/fixtures/match.json` | one match for the mock | `acceptance/helpers/mock-riot.ts` |
| `tests/dom/screenshots/` | PNGs written by the browser suite; git-ignored (`.gitignore`), uploaded by CI as `showcase-screenshots` | review only |

Each fixture directory has a README saying how it was made and how to re-record it.

## Snapshot policy

- Review every snapshot diff deliberately; never accept blindly (`docs/IMPLEMENTATION.md:94`; `tests/replay.rs:3`; `tests/fixtures/replay/README.md:22`).
- `tests/snapshots/openapi__openapi_document.snap` changes with every documented route; the pull request template asks for it (`PROGRESS.md:389`; `.github/PULL_REQUEST_TEMPLATE.md:21`).
- Snapshots are named `it__<module>__<test>.snap` (renamed from `<binary>__<test>.snap` by CI-04) and carry insta's header (`source`, `expression`); the 20 cover admin, analytics, auth (401/403/429 bodies), ddragon headers, the 404 envelope, the `/v1/lol` upstream calls, the OpenAPI document, the players composites, the two replays, Riot error bodies and a WebSocket session.
- Re-recording a replay fixture changes its snapshot; the players suites read the same recording, so expect those to move too.

> **TODO (unverified):** the accept workflow. No file in the repository mentions `cargo insta`, `INSTA_UPDATE` or `.snap.new`; insta's standard flow (a `.snap.new` written on mismatch, reviewed with `cargo insta review`, or `INSTA_UPDATE=always` for a deliberate bulk update) is assumed and `cargo-insta` is not installed on this machine.

## Coverage

Measured with `cargo llvm-cov` and **reported, not gated**; a module under 70 % needs a written justification in the pull request (`docs/IMPLEMENTATION.md:99`). CI's `coverage` job runs `cargo llvm-cov nextest --profile ci --all-features --workspace --summary-only` on every pull request and is not a required check (ADR-174, `docs/DECISIONS.md:1900`). Locally `just cov` needs `cargo-llvm-cov`, which is not installed here. Doctests are run by neither tool and the crate has none that matter (`docs/DECISIONS.md:1901`).

## The key-leak check

CI's `test` job runs, after the suite (`.github/workflows/ci.yml:52-53`):

```bash
! grep -rE "RGAPI-[0-9a-f-]{20,}" tests/ src/ docs/
```

Run it before pushing. `riot record` applies the same pattern and refuses to write a file that matches it or contains the literal key (`src/cli/record.rs:131-158`). `TEST_KEY` and `RGAPI-REDACTED` pass by construction.

## Browser suite prerequisites

1. Node 22 and npm (`.github/workflows/ci.yml` installs Node 22 in the `ui` job).
2. `cd tests/dom && npm ci --no-audit --no-fund` (jsdom and `playwright-core` 1.64.0 pinned, `tests/dom/package.json:9-12`).
3. `npx playwright-core install chromium-headless-shell` (`justfile:25`); CI adds `--with-deps` for the system libraries.
4. `npm test`. Without a browser the two `*.browser.test.mjs` files skip locally and fail under `CI` (`tests/dom/showcase.browser.test.mjs:12-13`; `tests/dom/admin.browser.test.mjs:6-7`). Screenshots land in `tests/dom/screenshots/`.

The three helper suites need only `node`: `tests/ui.rs` runs `node --test <file>` from the crate root, skips with `skipping: node not runnable` when it is missing locally, and panics under `CI` (`tests/ui.rs:42-64`). The same binary enforces the showcase coverage rule (`tests/ui.rs:426-467`; [adding-an-endpoint.md](./adding-an-endpoint.md#7-the-showcase-coverage-rule)).

## Known flake

`concurrent_misses_read_once` (analytics cache) flaked under llvm-cov and once under nextest; it now asserts the six reads (five before THR-03 added the slice stamp to the ETag), a 200 and the same body for each, `MISS` or `HIT` per request and at least one `MISS`, and passed 50 runs under `--stress-count` (ADR-174, `docs/DECISIONS.md:1902`). A new flake should fail CI, not pass on a retry: the `ci` profile has `retries = 0` (`.config/nextest.toml:9-10`).

## The run on 2026-10-19

`cargo test --all-features --no-fail-fast` at `52fa67d` (after CI-04 and CI-05): 5 test binaries (the lib, the bin, `it`, `telemetry` and the doc tests), 939 passed, 0 failed, 5 ignored (the four soaks and exit checks above plus the `migrate_v1` throughput test). The same suite at `7491ff6`, before CI-04, ran as 44 binaries with 938 passed. On this machine only `node --test tests/dev_ui.mjs` was re-run while writing this page (16 passed); the Rust suite was not re-run, and `just ui-test`, `just acceptance`, `just cov` and the ignored soaks were not run (they need `npm ci`, `cargo-llvm-cov`, or minutes of wall time).

The fifth ignored test is `tests/migrate_v1.rs:215`, a throughput measurement meant to run in release mode.

---
**Sources:** `docs/IMPLEMENTATION.md:L88-L99`, `L581`, `justfile:L15-L52`, `.config/nextest.toml:L1-L13`, `clippy.toml:L2-L4`, `tests/common/mod.rs:L1-L162`, `tests/riot_routes.rs:L4`, `L243-L245`, `tests/riot_client.rs:L241-L248`, `tests/replay.rs:L1-L60`, `tests/fetcher_states.rs:L1-L3`, `tests/ui.rs:L42-L64`, `L426-L467`, `tests/limiter_soak.rs:L1-L3`, `tests/ws_soak.rs:L1-L4`, `tests/p6_exit.rs:L1-L11`, `tests/p7_exit.rs:L1-L8`, `tests/dev_ui.mjs:L4-L5`, `tests/dom/package.json`, `tests/dom/showcase.browser.test.mjs:L1-L13`, `tests/dom/admin.browser.test.mjs:L6-L7`, `tests/fixtures/replay/README.md:L1-L26`, `tests/fixtures/matches/README.md`, `tests/fixtures/builds/README.md`, `acceptance/package.json`, `acceptance/helpers/env.ts:L19-L25`, `L244-L275`, `acceptance/helpers/setup.ts:L23-L54`, `acceptance/helpers/mock-riot.ts:L7`, `L297`, `src/riot/client.rs:L196`, `src/config.rs:L693-L702`, `src/cli/record.rs:L131-L158`, `src/riot/limiter/proptest.rs:L161`, `src/archive/facts/tests.rs:L142`, `.github/workflows/ci.yml:L36-L38`, `L52-L53`, `.github/PULL_REQUEST_TEMPLATE.md:L21`, `docs/DECISIONS.md:L1896-L1904` (ADR-174), `PROGRESS.md:L166-L170`, `L389`, `L399`, `L419`, `docs/_meta/scratch/tests-tooling-history.md` §1
**Last verified:** 2026-10-26 against commit `c9e1b33`

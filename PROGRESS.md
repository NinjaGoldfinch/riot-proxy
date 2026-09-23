# Progress

Legend: [ ] todo · [~] in progress (branch name) · [x] merged (#PR)

## P0 — Foundations
- [x] P0-00 bootstrap (direct to `main`, no PR — see notes)
- [x] P0-01 workspace, toolchain, CI (#1)
- [x] P0-02 config (#2)
- [x] P0-03 logging + metrics + request ids (#3)
- [x] P0-04 SQLite layer (#4)
- [x] P0-05 HTTP skeleton + health (#5)
- [ ] P0-06 CLI
- [ ] P0-07 dev tooling
Exit check: _pending_

## P1 — Riot client
- [ ] P1-01 routing
- [ ] P1-02 endpoint registry
- [ ] P1-03 HTTP client
- [ ] P1-04 rate-limit header parsing
- [ ] P1-05 dev subcommand
Exit check: _pending_

## P2 — Rate limiter
- [ ] P2-01 port v1 limiter tests first
- [ ] P2-02 windows and scopes
- [ ] P2-03 acquire (single priority)
- [ ] P2-04 observe + freeze
- [ ] P2-05 priorities
- [ ] P2-06 checkpoint/restore
- [ ] P2-07 property test + soak
Exit check: _pending_

## P3 — Cache, single-flight, fetcher
- [ ] P3-01 cache keys + key_scope
- [ ] P3-02 L1 (moka)
- [ ] P3-03 L2 (SQLite write-behind + warm)
- [ ] P3-04 single-flight
- [ ] P3-05 fetcher
- [ ] P3-06 replay harness
Exit check: _pending_

## P4 — Public surface
- [ ] P4-01 auth
- [ ] P4-02 consumer quota
- [ ] P4-03 OpenAPI scaffolding
- [ ] P4-04 `/v1/riot/*` passthrough
- [ ] P4-05 `/v1/lol/*` typed routes
- [ ] P4-06 dev UI + dashboard shells `[parallel-ok]`
Exit check: _pending_

## P5 — Archive and composites
- [ ] P5-01 archive schema
- [ ] P5-02 match archive
- [ ] P5-03 facts extraction
- [ ] P5-04 players + composites
- [ ] P5-05 admin routes (data)
Exit check: _pending_

## P6 — Scheduler, jobs, realtime
- [ ] P6-01 WebSocket hub spike (may run any time after P0)
- [ ] P6-02 events
- [ ] P6-03 durable jobs core
- [ ] P6-04 ticks
- [ ] P6-05 poll handlers
- [ ] P6-06 archive + backfill handlers
- [ ] P6-07 WS auth + wiring
- [ ] P6-08 admin routes (jobs)
Exit check: _pending_

## P7 — Data Dragon, ladder, analytics, dashboard
- [ ] P7-01 Data Dragon sync + static serving
- [ ] P7-02 ladder enumerate
- [ ] P7-03 ladder collect + archive
- [ ] P7-04 analytics
- [ ] P7-05 maintenance
- [ ] P7-06 dashboard wiring
Exit check: _pending_

## P8 — Contract, migration, packaging
- [ ] P8-01 port acceptance suite
- [ ] P8-02 `migrate-v1` subcommand
- [ ] P8-03 built-in TLS `[parallel-ok]`
- [ ] P8-04 Postgres feature flag (compile-only) `[parallel-ok]`
- [ ] P8-05 release pipeline
- [ ] P8-06 cut-over runbook
Exit check: _pending_

## Notes for the next task
- v1 reference is `NinjaGoldfinch/riot-proxy-deprecated` (cloned at `../riot-proxy-v1`, commit `c86e631`), not `ninja-recorder-deprecated` as §1 of the plan says.
- Repo is **public** (owner decision), not private.
- Toolchain pinned to 1.98.1 (`rust-toolchain.toml`); CI installs it with `rustup toolchain install`.
- rusqlite is held at **0.39** for refinery 0.9 compatibility (ADR-005). Don't bump it without checking refinery's range.
- reqwest 0.13: the feature is `rustls`, not `rustls-tls`. P1-03 must build the client with `tls_certs_only(<webpki roots>)` so `FROM scratch` needs no CA bundle (ADR-007).
- `metrics-exporter-prometheus` has default features off (no built-in HTTP listener); P0-03 renders `/metrics` from our own axum route.
- Config: `riot_proxy::config::Config::load(ConfigArgs)`; `ConfigArgs` is a `clap::Args` to `#[command(flatten)]` into `serve` in P0-06. `LOG_LEVEL` is a tracing filter string and `log_format` is already resolved (tty → pretty) for P0-03. Riot enum values (`DEFAULT_PLATFORM`, `LADDER_*`, `CACHE_TTL_OVERRIDES`) are still raw strings; P1-01, P1-02 and P7-02 must validate them at boot (ADR-008).
- Telemetry: `telemetry::init_tracing(&config)`, `telemetry::metrics_handle()` (idempotent), `telemetry::spawn_upkeep(handle)` and `telemetry::metrics_router(handle)` for P0-05 to merge. `http::request_id::request_id` goes on as the **outermost** `axum::middleware::from_fn` layer. Metric names are constants in `src/metrics.rs`; use those, not string literals.
- DB: `db::Db::open(path, readers)` (blocking) or `Db::open_async`; `db.write(|c: &mut Connection| …)` and `db.read(|c: &Connection| …)`, generic over the error type (`E: From<DbError>`). Migrations are `src/db/migrations/V000N__name.sql` (refinery naming, ADR-010); P5-01 adds `V0002__archive.sql`. `Config.database` gives the path (`Database::Sqlite(path)`).
- Error envelope (owner decision): `{error:{code,message,requestId,retryAfter?}}`. Return `http::ApiError` from handlers; `requestId` is filled in automatically (ADR-011).
- App: `app::router(AppState{config,db}, metrics_handle)` and `app::serve(listener, router, app::shutdown_signal()?)`. `src/main.rs` is a minimal serve-only entry; P0-06 replaces it with clap subcommands (`serve` flattens `ConfigArgs`). Integration-test fixtures are in `tests/common/mod.rs`.
- **For owner review at the P0 gate:** CORS was deliberately *not* added (v1 has none; ADR-011).
- v1's `scrubKey` log redaction (the `test/config.test.ts` "log redaction" cases) is not ported yet. `Secret` covers `Debug`. P1-03 (Riot client) must make sure `X-Riot-Token` is never logged, and should port those tests.
- The crate is lib + bin (`src/lib.rs`), so `tests/*.rs` can import modules.
- The musl build needs `musl-gcc`, which isn't on the dev box (no sudo), so it's verified in CI only.
- CI (owner decision): required status checks are the job names `fmt`, `clippy`, `test`, `build-musl` (not the workflow name `ci`, which GitHub never reports as a check). Docker steps in `build-musl` are added in P0-07, not before.
- v1 has no LICENSE file, so none was copied. Waiting on the owner to pick one.
- `acceptance/` is v1's suite verbatim (plus its `vitest.acceptance.config.ts`); it hits the real Riot API and is ported in P8-01.

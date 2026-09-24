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

## Owner review at the P0 gate — resolved 2026-09-24 (ADR-014)
- CORS deferred (off, as v1). License MIT. New metrics use design names without the `proxy_` prefix. Bootstrap-to-stderr and the `NODE_ENV` fallback are confirmed.

## Notes for the next task
- Dev CLI: `just riot account/by-riot-id europe 'Hide on bush' KR1`. A real dev key is in `./.env` (gitignored; dev keys expire every 24 h).
- Headers: `riot::limiter::headers::{RateLimitHeaders::from_headers, parse_limits, parse_counts, RateLimitType, BOOTSTRAP_APP_LIMITS}`. The client still reports the 429 type as a raw string; P2-04 can convert with `RateLimitType::parse`.
- Service-429 backoff (owner, ADR-021): use **v1's numbers**, 500 ms × 2ⁿ capped at 8 s, ±20 %, 3 tries, implemented in the fetcher (P3-05), not the client.
- Client: `RiotClient::new(&cfg)` / `with_base_url(&cfg, mock_uri)`; `RiotRequest::new(ep, target, &params)?.query(k, Some(v))?`; `client.send(&req) -> Result<RiotResponse, RiotError>` (errors carry `headers`). **P3-05 must port v1's retry policy** (ADR-017 lists the exact numbers).
- Endpoints: `riot::endpoints::{ENDPOINTS, Endpoint::by_id, Endpoint::path(&[..]), target_for_platform/region, TtlPolicy::from_config(&cfg).ttls(ep)}`. When the fetcher is wired (P3-05), `serve` should log `ineffective_overrides()` at warn.
- `X-Cache` for negative hits is **`HIT-NEG`** (owner, ADR-022), not design/03's `NEG`. That includes the P3 exit check list.
- Routing: `riot::routing::{Platform, Region}` with `region()`, `account_region()` (sea→asia), `host()`, `parse()` → `BAD_REGION`, and `Platform::from_match_id`. Config's `default_platform` and `ladder_platforms` are typed.
- v1 reference is `NinjaGoldfinch/riot-proxy-deprecated` (cloned at `../riot-proxy-v1`, commit `c86e631`), not `ninja-recorder-deprecated` as §1 of the plan says.
- Repo is **public** (owner decision), not private.
- Toolchain pinned to 1.98.1 (`rust-toolchain.toml`); CI installs it with `rustup toolchain install`.
- rusqlite is held at **0.39** for refinery 0.9 compatibility (ADR-005). Don't bump it without checking refinery's range.
- reqwest 0.13: the feature is `rustls`, not `rustls-tls`. Every reqwest client must use `tls_certs_only(...)` (webpki roots, or empty for plain HTTP), or it fails to build in `FROM scratch` (ADR-007/013).
- `metrics-exporter-prometheus` has default features off (no built-in HTTP listener); P0-03 renders `/metrics` from our own axum route.
- Config: `riot_proxy::config::Config::load(ConfigArgs)`; `ConfigArgs` is a `clap::Args` to `#[command(flatten)]` into `serve` in P0-06. `LOG_LEVEL` is a tracing filter string and `log_format` is already resolved (tty → pretty) for P0-03. Platforms are typed and validated at boot (P1-01). `LADDER_QUEUES`/`LADDER_TIER_FLOOR` are still raw strings; P7-02 must validate them at boot (ADR-008).
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

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
Exit check: _pending_

## P5 — Archive and composites
- [x] P5-01 archive schema (#40)
- [x] P5-02 match archive (#PR)
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
- [ ] P7-04 analytics (**+ v1's three `/v1/lol/analytics/*` routes, deferred from P4-05 by the owner, ADR-037**)
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
- **P4 exit check is still open:** the owner needs to confirm `/docs` renders in a browser (headless Chromium crashed on Scalar here). Tag `phase-P4` after that.
- **Ask the owner at P7-04:** v1's analytics routes read tables design/04 doesn't have (bans, runes, spells, analytics slices; ADR-039).
- Routes: follow `src/routes/riot.rs`, which uses `http::validate::*` for v1 rules and `routes::passthrough::{respond, options}`. Add read routers to `routes::docs::api_router`'s `read` group so they get auth+quota. `tests/common::app_with(env, wiremock_uri)` gives a full app. v1's `/v1/lol` bodies are raw passthrough too (`PassthroughResponse`), so P4-05's "typed" part is request validation.
- OpenAPI: add routes to `routes::docs::api_router()` as `OpenApiRouter`s with `#[utoipa::path]` handlers. `just`/CI check: `cargo run -- spec > /tmp/spec.json && scripts/compare-openapi.py docs/contract/v1-openapi.json /tmp/spec.json --prefix /v1/riot/ --prefix /v1/lol/` (16 missing at P4-03). The `tests/snapshots/openapi__openapi_document.snap` changes with every documented route; review it.
- Disk: `target/` reached ~26 GB with debug, release and feature builds and hit the session's disk allowance. `rm -rf target/release target/debug/incremental` frees ~10 GB.
- Auth: protect routers with `.route_layer(axum::middleware::from_fn_with_state(state.clone(), http::auth::require_read))` (or `require_admin`); handlers take `Extension<Arc<http::auth::Consumer>>` (has `quota_per_min` for P4-02). `AppState.auth: Arc<Auth>`; call `auth.invalidate(hash)` on admin revoke (P5-05).
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

# Repository inventory (Phase 0 discovery)

What is in the riot-proxy repository at commit `7491ff6` (version 2.7.0), gathered on 2026-10-19 before the documentation set was written. The seven detailed evidence inventories under [scratch/](./scratch/) carry file and line citations for every claim; this page is the map.

## Language, runtime, tooling

| Item | Value | Source |
|---|---|---|
| Language | Rust, edition 2024, `rust-version = "1.98"`; toolchain pinned to 1.98.1 with rustfmt, clippy and the `x86_64-unknown-linux-musl` target | `Cargo.toml`, `rust-toolchain.toml` |
| Framework | axum 0.8 on tokio 1.53; rusqlite 0.39 (bundled SQLite 3, `hooks`); moka; reqwest 0.13 (rustls); object_store 0.14; utoipa 6 | `Cargo.toml`, `Cargo.lock` |
| Package manager and lockfiles | cargo, `Cargo.lock` committed; npm for `tests/dom/package-lock.json` and `acceptance/package-lock.json` (Node 22) | repo root, `.github/workflows/ci.yml` |
| Task runner | `just` (`justfile`: dev, test, nextest, ui-test, lint, cov, musl, docker, riot, acceptance) | `justfile` |
| Features | `dev-cli` (raw Riot calls, fixture recording; never in releases), `postgres` (compile-only stub) | `Cargo.toml:52-56` |
| Build identity | `build.rs` embeds `GIT_SHA` (12 chars), `rustc --version` and the build time for `/v1/admin/info` | `build.rs` |
| Size | 57 471 lines under `src/` (Rust, SQL, HTML); 218 commits; 23 tags | `wc`, `git` |

## Directory tree (depth 3)

| Path | Purpose |
|---|---|
| `src/` | the crate: `main.rs` dispatches to `cli/`; `lib.rs` lists the modules |
| `src/app.rs`, `src/http/` | router, middleware, auth, quota, CORS, errors, validators, request ids, recent rings |
| `src/routes/`, `src/routes/{admin,analytics,players}/` | one handler file per prefix; `docs.rs` registers routes and generates OpenAPI; `ui.rs`, `dev.rs`, `statics.rs`, `ws.rs`, `health.rs` |
| `src/riot/`, `src/riot/limiter/` | endpoint registry, routing, the reqwest client, the sliding-log limiter and its checkpoint |
| `src/fetcher.rs`, `src/singleflight.rs`, `src/cache/` | the read funnel, in-flight coalescing, L1 and L2 caches, key scheme |
| `src/archive/` and its `*/tests.rs` dirs | bodies, facts, builds, tiers, ranks, player pools, analytics rebuilds, packs, object store, offload, index recovery |
| `src/jobs/` and `src/jobs/{activity,ladder,offload,scheduler,ticks,timelines}/` | scheduler, lanes, ticks, every job handler, live activity traces |
| `src/db/`, `src/db/migrations/` | writer thread, reader pool, disk gauges, the Postgres stub, 30 refinery migrations `V0001`…`V0030` (next free `V0031`) |
| `src/events/`, `src/ws/` | event catalogue and the WebSocket hub, socket, protocol, metrics topic |
| `src/static/`, `src/ui/` | Data Dragon mirror and image fill; the three embedded pages (`dev-ui.html`, `showcase.html`, `admin.html`) |
| `src/config.rs`, `src/config/tests.rs` | the configuration surface and its validation |
| `src/consumers.rs`, `src/players.rs`, `src/stats.rs`, `src/metrics.rs`, `src/telemetry.rs`, `src/clock.rs`, `src/build_info.rs` | consumer storage, player rows, dashboard sampling, metric catalogue, tracing and Prometheus, clock helpers, build info |
| `tests/`, `tests/common/`, `tests/fixtures/{builds,matches,replay,v1-dump}`, `tests/snapshots/` | 41 integration-test modules compiled into one `it` binary (`tests/it.rs`, CI-04) plus `telemetry` on its own, shared fixtures, recorded Riot exchanges, insta snapshots named `it__<module>__<test>.snap` |
| `tests/dom/`, `tests/dom/screenshots/` | jsdom and headless-Chromium suites for the three pages |
| `acceptance/`, `acceptance/{fixtures,helpers}` | v1's vitest black-box suite with a mock Riot |
| `proptest-regressions/riot/limiter/` | persisted proptest seeds |
| `deploy/`, `deploy/proxmox/`, `deploy/proxmox/files/` | the production replica timer and README; the Proxmox dev VM kit |
| `ops/`, `ops/grafana/` | Prometheus alert rules and the Grafana dashboard |
| `scripts/` | CI helpers (image description, edge tags, docker paths) with self-tests; `compare-openapi.py`; `perf-synthetic.py` |
| `docs/design/`, `docs/design/img/` | the v2 design (01–13, metrics.md), the authority |
| `docs/contract/` | v1's OpenAPI document and endpoint list for parity checks |
| `docs/flt/`, `docs/perf/`, `docs/thr/` | per-task plans and measurements (FLT-03..05, PERF-03, THR-03, THR-04) |
| `docs/adr/` | empty (`.gitkeep`); ADRs live in `docs/DECISIONS.md` |
| `docs/img/` | two orphaned v1-dashboard screenshots |
| `docs/` (this set) | `README.md`, `architecture/`, `integration/`, `api/`, `data/`, `workflows/`, `deployment/`, `operations/`, `reference/`, `_meta/` |
| `.github/workflows/` | `ci.yml`, `edge.yml`, `release.yml`; `.github/PULL_REQUEST_TEMPLATE.md` |
| `.config/nextest.toml` | the `ci` nextest profile |
| `Dockerfile`, `Dockerfile.release`, `docker-compose.yml` | source image, release image, compose stack |
| `data/` (ignored) | a local dev database and Data Dragon mirror |

## Entry points

| Entry | Where |
|---|---|
| Binary `riot-proxy` with subcommands `serve`, `migrate`, `key {create,list,revoke}`, `healthcheck`, `spec`, `migrate-v1`, `backup`, `packs {reindex,restore-index,snapshot}`, and `riot {get,record}` (dev-cli) | `src/cli/mod.rs:34-66` |
| Server bootstrap (23 ordered steps) | `src/cli/serve.rs:43-417`, summarised in [scratch/jobs-and-observability.md](./scratch/jobs-and-observability.md) §1 |
| HTTP routes (76 documented operations on 70 paths plus docs, pages, dev reset, `/ddragon` files, fallback) | registered in `src/routes/docs.rs:116-141`; table in [scratch/routes.md](./scratch/routes.md) §1 and [../api/endpoints.md](../api/endpoints.md) |
| WebSocket `/v1/ws` | `src/routes/ws.rs:497-499`, protocol in `src/ws/protocol.rs` |
| Ticks (periodic enqueues): `poll:live` 60 s, `poll:rank` 600 s, `poll:matches` 300 s, `ddragon:sync` 3600 s, `maintenance` and `names:backfill` daily, `ladder:crawl` on `LADDER_CRAWL_S`, `aggregate:analytics` on `AGGREGATE_INTERVAL_S`, `aggregate:delta` on `AGGREGATE_DELTA_S` (60 s), `timelines:backfill` hourly when enabled | `src/jobs/ticks.rs:108-139` |
| 22 job kinds with handlers | `src/jobs/mod.rs:20-71,97-174`; table in [scratch/jobs-and-observability.md](./scratch/jobs-and-observability.md) §3 |
| Loops: limiter checkpoint 10 s, L2 flush 2 s, metrics history 60 s (fast sample 15 s), `jobs_pending` sample 15 s, `metrics` topic 5 s while held, histogram upkeep 5 s | `src/cli/serve.rs`, `src/riot/limiter/persist.rs:25`, `src/cache/l2.rs:24-26`, `src/stats.rs:1041-1072` |

## Outbound network calls

| Target | Mechanism | Auth | Timeouts and retries |
|---|---|---|---|
| Riot API `*.api.riotgames.com` (18 endpoints) | reqwest, HTTP/2 capable, gzip | `X-Riot-Token` | connect 10 s, total 25 s; retries in the fetcher (5xx ×2, untyped 429 ×3 with backoff, typed 429 freeze) |
| Data Dragon `ddragon.leagueoflegends.com`, `static.developer.riotgames.com` | reqwest | none | connect 15 s, total 60 s; job retry |
| S3-compatible bucket (`ARCHIVE_OBJECT_URL`) | object_store `aws` | access key pair | crate defaults (unverified); job retry |
| Nothing else: no calls to the site, to ninja-monitor, to webhooks | grep | | |

Details in [scratch/riot-and-cache.md](./scratch/riot-and-cache.md) §1–§2 and [scratch/persistence.md](./scratch/persistence.md) §4.

## Persistence

| Store | What | Where documented |
|---|---|---|
| SQLite `$DATA_DIR/riot-proxy.db` (WAL, `synchronous=NORMAL`), 43 tables after `V0030` | consumers, cache (L2), limiter checkpoint, jobs, metrics history, players, matches, timelines, facts, bans, builds, timeline gaps, ladder crawls and entries, ranks, analytics tables, pack map | [../data/schema.md](../data/schema.md), [scratch/persistence.md](./scratch/persistence.md) |
| In-memory | L1 response cache (moka, `CACHE_L1_MAX_MB`), auth cache, analytics response cache, recent rings, job activity traces | [../data/caching.md](../data/caching.md) |
| Disk | `$DATA_DIR/ddragon/<version>/…`, `$DATA_DIR/backups/riot-proxy-<date>.db` | [scratch/persistence.md](./scratch/persistence.md) §5 |
| Onidel Object Storage `riot-proxy-archive` | `packs/{matches,timelines}/<yyyy>/<mm>/<ulid>.pack`, `index/{packs,locations}-<date>.jsonl.zst` | [../integration/object-storage.md](../integration/object-storage.md) |
| Cloudflare R2 `riot-proxy-archive` (replica), `ngf-backups/riot-proxy/` (SQLite backups) | written by rclone timers on the box, never by the proxy | [../deployment/backup-and-restore.md](../deployment/backup-and-restore.md) |

## CI/CD

Three workflows, no deploy step: `ci.yml` (fmt, clippy, test, ui, coverage, acceptance, build-musl, docker, ops), `edge.yml` (`:edge` image per push to `main`), `release.yml` (`v*` tag to binaries, image and GitHub release). Only `GITHUB_TOKEN` is used. See [../workflows/ci.md](../workflows/ci.md).

## Configuration surface

68 variables read from flags, environment and `.env` (`src/config.rs:23-94`), plus the legacy `NODE_ENV` fallback and the `TLS` refusal; ten global CLI flags. Complete table in [../deployment/configuration.md](../deployment/configuration.md), evidence in [scratch/config.md](./scratch/config.md).

## Build, test, lint, release, deploy tooling

`just` recipes; `cargo fmt`, `clippy -D warnings`; `cargo test` or `cargo nextest run --profile ci`; `cargo llvm-cov`; `cargo build --release --target x86_64-unknown-linux-musl` (24 MiB cap); `Dockerfile` and `Dockerfile.release`; `deploy/riot-proxy-replica.{service,timer}` and `deploy/test.sh`; `deploy/proxmox/create-vm.sh` and its files; the systemd unit for the proxy exists only as a template in `docs/design/07-deployment.md`; no nginx or Caddy configuration (Cloudflare Tunnel is the ingress). Details in [scratch/tests-tooling-history.md](./scratch/tests-tooling-history.md) §2–§3.

## Existing documentation

`README.md` (front door), `CLAUDE.md` (working rules), `docs/IMPLEMENTATION.md` (the plan and task list), `PROGRESS.md` (ticks and exit checks), `docs/DECISIONS.md` (ADRs numbered to ADR-185), `docs/CUTOVER.md` (v1 to v2, historical), `docs/design/01-13` and `metrics.md` (the design authority), `deploy/README.md` and `deploy/proxmox/README.md` (operator how-tos), `docs/flt/`, `docs/perf/`, `docs/thr/` (task plans), `tests/fixtures/*/README.md`. The map with keep/supersede notes is in [scratch/tests-tooling-history.md](./scratch/tests-tooling-history.md) §5. This set supersedes none of them; it describes what is built and links to them.

## Tests

5 test binaries under `cargo test` (the lib, the bin, `it` with 41 `tests/*.rs` modules, `telemetry`, and the doc tests), 939 tests passing, 0 failing and 5 ignored on 2026-10-19 at `52fa67d` (44 binaries and 938 passing at `7491ff6`, before CI-04); three Node layers (helper `.mjs` files run from `tests/ui.rs`, jsdom suites, Chromium layout suites); v1's acceptance suite in mock mode. How to run each is in [../workflows/testing.md](../workflows/testing.md).

## Git history highlights

218 commits from 2026-09-23 (`chore: bootstrap riot-proxy v2`) to 2026-10-19; phases P0–P8 tagged `phase-P<n>` between 2026-09-24 and 2026-10-04; `v2.0.0` on 2026-10-10 after four release candidates; `v2.1.0` to `v2.7.0` between 2026-10-11 and 2026-10-19. Post-release series: OPS, DEV, SCH, INC, LAD, SITE, BLD, TL, FLT, DSK, HOST, ADM, OBJ, PERF, CI. The busiest day was 2026-10-13 (the hosting move, DSK-01..07, ADM-00..17, OBJ-00: about 32 pull requests). Every commit since the bootstrap carries a `(#N)` pull request number. Details in [scratch/tests-tooling-history.md](./scratch/tests-tooling-history.md) §7.

## Sibling repositories

`/home/dev/projects/ninjagoldfinch-lol` (the site, read for the boundary), `/home/dev/projects/ninja-monitor` and `/home/dev/projects/ninjagoldfinch.lol` (a GitHub Pages placeholder) were available on this machine and read; findings in [scratch/siblings.md](./scratch/siblings.md). `/home/dev/projects/riot-proxy-v1` (the retired v1) was not re-read; its contract artefacts are in `docs/contract/`.

## Commands used for discovery

```bash
git fetch origin main && git pull --ff-only origin main      # 7491ff6, later merged to 52fa67d
cargo build --all-features                                   # ok
target/debug/riot-proxy --help; … <subcommand> --help       # CLI surface
RIOT_API_KEY=fake target/debug/riot-proxy spec > docs/_meta/scratch/openapi-generated.json
CARGO_INCREMENTAL=0 cargo test --all-features --no-fail-fast        # at 52fa67d
# 5 binaries, 939 passed, 0 failed, 5 ignored (44 binaries / 938 passed at 7491ff6 before CI-04)
# scratch run: ENV=development AUTH_DISABLED=true HOST=127.0.0.1 PORT=18080 DATA_DIR=/tmp/rp-smoke/data riot-proxy serve
curl localhost:18080/{healthz,readyz,metrics,docs,dev,dashboard,v1/admin/info,v1/static/versions}
npx -y @redocly/cli lint docs/api/openapi.json               # 1 error (duplicate operationId), 6 warnings
cargo tree --depth 1 --all-features
```

---
**Sources:** `Cargo.toml`, `rust-toolchain.toml`, `justfile`, `src/lib.rs`, `src/cli/mod.rs:L34-L66`, `src/cli/serve.rs:L43-L417`, `src/routes/docs.rs:L116-L141`, `src/jobs/mod.rs:L20-L71`, `src/jobs/ticks.rs:L108-L139`, `src/config.rs:L23-L94`, `.github/workflows/*.yml`, `docs/_meta/scratch/*.md`, `git log`, `git tag`
**Last verified:** 2026-10-26 against commit `c9e1b33`

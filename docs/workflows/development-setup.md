# Development setup

From a clean machine to a running local `riot-proxy serve`, a minted key, a first request and a green test run. Every command below was re-run on 2026-10-19 unless the section says otherwise (see [What was verified on this machine](#what-was-verified-on-this-machine)).

## Required tools

| Tool | Version | Why | Install |
|---|---|---|---|
| `rustup` with the pinned toolchain | Rust `1.98.1`, components `rustfmt` and `clippy`, target `x86_64-unknown-linux-musl` (`rust-toolchain.toml:1-5`) | the only build tool; `rustup` reads the pin and installs it on first `cargo` call | [rustup.rs](https://rustup.rs) |
| `just` | any recent (1.58.0 here) | the task runner; `just --list` prints every recipe (`justfile:8-9`) | `cargo install just` or a package |
| Node 22 and npm | CI uses Node 22 (`.github/workflows/ci.yml`, `ui` and `acceptance` jobs); 22.23.2 / 10.9.8 here | `tests/dom` (jsdom, playwright-core; `tests/dom/package.json:1-13`), `acceptance/` (vitest; `acceptance/package.json:1-17`), and the three helper suites `tests/ui.rs` runs with `node --test` (`tests/ui.rs:42-64`) | nodejs.org or a version manager |
| Chromium headless shell | installed by `npx playwright-core install chromium-headless-shell` (`justfile:25`) | the browser layout suites in `tests/dom/*.browser.test.mjs`; without it they skip locally and fail in CI (`tests/dom/showcase.browser.test.mjs:12-13`) | part of `just ui-test` |
| `cargo-nextest` (optional) | 0.9.148 here | `just nextest`, the way CI runs the Rust suite (`justfile:19-21`, `.config/nextest.toml`) | `cargo install cargo-nextest` |
| `cargo-llvm-cov` (optional) | not installed here | `just cov` (`justfile:32-34`); coverage is reported, not gated | `cargo install cargo-llvm-cov` |
| `musl-tools` (optional) | not installed here | `just musl`, the static release binary (`justfile:36-39`); CI builds it, so a dev box can skip it (`PROGRESS.md:420`) | `apt install musl-tools` |
| `sqlite3` CLI (optional) | not installed here | per-table sizes through `dbstat`, which the bundled SQLite lacks (ADR-135, `docs/DECISIONS.md:1457`) | a package |
| Docker (optional) | not installed here | `just docker` and the Proxmox dev VM; image behaviour is verified in CI only (`PROGRESS.md:422`) | a package |

Nothing else is needed: there is no Redis, no Postgres and no Node runtime in the service itself (`README.md:3`).

## Clone and configure

1. Clone the public repository and enter it: `git clone https://github.com/NinjaGoldfinch/riot-proxy.git && cd riot-proxy`.
2. Copy the documented configuration: `cp .env.example .env` (`README.md:62-63`). `.env` is git-ignored (`.gitignore`), and `.env.example` is the authoritative list of variables: a config test asserts it documents exactly the set the binary reads (`src/config/tests.rs:433-452`).
3. Set `RIOT_API_KEY=` in `.env` to a personal development key from [developer.riotgames.com](https://developer.riotgames.com/). The key must be at least 8 characters (`src/config.rs:623-627`); development keys expire every 24 hours (`PROGRESS.md:400`). Never commit it and never paste it into a fixture: CI greps `tests/`, `src/` and `docs/` for key-shaped strings (`.github/workflows/ci.yml:52-53`).
4. Leave everything else commented out. A commented line shows the default; an empty value (`FOO=`) also means the default (`.env.example:1-3`; `src/config.rs:887-893`).

Precedence is CLI flag, then environment, then `.env`, then the built-in default (`src/config.rs:3`, `.env.example:2`). The binary reads `.env` itself from the **current working directory**, so `just` deliberately does not pre-load it (`justfile:3`; `src/config.rs:581-595`).

## First run

```bash
just dev            # = cargo run -- serve  (justfile:11-13)
# or, with a built binary:
./target/debug/riot-proxy serve
```

The first `serve` on an empty `DATA_DIR` prints, on stdout as JSON (pretty when stdout is a terminal, `src/config.rs:662-669`):

1. refinery's lines: `schema history table is empty, going to apply all migrations`, then `going to batch apply 27 migrations in single transaction.` naming `V1__init` to `V27__matchup_builds` (`src/db/mod.rs:22-24`, `294-296`).
2. `database ready` with `path=<DATA_DIR>/riot-proxy.db` (`src/cli/serve.rs:52-55`).
3. `bootstrap admin consumer created` with its ULID id (`src/cli/serve.rs:57-68`).
4. `limiter checkpoint restored rows=0`, `L1 warmed from L2 entries=0`, `listening addr=<HOST>:<PORT>`, `starting job workers workers=<JOB_CONCURRENCY> kind_limits=…` (`src/cli/serve.rs:76-93`, `143-148`, `340-345`).
5. The ticks that fire immediately at boot (`src/jobs/ticks.rs:1-4`): `ddragon:sync` (downloads the current patch from `ddragon.leagueoflegends.com` into `DATA_DIR/ddragon`), `maintenance` (writes `DATA_DIR/backups/riot-proxy-<date>.db` and logs `backup written`, `maintenance done`) and `names:backfill` (`player names backfilled from archive`).

On **stderr**, and only when `BOOTSTRAP_ADMIN_KEY` was not set, the one-time banner (`src/cli/serve.rs:421-427`):

```text

  bootstrap admin key (shown once): rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM
  Scopes read,admin. Store it now; it cannot be recovered.
  Mint more keys with: riot-proxy key create --name <name>

```

Store it: only its SHA-256 is kept (`src/routes/docs.rs:98-99`). A second `serve` against the same `DATA_DIR` logs `going to batch apply 0 migrations` and prints no banner.

### The pages

With the defaults every page is on (`src/config.rs:677-679`, `778-781`):

| Page | What | Key needed |
|---|---|---|
| `http://127.0.0.1:8080/docs` | the OpenAPI reference (Scalar) and `/openapi.json`, `/openapi.yaml` (`src/routes/docs.rs:373-387`) | no |
| `/dev` and `/dev/showcase` | the dev explorer and the example frontend; never served when `ENV=production` (`README.md:88`; `src/config.rs:677-679`) | the page loads without one; its API calls need a read key unless `AUTH_DISABLED` (`docs/design/11-showcase.md:15`) |
| `/dashboard` | the operational dashboard; inert without an admin key (`.env.example:177-178`) | admin key in the page |
| `/healthz`, `/readyz`, `/metrics` | ops routes | no |

### Mint a key and make the first request

Every CLI subcommand except `spec` loads the config first, so run them from the directory holding `.env` (or export `RIOT_API_KEY`) and with the same `DATA_DIR` as the server (`src/cli/mod.rs:82-88`).

```bash
riot-proxy key create --name local-site                 # scopes default to read (src/cli/key.rs:13-20)
riot-proxy key create --name local-admin --scopes read,admin
riot-proxy key list                                     # ID NAME SCOPES QUOTA STATUS; keys are never shown
riot-proxy key revoke local-site
```

`key create` prints a block ending in `API KEY   rpx_…` and `This key is shown once and cannot be recovered. Store it now.` (`src/cli/key.rs:46-57`). A key is `rpx_` plus 32 base64url characters, 36 in all (`src/consumers.rs:12`, `95-96`); the default quota is 600 requests a minute (`src/consumers.rs:14`).

```bash
KEY=rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM
curl -s http://127.0.0.1:8080/v1/admin/info                       # 401 without a key
curl -s -H "Authorization: Bearer $KEY" http://127.0.0.1:8080/v1/admin/info   # admin scope: version, gitSha, features, config
curl -s -i -H "Authorization: Bearer $KEY" \
  'http://127.0.0.1:8080/v1/riot/accounts/by-riot-id/Hide%20on%20bush/KR1'    # read scope: Riot's body, X-Cache, X-Cache-Age
```

The error envelope is `{"error":{"code":"UNAUTHORIZED","message":"Missing or invalid API key","requestId":"…"}}`; a read key on an admin route answers `FORBIDDEN` with `This key lacks the 'admin' scope` (observed; codes in [../api/errors.md](../api/errors.md)). Every response carries `X-Request-Id` and the per-key `X-RateLimit-*` headers.

## Local frontend work: `AUTH_DISABLED=true`

For a frontend on the same machine, skip consumer keys entirely:

```bash
AUTH_DISABLED=true just dev
```

Every request then runs as a synthetic `dev-local` consumer with read and admin scope, and `serve` warns `AUTH_DISABLED=true: every request runs as the synthetic dev-local consumer (read+admin)` (`src/http/auth.rs:201-204`). It is refused with `ENV=production` (`src/config.rs:671-675`).

### Pointing ninjagoldfinch.lol at it

The site (`NinjaGoldfinch/ninjagoldfinch-lol`, SvelteKit) reads `PROXY_URL` (required, no default) and `PROXY_KEY` (optional unless `NODE_ENV=production`; sent as `Authorization: Bearer` only when set) from its own `.env`, created by its `just setup` from `.env.example` (site `src/lib/server/env.ts:10-15`, `47-55`; see [../integration/service-communication.md](../integration/service-communication.md)). For local work:

```ini
PROXY_URL=http://127.0.0.1:8080
# PROXY_KEY=            optional while the proxy runs with AUTH_DISABLED=true; otherwise a read key
```

The LAN dev instance `http://192.168.68.46:8080` that older site notes name was the Proxmox dev VM (VM 102, `docs/IMPLEMENTATION.md:434`); it is not running and must not be assumed. Local development runs against a locally started `serve`.

## Local or stubbed object storage

Packed match and timeline bodies go to object storage only when `ARCHIVE_OBJECT_URL` is set; unset keeps every body in SQLite (`.env.example:82-84`). Outside production a local directory stands in for a bucket, so no MinIO or cloud account is needed:

```bash
ARCHIVE_OBJECT_URL=file:///absolute/path/to/bucket just dev
```

`file://` must be absolute and is refused when `ENV=production`; `s3://` needs `ARCHIVE_OBJECT_ENDPOINT` and, in production, both keys (`src/config.rs:900-926`, `1003-1015`; [../deployment/configuration.md](../deployment/configuration.md) lists every `ARCHIVE_OBJECT_*` variable). With a `file://` store `serve` logs `object store configured store=file:///…` and `queued archive:offload`, and the directory fills with `packs/…` and `index/…` exactly as a bucket would (`src/archive/objects.rs:102-113`). Verified here: after boot the directory held `index/packs-<date>.jsonl.zst` and `index/locations-<date>.jsonl.zst`.

## Talking to Riot without Riot

| Need | Use |
|---|---|
| The whole server against a scripted Riot | `just acceptance` (`justfile:49-52`): builds, then `acceptance/helpers/mock-riot.ts` stands in for every Riot host (the proxy names the host it meant in `x-riot-host`, `mock-riot.ts:7`) and `serve` starts on port 18980 pointed at the mock on 18981 with a known admin key (`acceptance/helpers/env.ts:19-25`, `244-275`; `helpers/setup.ts:23-54`). `ACCEPTANCE_LIVE=1` is the real-Riot mode and spends your key's quota. |
| Pointing any `serve` at a mock | `RIOT_BASE_URL` and `DDRAGON_BASE_URL`; both refused in production (`src/config.rs:688-697`; `.env.example:167-170`) |
| Recorded real exchanges | `tests/fixtures/replay/` (`tests/fixtures/replay/README.md`), served by wiremock in `tests/replay.rs` and `tests/players_routes.rs` |
| One raw call with your key | `just riot account/by-riot-id europe 'Hide on bush' KR1` = `cargo run --features dev-cli -- riot get …` (`justfile:45-47`). Endpoint by id or slug, routing a platform or region, `-q k=v` for queries; it bypasses the cache and the limiter, so it counts against the key (`src/cli/riot.rs:1-3`, `15-32`, `63-81`). `riot record --out <dir> …` writes a redacted fixture ([adding-a-riot-call.md](./adding-a-riot-call.md)). |

## A quiet local instance

The defaults already schedule no crawl and no full analytics recompute (`LADDER_CRAWL_S=0`, `LADDER_PLATFORMS` empty, `AGGREGATE_INTERVAL_S=0`, `TIMELINE_BACKFILL_PATCHES=0`; `src/jobs/ticks.rs:123-137`), and the poll ticks fan out only over tracked players (`src/jobs/ticks.rs:44-74`). What still runs at boot is `ddragon:sync` (hourly, first tick immediate, real CDN), `maintenance` and `names:backfill` (daily), and `aggregate:delta` every 60 s (`AGGREGATE_DELTA_S`, `src/jobs/ticks.rs:130-131`); with `LADDER_PLATFORMS` empty it has no ladder to count, so it only runs its `builds:extract` pass (`src/jobs/analytics.rs:779-784`). For a quiet box:

```ini
LADDER_CRAWL_S=0          # already the default; stated for clarity
AGGREGATE_DELTA_S=0       # no aggregate:delta tick (src/config.rs:769)
JOB_CONCURRENCY=1         # one worker (also --job-concurrency)
BACKUP_KEEP=0             # no backup file in data/backups at every boot (src/config.rs:742)
LOG_LEVEL=riot_proxy=debug,tower_http=info   # a tracing filter (.env.example:17-18)
```

Setting `DDRAGON_BASE_URL=http://127.0.0.1:9` keeps it fully offline: the sync then fails (`job failed job=ddragon:sync … next=pending`) and retries with backoff, and `/v1/static/*` has nothing to serve.

## The `data/` directory

`DATA_DIR` (default `./data`, created at config load, `src/config.rs:602`, `809-814`) holds everything (`src/cli/serve.rs:211`; `src/config.rs:716-718`):

```text
data/
├── riot-proxy.db            the SQLite file (WAL mode)
├── riot-proxy.db-wal        the write-ahead log; never delete it by hand
├── riot-proxy.db-shm        shared memory for WAL readers
├── backups/
│   └── riot-proxy-YYYY-MM-DD.db   daily VACUUM INTO copies, newest BACKUP_KEEP kept
└── ddragon/
    ├── <version>/           champion.json, item.json, … and img/<kind>/<file> on first request
    └── meta/queues.json
```

`/data`, `*.db*` and `/backups` are git-ignored (`.gitignore`). A `file://` object store lives wherever its URL points.

## Disk space

- A debug build with every test binary is large: `target/` reached about 26 GB with debug, release and feature builds (`PROGRESS.md:390`); `rm -rf target/release target/debug/incremental` frees about 10 GB. On this box `target/` is 3.5 GB after a single `--all-features` debug build whose `riot-proxy` binary alone is 104 MB.
- `Cargo.toml` sets `[profile.dev] debug = "line-tables-only"` and `[profile.dev.package."*"] debug = false` (CI-05, ADR-177), so dev and test builds keep file and line in backtraces without full DWARF; `target/` after a full `--all-features` test build is about 3.5 GB rather than the 15 GB seen before that change. `CARGO_PROFILE_DEV_DEBUG=0` is no longer needed.
- A worktree has its own `target/`; old worktrees keep theirs until deleted.

## Running the tests

`just test`, `just nextest`, `just lint`, `just ui-test` and `just acceptance` are described in [testing.md](./testing.md); the CI equivalents in [ci.md](./ci.md#running-the-same-checks-locally).

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `error: binding 127.0.0.1:8080: Address already in use (os error 98)` | another `serve` (or anything) on the port (`src/cli/serve.rs:143-148`) | stop it, or `PORT=8081` / `--port 8081` |
| `error: Invalid environment configuration:` then `  RIOT_API_KEY: must be at least 8 characters` | a placeholder or truncated key (`src/config.rs:623-627`) | set a real development key |
| `  RIOT_API_KEY: is required` although `.env` has one | the command ran from another directory: `.env` is read relative to the process cwd (`src/config.rs:581-595`) | `cd` to the directory with `.env`, or export the variable |
| `key list` shows no keys although `serve` printed a banner | a different `DATA_DIR` | pass the same `DATA_DIR` (or `--data-dir`) to every command |
| `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy` | a v1 `.env` with `TLS=true` (`src/config.rs:681-686`) | remove it; terminate HTTPS in front (`README.md:118`) |
| `DATABASE_URL: postgres:// needs a build with the postgres feature` | a v1 `DATABASE_URL` (`src/config.rs:641-646`) | remove it; SQLite only |
| `AUTH_DISABLED cannot be enabled when ENV=production` | `ENV=production` (or a v1 `NODE_ENV=production` fallback, `src/config.rs:629-633`) | use `ENV=development` locally |
| `DEV_UI=true ignored: /dev is never served when ENV=production` | by design (`src/cli/serve.rs:95-97`) | run with `ENV=development` to see `/dev` |
| `invalid LOG_LEVEL filter: …` at `serve` | `LOG_LEVEL` is a tracing filter, validated only by `serve` (`src/telemetry.rs:31`, `41-42`) | e.g. `info` or `riot_proxy=debug` |
| `skipping: node not runnable` during `cargo test` | no `node` on `PATH`; the helper suites skip locally and fail in CI (`tests/ui.rs:52-56`) | install Node 22 |
| `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored` | a key that is not an `override_key`, or names only immutable endpoints (`src/cli/serve.rs:106-108`) | use `account, summoner, league, ladder, matchIds, spectator, mastery, rotations, status` |

## What was verified on this machine

Re-executed on 2026-10-19 with the already-built `target/debug/riot-proxy` (version 2.7.0, `--all-features`) from a scratch directory, `HOST=127.0.0.1`, `PORT=18091`, a fake key, `RIOT_BASE_URL` and `DDRAGON_BASE_URL` pointed at a closed port, then deleted:

- first run: the 27 migrations in one transaction, `database ready`, the stderr banner, `listening`, `starting job workers workers=1`, the three boot ticks (the Data Dragon sync failed as intended and went back to `pending`);
- `/healthz`, `/readyz`, `/dev`, `/dev/showcase`, `/docs`, `/dashboard`, `/openapi.json` all 200; `/v1/admin/info` 401 without a key, 200 with an admin key, `FORBIDDEN` with a read key; a Riot route with a read key answered a `502` `UPSTREAM_ERROR` envelope (dead upstream) with `X-Request-Id` and the quota headers;
- `key create` (read, and read+admin), `key list`, `migrate` (`…/riot-proxy.db: up to date`), `spec`, `--help` (the `riot` subcommand is present in this `dev-cli` build);
- a second `serve` on the same port, a 5-character key, and a run from another directory, for the troubleshooting rows;
- `AUTH_DISABLED=true` with `ARCHIVE_OBJECT_URL=file:///…`: the warning, `/v1/admin/info` 200 without a key, `object store configured`, an `index/` snapshot in the directory;
- `node --test tests/dev_ui.mjs`: 16 passed.

Not re-executed here: `cargo build`, `cargo test` and `just nextest` (a full run on this date passed: 5 binaries, 939 passed, 0 failed, 5 ignored; see [testing.md](./testing.md)), `just musl` (no `musl-gcc`), `just docker` (no Docker), `just cov` (no `cargo-llvm-cov`), `just ui-test` and `just acceptance` (they need `npm ci`), and anything needing `sudo` (none available).

> **TODO (unverified):** the exact first-run output of a **release** binary built without `dev-cli` (`Cargo.toml:52-54`); only the local `--all-features` debug build was run, and its `--help` lists `riot`.

---
**Sources:** `rust-toolchain.toml:L1-L5`, `justfile:L1-L52`, `.env.example:L1-L178`, `README.md:L60-L112`, `src/config.rs:L3`, `L581-L595`, `L623-L633`, `L671-L697`, `L900-L926`, `L1003-L1015`, `src/cli/mod.rs:L82-L88`, `src/cli/serve.rs:L52-L68`, `L76-L108`, `L143-L148`, `L340-L345`, `L420-L426`, `src/cli/key.rs:L13-L57`, `src/consumers.rs:L12-L14`, `L95-L96`, `src/http/auth.rs:L201-L204`, `src/jobs/ticks.rs:L1-L4`, `L44-L80`, `L108-L139`, `src/cli/riot.rs:L1-L32`, `L63-L81`, `src/archive/objects.rs:L102-L113`, `acceptance/helpers/env.ts:L19-L25`, `L244-L275`, `acceptance/helpers/setup.ts:L23-L54`, `acceptance/helpers/mock-riot.ts:L7`, `tests/ui.rs:L42-L64`, `tests/dom/package.json`, `acceptance/package.json`, `.github/workflows/ci.yml:L52-L53`, `PROGRESS.md:L390`, `L400`, `L420-L422`, `docs/IMPLEMENTATION.md:L434`, `docs/design/11-showcase.md:L15`, `docs/DECISIONS.md:L1457`, `docs/_meta/scratch/config.md`, `docs/_meta/scratch/persistence.md` §5, `docs/_meta/scratch/siblings.md` §1.2, §1.8
**Last verified:** 2026-10-22 against commit `6eda84e`

# riot-proxy configuration surface — evidence inventory

Scope: repo `/home/dev/projects/riot-proxy`, branch `docs/DOC-01-documentation-set`, HEAD `7491ff6`, package version 2.7.0 (`Cargo.toml:3`). Every claim cites `file:line`. Line numbers are those of the file on disk at HEAD. Live checks were run against the already-built `target/debug/riot-proxy` from `/tmp` with `RIOT_API_KEY=fake…` in the environment (that binary was built with `--features dev-cli`: its `--help` lists the `riot` subcommand).

Secrets are marked **SECRET**. The `Secret` wrapper (`src/config.rs:131-155`) prints `[REDACTED]` for `Debug`/`Display`; only `.expose()` returns the value (`src/config.rs:140`).

---

## 1. Every configuration variable

The authoritative list of variables the config reads is `VARS` (`src/config.rs:23-90`). Anything not in `VARS` (plus `NODE_ENV` and `TLS`) is dropped before merging by `known()` (`src/config.rs:873-880`). The config tests assert `.env.example` documents exactly `VARS` (`src/config/tests.rs:430-449`).

Conventions used below:
- "int(default, min..=max)" = parsed by `Vars::int` (`src/config.rs:1061-1076`): trimmed, must parse as a whole number, must satisfy `min <= n <= max`; otherwise the error `'<raw>' is not a whole number` or `'<raw>' is outside <min>..=<max>` (`src/config.rs:1092-1095`) and the default is kept while errors accumulate.
- "float(default, min..=max)" = `Vars::float` (`src/config.rs:1078-1090`): must be finite and in range.
- "bool(default)" = `Vars::bool` / `opt_bool` (`src/config.rs:1162-1176`): accepts `true|1|false|0`, case-insensitive after trim; anything else is `'<raw>' is not a boolean (true/false/1/0)`.
- "csv" = `csv()` (`src/config.rs:928-934`): split on `,`, trimmed, empties dropped.
- An **empty value** (`FOO=`) in `.env` *or* the environment is treated as unset (`src/config.rs:871-877`; test `src/config/tests.rs:264-274`), so it falls through to the lower layer / default rather than overriding.
- All validation errors are collected and reported together as `Invalid environment configuration:` followed by one `  NAME: message` line each (`src/config.rs:115-116`, `785-789`). Observed output and exit code 1 (live run, see §3).
- `tests.rs` below means `src/config/tests.rs`.

### 1.1 Core / service

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `RIOT_API_KEY` **SECRET** | — | `Secret` string | — | **yes** | length ≥ 8 chars; errors `RIOT_API_KEY: is required` / `must be at least 8 characters` (`src/config.rs:610-614`; test `tests.rs:226-236`). Never in `redacted()` output, "not even its length" (`src/config.rs:383-387`). Derives the 8-hex-char `KeyScope` = first 8 hex of sha256(key) (`src/cache/keys.rs:34-37`) | Riot developer key sent upstream | `src/config.rs:610`; consumed `src/riot/client.rs:221,227` |
| `RIOT_USER_AGENT` | — | string | `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)` (`src/config.rs:100`) | no | none | `User-Agent` on every Riot call and on Data Dragon fetches | `src/config.rs:697`; `src/riot/client.rs:221,228`; `src/jobs/ddragon.rs:110` |
| `ENV` | `--env <ENVIRONMENT>` | enum `development\|test\|production` (case-insensitive, `src/config.rs:174-184`) | `development` | no | invalid → `ENV: '<raw>': expected one of development, test, production`. Falls back to **`NODE_ENV`** only when `ENV` is unset (`src/config.rs:616-620`; test `tests.rs:200-210`). `production` triggers the refusals listed under `AUTH_DISABLED`, `DEV_UI`, `RIOT_BASE_URL`, `DDRAGON_BASE_URL`, `ARCHIVE_OBJECT_*` | Deployment environment | `src/config.rs:616-620` |
| `NODE_ENV` (legacy, v1) | — | same as `ENV` | — | no | Read **only** as a fallback when `ENV` is unset; `ENV` wins when both set (`src/config.rs:96-98`, `616-620`, kept by `known()` at `873-880`). Not listed in `VARS`, not in `redacted()` | v1's name for `ENV` | `src/config.rs:618` |
| `HOST` | `--host` | string | `0.0.0.0` | no | none (any string; `healthcheck` rewrites wildcards to loopback, `src/cli/healthcheck.rs:26-34`) | Bind address | `src/config.rs:699`; bound at `src/cli/serve.rs:144-148` |
| `PORT` | `--port` (clap `u16`) | u16 | `8080` | no | int(8080, 1..=65535) (`src/config.rs:700`); observed `PORT: '0' is outside 1..=65535` | Bind port | `src/config.rs:700` |
| `LOG_LEVEL` | `--log-level` | string (tracing `EnvFilter` directive) | `info` | no | **Not validated at config time.** Parsed by `EnvFilter::builder().parse()` only in `serve` (`src/telemetry.rs:41-42`); a bad filter fails `serve` with `invalid LOG_LEVEL filter: …` (`src/telemetry.rs:31`) → exit 1 | tracing filter, e.g. `riot_proxy=debug,tower_http=info` | `src/config.rs:701`; `src/telemetry.rs:42` |
| `LOG_FORMAT` | `--log-format` | enum `json\|pretty` (`src/config.rs:230-239`) | `pretty` if stdout is a TTY, else `json` (`src/config.rs:649-656`, `587`) | no | invalid → `LOG_FORMAT: '<raw>': expected one of json, pretty` | Log output format; JSON layer flattens event fields and nests the innermost span under `span` (`src/telemetry.rs:53-66`) | `src/config.rs:649`; `src/telemetry.rs:44-49` |
| `DATA_DIR` | `--data-dir` | path | `./data` | no | none; **created (with parents) at load** for every command that loads config (`src/config.rs:589`, `793-798`); failure → `could not create DATA_DIR <path>: …` (`src/config.rs:127-128`). Dockerfile sets `ENV DATA_DIR=/data` (`Dockerfile:23`) | Root for the SQLite DB, ddragon mirror, `backups/` (`src/cli/serve.rs:210`) | `src/config.rs:622` |
| `DATABASE_URL` | `--database-url` | `sqlite://<path>` or `postgres://…` | unset → `sqlite://$DATA_DIR/riot-proxy.db` (`src/config.rs:101`, `624`) | no | `sqlite://` prefix stripped to a path (`626-627`). `postgres://` / `postgresql://` → **always** an error `DATABASE_URL: postgres:// needs a build with the \`postgres\` feature (not available yet)` (`628-633`; no `cfg(feature)` gate on this branch). Any other scheme → `must start with sqlite:// or postgres://` (`634-637`). A Postgres URL is a `Secret` (may hold a password) and shows `[REDACTED]` in `redacted()` (`402-405`). Every CLI command additionally bails `only sqlite:// databases are supported in this build` (`src/cli/mod.rs:128-133`) | Database location | `src/config.rs:623-639` |
| `ROLE` | `--role` | enum `all\|api\|worker` (`src/config.rs:203-213`) | `all` | no | `api`/`worker` require a Postgres database → `ROLE: api and worker need DATABASE_URL=postgres://; SQLite supports only all` (`641-647`). Since Postgres is always refused (above), only `all` can boot on this branch | Process role | `src/config.rs:641` |
| `JOB_CONCURRENCY` | `--job-concurrency` (clap `u32`) | u32 | `8` | no | int(8, 1..=u32::MAX) | Scheduler worker count; `serve` logs it as `workers=` (`src/cli/serve.rs:319-324`) | `src/config.rs:709` |
| `JOB_KIND_LIMITS` | — | `kind=n,kind=n` or `none` | `aggregate:analytics=1,archive:offload=1` (`src/config.rs:111`) | no | `Vars::kind_limits` (`src/config.rs:1108-1139`): `none` (case-insensitive) → no caps; each item must be `kind=n`; `kind` must be in `jobs::kinds::ALL` (`src/jobs/mod.rs:46-68`) and **not** in `lanes::LANED` (`src/jobs/lanes.rs:67-78`, error `'<kind>' calls Riot; the rate limiter paces it`); `n` ≥ 1. Cappable kinds therefore: `ddragon:sync, ladder:crawl, ladder:archive, names:backfill, aggregate:analytics, facts:reextract, builds:extract, tiers:backfill, maintenance, archive:offload, archive:index-snapshot` (`src/jobs/mod.rs:20-43` minus `src/jobs/lanes.rs:67-78`). Test `tests.rs:620-654`. **Note:** `.env.example:34` shows `aggregate:analytics=1` only, which differs from the code default | Max concurrent jobs per kind for kinds with no rate-limit lane | `src/config.rs:710`; `src/cli/serve.rs:111` |
| `JOB_YIELD_BUDGET_MS` | — | u64 ms | `1000` | no | int(1000, 0..=900000) | How long a job's fetch waits for the limiter before yielding its worker | `src/config.rs:711`; `src/cli/serve.rs:137` |

### 1.2 Cache and rate limiting

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `CACHE_TTL_OVERRIDES` | — | CSV `key=seconds` | `""` | no | **Not validated at config time** (stored as the raw string, `src/config.rs:712`). Parsed by `parse_ttl_overrides` (`src/riot/endpoints.rs:424-444`): items without `=`, empty keys, non-numeric, negative or non-finite values are silently skipped; fractional seconds truncate to whole seconds. Valid keys are the endpoints' `override_key`s: `account, summoner, league, ladder, match, timeline, spectator, mastery, rotations, status` (5th argument of each `ep(` call in `src/riot/endpoints.rs`, field at `:50`). `match` and `timeline` are immutable (`ep()` sets `immutable: soft_ttl.is_none()`, `src/riot/endpoints.rs:83`; test `596-606`), so overrides on them are ineffective and `serve` warns `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored` per key (`src/cli/serve.rs:106-108`; `src/riot/endpoints.rs:500-510`) | Per-endpoint-group soft TTL overrides | `src/config.rs:712`; `src/riot/endpoints.rs:469` |
| `CACHE_L1_MAX_MB` | — | u32 MiB | `128` | no | int(128, 1..=1048576) | L1 in-memory response cache weight budget | `src/config.rs:713`; `src/cache/l1.rs:117-118` |
| `NEG_TTL_SECONDS` | — | u32 s | `30` | no | int(30, 1..=u32::MAX) | Negative-cache TTL for 404s | `src/config.rs:714`; `src/riot/endpoints.rs:470` |
| `NEG_TTL_ACCOUNT_SECONDS` | — | u32 s | `300` | no | int(300, 1..=u32::MAX) | Negative-cache TTL for account lookups | `src/config.rs:715`; `src/riot/endpoints.rs:471` |
| `CLIENT_WAIT_BUDGET_MS` | — | u64 ms | `2000` | no | int(2000, 0..=u64::MAX) | How long an interactive request waits for a limiter token | `src/config.rs:716`; `src/cli/serve.rs:136` |
| `BULK_USAGE_CEILING` | — | f64 | `0.8` | no | float(0.8, 0.0..=1.0) | Share of a rate bucket bulk work may use while someone is active | `src/config.rs:717`; `src/cli/serve.rs:71` |
| `BULK_IDLE_CEILING` | — | f64 | `0.95` (`src/config.rs:104`) | no | float(0.95, 0.0..=1.0), then (`src/config.rs:770-783`): if **unset**, silently raised to `max(0.95, BULK_USAGE_CEILING)`; if **set**, must be `< 1` (`'<v>' must be below 1`) and `>= BULK_USAGE_CEILING` (`'<v>' is below BULK_USAGE_CEILING (<busy>)`). Test `tests.rs:594-618` | Bulk share while a scope has had no interactive request for `BULK_IDLE_AFTER_S` | `src/config.rs:718`, `770-783`; `src/cli/serve.rs:71-74` |
| `BULK_IDLE_AFTER_S` | — | u32 s | `30` | no | int(30, 0..=86400) | Idle threshold for the idle ceiling | `src/config.rs:719`; `src/cli/serve.rs:73` |
| `STALE_WHILE_REVALIDATE` | — | bool | `true` | no | bool | Serve stale entries while refreshing in the background | `src/config.rs:720`; `src/riot/endpoints.rs:472`; `src/cli/serve.rs:138` |

### 1.3 Telemetry and worker cadence

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `METRICS_INTERVAL_S` | — | u32 s | `5` | no | int(5, 1..=300) | Tick of the `metrics` WebSocket topic | `src/config.rs:721`; `src/cli/serve.rs:350-353` |
| `METRICS_HISTORY_INTERVAL_S` | — | u32 s | `60` | no | int(60, 10..=3600) | Cadence of the recorded metrics history | `src/config.rs:722`; `src/cli/serve.rs:354-358`; `src/routes/admin.rs:2427` |
| `TRACK_POLL_LIVE_S` | — | u32 s | `60` | no | int(60, 10..=u32::MAX) | `poll:live` tick period | `src/config.rs:723`; `src/jobs/ticks.rs:112` |
| `TRACK_POLL_RANK_S` | — | u32 s | `600` | no | int(600, 30..=u32::MAX) | `poll:rank` tick period | `src/config.rs:724`; `src/jobs/ticks.rs:113` |
| `TRACK_POLL_MATCH_S` | — | u32 s | `300` | no | int(300, 30..=u32::MAX) | `poll:matches` tick period | `src/config.rs:725`; `src/jobs/ticks.rs:114` |
| `DDRAGON_SYNC_S` | — | u32 s | `3600` | no | int(3600, 60..=u32::MAX) | `ddragon:sync` tick period | `src/config.rs:726`; `src/jobs/ticks.rs:115` |
| `ARCHIVE_TIMELINES` | — | bool | `true` | no | bool | Archive jobs also fetch each match's timeline | `src/config.rs:727`; `src/cli/serve.rs:127,181,188,199` |
| `TIMELINE_BACKFILL_PATCHES` | — | u32 | `0` (off) | no | int(0, 0..=1000); `> 0` adds the hourly `timelines:backfill` tick (`src/jobs/ticks.rs:106,132-135`) | How many newest patches `timelines:backfill` covers | `src/config.rs:728`; `src/cli/serve.rs:235`; `src/routes/admin.rs:2307` |
| `BACKUP_KEEP` | — | u32 | `14` | no | int(14, 0..=365); test `tests.rs:656-669` | Daily `VACUUM INTO` backups kept in `$DATA_DIR/backups`; 0 = none | `src/config.rs:729`; `src/cli/serve.rs:208-212` |

### 1.4 Object storage (`ARCHIVE_OBJECT_*`, design/12)

All checked together by `Vars::object_store` (`src/config.rs:958-1008`).

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `ARCHIVE_OBJECT_URL` | — | `s3://<bucket>` or `file:///<abs dir>` | unset (bodies stay in SQLite; no offload jobs registered, `src/cli/serve.rs:248-270`) | no | `parse_object_url` (`src/config.rs:884-910`): `s3://` bucket must be 3–63 chars of `a-z0-9.-`, start/end alphanumeric, no path (trailing `/` tolerated); `file://` must be absolute. Production: `file://` refused (`997-999`, `file:// cannot be used when ENV=production`); `s3://` requires both keys (`990-995`, `needs ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY when ENV=production`). `s3://` requires `ARCHIVE_OBJECT_ENDPOINT` (`987-989`). Tests `tests.rs:743-880` | Where packed bodies are stored | `src/config.rs:962-968`; `src/archive/objects.rs:99-139` |
| `ARCHIVE_OBJECT_ENDPOINT` | — | `scheme://host[:port]` | unset | with `s3://` | `check_endpoint` (`src/config.rs:914-926`): trailing `/` dropped; `https://` always OK; `http://` refused in production (`must be https:// when ENV=production`); host must be non-empty ASCII without `/ ? # space @`. Virtual-hosted addressing when the host starts with `<bucket>.`, else path style (`src/archive/objects.rs:322-326`). `allow_http` only if it starts with `http://` (`objects.rs:121`) | S3-compatible endpoint | `src/config.rs:969-978` |
| `ARCHIVE_OBJECT_REGION` | — | string | `auto` | no | none; passed straight to the S3 builder (`src/archive/objects.rs:118`) | Signing region | `src/config.rs:732` |
| `ARCHIVE_OBJECT_ACCESS_KEY` **SECRET** | — | `Secret` | unset | both-or-neither | Exactly one of the pair set → `ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY: set both or neither` (`src/config.rs:981-984`). Keys without a URL are accepted and inert (test `tests.rs:870-876`). `[REDACTED]` in `redacted()` (`444-455`) | S3 access key id | `src/config.rs:979`; `src/archive/objects.rs:123-130` |
| `ARCHIVE_OBJECT_SECRET_KEY` **SECRET** | — | `Secret` | unset | both-or-neither | as above | S3 secret key | `src/config.rs:980` |
| `ARCHIVE_OBJECT_CONCURRENCY` | — | u32 | `16` | no | int(16, 1..=256) | Ranged GETs run at once against the bucket | `src/config.rs:735`; `src/archive/objects.rs:112,136` |
| `ARCHIVE_BODY_LOCAL_DAYS` | — | u32 days | `14` | no | int(14, 0..=365) | How long SQLite keeps a body already packed; 0 frees all | `src/config.rs:736`; `src/cli/serve.rs:254` |
| `ARCHIVE_PACK_TARGET_BYTES` | — | u64 bytes | `67108864` (64 MiB) | no | int(64<<20, 1<<20 ..= 1<<30) | A pack closes at this many bytes | `src/config.rs:737`; `src/cli/serve.rs:115,255` |
| `ARCHIVE_OFFLOAD_PACE_MS` | — | u64 ms | `50` | no | int(50, 0..=60000) | Pause between offload turns | `src/config.rs:738`; `src/cli/serve.rs:256` |

### 1.5 Backfill, ladder crawl, ranks

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `LOOKUP_BACKFILL_LIMIT` | — | u32 | `4294967295` (u32::MAX = unlimited) | no | int(u32::MAX, 0..=u32::MAX); `4294967296` is rejected (test `tests.rs:276-309`) | Matches walked on a player's first lookup; 0 disables | `src/config.rs:739`; `src/routes/players.rs:553`; `src/cli/serve.rs:180,189,198` |
| `TRACK_CATCHUP_LIMIT` | — | u32 | `500` | no | int(500, 0..=10000) | How far a match poll pages back | `src/config.rs:740`; `src/cli/serve.rs:179` |
| `LADDER_CRAWL_S` | — | u32 s | `0` (no scheduled crawl) | no | int(0, 0..=604800); `> 0` adds the `ladder:crawl` tick (`src/jobs/ticks.rs:124`) | Seconds between scheduled ladder crawls | `src/config.rs:741` |
| `LADDER_QUEUES` | — | CSV | `RANKED_SOLO_5x5` | no | each must be exactly one of `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` (`src/riot/ladder.rs:7`; `src/config.rs:1141-1148`, case-sensitive); invalid entries error *and* are dropped (`742-745`). Test `tests.rs:524-548` | Ladders crawled | `src/config.rs:742`; `src/jobs/ticks.rs:142-153`; `src/cli/serve.rs:219-221` |
| `LADDER_PLATFORMS` | — | CSV of platforms | `""` (crawl scheduled on no platform) | no | each parsed by `Platform::from_str` (case-insensitive, `src/riot/routing.rs:214-228`): `na1 br1 la1 la2 euw1 eun1 tr1 ru kr jp1 oc1 ph2 sg2 th2 tw2 vn2` (`src/riot/routing.rs:65-84`); error `LADDER_PLATFORMS: Unknown platform '<x>'. Expected one of: …` (`src/config.rs:1098-1103`; test `tests.rs:451-461`). v1's `DEFAULT_PLATFORM` is ignored (test `tests.rs:362-369`) | Platforms the crawl tick covers | `src/config.rs:686-689`; `src/jobs/ticks.rs:142-153`; `src/routes/admin.rs:1753` |
| `LADDER_TIER_FLOOR` | — | tier name | `MASTER` | no | trimmed + upper-cased, must be one of `IRON BRONZE SILVER GOLD PLATINUM EMERALD DIAMOND MASTER GRANDMASTER CHALLENGER` (`src/riot/ladder.rs:9-13,29-36`; `src/config.rs:1151-1160`) | Lowest tier enumerated by a crawl | `src/config.rs:747`; `src/cli/serve.rs:196`; `src/routes/admin.rs:1721,1755` |
| `LADDER_BACKFILL_LIMIT` | — | u32 | `100` | no | int(100, 0..=10000) | Matches walked per discovered player | `src/config.rs:748`; `src/cli/serve.rs:197`; `src/routes/admin.rs:1756` |
| `RANK_LOOKUP_LIMIT` | — | u32 | `50000` | no | int(50000, 0..=10000000) | Players one `ranks:lookup` run looks up; 0 off | `src/config.rs:749`; `src/cli/serve.rs:200` |
| `RANK_LOOKUP_RECHECK_S` | — | u32 s | `604800` (7 d) | no | int(604800, 0..=31536000) | Time a lookup keeps a player off the next list | `src/config.rs:750`; `src/cli/serve.rs:201` |
| `TIER_LATE_STAMP_DAYS` | — | u32 days | `14` | no | int(14, 0..=365) | Days back a later-seen rank stamps `UNKNOWN` tiers; 0 none | `src/config.rs:751`; `src/cli/serve.rs:128,202` |

### 1.6 Facts and analytics

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `FACTS_REEXTRACT_BATCH` | — | u32 | `500` | no | int(500, 1..=10000) | Matches per `facts:reextract` batch | `src/config.rs:752`; `src/cli/serve.rs:229` |
| `AGGREGATE_PATCH_LIMIT` | — | u32 | `4` | no | int(4, 0..=100) | Patches rebuilt per recompute; 0 = every patch | `src/config.rs:753`; `src/cli/serve.rs:228` |
| `ANALYTICS_CACHE_ENTRIES` | `--analytics-cache-entries` (clap `u32`) | u32 | `512` | no | int(512, 0..=4096); test `tests.rs:917-939` | Analytics responses kept in memory; 0 none | `src/config.rs:754`; `src/cli/serve.rs:220` |
| `AGGREGATE_INTERVAL_S` | — | u32 s | `0` (off) | no | int(0, 0..=604800); `> 0` adds the `aggregate:analytics` tick (`src/jobs/ticks.rs:126-129`) | Seconds between unprompted analytics recomputes | `src/config.rs:755` |

### 1.7 Static data

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `DDRAGON_DIR` | — | path | `$DATA_DIR/ddragon` (`src/config.rs:703-705`) | no | none | Data Dragon mirror directory | `src/config.rs:703`; `src/cli/serve.rs:154` |
| `DDRAGON_LOCALE` | — | string | `en_US` | no | none | Data Dragon locale | `src/config.rs:756`; `src/jobs/ddragon.rs:119` |

### 1.8 Security and UIs

| Env | CLI flag | Type | Default | Required | Validation / constraints | Meaning | Read at |
|---|---|---|---|---|---|---|---|
| `ADMIN_IP_ALLOWLIST` | — | CSV of IPs / CIDRs | `""` | no | Config only splits/trims (`src/config.rs:757`). Entries are parsed at `serve` by `AllowEntry::parse` (`src/http/auth.rs:66-77`): a bare IP, or `ip/bits` with bits ≤ 32 (v4) / 128 (v6); an unparsable entry is **warned and ignored** (`ADMIN_IP_ALLOWLIST entry is not an IP or CIDR; ignored`, `src/http/auth.rs:193-199`), not a boot error | Sources allowed to reach `/v1/admin/*`; empty = any source (`.env.example:147-148`) | `src/config.rs:757`; `src/http/auth.rs:193` |
| `TRUST_PROXY` | — | bool | `false` | no | bool | Take the client address from `X-Forwarded-For` | `src/config.rs:758`; `src/http/auth.rs:215` |
| `ADMIN_CORS_ORIGINS` | — | CSV of origins | `""` (no CORS headers at all, `src/http/cors.rs:27-29`) | no | `Vars::origins` (`src/config.rs:1013-1031`): `*` → `'*' is refused; list each origin`; each must be `http://` or `https://` + non-empty ASCII host without `/ ? # * space` (no path, no trailing `/`). Test `tests.rs:701-726` | Browser origins allowed on `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz` (`src/http/cors.rs:40-43`); allowed methods GET/POST/DELETE, headers `Authorization`/`Content-Type`, exposes `x-request-id` (`src/http/cors.rs:30-37`) | `src/config.rs:759`; `src/app.rs:78` |
| `BOOTSTRAP_ADMIN_KEY` **SECRET** | — | `Secret` | unset (one is generated) | no | none at config time. Used only by `serve`, only when the `consumers` table is **empty** (`src/consumers.rs:308-333`); then it becomes the `bootstrap-admin` consumer's key (name `src/consumers.rs:16`, scopes read+admin, quota `DEFAULT_QUOTA_PER_MIN`=600 `src/consumers.rs:14`). When imported, the key is **not** printed (`src/cli/serve.rs:61-67`). `[REDACTED]` in `redacted()` (`src/config.rs:479-482`) | Operator-chosen first admin key | `src/config.rs:760`; `src/cli/serve.rs:57-60` |
| `AUTH_DISABLED` | — | bool | `false` | no | bool; `true` with `ENV=production` → `AUTH_DISABLED cannot be enabled when ENV=production` (`src/config.rs:658-662`; tests `tests.rs:169-198`). At `serve`, warns `AUTH_DISABLED=true: every request runs as the synthetic dev-local consumer (read+admin)` (`src/http/auth.rs:201-204`) | Skip consumer-key checks (local testing) | `src/config.rs:658`; `src/http/auth.rs:201,213`; `src/routes/ui.rs:178` |
| `DEV_UI` | — | bool | unset → `true` outside production, always `false` in production (`src/config.rs:664-666`, `762`) | no | bool; in production the value is ignored: `dev_ui` is forced `false` and, if `DEV_UI` was explicitly set true, `dev_ui_ignored` is set (`763`) and `serve` warns `DEV_UI=true ignored: /dev is never served when ENV=production` (`src/cli/serve.rs:95-97`). Test `tests.rs:212-224` | Serve `/dev`, `/dev/showcase`, `/dev/config.json`, `/dev/openapi.json` (`src/routes/ui.rs:176-212`), the `/dev/jobs` redirect (`src/routes/ui.rs:222-229`) and the dev reset routes (`src/app.rs:77`) | `src/config.rs:664-666,762-763` |
| `DOCS_UI` | — | bool | `true` | no | bool | Serve `/docs` + OpenAPI (`src/app.rs:71`; `src/routes/ui.rs:42`) | `src/config.rs:764` |
| `DASHBOARD_UI` | — | bool | `true` | no | bool | Serve `/dashboard` (`src/routes/ui.rs:217-221`) | `src/config.rs:765` |
| `RIOT_BASE_URL` | — | URL string | unset (Riot's hosts) | no | any string; set with `ENV=production` → `RIOT_BASE_URL cannot be set when ENV=production` (`src/config.rs:675-684`; test `tests.rs:550-572`) | Send every Riot request to a mock | `src/config.rs:675`; `src/cli/serve.rs:119-122` |
| `DDRAGON_BASE_URL` | — | URL string | unset | no | same production refusal (`src/config.rs:677-684`) | Fetch Data Dragon from a mock | `src/config.rs:676`; `src/cli/serve.rs:160-166` |

### 1.9 Legacy / retired v1 variables and what happens

| Variable | Behaviour | Evidence |
|---|---|---|
| `NODE_ENV` | Read as a fallback for `ENV` only (see §1.1). | `src/config.rs:96-98,616-620,877` |
| `TLS` | Read **only to refuse**: `true`/`1` → `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`; `false`/`0` accepted and ignored. (Live run confirmed the message, exit 1.) | `src/config.rs:92-94,668-673,877`; test `tests.rs:574-592`; design `docs/design/07-deployment.md:59-61` |
| `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT` | Not in `VARS` → silently dropped by `known()`. | `src/config.rs:873-880`; test `tests.rs:585-591` |
| `REDIS_URL`, `SF_LOCK_MS` | Not in `VARS` → silently dropped. Design lists both as **removed**. | `src/config.rs:873-880`; `docs/design/07-deployment.md:129-130`; both present in the v1 fixture `tests/fixtures/v1.env.example` |
| `DEFAULT_PLATFORM` | Silently dropped; does **not** seed `LADDER_PLATFORMS`. | `src/config.rs:873-880`; test `tests.rs:362-369` |
| `AGGREGATE_MIN_GAMES` | In the v1 fixture, not in `VARS` → silently dropped. | `src/config.rs:23-90,873-880`; `tests/fixtures/v1.env.example` |
| v1 `DATABASE_URL=postgres://…` | The one loud incompatibility: refused (see §1.1). | test `tests.rs:491-515` |

### 1.10 Build-only / process environment read outside `Config`

| Variable | Where | Effect |
|---|---|---|
| `GIT_SHA`, `RUSTC`, `SOURCE_DATE_EPOCH` | `build.rs:23,31,35` | compile-time only, see §4 |
| `CI` | `tests/ui.rs:52` | test-only: tolerates a missing browser when unset |
| `PATH` | `tests/migrate_v1.rs:162` (test) and implicitly by `migrate-v1`, which spawns `pg_restore` (`src/cli/migrate_v1.rs:360-372`) | `pg_restore` must be on `PATH` for `PGDMP` inputs |

No other `std::env::var` / `var_os` reads exist in `src/` (grep over `src/`, `build.rs`: the only process-environment read is `std::env::vars()` at `src/config.rs:569`). `clap`'s `env` feature is enabled (`Cargo.toml:17`) but no `#[arg(env = …)]` is used; the `[env: X]` text in `--help` is literal doc text in `src/config.rs:496-525`.

---

## 2. Precedence as implemented

Documented: "Precedence, lowest to highest: built-in defaults < `.env` < environment < flags" (`src/config.rs:3`; `.env.example:2`).

Implementation:

1. `Config::load(args)` → `load_with(Some(Path::new(".env")), std::env::vars(), args)` (`src/config.rs:568-570`). The `.env` path is **relative to the process cwd**; a missing file is not an error (`src/config.rs:579-582`). `justfile:3` sets `set dotenv-load := false` so `just` does not pre-load it — the binary reads it itself.
2. `.env` is parsed by `parse_dotenv` (`src/config.rs:826-869`) with node-`dotenv` rules (ADR-020): blank/`#` lines skipped, `export KEY=` allowed, unquoted values trimmed and cut at ` #`, `'…'`/`"…"`/`` `…` `` quoted values literal except `"…"` expands `\n`; no multi-line values; a bad line is `ConfigError::DotEnv { path, line, message }` (`src/config.rs:119-124`) → `error: <path> line N: …`, exit 1. Tests `tests.rs:463-522`.
3. `from_sources` (`src/config.rs:594-602`) merges three figment `Serialized::defaults` layers in order **dotenv → env → args.overrides()**; later layers win. Each layer first passes `known()` (`src/config.rs:873-880`) which drops unknown names and **empty values**, so `FOO=` never shadows a lower layer (test `tests.rs:264-274`). figment's `Env` provider is deliberately not used so numeric-looking strings stay strings (`src/config.rs:6-9`; test `tests.rs:238-246`).
4. `ConfigArgs::overrides()` (`src/config.rs:528-553`) maps the ten global flags onto their variable names: `--host→HOST`, `--port→PORT`, `--data-dir→DATA_DIR`, `--database-url→DATABASE_URL`, `--env→ENV`, `--role→ROLE`, `--job-concurrency→JOB_CONCURRENCY`, `--analytics-cache-entries→ANALYTICS_CACHE_ENTRIES`, `--log-level→LOG_LEVEL`, `--log-format→LOG_FORMAT`. Flags are `global = true` (`src/config.rs:497-525`) and flattened into the root command (`src/cli/mod.rs:29-30`), so they are accepted before or after any subcommand. Flag values go through the **same** validation as env values (e.g. `--env production` triggers the production refusals, test `tests.rs:187-198`).
5. `from_map` (`src/config.rs:604-790`) parses and validates the merged map, collecting every error; then `ensure_data_dir()` (`589`, `793-798`).
6. Built-in defaults are the literals in `from_map` (`src/config.rs:695-768`); `tests.rs:30-104` pins them.

Precedence tests: `tests.rs:106-138` (flag beats env beats `.env` beats default), `tests.rs:140-167` (every flag).

---

## 3. CLI

Entry: `src/main.rs:1-3` → `cli::run()` (`src/cli/mod.rs:70-121`). Root command `riot-proxy`, `version` from `CARGO_PKG_VERSION` (`src/cli/mod.rs:22-26`); `propagate_version` and `arg_required_else_help` (`src/cli/mod.rs:27`). Observed: `riot-proxy --version` → `riot-proxy 2.7.0`.

Dispatch order (`src/cli/mod.rs:70-121`):
1. `Cli::parse()` — clap usage errors print to stderr and exit **2** (observed: no arguments → help, exit 2; `--nope` → `error: unexpected argument '--nope' found`, exit 2).
2. `spec` runs **before** config load (`src/cli/mod.rs:72-81`); it needs no `RIOT_API_KEY`.
3. Every other command calls `Config::load` first (`src/cli/mod.rs:82-88`): a config error prints `error: <ConfigError>` and exits **1** (observed: `error: Invalid environment configuration:` + `  RIOT_API_KEY: must be at least 8 characters`). This means `healthcheck`, `backup`, `key …` etc. all require a valid `RIOT_API_KEY` and all create `DATA_DIR`.
4. A tokio multi-thread runtime with `min(available_parallelism, 4)` workers (`src/cli/mod.rs:89-99,123-126`); failure → exit 1.
5. Command result: `Ok` → exit **0**; `Err(e)` → `error: {e:#}` on stderr, exit **1** (`src/cli/mod.rs:114-120`).

Global options (all subcommands, from `--help`): `--host`, `--port`, `--data-dir`, `--database-url`, `--env <ENVIRONMENT>`, `--role`, `--job-concurrency`, `--analytics-cache-entries`, `--log-level`, `--log-format`, `-h/--help`, `-V/--version` (`src/config.rs:493-526`; observed help text).

### `serve`
`src/cli/mod.rs:37-38`, `src/cli/serve.rs:39-392`. Only command that installs the tracing subscriber (`src/cli/serve.rs:45`; no other caller of `init_tracing` in `src/`). Sequence and log lines (tracing `info` unless marked `warn`; the bootstrap block is plain **stderr**):

| Step | Log / output | Line |
|---|---|---|
| open DB (runs migrations: `Db::open` → `migrate`, `src/db/mod.rs:135-146,175`) | `database ready` `path=` | `src/cli/serve.rs:52-55` |
| bootstrap admin if `consumers` is empty | `bootstrap admin consumer created` `id=`; then, **only if `BOOTSTRAP_ADMIN_KEY` was not set**, stderr: blank line, `  bootstrap admin key (shown once): <key>`, `  Scopes read,admin. Store it now; it cannot be recovered.`, `  Mint more keys with: riot-proxy key create --name <name>`, blank line | `src/cli/serve.rs:57-68`, `399-405`; `src/consumers.rs:308-333` |
| limiter restore | `limiter checkpoint restored` `rows=` or warn `limiter checkpoint could not be read; starting fresh` | `src/cli/serve.rs:76-80` |
| cache warm/sweep | `L1 warmed from L2` `entries=` / warn `L2 warm failed; starting with a cold cache`; warn `L2 sweep failed` | `src/cli/serve.rs:86-93` |
| DEV_UI in production | warn `DEV_UI=true ignored: /dev is never served when ENV=production` | `src/cli/serve.rs:95-97` |
| object store (if `ARCHIVE_OBJECT_URL`) | `object store configured` `store=` (e.g. `s3://<bucket> at <endpoint>`, `src/archive/objects.rs:134`); a bad store fails the boot here with context `ARCHIVE_OBJECT_URL` | `src/cli/serve.rs:101-105` |
| TTL overrides | warn `CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored` `key=` per key | `src/cli/serve.rs:106-108` |
| bind | `listening` `addr=`; failure → `binding <host>:<port>: …` | `src/cli/serve.rs:143-148` |
| job recovery | `re-queued jobs a previous process left running` `jobs=`; `gave queued jobs their rate-limit lane`; `queued the missing halves of archived matches` `matches=`; `queued facts:reextract for matches an older facts version derived`; `queued tiers:backfill for matches archived without tier stamps`; `queued analytics rebuilds to split rows by tier and side` `ladders=`; `queued archive:offload` (bucket only); each has a `warn … could not …` variant | `src/cli/serve.rs:274-318` |
| workers | `starting job workers` `workers=<JOB_CONCURRENCY>` `kind_limits=<map>` | `src/cli/serve.rs:319-324` |
| auth | warn `AUTH_DISABLED=true: every request runs as the synthetic dev-local consumer (read+admin)`; warn `ADMIN_IP_ALLOWLIST entry is not an IP or CIDR; ignored` `entry=` | `src/http/auth.rs:193-204` (called at `src/cli/serve.rs:360`) |
| shutdown on SIGTERM / Ctrl-C | `SIGTERM` (`src/app.rs:240-253`); running jobs get `JOB_GRACE` = 10 s (`src/cli/serve.rs:397`); final `limiter checkpoint and L2 flushed; stopped` | `src/cli/serve.rs:383-391` |

Exit: 0 after a clean shutdown; 1 on any error (`served?` at `src/cli/serve.rs:392`).

### `migrate`
`src/cli/mod.rs:39-40`, `144-168`. Creates the DB's parent dir, opens with rusqlite, runs refinery migrations; prints `<path>: up to date` or `<path>: applied V…, V…` to stdout. Exit 0/1.

### `key create|list|revoke`
`src/cli/mod.rs:41-43`, `src/cli/key.rs`.
- `key create --name/-n <NAME> [--scopes/-s read,admin] [--quota/-q N]`: `--name` required (`key.rs:13-14`); `--scopes` comma-delimited, default `read`, each parsed by `Scope::from_str` (`read`|`admin`, `src/consumers.rs:25-35`); `--quota` u32 default `DEFAULT_QUOTA_PER_MIN` = 600 (`key.rs:19-20`; `src/consumers.rs:14`). Output to stdout (`key.rs:46-57`): blank, `  Consumer created`, `  ────────────────`, `  id        <ulid>`, `  name      <name>`, `  scopes    <a,b>`, `  quota     <n>/min`, blank, `  API KEY   <rpx_…>`, blank, `  This key is shown once and cannot be recovered. Store it now.`, blank. Key format `rpx_` + base64url(24 random bytes) = 36 chars (`src/consumers.rs:12,95-96`).
- `key list`: header `ID NAME SCOPES QUOTA STATUS` with widths `{:<26}  {:<24}  {:<10}  {:>6}` then one row per consumer, status `active`/`revoked` (`key.rs:59-79`). Keys never shown.
- `key revoke <CONSUMER>` (id or name): prints `revoked <name> (<id>)` (`key.rs:80-83`).

### `healthcheck [--timeout N]`
`src/cli/mod.rs:44-49`, `src/cli/healthcheck.rs`. `--timeout` seconds, default 3. GETs `http://<host>:<PORT>/healthz` where `HOST` `0.0.0.0`/`""` → `127.0.0.1`, `::`/`[::]` → `[::1]`, bare IPv6 bracketed (`healthcheck.rs:26-34`). Uses an empty root store so it builds in `FROM scratch` (`healthcheck.rs:18-23`). Exit 0 on 2xx; otherwise error `<url> answered <status>` or the reqwest error → exit 1 (observed: `Connection refused` → exit 1). Used by `Dockerfile:24` `HEALTHCHECK … CMD ["/riot-proxy","healthcheck"]`. Note it loads config first, so it needs `RIOT_API_KEY` in the container environment.

### `spec`
`src/cli/mod.rs:50-51,72-81`. Prints the OpenAPI document as pretty JSON to stdout (`crate::routes::docs::spec()`, `src/routes/docs.rs:368-371`); serialisation failure → `error: …`, exit 1. Does not load config.

### `migrate-v1 --from <PATH> [--key-scope <SCOPE>]`
`src/cli/mod.rs:52-53`, `src/cli/migrate_v1.rs:39-48,379-423`. `--from` required: a `pg_dump -Fc` archive (detected by the `PGDMP` magic, piped through `pg_restore --data-only --table=matches --table=players --table=consumers -f -`, which must be on `PATH`, `migrate_v1.rs:354-377`) or `pg_restore` text output. `--key-scope` files match facts under that scope instead of this key's (`381-384`; the "8 hex characters" in the help is not validated in code). Prints `imported N matches (R/s), T timelines, P players in S.s; facts under key scope X`, then `  skipped <id>: <why>` lines, `  N consumers not migrated: mint new keys with \`riot-proxy key create\``, and a `  note: players carry key scope(s) … pass --key-scope …` hint (`399-421`). `pg_restore` non-zero → error, exit 1 (`388-395`).

### `backup <OUT>`
`src/cli/mod.rs:54-58`, `src/cli/backup.rs:10-20`. Bails `<db> does not exist` if the SQLite file is missing; runs `VACUUM INTO` via `jobs::maintenance::vacuum_into`; prints `<out>: <size> bytes`. Help says OUT "must not exist" (`mod.rs:56`).

### `packs reindex [--since <ULID>] | restore-index | snapshot`
`src/cli/mod.rs:59-61`, `src/cli/packs.rs:14-83`. Requires `ARCHIVE_OBJECT_URL` (`Objects::from_config` with context `ARCHIVE_OBJECT_URL`, `packs.rs:29`; `NotConfigured` at `src/archive/objects.rs:101`).
- `reindex`: prints `reindexed N packs, M bodies`; unreadable packs are listed on stderr as `not reindexed: <key>: <why>` and then fail the command with `N packs could not be read; the others were recorded` (`packs.rs:32-35,71-83`).
- `restore-index`: prints `skipped the snapshot of <date>: <why>` lines, then `restored the snapshot of <date>: N packs, M locations` or `no readable snapshot; reindexed every pack`, then `reindexed N newer packs, M bodies` (`packs.rs:37-53`). Help: "Stop the service first."
- `snapshot`: prints `<packs-key> and <locations-key>: N packs, M locations, last pack <id|none>` (`packs.rs:55-65`).

### `riot get|record` — **dev-cli only**
Feature `dev-cli` (`Cargo.toml:52-54`; modules and variant gated at `src/cli/mod.rs:8-11,63-65,110-111`); `just riot …` builds with it (`justfile:46-47`). Bypasses cache and limiter (`src/cli/riot.rs:1-3`).
- `riot get <ENDPOINT> <ROUTING> [PARAMS]... [-q/--query k=v]... [--base-url URL (hidden)]` (`riot.rs:15-32`): endpoint by id (`account.byRiotId`) or slug (`account/by-riot-id`, `riot.rs:63-81`); routing a platform or region (`riot.rs:83-94`). Prints `<status> <path?query> (<ms> ms)` to **stderr** and the raw body + newline to stdout (`riot.rs:143-149`). Unknown endpoint → `unknown endpoint '<x>'. Known: …` list; transport/limiter error → `<path> → <kind> (status …)`; both exit 1 (`riot.rs:127-132,151`).
- `riot record --out <DIR> <ENDPOINT> <ROUTING> [PARAMS]... [-q k=v]... [--base-url (hidden)]` (`riot.rs:34-55`, `src/cli/record.rs:40-104`): writes `NN-<method>.json` + `NN-<method>.body` (NN = count of existing `.json` + 1, `record.rs:69,106-112`), keeps only `KEPT_HEADERS` (`record.rs:19-27`), replaces the key with `RGAPI-REDACTED` (`record.rs:29,114-129`) and refuses to keep a file still containing the key or any `RGAPI-` + ≥20 `[0-9a-f-]` string (`record.rs:132-158`). Error responses are recorded with status (or 0) and an empty body (`record.rs:61-65`). Prints `<status> <path> → <meta path>` to stderr (`record.rs:102`).

---

## 4. Build-time embedding (`build.rs`)

`build.rs:1-10` documents the three values; nothing there can fail a build.

| Value | Source | Fallback | Emitted as |
|---|---|---|---|
| `GIT_SHA` | env `GIT_SHA` (trimmed, non-empty) else `git rev-parse HEAD` (`build.rs:23-28,51-62`, which also registers `rerun-if-changed` on `.git/HEAD`, `packed-refs` and the current ref) | `unknown`; truncated to 12 chars (`build.rs:16,29`) | `cargo:rustc-env=GIT_SHA=` (`build.rs:29`) |
| `RUSTC_VERSION` | `$RUSTC --version` (`RUSTC` env, default `rustc`, `build.rs:31-33`) | `unknown` | `cargo:rustc-env=RUSTC_VERSION=` |
| `BUILD_UNIX_MS` | `SOURCE_DATE_EPOCH` (seconds) ×1000 (`build.rs:35-38`) | `SystemTime::now()` ms, else 0 (`build.rs:39-46`) | `cargo:rustc-env=BUILD_UNIX_MS=` |

`rerun-if-env-changed` is declared for `GIT_SHA` and `SOURCE_DATE_EPOCH` (`build.rs:20-21`).

Where they surface (`src/build_info.rs:1-15`: `VERSION`=`CARGO_PKG_VERSION`, `GIT_SHA`, `RUSTC_VERSION`, `built_at_ms()`):
- `GET /v1/admin/info` → `version`, `git_sha`, `rustc`, `built_at` (unix ms), plus `started_at`, `uptime_seconds`, `env`, `role`, `key_scope`, `features{dev_ui,docs_ui,dashboard_ui,auth_disabled,archive_timelines,stale_while_revalidate,trust_proxy,postgres}`, `workers` (= `JOB_CONCURRENCY`) and `config` = `Config::redacted()` (`src/routes/admin.rs:2485-2512`; `src/config.rs:388-490`).
- `--version` → `riot-proxy <CARGO_PKG_VERSION>` (clap `version`, `src/cli/mod.rs:24`; observed `riot-proxy 2.7.0`).
- `CARGO_PKG_VERSION` also in `/dev/config.json` (`src/routes/ui.rs:179`), the UI page header (`src/routes/ui.rs:75`) and `info.version` of the OpenAPI document (`src/routes/docs.rs:351`).

Who sets `GIT_SHA`: CI `env: GIT_SHA: ${{ github.sha }}` (`.github/workflows/ci.yml:11-12`, `release.yml:24-25`, `edge.yml:25`), the Docker build-arg (`Dockerfile:13-15`; `ci.yml:187`). CI asserts the 12-char sha is embedded in the binary (`ci.yml:133`, `release.yml:73-75`, `edge.yml:48`). No workflow or Dockerfile sets `SOURCE_DATE_EPOCH` (grep), so shipped binaries carry the wall-clock build time. A unit test checks the values look sane (`src/build_info.rs:21-27`).

---

## 5. Ambiguities / unverified

- TODO (unverified): whether `backup`'s "OUT must not exist" (`src/cli/mod.rs:56`) is enforced by `jobs::maintenance::vacuum_into` or only by SQLite's `VACUUM INTO` failing on an existing file; `src/jobs/maintenance.rs` was not read.
- TODO (unverified): the `--key-scope` "8 hex characters" claim in `migrate-v1 --help` (`src/cli/migrate_v1.rs:44-47`) — no format validation is visible in `run()` (`381-384`); any string is accepted as far as the CLI module shows.
- TODO (unverified): exact semantics of an **empty** `ADMIN_IP_ALLOWLIST` ("any source") come from `.env.example:147-148`; the enforcement path in `src/http/auth.rs` beyond `AllowEntry::parse` (`66-77`) and `Auth::new` (`191-216`) was not read.
- TODO (unverified): whether building with `--features postgres` (`Cargo.toml:55-56`) changes any config validation — `src/config.rs:628-633` and `src/cli/mod.rs:128-133` have no `cfg(feature = "postgres")` gates, so on this reading `postgres://` is refused in every build; the feature only flips `features.postgres` in `/v1/admin/info` (`src/routes/admin.rs:2508`) and compiles stubs in `src/db/store.rs`.
- Discrepancy (verified, not ambiguous): `.env.example:34` documents `JOB_KIND_LIMITS=aggregate:analytics=1` while the code default is `aggregate:analytics=1,archive:offload=1` (`src/config.rs:111`; test `tests.rs:44-50`).
- TODO (unverified): the debug binary used for `--help` output was built with `dev-cli` (it lists `riot`); release artefacts are presumably built without it (`Cargo.toml:53` "Never in release builds") — not confirmed from the release workflow's cargo invocation.
- TODO (unverified): `LOG_LEVEL` is only applied by `serve`; whether other commands emit any `tracing` output at all (no subscriber is installed, so `tracing::warn!` in `Auth::new` etc. would be dropped) follows from the single `init_tracing` call site but was not exercised.

# Configuration reference

Every variable the proxy reads, with its type, default, validation and the line in `src/config.rs` that reads it; then how secrets are provisioned and redacted, the precedence rules, what `ENV=production` refuses, retired v1 names, the CLI flags and the last-known production values. The evidence inventory behind this page is `docs/_meta/scratch/config.md`.

The authoritative list is `VARS` (`src/config.rs:23-102`); a test asserts `.env.example` documents exactly that list (`src/config/tests.rs:435-454`). Anything else in the environment or `.env` is dropped before merging (`src/config.rs:985-992`).

Notation: `int(min..=max)` means a whole number in that range, `bool` accepts `true|1|false|0` case-insensitively, `csv` splits on commas and drops empties (`src/config.rs:1040-1046,1190-1219,1321-1335`). A value that fails validation keeps the default while the error is collected; all errors print together as `Invalid environment configuration:` and the command exits 1 (`src/config.rs:130-131,897-901`).

## Riot

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `RIOT_API_KEY` | secret string, ≥ 8 chars | none | yes | the Riot key sent upstream as `X-Riot-Token`; left out of `/v1/admin/info` entirely | `RGAPI-<redacted>` | `src/config.rs:693-697` |
| `KEY_SCOPE` | 8 lowercase hex | unset | no | pins the key scope instead of `sha256(RIOT_API_KEY)[:8]`, so rows written under another key stay readable (staging pins production's; ADR-200); `/readyz` reports the scope in effect | `3f9a1c2e` | `src/config.rs`, `src/cache/keys.rs` (`KeyScope::for_config`) |
| `RIOT_API_KEY_EXPIRES_AT` | RFC 3339 time | unset | no | when the key expires: `/status`'s `riot_key` is `degraded` from 48 h before it (development keys last 24 h, so it always is with one); a value that does not parse refuses the boot (STS-02b) | `2026-11-07T09:00:00Z` | `src/config.rs:785,1247-1254`; `src/status/checks.rs:132-159` |
| `RIOT_USER_AGENT` | string | `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)` | no | `User-Agent` on every Riot and Data Dragon call | the default | `src/config.rs:112,798` |

## Service

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ENV` | `development`, `test`, `production` (case-insensitive) | `development` | no | deployment environment; `production` turns on the refusals below. Falls back to `NODE_ENV` when unset | `production` | `src/config.rs:189-199,707-711` |
| `HOST` | string | `0.0.0.0` | no | bind address | `127.0.0.1` | `src/config.rs:800` |
| `PORT` | int(1..=65535) | `8080` | no | bind port | `8080` | `src/config.rs:801` |
| `LOG_LEVEL` | tracing filter directive | `info` | no | not validated at load; a bad filter fails `serve` with `invalid LOG_LEVEL filter` | `riot_proxy=debug,tower_http=info` | `src/config.rs:802`; `src/telemetry.rs:41-42` |
| `LOG_FORMAT` | `json`, `pretty` | `pretty` on a terminal, else `json` | no | log output format | `json` | `src/config.rs:245-254,740-747` |
| `DATA_DIR` | path | `./data` | no | SQLite file, Data Dragon mirror and `backups/`; created with parents on load | `/var/lib/riot-proxy/data` | `src/config.rs:713,905-910` |
| `DATABASE_URL` | `sqlite://<path>` | `sqlite://$DATA_DIR/riot-proxy.db` | no | `postgres://` is always refused on this build; any other scheme is an error | `sqlite:///var/lib/riot-proxy/data/riot-proxy.db` | `src/config.rs:113,714-730` |
| `ROLE` | `all`, `api`, `worker` | `all` | no | `api` and `worker` need Postgres, so only `all` boots | `all` | `src/config.rs:218-228,732-738` |
| `JOB_CONCURRENCY` | int(1..=u32::MAX) | `8` | no | scheduler worker tasks | `4` | `src/config.rs:810` |
| `JOB_KIND_LIMITS` | `kind=n,…` or `none` | `aggregate:analytics=1,archive:offload=1` | no | most running jobs per kind, for kinds without a rate-limit lane; `n` ≥ 1 | the default | `src/config.rs:126,811,1267-1298` |
| `JOB_YIELD_BUDGET_MS` | int(0..=900000) | `1000` | no | how long a job's fetch waits for the limiter before yielding its worker | `1000` | `src/config.rs:812` |

## Behaviour

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `CACHE_TTL_OVERRIDES` | csv `key=seconds` | empty | no | soft-TTL overrides for `account, summoner, league, ladder, match, timeline, spectator, mastery, rotations, status`; bad items are skipped silently; `match` and `timeline` are immutable and warned as ignored | `league=120,spectator=20` | `src/config.rs:813`; `src/riot/endpoints.rs:424-444` |
| `CACHE_L1_MAX_MB` | int(1..=1048576) MiB | `128` | no | in-memory response cache budget | `128` | `src/config.rs:814` |
| `NEG_TTL_SECONDS` | int(1..=u32::MAX) | `30` | no | negative-cache TTL for 404s | `30` | `src/config.rs:815` |
| `NEG_TTL_ACCOUNT_SECONDS` | int(1..=u32::MAX) | `300` | no | negative-cache TTL for account lookups | `300` | `src/config.rs:816` |
| `CLIENT_WAIT_BUDGET_MS` | int(0..=u64::MAX) | `2000` | no | how long an interactive request waits for a limiter token | `2000` | `src/config.rs:817` |
| `BULK_USAGE_CEILING` | float(0..=1) | `0.8` | no | share of a rate bucket bulk work may use while someone is active | `0.80` | `src/config.rs:818` |
| `BULK_IDLE_CEILING` | float(0..=1), `< 1`, ≥ `BULK_USAGE_CEILING` | `0.95`, raised to `BULK_USAGE_CEILING` if that is higher and this is unset | no | bulk share once a scope has been idle `BULK_IDLE_AFTER_S` | `0.95` | `src/config.rs:116,819,882-895` |
| `BULK_IDLE_AFTER_S` | int(0..=86400) | `30` | no | idle threshold for the idle ceiling | `30` | `src/config.rs:820` |
| `STALE_WHILE_REVALIDATE` | bool | `true` | no | serve stale entries while refreshing | `true` | `src/config.rs:821` |
| `METRICS_INTERVAL_S` | int(1..=300) | `5` | no | tick of the `metrics` WebSocket topic | `5` | `src/config.rs:822` |
| `METRICS_HISTORY_INTERVAL_S` | int(10..=3600) | `60` | no | cadence of the recorded metrics history | `60` | `src/config.rs:823` |
| `ARCHIVE_STATS_INTERVAL_S` | int(60..=86400) | `3600` | no | how often the archive's counts and byte sums for `GET /v1/admin/stats` are sampled: a pass over every stored body, so rare; reused in between, dated by `sampledAt.slow` (AVL-06, ADR-213) | `3600` | `src/config.rs:827`; `src/stats.rs:1074-1119` |

## Worker cadence

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `TRACK_POLL_LIVE_S` | int(10..=u32::MAX) | `60` | no | `poll:live` tick | `60` | `src/config.rs:824` |
| `TRACK_POLL_RANK_S` | int(30..=u32::MAX) | `600` | no | `poll:rank` tick | `600` | `src/config.rs:825` |
| `TRACK_POLL_MATCH_S` | int(30..=u32::MAX) | `300` | no | `poll:matches` tick | `300` | `src/config.rs:826` |
| `DDRAGON_SYNC_S` | int(60..=u32::MAX) | `3600` | no | `ddragon:sync` tick | `3600` | `src/config.rs:828` |
| `ARCHIVE_TIMELINES` | bool | `true` | no | archive jobs also fetch each match's timeline | `true` | `src/config.rs:829` |
| `TIMELINE_BACKFILL_PATCHES` | int(0..=1000) | `0` | no | newest patches `timelines:backfill` covers; `> 0` adds the hourly tick | `0` | `src/config.rs:830` |
| `BACKUP_KEEP` | int(0..=365) | `14` | no | daily `VACUUM INTO` backups kept in `$DATA_DIR/backups`; `0` takes none | `1` | `src/config.rs:831` |

## Object storage for raw bodies

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ARCHIVE_OBJECT_URL` | `s3://<bucket>` or `file:///<abs dir>` | unset | no | where packed bodies go; unset keeps every body in SQLite and registers no offload jobs. Bucket 3–63 chars of `a-z0-9.-`, no path | `s3://riot-proxy-archive` | `src/config.rs:999-1025,1077-1083` |
| `ARCHIVE_OBJECT_ENDPOINT` | `scheme://host[:port]` | unset | with `s3://` | trailing `/` dropped; a host starting with `<bucket>.` is virtual-hosted, else path style | `https://s3.ap-southeast-2.onidel.cloud` | `src/config.rs:1029-1041,1084-1093` |
| `ARCHIVE_OBJECT_REGION` | string | `auto` | no | signing region, passed through | `ap-southeast-2` | `src/config.rs:837` |
| `ARCHIVE_OBJECT_ACCESS_KEY` | secret | unset | both or neither | S3 access key id | `<access key id>` | `src/config.rs:1094-1099` |
| `ARCHIVE_OBJECT_SECRET_KEY` | secret | unset | both or neither | S3 secret key | `<secret>` | `src/config.rs:1095-1099` |
| `ARCHIVE_OBJECT_READ_ONLY` | bool | `false` | no | read packed bodies, never write the bucket: no offload, no index snapshot, no body freed, `put`/`delete` refused; needs `ARCHIVE_OBJECT_URL` (staging, ADR-201) | `true` | `src/config.rs`, `src/archive/objects.rs` |
| `ARCHIVE_OBJECT_CONCURRENCY` | int(1..=256) | `16` | no | ranged GETs in flight against the bucket | `16` | `src/config.rs:840` |
| `ARCHIVE_BODY_LOCAL_DAYS` | int(0..=365) | `14` | no | how long SQLite keeps a body already packed; `0` frees every packed body | `14` | `src/config.rs:842` |
| `ARCHIVE_PACK_TARGET_BYTES` | int(1 MiB..=1 GiB) | `67108864` | no | a pack closes at this many bytes | `67108864` | `src/config.rs:843` |
| `ARCHIVE_OFFLOAD_PACE_MS` | int(0..=60000) | `50` | no | pause between offload turns | `50` | `src/config.rs:844` |
| `LOOKUP_BACKFILL_LIMIT` | int(0..=4294967295) | `4294967295` | no | matches walked on a player's first lookup; `0` disables | the default | `src/config.rs:845` |
| `TRACK_CATCHUP_LIMIT` | int(0..=10000) | `500` | no | how far a match poll pages back | `500` | `src/config.rs:846` |

## Ladder crawl

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `LADDER_CRAWL_S` | int(0..=604800) | `0` | no | seconds between scheduled crawls; `0` is on demand only | `86400` | `src/config.rs:847` |
| `LADDER_QUEUES` | csv of `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` (case-sensitive) | `RANKED_SOLO_5x5` | no | ladders crawled; invalid entries error and are dropped | `RANKED_SOLO_5x5` | `src/config.rs:848-851,1303-1310` |
| `LADDER_PLATFORMS` | csv of platforms (case-insensitive) | empty | no | platforms the crawl tick covers; empty schedules none even with `LADDER_CRAWL_S` set | `oc1,kr,euw1,na1` | `src/config.rs:780-783,1260-1265` |
| `LADDER_TIER_FLOOR` | tier name, upper-cased | `MASTER` | no | lowest tier a crawl enumerates | `MASTER` | `src/config.rs:853,1313-1322` |
| `LADDER_BACKFILL_LIMIT` | int(0..=10000) | `100` | no | matches walked per player at first sight (no collect cursor yet); `0` walks nobody | `100` | `src/config.rs:854` |
| `LADDER_CRAWL_MATCH_LIMIT` | int(0..=100000000) | `0` | no | the most matches a platform may hold, archived or queued for download, before its crawls' archive hand-off queues no more; the rest are counted as `matchesSkipped`. Every match on the platform counts, whatever archived it. `0` is no cap. Staging sets `10000` (STG-03, ADR-202) | `10000` | `src/config.rs`, `src/jobs/ladder.rs` (`archive_set`) |
| `LADDER_COLLECT_OVERLAP_S` | int(0..=86400) | `7200` | no | a player collected before is asked only for games since then (THR-04); this many seconds before that last collect, for games in progress at the time | unset (default) | `src/config.rs:856` |
| `RANK_LOOKUP_LIMIT` | int(0..=10000000) | `50000` | no | players one `ranks:lookup` looks up; `0` off | `50000` | `src/config.rs:857` |
| `RANK_LOOKUP_RECHECK_S` | int(0..=31536000) | `604800` | no | a looked-up player stays off the next list this long | `604800` | `src/config.rs:858` |
| `TIER_LATE_STAMP_DAYS` | int(0..=365) | `14` | no | days back a later-seen rank stamps `UNKNOWN` tiers | `14` | `src/config.rs:859` |

## Facts and analytics

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `FACTS_REEXTRACT_BATCH` | int(1..=10000) | `500` | no | matches per `facts:reextract` batch | `500` | `src/config.rs:857` |
| `AGGREGATE_PATCH_LIMIT` | int(0..=100) | `4` | no | patches rebuilt per recompute; `0` rebuilds every patch | `0` | `src/config.rs:858` |
| `AGGREGATE_INTERVAL_S` | int(0..=604800) | `0` | no | seconds between unprompted recomputes; `0` off | `0` | `src/config.rs:860` |
| `AGGREGATE_DELTA_S` | int(0..=86400) | `60` | no | seconds between `aggregate:delta` runs, which count newly archived matches in short write transactions (THR-03, ADR-182); `0` off, and crawls and rank lookups queue full recomputes instead | `60` | `src/config.rs:861`; `.env.example:155-161` |
| `AGGREGATE_DELTA_TARGET_MS` | int(20..=1000) | `100` | no | the statement time each delta batch is sized toward | `100` | `src/config.rs:862` |
| `AGGREGATE_DELTA_BATCH` | int(5..=5000) | `500` | no | the most matches in one delta batch | `500` | `src/config.rs:863` |
| `ANALYTICS_CACHE_ENTRIES` | int(0..=4096) | `512` | no | analytics responses kept in memory (about 40 kB each) | `512` | `src/config.rs:859`; `.env.example:162-164` |

## Static data

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `DDRAGON_DIR` | path | `$DATA_DIR/ddragon` | no | Data Dragon mirror directory | `/var/lib/riot-proxy/data/ddragon` | `src/config.rs:804-806` |
| `DDRAGON_LOCALE` | string | `en_US` | no | Data Dragon locale | `en_US` | `src/config.rs:864` |

## Security

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ADMIN_IP_ALLOWLIST` | csv of IPs or CIDRs | empty | no | sources allowed to reach `/v1/admin/*`; empty allows any; an unparseable entry is warned and ignored at `serve` | `10.0.0.0/8` | `src/config.rs:865`; `src/http/auth.rs:66-77,196-202` |
| `STATUS_ALLOW_CIDR` | csv of IPs or CIDRs, or `none` | `127.0.0.1/32,::1/128` | no | TCP peers that may read `GET /status` without a key, on a request carrying none of `Cf-Connecting-IP`, `X-Forwarded-For`, `Forwarded` or `X-Real-IP` (a reverse proxy typically adds one; only their presence counts, never their values, whatever `TRUST_PROXY` says); `none` lets nobody in keyless. Set `none` when a reverse proxy on the same host is configured to add none of these (nginx without `proxy_set_header X-Forwarded-For`, HAProxy without `option forwardfor`): its requests arrive from loopback and would read as local. An unparseable entry refuses the boot (STS-02, ADR-203, ADR-205) | `none` | `src/config.rs:119,870,1161-1173`; `src/http/auth.rs:236-242,318-323,381-405` |
| `STATUS_CHECK_INTERVAL_S` | int(1..=3600) | `30` | no | how often `/status`'s checks run; the first platform below is probed every round while idle, the 60 s cadences round up to whole intervals (STS-02b, ADR-206) | `30` | `src/config.rs:871`; `src/status/checks.rs:250-257,326-335` |
| `STATUS_CHECK_PLATFORMS` | csv of platforms, or `none` | `LADDER_PLATFORMS` | no | the platforms `riot_api` probes with `lol-status-v4`, in order without repeats; only the first can make it `down`. `none`, or no platform at all, leaves `riot_api` out of `checks` (STS-02b, ADR-206) | `oc1,kr` | `src/config.rs:784,1229-1245`; `src/status/checks.rs:75-101` |
| `TRUST_PROXY` | bool | `false` | no | take the client address from the leftmost `X-Forwarded-For` | `true` | `src/config.rs:866`; `src/http/auth.rs:128-139` |
| `ADMIN_CORS_ORIGINS` | csv of `http(s)://host[:port]` | empty | no | browser origins allowed on `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz`; `*` refused; empty sends no CORS headers | `https://dash.example.com` | `src/config.rs:867,1125-1143` |
| `BOOTSTRAP_ADMIN_KEY` | secret, ≥ 8 chars | unset | no | the first admin key; used only when `consumers` is empty, otherwise one is generated and printed once | `<rpx_… you chose>` | `src/config.rs:868`; `src/consumers.rs:308-333` |
| `AUTH_DISABLED` | bool | `false` | no | every request runs as the synthetic `dev-local` consumer; refused in production | `false` | `src/config.rs:749-753` |
| `RIOT_BASE_URL` | URL | unset | no | send every Riot request to a mock; refused in production | `http://127.0.0.1:9000` | `src/config.rs:766-775` |
| `DDRAGON_BASE_URL` | URL | unset | no | fetch Data Dragon from a mock; refused in production | `http://127.0.0.1:9001` | `src/config.rs:767-775` |

## UIs

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `DEV_UI` | bool | `true` outside production; always `false` in production | no | serves `/dev`, `/dev/showcase`, `/dev/config.json` and the dev reset routes; a `true` in production is warned and ignored | `false` | `src/config.rs:755-757,874-875` |
| `DOCS_UI` | bool | `true` | no | serves `/docs`, `/openapi.json`, `/openapi.yaml` | `true` | `src/config.rs:876` |
| `DASHBOARD_UI` | bool | `true` | no | serves `/dashboard` | `true` | `src/config.rs:877` |

## Secrets

The `Secret` wrapper prints `[REDACTED]` for `Debug` and `Display`; only `.expose()` returns the value (`src/config.rs:146-170`). `GET /v1/admin/info` returns `Config::redacted()`: every variable in effect except `RIOT_API_KEY`, which is left out entirely, "not even its length" (`src/config.rs:436-443`; `src/routes/admin.rs:2501-2528`).

| Secret | Provisioned how | Lives where on the host | Redaction | Evidence |
|---|---|---|---|---|
| `RIOT_API_KEY` | issued by the Riot developer portal; a personal key today (100 per 2 min, 20 per 1 s) | `/etc/riot-proxy/env` only | absent from `/v1/admin/info`; `X-Riot-Token` header marked sensitive so it never reaches a log; CI fails on any `RGAPI-` string in `tests/`, `src/`, `docs/` | `docs/design/12-object-storage-and-hosting.md:484`; `src/riot/client.rs:327-329`; `.github/workflows/ci.yml:52-53` |
| `BOOTSTRAP_ADMIN_KEY` | chosen by the operator, or omitted so `serve` mints one and prints it to stderr once | `/etc/riot-proxy/env` (optional); the consumer row keeps only its sha256 | `[REDACTED]` in `/v1/admin/info`; an imported key is never printed | `src/config.rs:562-565`; `src/cli/serve.rs:64-75,443-450`; `src/consumers.rs:105-107` |
| `ARCHIVE_OBJECT_ACCESS_KEY` / `ARCHIVE_OBJECT_SECRET_KEY` | Onidel console, key scoped to bucket `riot-proxy-archive` | `/etc/riot-proxy/env` and the `[onidel]` remote of `/etc/riot-proxy/rclone.conf` | `[REDACTED]` in `/v1/admin/info`; boot logs `s3://<bucket> at <endpoint>` only | `src/config.rs:499-510`; `src/archive/objects.rs:142,201-204`; `deploy/README.md:24-25,51-62` |
| Consumer keys (`rpx_…`) | `riot-proxy key create` or `POST /v1/admin/consumers`; shown once | only their sha256 in `consumers.key_sha256`; the site holds its own copy in `/etc/ninjagoldfinch-lol/env` as `PROXY_KEY` | never listed again; `key list` shows no keys | `src/consumers.rs:95-107,154-157`; `src/cli/key.rs:59-79`; `docs/_meta/scratch/siblings.md:141` |
| R2 tokens | Cloudflare dashboard, one per bucket | `/etc/riot-proxy/rclone.conf` (`[r2]`, replica) and `/etc/rclone/r2.conf` (backups) | never seen by the proxy | `deploy/README.md:26-28,64-72,151` |

## Precedence

Lowest to highest: built-in defaults, `.env`, the process environment, CLI flags (`src/config.rs:3`; `.env.example:2`). `.env` is read relative to the process working directory and may be absent (`src/config.rs:651-665`). Each layer is filtered by `known()`, which drops unknown names and **empty values**, so `FOO=` never shadows a lower layer: an empty value means "use the default" (`src/config.rs:983-992`; `src/config/tests.rs:269-279`). Flag values go through the same validation as environment values (`src/config/tests.rs:192-203`).

## Production refusals

With `ENV=production` (or `--env production`), `Config::load` fails on any of these (`src/config.rs:749-775,1099-1111`):

| Setting | Error |
|---|---|
| `AUTH_DISABLED=true` | `AUTH_DISABLED cannot be enabled when ENV=production` |
| `RIOT_BASE_URL` set | `RIOT_BASE_URL cannot be set when ENV=production` |
| `DDRAGON_BASE_URL` set | `DDRAGON_BASE_URL cannot be set when ENV=production` |
| `ARCHIVE_OBJECT_URL=file://…` | `file:// cannot be used when ENV=production` |
| `ARCHIVE_OBJECT_URL=s3://…` without both keys | `needs ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY when ENV=production` |
| `ARCHIVE_OBJECT_ENDPOINT=http://…` | `must be https:// when ENV=production` |
| `DEV_UI=true` | not an error: `dev_ui` is forced `false` and `serve` warns `DEV_UI=true ignored: /dev is never served when ENV=production` (`src/config.rs:874-875`; `src/cli/serve.rs:102-104`) |

## Legacy v1 variables

| Variable | Behaviour | Evidence |
|---|---|---|
| `NODE_ENV` | read only as a fallback when `ENV` is unset; `ENV` wins when both are set | `src/config.rs:108-110,707-711` |
| `TLS` | `true`/`1` refuses to start: `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`; `false`/`0` is ignored | `src/config.rs:104-106,759-764` |
| `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT`, `REDIS_URL`, `SF_LOCK_MS`, `DEFAULT_PLATFORM`, `AGGREGATE_MIN_GAMES` | not in `VARS`: silently dropped. `DEFAULT_PLATFORM` does not seed `LADDER_PLATFORMS` | `src/config.rs:985-992`; `src/config/tests.rs:367-374,590-596` |
| `DATABASE_URL=postgres://…` | refused with `postgres:// needs a build with the postgres feature (not available yet)` | `src/config.rs:719-724` |

## CLI flags

Ten global flags map onto variables and are accepted before or after any subcommand (`src/config.rs:576-636`; `src/cli/mod.rs:29-30`): `--host` → `HOST`, `--port` → `PORT`, `--data-dir` → `DATA_DIR`, `--database-url` → `DATABASE_URL`, `--env` → `ENV`, `--role` → `ROLE`, `--job-concurrency` → `JOB_CONCURRENCY`, `--analytics-cache-entries` → `ANALYTICS_CACHE_ENTRIES`, `--log-level` → `LOG_LEVEL`, `--log-format` → `LOG_FORMAT`. Every subcommand but `spec` loads the configuration first and therefore needs a valid `RIOT_API_KEY` and creates `DATA_DIR` (`src/cli/mod.rs:72-88`). Subcommands: `serve`, `migrate`, `key create|list|revoke`, `healthcheck`, `spec`, `migrate-v1`, `backup <OUT>`, `packs reindex|restore-index|snapshot` (`src/cli/mod.rs:37-61`); `riot get|record` exists only in `dev-cli` builds (`Cargo.toml:52-54`).

## Known discrepancy

`.env.example:38` documents `JOB_KIND_LIMITS=aggregate:analytics=1` and `README.md:102` says the same, while the code default is `aggregate:analytics=1,archive:offload=1` (`src/config.rs:126`; pinned by `src/config/tests.rs:44-50`; `docs/design/07-deployment.md:125` agrees with the code). The code is right: leave the variable unset and both caps apply.

## Last-known production values

From session notes, not from the live file. The box's env file was last described on 2026-10-10 and 2026-10-13; the object-storage values were added on 2026-10-13 and 2026-10-14 (`docs/design/12-object-storage-and-hosting.md:437-446`; `reports/OBJ implementation handoff.md:20`, dated 2026-10-13).

| Variable | Value | As of |
|---|---|---|
| `ENV` | `production` | implied by `/dev` being off and `AUTH_DISABLED` refused; `README.md:97` |
| `HOST` | `127.0.0.1` | 2026-10-13 (`docs/design/12-object-storage-and-hosting.md:806-807`) |
| `PORT` | `8080` | 2026-10-13 |
| `TRUST_PROXY` | `true` | 2026-10-14 (`docs/design/12-object-storage-and-hosting.md:868-873`) |
| `ADMIN_IP_ALLOWLIST` | unset | 2026-10-14 (same) |
| `LADDER_PLATFORMS` | `oc1,kr,euw1,na1` | 2026-10-10 |
| `LADDER_CRAWL_S` | `86400` | 2026-10-13 |
| `LADDER_TIER_FLOOR` | `MASTER` | 2026-10-10 |
| `LADDER_BACKFILL_LIMIT` | `100` | 2026-10-10 |
| `LADDER_COLLECT_OVERLAP_S` | unset (default `7200`, from the release that carries THR-04) | 2026-10-26 |
| `ARCHIVE_TIMELINES` | `true` | 2026-10-10 |
| `BULK_USAGE_CEILING` | `0.80` | 2026-10-13 |
| `BULK_IDLE_CEILING` | unset (default `0.95`) | 2026-10-13 |
| `AGGREGATE_PATCH_LIMIT` | `0` | 2026-10-10; `docs/DECISIONS.md:1862` |
| `JOB_CONCURRENCY` | `4` | 2026-10-13 (`reports/OBJ implementation handoff.md:20`) |
| `BACKUP_KEEP` | `1` | 2026-10-13 (same; `docs/design/12-object-storage-and-hosting.md:828-830`) |
| `ARCHIVE_OBJECT_URL` | `s3://riot-proxy-archive` | 2026-10-13 |
| `ARCHIVE_OBJECT_ENDPOINT` | `https://s3.ap-southeast-2.onidel.cloud` | 2026-10-13 |
| `ARCHIVE_OBJECT_REGION` | `ap-southeast-2` | 2026-10-14 (`docs/design/12-object-storage-and-hosting.md:443`) |
| `ARCHIVE_BODY_LOCAL_DAYS` | unset (default `14`) | 2026-10-18 |

> **TODO (unverified):** the live `/etc/riot-proxy/env` on `ngf-prod-syd1` was not read (it is root-only on the box and holds the Riot key). The values above come from dated session notes and the design record; any may have changed since.

---
**Sources:** `src/config.rs:L3,L23-L170,L189-L254,L436-L573,L576-L636,L651-L665,L687-L912,L938-L992,L996-L1143,L1190-L1335`, `src/config/tests.rs:L44-L50,L192-L203,L269-L279,L367-L374,L435-L454,L590-L596,L875-L881`, `src/cli/mod.rs:L29-L88`, `src/cli/serve.rs:L64-L75,L102-L104,L443-L450`, `src/consumers.rs:L95-L107,L154-L157,L308-L333`, `src/http/auth.rs:L66-L77,L128-L139,L196-L202`, `src/riot/client.rs:L327-L329`, `src/riot/endpoints.rs:L424-L444`, `src/routes/admin.rs:L2501-L2528`, `src/archive/objects.rs:L142,L201-L204`, `.env.example`, `README.md:L97,L102`, `.github/workflows/ci.yml:L52-L53`, `deploy/README.md:L24-L28,L51-L72,L151`, `docs/design/07-deployment.md:L125`, `docs/design/12-object-storage-and-hosting.md:L437-L446,L484,L806-L807,L828-L830,L868-L873`, `docs/DECISIONS.md:L1862`, `reports/OBJ implementation handoff.md:L20`, `docs/_meta/scratch/config.md`
**Last verified:** 2026-11-09 against commit `302025d`

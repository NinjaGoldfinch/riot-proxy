# Configuration reference

Every variable the proxy reads, with its type, default, validation and the line in `src/config.rs` that reads it; then how secrets are provisioned and redacted, the precedence rules, what `ENV=production` refuses, retired v1 names, the CLI flags and the last-known production values. The evidence inventory behind this page is `docs/_meta/scratch/config.md`.

The authoritative list is `VARS` (`src/config.rs:23-105`); a test asserts `.env.example` documents exactly that list (`src/config/tests.rs:443-462`). Anything else in the environment or `.env` is dropped before merging (`src/config.rs:1039-1046`).

Notation: `int(min..=max)` means a whole number in that range, `bool` accepts `true|1|false|0` case-insensitively, `csv` splits on commas and drops empties (`src/config.rs:1094-1100,1244-1273,1376-1390`). A value that fails validation keeps the default while the error is collected; all errors print together as `Invalid environment configuration:` and the command exits 1 (`src/config.rs:133-134,951-955`).

## Riot

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `RIOT_API_KEY` | secret string, ≥ 8 chars | none | yes | the Riot key sent upstream as `X-Riot-Token`; left out of `/v1/admin/info` entirely | `RGAPI-<redacted>` | `src/config.rs:743-747` |
| `KEY_SCOPE` | 8 lowercase hex | unset | no | pins the key scope instead of `sha256(RIOT_API_KEY)[:8]`, so rows written under another key stay readable (staging pins production's; ADR-200); `/readyz` reports the scope in effect | `3f9a1c2e` | `src/config.rs`, `src/cache/keys.rs` (`KeyScope::for_config`) |
| `RIOT_API_KEY_EXPIRES_AT` | RFC 3339 time | unset | no | when the key expires: `/status`'s `riot_key` is `degraded` from 48 h before it (development keys last 24 h, so it always is with one); a value that does not parse refuses the boot (STS-02b) | `2026-11-07T09:00:00Z` | `src/config.rs:836,1302-1309`; `src/status/checks.rs:132-159` |
| `RIOT_USER_AGENT` | string | `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)` | no | `User-Agent` on every Riot and Data Dragon call | the default | `src/config.rs:115,849` |

## Service

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ENV` | `development`, `test`, `production` (case-insensitive) | `development` | no | deployment environment; `production` turns on the refusals below. Falls back to `NODE_ENV` when unset | `production` | `src/config.rs:216-226,757-761` |
| `HOST` | string | `0.0.0.0` | no | bind address | `127.0.0.1` | `src/config.rs:851` |
| `PORT` | int(1..=65535) | `8080` | no | bind port | `8080` | `src/config.rs:852` |
| `LOG_LEVEL` | tracing filter directive | `info` | no | not validated at load; a bad filter fails `serve` with `invalid LOG_LEVEL filter` | `riot_proxy=debug,tower_http=info` | `src/config.rs:853`; `src/telemetry.rs:41-42` |
| `LOG_FORMAT` | `json`, `pretty` | `pretty` on a terminal, else `json` | no | log output format | `json` | `src/config.rs:272-281,790-797` |
| `DATA_DIR` | path | `./data` | no | SQLite file, Data Dragon mirror and `backups/`; created with parents on load | `/var/lib/riot-proxy/data` | `src/config.rs:763,959-964` |
| `DATABASE_URL` | `sqlite://<path>` | `sqlite://$DATA_DIR/riot-proxy.db` | no | `postgres://` is always refused on this build; any other scheme is an error | `sqlite:///var/lib/riot-proxy/data/riot-proxy.db` | `src/config.rs:116,764-780` |
| `ROLE` | `all`, `api`, `worker` | `all` | no | `api` and `worker` need Postgres, so only `all` boots | `all` | `src/config.rs:245-255,782-788` |
| `JOB_CONCURRENCY` | int(1..=u32::MAX) | `8` | no | scheduler worker tasks | `4` | `src/config.rs:861` |
| `JOB_KIND_LIMITS` | `kind=n,…` or `none` | `aggregate:analytics=1,archive:offload=1` | no | most running jobs per kind, for kinds without a rate-limit lane; `n` ≥ 1 | the default | `src/config.rs:129,862,1322-1353` |
| `JOB_YIELD_BUDGET_MS` | int(0..=900000) | `1000` | no | how long a job's fetch waits for the limiter before yielding its worker | `1000` | `src/config.rs:863` |

## Behaviour

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `CACHE_TTL_OVERRIDES` | csv `key=seconds` | empty | no | soft-TTL overrides for `account, summoner, league, ladder, match, timeline, spectator, mastery, rotations, status`; bad items are skipped silently; `match` and `timeline` are immutable and warned as ignored | `league=120,spectator=20` | `src/config.rs:864`; `src/riot/endpoints.rs:469-489` |
| `CACHE_L1_MAX_MB` | int(1..=1048576) MiB | `128` | no | in-memory response cache budget | `128` | `src/config.rs:865` |
| `NEG_TTL_SECONDS` | int(1..=u32::MAX) | `30` | no | negative-cache TTL for 404s | `30` | `src/config.rs:866` |
| `NEG_TTL_ACCOUNT_SECONDS` | int(1..=u32::MAX) | `300` | no | negative-cache TTL for account lookups | `300` | `src/config.rs:867` |
| `CLIENT_WAIT_BUDGET_MS` | int(0..=u64::MAX) | `2000` | no | how long an interactive request waits for a limiter token; plus 250 ms, how long a `/v1/players/*` part with a stored copy waits for its refresh before that copy is served (AVL-09; a forced `?refresh=true` read, and a part with nothing stored, waits 20 s, a constant, or this + 250 ms when that is longer); at 0 an expired part gets 250 ms; raising it eats into the site's 30 s (up to three stages by Riot ID) | `2000` | `src/config.rs:868` |
| `BULK_USAGE_CEILING` | float(0..=1) | `0.8` | no | share of a rate bucket bulk work may use while someone is active | `0.80` | `src/config.rs:869` |
| `BULK_IDLE_CEILING` | float(0..=1), `< 1`, ≥ `BULK_USAGE_CEILING` | `0.95`, raised to `BULK_USAGE_CEILING` if that is higher and this is unset | no | bulk share once a scope has been idle `BULK_IDLE_AFTER_S` | `0.95` | `src/config.rs:119,870,936-949` |
| `BULK_IDLE_AFTER_S` | int(0..=86400) | `30` | no | idle threshold for the idle ceiling | `30` | `src/config.rs:871` |
| `STALE_WHILE_REVALIDATE` | bool | `true` | no | serve stale entries while refreshing | `true` | `src/config.rs:872` |
| `METRICS_INTERVAL_S` | int(1..=300) | `5` | no | tick of the `metrics` WebSocket topic | `5` | `src/config.rs:873` |
| `METRICS_HISTORY_INTERVAL_S` | int(10..=3600) | `60` | no | cadence of the recorded metrics history | `60` | `src/config.rs:874` |
| `ARCHIVE_STATS_INTERVAL_S` | int(60..=86400) | `3600` | no | how often the archive's counts and byte sums for `GET /v1/admin/stats` are sampled: a pass over every stored body, so rare; reused in between, dated by `sampledAt.slow` (AVL-06, ADR-213) | `3600` | `src/config.rs:878`; `src/stats.rs:1074-1119` |

## Worker cadence

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `TRACK_POLL_LIVE_S` | int(10..=u32::MAX) | `60` | no | `poll:live` tick | `60` | `src/config.rs:875` |
| `TRACK_POLL_RANK_S` | int(30..=u32::MAX) | `600` | no | `poll:rank` tick | `600` | `src/config.rs:876` |
| `TRACK_POLL_MATCH_S` | int(30..=u32::MAX) | `300` | no | `poll:matches` tick | `300` | `src/config.rs:877` |
| `DDRAGON_SYNC_S` | int(60..=u32::MAX) | `3600` | no | `ddragon:sync` tick | `3600` | `src/config.rs:879` |
| `ARCHIVE_TIMELINES` | bool | `true` | no | archive jobs also fetch each match's timeline | `true` | `src/config.rs:880` |
| `TIMELINE_BACKFILL_PATCHES` | int(0..=1000) | `0` | no | newest patches `timelines:backfill` covers; `> 0` adds the hourly tick | `0` | `src/config.rs:881` |
| `BACKUP_KEEP` | int(0..=365) | `14` | no | daily `VACUUM INTO` backups kept in `$DATA_DIR/backups`; `0` takes none; above 0 it also makes the `maintenance` tick count from the newest backup at boot (AVL-13) | `1` | `src/config.rs:882` |

## Object storage for raw bodies

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ARCHIVE_OBJECT_URL` | `s3://<bucket>` or `file:///<abs dir>` | unset | no | where packed bodies go; unset keeps every body in SQLite and registers no offload jobs. Bucket 3–63 chars of `a-z0-9.-`, no path | `s3://riot-proxy-archive` | `src/config.rs:1053-1079,1131-1137` |
| `ARCHIVE_OBJECT_ENDPOINT` | `scheme://host[:port]` | unset | with `s3://` | trailing `/` dropped; a host starting with `<bucket>.` is virtual-hosted, else path style | `https://s3.ap-southeast-2.onidel.cloud` | `src/config.rs:1083-1095,1138-1147` |
| `ARCHIVE_OBJECT_REGION` | string | `auto` | no | signing region, passed through | `ap-southeast-2` | `src/config.rs:888` |
| `ARCHIVE_OBJECT_ACCESS_KEY` | secret | unset | both or neither | S3 access key id | `<access key id>` | `src/config.rs:1148-1153` |
| `ARCHIVE_OBJECT_SECRET_KEY` | secret | unset | both or neither | S3 secret key | `<secret>` | `src/config.rs:1149-1153` |
| `ARCHIVE_OBJECT_READ_ONLY` | bool | `false` | no | read packed bodies, never write the bucket: no offload, no index snapshot, no body freed, `put`/`delete` refused; needs `ARCHIVE_OBJECT_URL` (staging, ADR-201) | `true` | `src/config.rs`, `src/archive/objects.rs` |
| `ARCHIVE_OBJECT_CONCURRENCY` | int(1..=256) | `16` | no | ranged GETs in flight against the bucket | `16` | `src/config.rs:891` |
| `ARCHIVE_BODY_LOCAL_DAYS` | int(0..=365) | `14` | no | how long SQLite keeps a body already packed; `0` frees every packed body | `14` | `src/config.rs:893` |
| `ARCHIVE_PACK_TARGET_BYTES` | int(1 MiB..=1 GiB) | `67108864` | no | a pack closes at this many bytes | `67108864` | `src/config.rs:894` |
| `ARCHIVE_OFFLOAD_PACE_MS` | int(0..=60000) | `50` | no | pause between offload turns | `50` | `src/config.rs:895` |
| `LOOKUP_BACKFILL_LIMIT` | int(0..=4294967295) | `4294967295` | no | matches walked on a player's first lookup; `0` disables | the default | `src/config.rs:896` |
| `TRACK_CATCHUP_LIMIT` | int(0..=10000) | `500` | no | how far a match poll pages back | `500` | `src/config.rs:897` |

## Ladder crawl

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `LADDER_CRAWL_S` | int(0..=604800) | `0` | no | seconds between scheduled crawls; `0` is on demand only | `86400` | `src/config.rs:898` |
| `LADDER_QUEUES` | csv of `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` (case-sensitive) | `RANKED_SOLO_5x5` | no | ladders crawled; invalid entries error and are dropped | `RANKED_SOLO_5x5` | `src/config.rs:899-902,1358-1365` |
| `LADDER_PLATFORMS` | csv of platforms (case-insensitive): any of the 17 in `Platform::ALL`, `me1` included since ME-02 (ADR-216) | empty | no | platforms the crawl tick covers; empty schedules none even with `LADDER_CRAWL_S` set | `oc1,kr,euw1,na1` | `src/config.rs:830-833,1315-1320` |
| `LADDER_TIER_FLOOR` | tier name, upper-cased | `MASTER` | no | lowest tier a crawl enumerates | `MASTER` | `src/config.rs:904,1368-1377` |
| `LADDER_BACKFILL_LIMIT` | int(0..=10000) | `100` | no | matches walked per player at first sight (no collect cursor yet); `0` walks nobody | `100` | `src/config.rs:905` |
| `LADDER_CRAWL_MATCH_LIMIT` | int(0..=100000000) | `0` | no | the most matches a platform may hold, archived or queued for download, before its crawls' archive hand-off queues no more; the rest are counted as `matchesSkipped`. Every match on the platform counts, whatever archived it. `0` is no cap. Staging sets `10000` (STG-03, ADR-202) | `10000` | `src/config.rs`, `src/jobs/ladder.rs` (`archive_set`) |
| `LADDER_COLLECT_OVERLAP_S` | int(0..=86400) | `7200` | no | a player collected before is asked only for games since then (THR-04); this many seconds before that last collect, for games in progress at the time | unset (default) | `src/config.rs:907` |
| `RANK_LOOKUP_LIMIT` | int(0..=10000000) | `50000` | no | players one `ranks:lookup` looks up; `0` off | `50000` | `src/config.rs:908` |
| `RANK_LOOKUP_RECHECK_S` | int(0..=31536000) | `604800` | no | a looked-up player stays off the next list this long | `604800` | `src/config.rs:909` |
| `TIER_LATE_STAMP_DAYS` | int(0..=365) | `14` | no | days back a later-seen rank stamps `UNKNOWN` tiers | `14` | `src/config.rs:910` |

## Facts and analytics

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `FACTS_REEXTRACT_BATCH` | int(1..=10000) | `500` | no | matches per `facts:reextract` batch | `500` | `src/config.rs:908` |
| `AGGREGATE_PATCH_LIMIT` | int(0..=100) | `4` | no | patches rebuilt per recompute; `0` rebuilds every patch | `0` | `src/config.rs:909` |
| `AGGREGATE_INTERVAL_S` | int(0..=604800) | `0` | no | seconds between unprompted recomputes; `0` off | `0` | `src/config.rs:911` |
| `AGGREGATE_DELTA_S` | int(0..=86400) | `60` | no | seconds between `aggregate:delta` runs, which count newly archived matches in short write transactions (THR-03, ADR-182); `0` off, and crawls and rank lookups queue full recomputes instead | `60` | `src/config.rs:912`; `.env.example:160-166` |
| `AGGREGATE_DELTA_TARGET_MS` | int(20..=1000) | `100` | no | the statement time each delta batch is sized toward | `100` | `src/config.rs:913` |
| `AGGREGATE_DELTA_BATCH` | int(5..=5000) | `500` | no | the most matches in one delta batch | `500` | `src/config.rs:914` |
| `ANALYTICS_CACHE_ENTRIES` | int(0..=4096) | `512` | no | analytics responses kept in memory (about 40 kB each) | `512` | `src/config.rs:910`; `.env.example:167-169` |

## Static data

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `DDRAGON_DIR` | path | `$DATA_DIR/ddragon` | no | Data Dragon mirror directory | `/var/lib/riot-proxy/data/ddragon` | `src/config.rs:855-857` |
| `DDRAGON_LOCALE` | string | `en_US` | no | Data Dragon locale | `en_US` | `src/config.rs:915` |

## Security

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `ADMIN_IP_ALLOWLIST` | csv of IPs or CIDRs | empty | no | sources allowed to reach `/v1/admin/*`; empty allows any; an unparseable entry is warned and ignored at `serve` | `10.0.0.0/8` | `src/config.rs:916`; `src/http/auth.rs:66-77,196-202` |
| `STATUS_ALLOW_CIDR` | csv of IPs or CIDRs, or `none` | `127.0.0.1/32,::1/128` | no | TCP peers that may read `GET /status` without a key, on a request carrying none of `Cf-Connecting-IP`, `X-Forwarded-For`, `Forwarded` or `X-Real-IP` (a reverse proxy typically adds one; only their presence counts, never their values, whatever `TRUST_PROXY` says); `none` lets nobody in keyless. Set `none` when a reverse proxy on the same host is configured to add none of these (nginx without `proxy_set_header X-Forwarded-For`, HAProxy without `option forwardfor`): its requests arrive from loopback and would read as local. An unparseable entry refuses the boot (STS-02, ADR-203, ADR-205) | `none` | `src/config.rs:122,921,1215-1227`; `src/http/auth.rs:236-242,318-323,381-405` |
| `STATUS_CHECK_INTERVAL_S` | int(1..=3600) | `30` | no | how often `/status`'s checks run; the first platform below is probed every round while idle, the 60 s cadences round up to whole intervals (STS-02b, ADR-206) | `30` | `src/config.rs:922`; `src/status/checks.rs:250-257,326-335` |
| `STATUS_CHECK_PLATFORMS` | csv of platforms, or `none` | `LADDER_PLATFORMS` | no | the platforms `riot_api` probes with `lol-status-v4`, in order without repeats; only the first can make it `down`. `none`, or no platform at all, leaves `riot_api` out of `checks` (STS-02b, ADR-206) | `oc1,kr` | `src/config.rs:834,1283-1300`; `src/status/checks.rs:75-101` |
| `STATUS_POLL_INTERVAL_S` | int(60..=3600) | `240` | no | how often the status poller reads `lol-status-v4` `platform-data` on every platform of `STATUS_POLL_PLATFORMS`, at bulk priority, through the response cache (written through) and into `platform_status`; a platform without limiter room within 30 s waits for the next round. 240 s keeps the per-platform route's copy inside its 300 s soft TTL between rounds (owner decision of 2026-11-17); past 300 s it goes stale between rounds and the route calls Riot (SITE-13, SITE-17, ADR-219) | `240` | `src/config.rs:924`; `src/status/poll.rs:351-370,440-451` |
| `STATUS_POLL_PLATFORMS` | csv of platforms, or `none` | every platform but `ph2`, `th2` and `tw2` (`config::STATUS_POLL_DEFAULT`, 14) | no | the platforms the status poller reads, in order without repeats; `none` turns the poller off. Each is one call per round. The default leaves out the three platforms the portal's lol-status-v4 list does not name: for `ph2` and `th2`, on 2026-11-17 a keyless GET of `/lol/status/v4/platform-data` got HTTP 530 with body `error code: 1016` from both hosts; `tw2` is unconfirmed (owner decision of 2026-11-17); list one to read it anyway | `oc1,kr` | `src/config.rs:835,1283-1300`; `src/status/poll.rs:351-356` |
| `STATUS_RECORDINGS_DIR` | path | unset | no | where the status poller writes a 200 body that lists an incident, as `<platform>-incident-<id>.json`, once per platform and incident id, with the API key replaced by `[REDACTED]` (a body still holding an `RGAPI-` string is not written); unset writes none. Must be writable by the service user: under systemd's `ProtectSystem=strict` that means under `/var/lib/riot-proxy` (`ReadWritePaths`), in the container under `/data`; a relative path resolves against the working directory. It is checked when the poller starts: one it cannot write logs `error` `STATUS_RECORDINGS_DIR cannot be written` and each recording then logs `warn` `status recording not written`; the rounds and their rows go on. For checked-in fixtures (SITE-16) | `/var/lib/riot-proxy/status-recordings` | `src/config.rs:926`; `src/status/poll.rs:149-227` |
| `TRUST_PROXY` | bool | `false` | no | take the client address from the leftmost `X-Forwarded-For` | `true` | `src/config.rs:917`; `src/http/auth.rs:128-139` |
| `ADMIN_CORS_ORIGINS` | csv of `http(s)://host[:port]` | empty | no | browser origins allowed on `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz`; `*` refused; empty sends no CORS headers | `https://dash.example.com` | `src/config.rs:918,1179-1197` |
| `BOOTSTRAP_ADMIN_KEY` | secret, ≥ 8 chars | unset | no | the first admin key; used only when `consumers` is empty, otherwise one is generated and printed once | `<rpx_… you chose>` | `src/config.rs:919`; `src/consumers.rs:308-333` |
| `AUTH_DISABLED` | bool | `false` | no | every request runs as the synthetic `dev-local` consumer; refused in production | `false` | `src/config.rs:799-803` |
| `RIOT_BASE_URL` | URL | unset | no | send every Riot request to a mock; refused in production | `http://127.0.0.1:9000` | `src/config.rs:816-825` |
| `DDRAGON_BASE_URL` | URL | unset | no | fetch Data Dragon from a mock; refused in production | `http://127.0.0.1:9001` | `src/config.rs:817-825` |

## UIs

| Name | Type | Default | Required | Description | Example | Where read |
|---|---|---|---|---|---|---|
| `DEV_UI` | bool | `true` outside production; always `false` in production | no | serves `/dev`, `/dev/showcase`, `/dev/config.json` and the dev reset routes; a `true` in production is warned and ignored | `false` | `src/config.rs:805-807,928-929` |
| `DOCS_UI` | bool | `true` | no | serves `/docs`, `/openapi.json`, `/openapi.yaml` | `true` | `src/config.rs:930` |
| `DASHBOARD_UI` | bool | `true` | no | serves `/dashboard` | `true` | `src/config.rs:931` |

## Secrets

The `Secret` wrapper prints `[REDACTED]` for `Debug` and `Display`; only `.expose()` returns the value (`src/config.rs:173-197`). `GET /v1/admin/info` returns `Config::redacted()`: every variable in effect except `RIOT_API_KEY`, which is left out entirely, "not even its length" (`src/config.rs:472-479`; `src/routes/admin.rs:2501-2528`).

| Secret | Provisioned how | Lives where on the host | Redaction | Evidence |
|---|---|---|---|---|
| `RIOT_API_KEY` | issued by the Riot developer portal; a personal key today (100 per 2 min, 20 per 1 s) | `/etc/riot-proxy/env` only | absent from `/v1/admin/info`; `X-Riot-Token` header marked sensitive so it never reaches a log; CI fails on any `RGAPI-` string in `tests/`, `src/`, `docs/` | `docs/design/12-object-storage-and-hosting.md:484`; `src/riot/client.rs:386-388`; `.github/workflows/ci.yml:52-53` |
| `BOOTSTRAP_ADMIN_KEY` | chosen by the operator, or omitted so `serve` mints one and prints it to stderr once | `/etc/riot-proxy/env` (optional); the consumer row keeps only its sha256 | `[REDACTED]` in `/v1/admin/info`; an imported key is never printed | `src/config.rs:612-615`; `src/cli/serve.rs:68-96,483-490`; `src/consumers.rs:105-107` |
| `ARCHIVE_OBJECT_ACCESS_KEY` / `ARCHIVE_OBJECT_SECRET_KEY` | Onidel console, key scoped to bucket `riot-proxy-archive` | `/etc/riot-proxy/env` and the `[onidel]` remote of `/etc/riot-proxy/rclone.conf` | `[REDACTED]` in `/v1/admin/info`; boot logs `s3://<bucket> at <endpoint>` only | `src/config.rs:535-546`; `src/archive/objects.rs:142,201-204`; `deploy/README.md:24-25,51-62` |
| Consumer keys (`rpx_…`) | `riot-proxy key create` or `POST /v1/admin/consumers`; shown once | only their sha256 in `consumers.key_sha256`; the site holds its own copy in `/etc/ninjagoldfinch-lol/env` as `PROXY_KEY` | never listed again; `key list` shows no keys | `src/consumers.rs:95-107,154-157`; `src/cli/key.rs:59-79`; `docs/_meta/scratch/siblings.md:141` |
| R2 tokens | Cloudflare dashboard, one per bucket | `/etc/riot-proxy/rclone.conf` (`[r2]`, replica) and `/etc/rclone/r2.conf` (backups) | never seen by the proxy | `deploy/README.md:26-28,64-72,151` |

## Precedence

Lowest to highest: built-in defaults, `.env`, the process environment, CLI flags (`src/config.rs:3`; `.env.example:2`). `.env` is read relative to the process working directory and may be absent (`src/config.rs:701-715`). Each layer is filtered by `known()`, which drops unknown names and **empty values**, so `FOO=` never shadows a lower layer: an empty value means "use the default" (`src/config.rs:1037-1046`; `src/config/tests.rs:269-279`). Flag values go through the same validation as environment values (`src/config/tests.rs:192-203`).

## Production refusals

With `ENV=production` (or `--env production`), `Config::load` fails on any of these (`src/config.rs:799-825,1153-1165`):

| Setting | Error |
|---|---|
| `AUTH_DISABLED=true` | `AUTH_DISABLED cannot be enabled when ENV=production` |
| `RIOT_BASE_URL` set | `RIOT_BASE_URL cannot be set when ENV=production` |
| `DDRAGON_BASE_URL` set | `DDRAGON_BASE_URL cannot be set when ENV=production` |
| `ARCHIVE_OBJECT_URL=file://…` | `file:// cannot be used when ENV=production` |
| `ARCHIVE_OBJECT_URL=s3://…` without both keys | `needs ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY when ENV=production` |
| `ARCHIVE_OBJECT_ENDPOINT=http://…` | `must be https:// when ENV=production` |
| `DEV_UI=true` | not an error: `dev_ui` is forced `false` and `serve` warns `DEV_UI=true ignored: /dev is never served when ENV=production` (`src/config.rs:928-929`; `src/cli/serve.rs:123-125`) |

## Legacy v1 variables

| Variable | Behaviour | Evidence |
|---|---|---|
| `NODE_ENV` | read only as a fallback when `ENV` is unset; `ENV` wins when both are set | `src/config.rs:111-113,757-761` |
| `TLS` | `true`/`1` refuses to start: `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`; `false`/`0` is ignored | `src/config.rs:107-109,809-814` |
| `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT`, `REDIS_URL`, `SF_LOCK_MS`, `DEFAULT_PLATFORM`, `AGGREGATE_MIN_GAMES` | not in `VARS`: silently dropped. `DEFAULT_PLATFORM` does not seed `LADDER_PLATFORMS` | `src/config.rs:1039-1046`; `src/config/tests.rs:375-382,598-604` |
| `DATABASE_URL=postgres://…` | refused with `postgres:// needs a build with the postgres feature (not available yet)` | `src/config.rs:769-774` |

## CLI flags

Ten global flags map onto variables and are accepted before or after any subcommand (`src/config.rs:626-686`; `src/cli/mod.rs:29-30`): `--host` → `HOST`, `--port` → `PORT`, `--data-dir` → `DATA_DIR`, `--database-url` → `DATABASE_URL`, `--env` → `ENV`, `--role` → `ROLE`, `--job-concurrency` → `JOB_CONCURRENCY`, `--analytics-cache-entries` → `ANALYTICS_CACHE_ENTRIES`, `--log-level` → `LOG_LEVEL`, `--log-format` → `LOG_FORMAT`. Every subcommand but `spec` loads the configuration first and therefore needs a valid `RIOT_API_KEY` and creates `DATA_DIR` (`src/cli/mod.rs:72-88`). Subcommands: `serve`, `migrate`, `key create|list|revoke`, `healthcheck`, `spec`, `migrate-v1`, `backup <OUT>`, `packs reindex|restore-index|snapshot` (`src/cli/mod.rs:37-61`); `riot get|record` exists only in `dev-cli` builds (`Cargo.toml:52-54`).

## Known discrepancy

`.env.example:38` documents `JOB_KIND_LIMITS=aggregate:analytics=1` and `README.md:102` says the same, while the code default is `aggregate:analytics=1,archive:offload=1` (`src/config.rs:129`; pinned by `src/config/tests.rs:44-50`; `docs/design/07-deployment.md:129` agrees with the code). The code is right: leave the variable unset and both caps apply.

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
**Sources:** `src/config.rs:L3,L23-L197,L216-L281,L472-L623,L626-L686,L701-L715,L737-L966,L992-L1046,L1050-L1197,L1244-L1390`, `src/config/tests.rs:L44-L50,L192-L203,L269-L279,L367-L382,L443-L462,L598-L604,L883-L889`, `src/cli/mod.rs:L29-L88`, `src/cli/serve.rs:L64-L75,L102-L104,L456-L465`, `src/consumers.rs:L95-L107,L154-L157,L308-L333`, `src/http/auth.rs:L66-L77,L128-L139,L196-L202`, `src/riot/client.rs:L386-L388`, `src/riot/endpoints.rs:L469-L489`, `src/routes/admin.rs:L2501-L2528`, `src/archive/objects.rs:L142,L201-L204`, `.env.example`, `README.md:L97,L102`, `.github/workflows/ci.yml:L52-L53`, `deploy/README.md:L24-L28,L51-L72,L151`, `docs/design/07-deployment.md:L125`, `docs/design/12-object-storage-and-hosting.md:L437-L446,L484,L806-L807,L828-L830,L868-L873`, `docs/DECISIONS.md:L1862`, `reports/OBJ implementation handoff.md:L20`, `docs/_meta/scratch/config.md`
**Last verified:** 2026-11-20 against commit `1f37fc7`

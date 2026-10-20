# FAQ

Short answers to the questions a new contributor or integrator asks first, each with the document that has the detail.

## Design

**Why one process?** A Riot key gives one application-level rate limit shared by every request made with it; two replicas would split that budget and then need Redis to agree on the split, while one tokio runtime can serve far more requests than a Riot key allows. Availability during a deploy comes from a sub-100 ms restart with a persisted limiter and cache instead (ADR-001, `docs/design/03-architecture.md:L32-L36`). See [../architecture/overview.md](../architecture/overview.md).

**Why SQLite?** Everything lives in one WAL-mode file under `DATA_DIR`: cache, archive, jobs, consumers, limiter checkpoint. One file is one backup (`VACUUM INTO`) and no second service to run; Postgres stays behind a feature flag for archives beyond roughly 50 GB (ADR-002, `docs/design/02-stack-options.md`). See [../data/schema.md](../data/schema.md).

**Why packs rather than one object per match?** Onidel caps a bucket at 3 million objects, which one object per body would pass within months; a 64 MiB pack holds about 6 000 matches or 1 000 timelines, so the whole archive is a few hundred objects with years of headroom (`docs/design/12-object-storage-and-hosting.md:L83-L94`, `L151-L156`). See [../integration/object-storage.md](../integration/object-storage.md).

**Why is there no Docker in production?** The release is one static musl binary, so a deploy is copying it to `/usr/local/bin` and restarting `riot-proxy.service`; the host has no Docker, no Redis and no Postgres to orchestrate (`docs/design/07-deployment.md:L72-L161`, `README.md` "systemd"). The Docker image exists for the dormant dev VM and for other users. See [../deployment/overview.md](../deployment/overview.md).

**Why is v1 parity mentioned everywhere?** v2 is a Rust rewrite of a Node service and kept v1's paths, headers, error codes and metric names so consumers and dashboards did not change (`README.md`). The owner retired v1 on 2026-10-04, so parity no longer binds; breaking changes since then are owner-approved ADRs such as ADR-065 and ADR-123, and v1 remains the reference for ported tests (`CLAUDE.md`, "Non-negotiables"). See [../api/pagination-filtering-and-versioning.md](../api/pagination-filtering-and-versioning.md#api-versioning).

**Can I run two proxies?** Not on one Riot key: they would halve each other's budget and neither would see the other's `X-App-Rate-Limit-Count`. `ROLE=api` and `ROLE=worker` exist in the config but require `DATABASE_URL=postgres://`, which this branch refuses, so only `ROLE=all` boots (`src/config.rs:L641-L647`, `docs/_meta/scratch/config.md`). A second proxy on a second Riot key is a separate deployment with its own `keyScope`.

## Behaviour you will notice

**Why does my request say `X-Cache: STALE`?** Either the entry is between its soft and hard TTL (hard is soft × 4) and a background refresh is running, or Riot or the limiter failed and the proxy served the copy it still had rather than an error (`src/fetcher.rs:L332-L338`, `L460-L466`, `L600-L606`). Both are by design. See [../data/caching.md](../data/caching.md).

**Why is `?refresh=true` ignored?** On the passthrough routes it is honoured only for a consumer with the `admin` scope and silently ignored otherwise (`src/routes/passthrough.rs:L470-L476`). On `/v1/players/*` any consumer may ask, but one claim per 60 s per part and PUUID wins; the body reports `refreshed:false` and `refreshAvailableIn` (`src/http/refresh.rs:L18`, `L43-L71`). See [../api/endpoints-players-analytics.md](../api/endpoints-players-analytics.md).

**Why 503 and not 429 for `RATE_LIMITED`?** 429 `QUOTA_EXCEEDED` is about your key's own quota; 503 `RATE_LIMITED` is the shared Riot budget for a platform or region, which every consumer sees at once. The two are kept distinct on purpose and over-quota is never reported as `RATE_LIMITED` (ADR-034, `docs/DECISIONS.md:L226`, `src/http/quota.rs:L305-L306`). See [../api/errors.md](../api/errors.md#distinctions-that-matter).

**Why is `EUW1` rejected?** Region and platform path values are ajv enums and case-sensitive; `euw1` is the value (`src/http/validate.rs:L53-L78`). The error is `400 BAD_REGION` `params/platform must be equal to one of the allowed values`. See [../api/endpoints.md](../api/endpoints.md#regions-and-platforms).

**Why does an account lookup need no region?** Riot serves every account from each account-v1 cluster, so the proxy picks the cluster with rate-limit room (`asia`, then `americas`, then `europe`) and caches the answer once for all of them; add `/{region}/` only to pin one (ADR-066, `src/riot/routing.rs:L151-L153`). See [../reference/riot-api-usage.md](../reference/riot-api-usage.md#routing).

**Why does the consumer get `502 UPSTREAM_ERROR` when the Riot key expired?** Riot's 401/403 is reported as `Upstream authentication failed` without naming the key; the operator sees `RIOT KEY REJECTED` in the log with the reminder that development keys expire every 24 hours (`src/riot/client.rs:L305`). Mint a new key on the developer portal and restart with it. See [../operations/troubleshooting.md](../operations/troubleshooting.md).

**Why does the first match page say a backfill was queued?** The first page (`start=0`) of a player the proxy has not walked yet queues `backfill:player` so the archive catches up with their history; `backfill{jobId,status,limit}` reports it and `limit` is `LOOKUP_BACKFILL_LIMIT` (the whole history by default) (`src/routes/players.rs:L547-L599`, `src/config.rs:L739`). See [../architecture/data-flow.md](../architecture/data-flow.md).

**Why do analytics rows say `"tier": null`?** Without a `tier` filter each champion is one row summed over every tier, with pick and ban rates over distinct matches; name a `tier` or `minTier` for per-tier rows (ADR-123). See [../api/pagination-filtering-and-versioning.md](../api/pagination-filtering-and-versioning.md#analytics-filters).

**Why does the WebSocket connect and then close with 4401?** The upgrade always completes; a missing or unknown key is answered with an `UNAUTHORIZED` error frame and close `4401 "unauthorized"` afterwards (`src/routes/ws.rs:L542-L555`). The same code with reason `"key revoked"` means the consumer was revoked while connected. See [../api/endpoints.md](../api/endpoints.md#v1ws-the-realtime-socket).

## Operating it

**How do I get an admin key?** On the first `serve` with an empty `consumers` table the proxy prints `bootstrap admin key (shown once): rpx_…` (or imports `BOOTSTRAP_ADMIN_KEY`); afterwards mint one with `riot-proxy key create --name ops --scopes read,admin` on the host, or `POST /v1/admin/consumers` with an existing admin key (`src/consumers.rs:L308-L333`, `README.md` "First run"). Admin calls must also come from an address on `ADMIN_IP_ALLOWLIST` when that list is set. See [../integration/authentication.md](../integration/authentication.md).

**Why can't I reach `/dashboard` on `api.ninjagoldfinch.lol`?** A Cloudflare WAF rule blocks `/metrics`, `/dashboard*`, `/dev*` and `/v1/admin*` on that host; the dashboard is served on `admin.ninjagoldfinch.lol` behind Cloudflare Access, and on the loopback (`docs/design/12-object-storage-and-hosting.md:L904-L925`). See [../deployment/security.md](../deployment/security.md).

**Does `/readyz` flip to 503 during a rebuild?** Not since WR-02 (ADR-180). While the writer runs a long write, such as an `aggregate:analytics` step, `/readyz` does not queue its probe behind it; it answers 200 with `"writerBusy":true` as long as the write makes progress, judged by SQLite's progress stamps and the writer thread's CPU clock (`src/routes/health.rs:L34-L46`, `L148-L200`). It answers 503 when the running write waits on another process's lock or is stuck for 5 s, or when the idle writer cannot get the write lock within 1.5 s; `/healthz` stays 200. See [../operations/troubleshooting.md](../operations/troubleshooting.md).

**Where are the timelines?** In the `timelines` table next to `matches`, fetched by `archive:match` when `ARCHIVE_TIMELINES=true` (the default) and by any API read of `/v1/lol/matches/{region}/{matchId}/timeline`; once packed and past the 14-day local window the bytes live in the bucket under `packs/timelines/` and are read back by byte range (`src/riot/endpoints.rs:L195-L204`, `docs/_meta/scratch/persistence.md`). `TIMELINE_BACKFILL_PATCHES` (default 0) controls the hourly backfill of missing ones. See [../data/models.md](../data/models.md).

**How big does the database get?** design/12 recorded 16 GB on disk before offloading, about 40 match packs and 210 timeline packs, growing by a handful of packs a day (`docs/design/12-object-storage-and-hosting.md:L155-L157`). With `ARCHIVE_OBJECT_URL` set, bodies older than `ARCHIVE_BODY_LOCAL_DAYS` (14) are freed locally, so the file holds the index, facts, analytics and the local window; the host has 115 GB usable and keeps 14 daily backups. See [../deployment/capacity.md](../deployment/capacity.md).

**What does changing the Riot key do (the key scope)?** `keyScope` is the first 8 hex characters of `sha256(RIOT_API_KEY)` and prefixes every cache key; tracked players, crawls and admin listings are scoped to it (`src/cache/keys.rs:L31-L37`, `src/routes/admin.rs:L386-L389`, `L1848`). A new key therefore starts with a cold cache and an empty tracked-player list, while the archive (matches, timelines, analytics) is shared. `/readyz` and `/v1/admin/info` show the current scope.

**Why is `/dev` missing in production?** `DEV_UI` is forced off when `ENV=production`, with a warning if it was set (`src/config.rs:L664-L666`, `L762-L763`). `/docs` and `/dashboard` keep their own toggles.

**How do I know which version is running?** `GET /openapi.json` and read `info.version` (no key), `GET /v1/admin/info` for version, git SHA and build time (admin key), or `riot-proxy --version` on the host. See [../api/pagination-filtering-and-versioning.md](../api/pagination-filtering-and-versioning.md#detecting-the-server-version).

## Developing against it

**How do I point the site at a local proxy?** Start `riot-proxy serve` locally, mint a read key, and set `PROXY_URL=http://127.0.0.1:8080` and `PROXY_KEY=rpx_…` in the site's `.env` (`PROXY_URL` has no default; `docs/_meta/scratch/siblings.md` §1.2). The site only ever calls the proxy server-side. See [../integration/service-communication.md](../integration/service-communication.md) and [../workflows/development-setup.md](../workflows/development-setup.md).

**How do I call Riot directly while developing?** `riot-proxy riot get <endpoint> <platform|region> [params]` prints Riot's raw body, and `GET /v1/admin/debug/riot?scope=&path=` does the same through the running server with its limiter (`src/cli/riot.rs`, `src/routes/admin.rs:L1184`). See [../reference/riot-api-usage.md](../reference/riot-api-usage.md#raw-calls-for-development).

**How do I add a route or a Riot call?** Every route goes through `api_router`, which also generates the OpenAPI document and feeds `/dev/showcase`; a Riot call is one entry in `ENDPOINTS` with its TTLs. See [../workflows/adding-an-endpoint.md](../workflows/adding-an-endpoint.md) and [../workflows/adding-a-riot-call.md](../workflows/adding-a-riot-call.md).

---
**Sources:** `docs/DECISIONS.md` ADR-001, ADR-002, ADR-065, ADR-066, ADR-123, `docs/DECISIONS.md:L226`, `docs/design/03-architecture.md:L32-L36`, `docs/design/07-deployment.md:L72-L161`, `docs/design/12-object-storage-and-hosting.md:L83-L94,L151-L157,L904-L925`, `src/config.rs:L641-L647,L664-L666,L739,L762-L763`, `src/fetcher.rs:L332-L338,L460-L466,L600-L606`, `src/routes/passthrough.rs:L470-L476`, `src/http/refresh.rs:L18,L43-L71`, `src/http/quota.rs:L305-L306`, `src/http/validate.rs:L53-L78`, `src/riot/routing.rs:L151-L153`, `src/riot/client.rs:L305`, `src/riot/endpoints.rs:L195-L204`, `src/routes/players.rs:L547-L599`, `src/routes/ws.rs:L542-L555`, `src/routes/health.rs:L21-L46,L148-L200`, `src/consumers.rs:L308-L333`, `src/cache/keys.rs:L31-L37`, `src/routes/admin.rs:L386-L389,L1184,L1848`, `src/cli/riot.rs`, `README.md`, `CLAUDE.md`, `docs/_meta/scratch/{config,persistence,siblings,WRITING-BRIEF}.md`
**Last verified:** 2026-10-20 against commit `b7b363c`

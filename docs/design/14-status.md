# 14 — Status endpoint and outbound events

| | |
|---|---|
| **Status** | Accepted (owner request 2026-11-05, series STS, ADR-203) |
| **Consumers** | the ninjagoldfinch status bot (Discord, polls `/status`), the planned self-hosted dev pipeline, ninja-monitor |
| **Contract** | the shared ninjagoldfinch status contract, `schema: 1` (every ninjagoldfinch service answers it) |

riot-proxy already says a lot about itself: `/healthz`, `/readyz`, `/metrics` (Prometheus), `/v1/admin/info`, `/v1/admin/stats` and `/v1/admin/limits/{scope}`. None of it is in the shape the other ninjagoldfinch services answer, and none of it says whether Riot is reachable or the key still works without someone reading logs. This series adds one cheap JSON answer to "is it healthy, and why not", in the shared shape, plus plain JSON events on state changes. It adds no Discord, webhook-formatting or notification logic: consumers decide what to do with the answer.

## The shared contract

```json
{
  "schema": 1,
  "service": "riot-proxy",
  "host": "ngf-prod-syd1",
  "version": "2.11.0",
  "commit": "a1b2c3d",
  "built_at": "2026-11-04T09:12:00Z",
  "started_at": "2026-11-04T10:00:00Z",
  "uptime_s": 86400,
  "status": "ok",
  "checks": [
    { "name": "riot_api", "status": "ok", "latency_ms": 85, "checked_at": "2026-11-05T01:00:00Z", "detail": "" }
  ],
  "metrics": { "requests_total": 1234 },
  "metrics_meta": { "riot_last_success_at": "2026-11-05T01:00:01Z" }
}
```

- Top-level keys are fixed. `metrics` is a flat map of snake_case keys to numbers; counters are monotonic since `started_at`, gauges are current values, units are suffixed (`_total`, `_bytes`, `_ms`, `_s`, `_pct`). Strings go in `metrics_meta`.
- `status` is derived, never set: any check `down` → `down`; else any `degraded` → `degraded`; else `ok`.
- The answer is **200 even when `status` is `down`**. A non-200 means the status endpoint itself is broken.
- It answers in under 50 ms from in-memory state: cached check results, the Prometheus recorder, the rate-limit table. A status request never calls Riot, the bucket or the event URL.

## Endpoints (STS-02, STS-03)

| Route | Who | Body |
|---|---|---|
| `GET /status` | the admin key, or a keyless *local* request | the contract above |
| `GET /status/ratelimits` | the admin key only | the full rate-limit table (below) |

Both are at the root, beside `/healthz`, not under `/v1`: they are operational, like `/metrics`, and the path is the contract's. Neither is rate-quota'd per consumer nor written to the recent-requests ring (ADM-05), so a 30 s poll leaves no trace in either.

**A local request.** The admin key is `Authorization: Bearer rpx_…`, checked by the auth layer like any admin route (scope `admin`, `ADMIN_IP_ALLOWLIST`). A request without a key is let through to `/status` only when both hold:

1. the TCP peer address (never `X-Forwarded-For`, whatever `TRUST_PROXY` says) is inside `STATUS_ALLOW_CIDR` (default loopback only, `127.0.0.1/32,::1/128`), and
2. the request has none of the headers a reverse proxy adds: `Cf-Connecting-IP`, `X-Forwarded-For`, `Forwarded` or `X-Real-IP`. Only their presence counts, never their values, and `TRUST_PROXY` plays no part.

The second rule exists because production's `cloudflared` runs on the same host and connects to `127.0.0.1:8080`: every public request through the tunnel arrives from loopback (ADR-144 §Finding). cloudflared adds `Cf-Connecting-IP` to every request it forwards, and a client cannot remove a header the tunnel adds, so a loopback request without it came from the box itself. nginx, Caddy or Traefik on the same host forward from loopback too, without `Cf-Connecting-IP` but typically with one of the other three, so those count the same (ADR-205). A proxy configured to add none of them (nginx without `proxy_set_header X-Forwarded-For`, HAProxy without `option forwardfor`) still reads as local, so a host behind one sets `STATUS_ALLOW_CIDR=none` or has the proxy add one. As a second gate, the ADM-01 WAF rule on `api.ninjagoldfinch.lol` gains `/status*` (owner runbook). A keyless request that fails either rule gets the auth layer's usual `401 UNAUTHORIZED`. `/status/ratelimits` never accepts a keyless request.

## Build identity (STS-01)

| Key | Release build | Any other build |
|---|---|---|
| `version` | the tag without `v`, set only on a tag push (equal to `Cargo.toml`'s version: `release.yml`'s `version` job already refuses a tag that is not) | `dev` |
| `commit` | the first 7 characters of `GIT_SHA` | the same, from `git rev-parse` |
| `built_at` | `SOURCE_DATE_EPOCH` when set, else the build time, as RFC 3339 UTC | the build time |

`build.rs` already sets `GIT_SHA` and `BUILD_UNIX_MS` (ADM-03). It gains `RELEASE_VERSION`: set by `release.yml` on the build steps, empty otherwise, and read at compile time; never read from git at run time. `riot-proxy --version`, `/readyz`'s `version` and `/v1/admin/info` are unchanged (they print `Cargo.toml`'s version, and DEP-01's deploy wait depends on it). `serve` logs one startup line with `version`, `commit` and `built_at`.

## Metrics (STS-02)

The numbers come from the Prometheus recorder that is already there (`telemetry::metrics_handle()`, `telemetry::counter_values`), not a second registry, so `/status` and `/metrics` cannot disagree. One counter is new:

- `consumer_requests_total{consumer, group, class}`: every request that reached a handler, by consumer **name** (never the key or its hash; `anonymous` without one), route group and status class (`2xx`, `3xx`, `4xx`, `5xx`, and `1xx` for a WebSocket upgrade's 101). Groups: `riot` (`/v1/riot`), `lol` (`/v1/lol` but analytics), `analytics` (`/v1/lol/analytics`), `players` (`/v1/players`), `admin` (`/v1/admin`), `ws` (`/v1/ws`), `ops` (`/healthz`, `/readyz`, `/metrics`, `/status*`, `/openapi.json`, `/openapi.yaml`, `/docs`), `ui` (`/dashboard`, `/dev`, static: Data Dragon's `/v1/static` and `/ddragon`), `other`. As built (STS-02, ADR-205), "reached a handler" is every request the router answered, so an auth refusal counts: as `anonymous` for a 401 or 403 (a refusal carries no consumer), and under the consumer for its quota's 429.

| `metrics` key | Kind | From |
|---|---|---|
| `requests_total`, `requests_errors_total` | counter | `consumer_requests_total`, all; errors are `5xx` from us |
| `requests_total_<consumer>`, `requests_errors_total_<consumer>` | counter | the same, by consumer |
| `requests_total_<group>`, `requests_errors_total_<group>` | counter | the same, by route group |
| `riot_requests_total`, `riot_requests_2xx_total`, `riot_requests_4xx_total`, `riot_requests_5xx_total`, `riot_errors_total` | counter | `proxy_upstream_requests_total` by its `status` label; errors are every non-2xx, network failures included |
| `riot_429_total`, `riot_403_total`, `riot_401_total` | counter | the same, those statuses |
| `cache_hits_total`, `cache_misses_total` | counter | `proxy_cache_reads_total` by `state` |
| `cache_entries`, `cache_bytes` | gauge | L1's entry count and weighted size (moka keeps both; reading them is free) |
| `fetch_queue_depth` | gauge | `jobs_pending` for the kinds that fetch matches and timelines |
| `matches_fetched_total`, `timelines_fetched_total` | counter | `proxy_upstream_requests_total` 2xx for the match and timeline endpoints |
| `riot_app_limit_remaining_pct_<scope>` | gauge | STS-03 |
| `riot_retry_after_s` | gauge | STS-03 |

`metrics_meta.riot_last_success_at` is the time of the last 2xx from Riot, RFC 3339, or `null` before the first. `<consumer>` and `<scope>` are lowercased with every character outside `[a-z0-9]` turned into `_`: `ninjagoldfinch-lol-prod` → `requests_total_ninjagoldfinch_lol_prod`. A consumer whose name flattens to a group's (`admin`, `ops`, …) is keyed `requests_total_consumer_<name>`, so it never adds to the group's count (ADR-205). The fixed keys, the nine groups' included, are present from boot at 0. The exact recorder label values behind each row are listed in `docs/design/metrics.md` when STS-02 lands.

## Rate-limit table (STS-03)

The limiter already parses Riot's headers to pace itself (`riot::limiter::headers`, `Limiter::observe`), but its windows are its own sliding logs. The table keeps what Riot last *said*, for people: on every upstream response, per scope (a platform like `oc1` or a region like `sea`) the latest `X-App-Rate-Limit` and `X-App-Rate-Limit-Count`, per `(scope, method)` the latest `X-Method-Rate-Limit` and `X-Method-Rate-Limit-Count`, and the last `Retry-After` with its time. It is a `std::sync::Mutex` map written in the client's response path, never held across an await.

- `riot_app_limit_remaining_pct_<scope>`: the smallest `(limit − count) / limit × 100` across the scope's app windows in the latest headers.
- `riot_retry_after_s`: the longest time any scope is still frozen after a 429 (`Limiter::frozen_for`), rounded up; 0 when nothing is throttled.

`GET /status/ratelimits`:

```json
{
  "schema": 1,
  "at": "2026-11-05T01:00:00Z",
  "retry_after_s": 0,
  "scopes": {
    "oc1": {
      "observed_at": "2026-11-05T00:59:58Z",
      "app": [ { "window_s": 1, "limit": 20, "count": 3, "remaining_pct": 85 },
               { "window_s": 120, "limit": 100, "count": 41, "remaining_pct": 59 } ],
      "remaining_pct": 59,
      "retry_after": { "s": 0, "at": null },
      "methods": {
        "summoner.byPuuid": { "observed_at": "…", "windows": [ { "window_s": 60, "limit": 2000, "count": 7, "remaining_pct": 99 } ] }
      }
    }
  }
}
```

**As built (STS-03, ADR-207).** The table is `UpstreamWatch::rate_limits()` beside the key verdict, recorded in `RiotClient::send_as` for every answer that has a status, the probe's included; a network failure records nothing. It reads the headers with the limiter's parser, quietly (`RateLimitHeaders::from_headers_quiet`), since the fetcher or the probe has already read and warned on them. Limit and count windows pair by length and an unpaired window is dropped; an answer without a pair (a service 429, a bare 5xx) leaves the last one, and each pair updates on its own. `remaining_pct` is an integer, rounded down and 0 when the count is at or over the limit. A scope's `observed_at` is when its app headers were last seen; a scope seen only through a method pair or a `Retry-After` has `app: []` and null `observed_at` and `remaining_pct`. A scope's `retry_after` is the last header Riot sent on it (`{ "s": 0, "at": null }` before any); the top-level `retry_after_s` is the live longest freeze (`Limiter::longest_freeze`, the most of every scope's `frozen_for`), the same number as `/status`'s `riot_retry_after_s`. `riot_app_limit_remaining_pct_<scope>` is absent until a scope's first app headers; `riot_retry_after_s` is present from boot at 0. `/status/ratelimits` is gated by `http::auth::require_status_admin`: the admin key and `ADMIN_IP_ALLOWLIST`, no consumer quota.

## Checks (STS-02b)

A background task runs every `STATUS_CHECK_INTERVAL_S` (default 30) and stores each result; `/status` reads the stored results. A check that has not run yet reads `ok` with detail `pending` for the first interval after boot.

| Check | `down` | `degraded` | How |
|---|---|---|---|
| `riot_api` | the first platform's probe failed on the network or with a 5xx | a probe got a 429, `riot_retry_after_s > 0`, another probed platform is down, or a platform's probe was skipped 4 times in a row | `lol-status-v4` `platform-data` (`status.platformData`), see cadence below |
| `riot_key` | the last upstream response that was a 2xx, 401 or 403 was a 401 or 403 | `RIOT_API_KEY_EXPIRES_AT` is set and less than 48 h away | from every upstream response, not a probe of its own |
| `cache` | L2 (SQLite) fails a read, or does not answer one within 2 s two rounds in a row | L2 does not answer a read within 2 s once | L1 is in-process and cannot be unreachable; L2 is the backend |
| `storage` | | the bucket does not answer a list of one key within 5 s | only when `ARCHIVE_OBJECT_URL` is set; absent from `checks` otherwise |

**The `riot_api` probe** (owner, 2026-11-05). It calls Riot directly through the client and the limiter at bulk priority, skipping the response cache (`status.platformData` was cached for 60 s, and since SITE-13 the status poller keeps it warm, so a cached probe would rarely reach Riot), and never writes the cache. The poller (§Status poller) is a separate task and leaves the probe unchanged. Its response does update the rate-limit table and `riot_key`, like any other.

- The **first platform** of `STATUS_CHECK_PLATFORMS` (default: the `LADDER_PLATFORMS` entries) stands in for the `DEFAULT_PLATFORM` this section first named, which ADR-065 removed from v2 (owner, ADR-206). It is probed every 60 s while the proxy is *active* (it had an upstream response on that platform's scope in the last 60 s that was not a probe, so real traffic already shows Riot answers), and every 30 s while idle.
- Every other platform in `STATUS_CHECK_PLATFORMS` is probed every 60 s. A failure there degrades `riot_api` and is named in `detail`; it does not make it `down`, since one Riot shard being out is not the proxy being out. When the key's limits allow, the owner can list every platform.
- If the limiter has no bulk room within 1 s, the probe is skipped and the last result stands, with `detail` saying so: a full limiter is not Riot being down. After 4 skipped attempts in a row on a platform the result is too old to stand, and `riot_api` is `degraded` (owner, ADR-206).
- A 401 or 403 from the probe leaves `riot_api` `ok` (Riot answered) and shows on `riot_key`.

With the first platform and N others this is at most 2 + N calls a minute, outside the response cache, at bulk priority.

**As built (STS-02b, ADR-206).**

- *Rounds.* The task runs a round at boot and every `STATUS_CHECK_INTERVAL_S` after. "Every 30 s while idle" is every round. "Every 60 s" is the first round at least 60 s after that platform's last attempt, so with an interval that does not divide 60 it rounds up to whole intervals. The probes, `cache` and `storage` run concurrently within a round, and each is stored as soon as it is known: a probe waiting out the client's 25 s timeout holds back neither `cache` nor `storage`. A probe is one call with no retries, under that timeout. Each platform's probe request is built once, at boot.
- *No platforms.* With `STATUS_CHECK_PLATFORMS=none`, or unset with no `LADDER_PLATFORMS`, `riot_api` is absent from `checks`, as `storage` is without a bucket. Nothing in `checks` then reports a limiter freeze (`riot_retry_after_s > 0`), which this design puts on `riot_api`: by design, it shows only in STS-03's `riot_retry_after_s` metric.
- *Skipped probes.* `detail` names each skip. A platform skipped 4 attempts in a row (`STALE_AFTER_SKIPS`) makes `riot_api` `degraded`, first platform or not, until a probe there runs.
- *What a probe's answer means.* A 2xx is `ok`. A 401, 403, 404 or other 4xx except 429 is Riot answering, so `ok` with the status in `detail`. A 429 degrades. A 5xx, or no answer at all, is a failure.
- *The freeze is read live.* `riot_retry_after_s > 0` is checked when `/status` is read (`Limiter::longest_freeze`), not when the round ran, so it clears as soon as the freeze does.
- *A rejected key in the log.* The probe's 401 or 403 logs `RIOT KEY REJECTED` at error only when it turns the key's verdict to rejected, and at debug while it stays rejected; real traffic logs it every time. `RiotKeyRejected` firing on probe traffic alone is intended (ADR-206).
- *`riot_key`* reads the client's last verdict when `/status` is read. Before any 2xx, 401 or 403 it is `ok` with detail `pending` (unless the expiry already degrades it). Its `checked_at` is the time of the response it was judged on, and `latency_ms` is null.
- *`cache`* reads one row's worth of L2's `cache` table. Its 2 s include the wait for a free reader, so one timeout is `degraded` and only a second in a row is `down`; a read SQLite refuses is `down` at once. *`storage`* reads only the first entry of the bucket's listing (one page), never the whole listing.
- `latency_ms` and `checked_at` of `riot_api` are the first platform's last probe.

## Status poller (SITE-13)

The owner's request of 2026-11-10 (IMPLEMENTATION.md §Post-release — SITE (2), ADR-217, ADR-219): every platform's status is read on a timer, not when a caller asks, so a status read never waits on Riot. It feeds the response cache and a table; it is not one of `/status`'s checks and changes nothing there.

- *A task, not a job.* An in-process task (`status::poll::Poller`), started with the checks after the socket is bound and aborted with them at shutdown, reads `lol-status-v4` `platform-data` (`status.platformData`) on each of `STATUS_POLL_PLATFORMS` (default every platform, `Platform::ALL`, 17; `none` turns it off) at boot and every `STATUS_POLL_INTERVAL_S` (300 s) after. A round that runs long delays the next. Seventeen rows every five minutes would only fill the jobs table.
- *Through the fetcher.* Each read is a fetch at bulk priority, past the cache read and writing through (`bypass`, `X-Cache: BYPASS`), with `FetchOptions::fresh_only`: it never takes the cached copy in place of Riot's answer, so a failure is seen as one. L1 then holds the body, and `GET /v1/lol/status/{platform}` answers `HIT` with an `X-Cache-Fetched-Age` under one interval. `status`'s TTL is soft 300 s, hard 600 s (design/04 §TTLs). The fetch counts in `proxy_upstream_requests_total` and, as a bypass, as a `miss` in `proxy_cache_reads_total`, like any.
- *Rate limits.* The reads go through the limiter like any (whether `lol-status-v4` counts against the app limit is not in our sources, ADR-217): 17 calls every 5 min. A platform the limiter has no bulk room for within 1 s (`FetchOptions::budget`) is skipped: nothing is called or stored, and it is read the next round, as the probe is. A typed 429 from Riot freezes the scope as usual (`observe`); the fetcher's re-acquire then finds no room within the second, so that read is counted `skipped` too and writes nothing. A service 429 (no `X-Rate-Limit-Type`) still backs off and retries, then fails.
- *Each platform's last answer* is stored in `platform_status` (design/04 §Schema, one write per round through `Db::write`): a 200's body (JSON as Riot sent it) and its `fetched_at`; a failure's `failed_at` and error (the fetcher's error code and message, or that the 200 was not JSON), keeping the last body. A 200 keeps the last failure's stamp too, so a reader compares `failed_at` with `fetched_at`. A platform Riot does not serve (the portal's region list for lol-status-v4 has no `tw2`) is just a failure row. `status::poll::stored` reads every row, in `Platform::ALL` order, for SITE-14.
- *Recordings.* With `STATUS_RECORDINGS_DIR` set, a 200 whose `incidents[]` lists an entry is written there as `<platform>-incident-<id>.json`, one file per platform and incident `id` (an integer in the portal's `StatusDto`), once: a file that exists is never rewritten. The API key is replaced by `[REDACTED]`, and a body that would still hold an `RGAPI-` string is not written (CI greps for it). Unset, nothing is written. These are SITE-16's fixtures.
- *Metric.* `status_polls_total{platform,result}` counts each platform's read per round: `ok`, `failed` or `skipped` (design/metrics).

## Outbound events (STS-04)

When `EVENTS_URL` is set, state transitions are POSTed there as the contract's event:

```json
{ "schema": 1, "service": "riot-proxy", "host": "ngf-prod-syd1", "event": "riot_key_rejected", "severity": "critical", "at": "2026-11-05T01:00:00Z", "detail": { "status": 403, "method": "summoner.byPuuid", "scope": "oc1" } }
```

| Event | Severity | Fires when |
|---|---|---|
| `riot_key_rejected` | critical | an upstream response is a 401 or 403 and the key was not already rejected (at boot it is *unknown*, so a bad key at boot fires once) |
| `riot_rate_limited` | warning | `riot_retry_after_s` goes above 10 |
| `riot_rate_limit_cleared` | info | it returns to 0 after a `riot_rate_limited` |
| `riot_upstream_down` | critical | `riot_api` has been `down` for 3 consecutive intervals |
| `riot_upstream_recovered` | info | `riot_api` is no longer `down` after a `riot_upstream_down` |
| `cache_unavailable` | critical | the `cache` check goes `down` |
| `cache_recovered` | info | it is no longer `down` after a `cache_unavailable` |

- Transitions only, never per interval: each event has a pair of states and fires once per change. A key that is rejected, accepted and rejected again fires twice.
- Delivery never touches the request path: the transition pushes onto a bounded queue (64; a full queue drops the event and logs a warning) and one worker task POSTs it with a 5 s timeout, retrying after 1, 2, 4 and 8 s and dropping it after 5 attempts. Any 2xx is delivered.
- `host` is the machine's hostname (`gethostname`), the same as `/status`'s.
- Nothing here formats for Discord or any other service.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `STATUS_ALLOW_CIDR` | `127.0.0.1/32,::1/128` | peers that may read `/status` without a key (plus the forwarding-header rule); `none` for none (an empty value is unset, as for every variable); an entry that does not parse refuses the boot |
| `STATUS_CHECK_INTERVAL_S` | `30` | how often the checks run |
| `STATUS_CHECK_PLATFORMS` | the `LADDER_PLATFORMS` entries | platforms probed every 60 s besides the default |
| `STATUS_POLL_INTERVAL_S` | `300` | how often the status poller reads every platform (§Status poller); `60`–`3600` |
| `STATUS_POLL_PLATFORMS` | every platform | the platforms it reads; `none` turns it off |
| `STATUS_RECORDINGS_DIR` | unset | where it writes a body with an incident, redacted; unset writes none |
| `RIOT_API_KEY_EXPIRES_AT` | unset | RFC 3339; `riot_key` is `degraded` within 48 h of it |
| `EVENTS_URL` | unset | where events are POSTed; unset sends none |

## Tasks

STS-00 (this document, ADR-203) → STS-01 build identity → STS-02 metrics and `GET /status` → STS-02b the checks → STS-03 the rate-limit table and `GET /status/ratelimits` → STS-04 outbound events. Each is one PR off `origin/main`.

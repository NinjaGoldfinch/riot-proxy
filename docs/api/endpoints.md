# API endpoints

The human-readable reference for every route riot-proxy 2.7.0 serves. This page holds the conventions every route shares, the operational and documentation routes, the 404 fallback and the full `/v1/ws` protocol; the resource groups live in three sibling pages.

| Page | Routes |
|---|---|
| this page | `/healthz`, `/readyz`, `/metrics`, `/docs`, `/openapi.json`, `/openapi.yaml`, `/dev*`, `/dashboard`, `/v1/ws`, the 404 fallback |
| [endpoints-riot-lol.md](endpoints-riot-lol.md) | `/v1/riot/*`, `/v1/lol/*` (passthrough), `/v1/static/*`, `/ddragon/*` |
| [endpoints-players-analytics.md](endpoints-players-analytics.md) | `/v1/players/*`, `/v1/lol/analytics/*` |
| [endpoints-admin.md](endpoints-admin.md) | all 40 `/v1/admin/*` operations |

The machine-readable contract is [openapi.json](openapi.json) (OpenAPI 3.1.0, `info.version` `2.7.0`, 70 paths, 76 operations). The server builds that document from the same router it serves, so the two cannot drift (`src/routes/docs.rs:L116-L141`). Errors are in [errors.md](errors.md); paging, filters, ETags and versioning in [pagination-filtering-and-versioning.md](pagination-filtering-and-versioning.md); copy-pasteable walkthroughs in [examples.md](examples.md).

## Conventions

### Base URL and hosts

| Where | Base | Notes |
|---|---|---|
| Local development | `http://127.0.0.1:8080` | `riot-proxy serve` binds `HOST`/`PORT` (defaults `0.0.0.0:8080`) |
| Production, on the host | `http://127.0.0.1:8080` | `riot-proxy.service`, loopback only; ninjagoldfinch.lol calls it here |
| Production, public | `https://api.ninjagoldfinch.lol` | Cloudflare Tunnel to `:8080`; a WAF rule blocks `/metrics`, `/dashboard*`, `/dev*` and `/v1/admin*` on this host (`docs/design/12-object-storage-and-hosting.md:L904-L925`) |
| Production, admin | `https://admin.ninjagoldfinch.lol` | Same process, behind Cloudflare Access One-time PIN |

The proxy speaks plain HTTP only; TLS is the tunnel's or a reverse proxy's job (`README.md`, "HTTPS"; ADR-067).

### Authentication

Every `/v1/*` route except `/v1/ws` sits behind a guard; `/healthz`, `/readyz`, `/metrics`, `/ddragon/*`, `/docs` and the UI pages need no key (`src/routes/docs.rs:L118-L141`, `src/app.rs:L69-L119`).

| Auth value in the tables | Meaning |
|---|---|
| none | No key, no quota |
| read | A consumer key with the `read` scope (`require_read`, `src/http/auth.rs:L346-L353`) |
| admin | A consumer key with the `admin` scope **and** a source IP on `ADMIN_IP_ALLOWLIST` (empty list = everyone) (`src/http/auth.rs:L276-L288`) |
| socket | Any active key, checked inside the handler after the upgrade (`src/http/auth.rs:L246-L260`) |
| toggle | Served only when the named variable is on (`DEV_UI`, `DOCS_UI`, `DASHBOARD_UI`) |

Send the key as `Authorization: Bearer rpx_…` (scheme matched case-insensitively) or as the first `token=` pair of the query string; both forms work on every guarded route, not only the socket (`src/http/auth.rs:L141-L156`, `L302-L315`). Keys are 36 characters starting `rpx_`; only their SHA-256 is stored (`src/consumers.rs:L95-L107`). See [../integration/authentication.md](../integration/authentication.md).

### Consumer quota and `X-RateLimit-*`

Each consumer has `quotaPerMin` (default 600) admissions in any rolling 60 s window, counted per consumer id (`src/http/quota.rs:L320`, `L396-L409`). Every response of a guarded route after a successful auth carries `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` (seconds); 401 and 403 carry none because they are refused before metering (`src/http/auth.rs:L327-L342`). Exhaustion is `429 QUOTA_EXCEEDED` with `Retry-After` (`src/http/quota.rs:L429-L431`). This quota is independent of Riot's limits: the Riot-side limiter answers `503 RATE_LIMITED` instead (see [errors.md](errors.md)).

Captured against a scratch server with a 3/min key:

```text
HTTP/1.1 429 Too Many Requests
retry-after: 60
x-ratelimit-limit: 3
x-ratelimit-remaining: 0
x-ratelimit-reset: 60
{"error":{"code":"QUOTA_EXCEEDED","message":"Quota of 3/min exceeded","requestId":"01M58NAJ68D1A5HK99Q2PF0GCC","retryAfter":60}}
```

### Shared passthrough behaviour (`/v1/riot/*`, `/v1/lol/*`)

These routes return Riot's bytes verbatim with status 200 and `Content-Type: application/json; charset=utf-8` (`src/routes/passthrough.rs:L401`, `L435-L466`). Three cache headers describe where the body came from:

| Header | Meaning |
|---|---|
| `X-Cache` | `HIT` (fresh L1), `MISS` (fetched from Riot), `STALE` (between soft and hard TTL, or served from a copy because Riot or the limiter failed), `HIT-NEG` (a cached 404, sent on the error response), `ARCHIVE` (immutable body from the match archive), `BYPASS` (`?refresh=true` by an admin-scoped consumer) (`src/fetcher.rs:L50-L69`, `L301-L367`) |
| `X-Cache-Age` | Age of the **content** in whole seconds; a byte-identical refresh does not reset it (`src/routes/passthrough.rs:L441-L446`, `src/cache/l1.rs:L161-L176`) |
| `X-Cache-Fetched-Age` | Seconds since the proxy last read it from Riot; absent for `ARCHIVE` (`src/routes/passthrough.rs:L447-L454`) |

`?refresh=true` (or `=1`) skips cache reads but still writes, and only for consumers with the `admin` scope; for a read-only key it is ignored silently (`src/routes/passthrough.rs:L470-L476`). The `/v1/players/*` composites allow it for every consumer but meter it to one claim per 60 s per part and PUUID (see [endpoints-players-analytics.md](endpoints-players-analytics.md)).

TTLs per Riot endpoint are in [../reference/riot-api-usage.md](../reference/riot-api-usage.md). Matches and timelines are immutable: after the first fetch they are served from the archive forever with `X-Cache: ARCHIVE`, `X-Cache-Age: 0` (`src/fetcher.rs:L306-L317`).

A malformed path segment (for example a bad percent-encoding) is `400 VALIDATION` with the message `params <axum text>` (`src/routes/riot.rs:L137-L139`).

### Regions and platforms

Path values are **case-sensitive** ajv enums: `EUW1` is refused with `400 BAD_REGION` `params/platform must be equal to one of the allowed values` (`src/http/validate.rs:L53-L78`). `region` is one of `americas`, `europe`, `asia`, `sea` (`src/riot/routing.rs:L146`); `platform` is one of the 16 below (`src/riot/routing.rs:L46-L63`).

| platform | label | match-v5 region | account-v1 cluster |
|---|---|---|---|
| `na1` | North America | `americas` | `americas` |
| `br1` | Brazil | `americas` | `americas` |
| `la1` | LATAM North | `americas` | `americas` |
| `la2` | LATAM South | `americas` | `americas` |
| `euw1` | EU West | `europe` | `europe` |
| `eun1` | EU Nordic & East | `europe` | `europe` |
| `tr1` | Türkiye | `europe` | `europe` |
| `ru` | Russia | `europe` | `europe` |
| `kr` | Korea | `asia` | `asia` |
| `jp1` | Japan | `asia` | `asia` |
| `oc1` | Oceania | `sea` | `asia` |
| `ph2` | Philippines | `sea` | `asia` |
| `sg2` | Singapore | `sea` | `asia` |
| `th2` | Thailand | `sea` | `asia` |
| `tw2` | Taiwan | `sea` | `asia` |
| `vn2` | Vietnam | `sea` | `asia` |

Source: `src/riot/routing.rs:L86-L121`. `sea` has no account-v1 cluster, so account routes pinned to `sea` go to `asia` (`src/riot/routing.rs:L174-L181`).

**Account-cluster picking (ADR-066).** Riot serves every account from each of `americas`, `asia` and `europe`. The region-less `/v1/riot/accounts/*` routes, and the internal account lookups of the player profile and admin tracking, take a token from the first cluster with room now in the order `asia`, `americas`, `europe`; if none has room they wait for the soonest, inside the usual budget; a cluster that answered a 429 goes last for the rest of the request (`src/riot/routing.rs:L151-L153`, `src/fetcher.rs:L612-L650`). All clusters share one cache entry keyed on the literal host `account` (`src/cache/keys.rs:L72-L79`). The `/{region}/` variants pin a cluster and never move.

### Other headers

| Header | When |
|---|---|
| `X-Request-Id` | Every response, including the 404 fallback and panics. An inbound `X-Request-Id` of up to 128 chars in `[A-Za-z0-9-_.:]` is echoed back; otherwise a ULID is generated (`src/http/request_id.rs:L287-L339`). Quote it when reporting a problem. |
| `Retry-After` | With any envelope that has `retryAfter` (`src/http/error.rs:L190-L193`) |
| `Content-Encoding` | Negotiated from `Accept-Encoding` by tower-http's `CompressionLayer` (`src/app.rs:L106`) |
| `Allow` | On a 404 for a known path with the wrong method (observed `allow: GET,HEAD` on `POST /healthz`) |

Request bodies are capped at 1 MiB (`src/app.rs:L30-L31`, `L105`).

> **TODO (unverified):** the exact status and body sent when a request body exceeds the 1 MiB limit. It is axum's extractor rejection, not the error envelope; no test exercises it (`docs/_meta/scratch/routes.md` §7).

## Operations routes

| Method | Path | Auth | Response | Handler |
|---|---|---|---|---|
| GET | `/healthz` | none | `200 {"ok":true}`; touches nothing | `src/routes/health.rs:L70` |
| GET | `/readyz` | none | `{"ok","sqlite","limiter","keyScope","sqliteReaders","sqliteReadersFree","writerBusy","writerBusyMs","writerQueued"}`; 200 when `sqlite && limiter`, else **503 with the same body**. With the writer idle, `sqlite` is a `BEGIN IMMEDIATE; ROLLBACK;` on it within 2 s; with the writer busy, nothing is queued behind it and `sqlite` says whether the running write made progress (SQLite stamps or CPU) in the last 5 s (ADR-180). `limiter` is the restored-checkpoint flag; the `writer*` fields are informational | `src/routes/health.rs:L119` (`L21-L46`, `L148-L200`) |
| GET | `/metrics` | none | Prometheus text, `Content-Type: text/plain; version=0.0.4; charset=utf-8` | `src/telemetry.rs:L119` |

Captured:

```text
GET /readyz
HTTP/1.1 200 OK
content-type: application/json
x-request-id: 01M5B6GNQ9SCSC5X3GTAV136MG

{"ok":true,"sqlite":true,"limiter":true,"keyScope":"d63166ad","sqliteReaders":4,"sqliteReadersFree":4,"writerBusy":true,"writerBusyMs":230814,"writerQueued":9}
```

`riot-proxy healthcheck` calls `/healthz` for container health checks (`src/cli/healthcheck.rs:L113-L140`). These three paths are not recorded in the admin "recent requests" ring (`src/http/recent.rs:L116-L122`). Metric names are byte-identical to v1 (`docs/design/metrics.md`).

## Documentation routes (`DOCS_UI`, default on)

| Method | Path | Response | Handler |
|---|---|---|---|
| GET | `/docs` | Scalar HTML reference | `src/routes/docs.rs:L386` |
| GET | `/openapi.json` | the document, pretty JSON | `src/routes/docs.rs:L378-L381` |
| GET | `/openapi.yaml` | `Content-Type: application/yaml` | `src/routes/docs.rs:L382-L385` |

No key. Not listed in the document itself. `riot-proxy spec` prints the same JSON from the CLI.

## Dev and dashboard pages (toggles)

`DEV_UI` defaults on but is forced off when `ENV=production`; `DASHBOARD_UI` defaults on (`src/config.rs:L664-L666`, `L762-L765`). None of these are in the OpenAPI document.

| Method | Path | Toggle | Auth | Response | Handler |
|---|---|---|---|---|---|
| GET | `/dev` | `DEV_UI` | none | explorer HTML, `Cache-Control: no-store` | `src/routes/ui.rs:L194` |
| GET | `/dev/showcase` | `DEV_UI` | none | showcase HTML, `no-store` | `src/routes/ui.rs:L195-L198` |
| GET | `/dev/config.json` | `DEV_UI` | none | `{authDisabled,version,env,docsUi,dashboardUi,regions[],platforms[{value,label,region}]}`, `no-store` | `src/routes/ui.rs:L199-L202` |
| GET | `/dev/openapi.json` | `DEV_UI` | none | the OpenAPI JSON even when `DOCS_UI` is off, `no-store` | `src/routes/ui.rs:L203-L215` |
| GET | `/dev/jobs` | `DEV_UI` and `DASHBOARD_UI` | none | `302 Location: /dashboard#jobs` | `src/routes/ui.rs:L223-L230` |
| GET | `/dev/reset` | `DEV_UI` | admin | preview `{tables[{name,rows}],l1Entries,runningJobs,kept[]}` | `src/routes/dev.rs:L152` |
| POST | `/dev/reset` | `DEV_UI` | admin | body must be `{"confirm":"reset"}` else `400 VALIDATION` `body/confirm must be equal to 'reset'`; wipes data tables, keeps `consumers`, `limiter_state`, `refinery_schema_history` | `src/routes/dev.rs:L185` (`L36`, `L83`, `L186-L198`) |
| GET | `/dashboard` | `DASHBOARD_UI` | none for the page; every call it makes needs an admin key | HTML with a strong `ETag`, `Cache-Control: no-cache`; `If-None-Match` hit is 304 | `src/routes/ui.rs:L218-L221` (`L113-L131`) |

## The 404 fallback

Any unknown path, and any known path with the wrong method, answers `404 NOT_FOUND` with the envelope (`src/app.rs:L90-L93`, `L189-L191`). There is no 405, a Fastify-parity choice.

```text
GET /v1/nope?x=1
HTTP/1.1 404 Not Found
{"error":{"code":"NOT_FOUND","message":"No route for GET /v1/nope?x=1","requestId":"01M58N9D62F2T9KP84MCX20YTS"}}
```

Under `/ddragon` the fallback is a 404 with an **empty body** instead (`src/routes/statics.rs:L72`, `L151-L156`). A panic inside a handler is `500 INTERNAL` with the envelope (`src/app.rs:L117`, `L193-L195`).

## `/v1/ws`: the realtime socket

| Method | Path | Auth | Handler |
|---|---|---|---|
| GET (Upgrade) | `/v1/ws` | socket | `src/routes/ws.rs:L532` |

### Handshake

1. Send a standard WebSocket upgrade with `Authorization: Bearer rpx_…` or `?token=rpx_…` (`src/routes/ws.rs:L525`, `src/http/auth.rs:L246-L260`).
2. The handshake is metered like any request: over quota is an HTTP `429 QUOTA_EXCEEDED` before the upgrade; otherwise the `101` carries `X-RateLimit-*` (`src/routes/ws.rs:L556-L572`).
3. A missing or unknown key **still gets its 101**, then one error frame and close code `4401` reason `unauthorized` (`src/routes/ws.rs:L542-L555`). Captured raw:

```text
HTTP/1.1 101 Switching Protocols
x-request-id: 01M58NBCHWFKTN9TJFHFXGP2RM

{"op":"error","error":{"code":"UNAUTHORIZED","message":"Invalid key"}}   then close 4401 "unauthorized"
```

4. The first frame on a good key is `{"op":"ready","consumer":"<consumer name>"}` (`src/ws/socket.rs:L63-L68`).

Whether the socket may hold admin topics is decided once at the handshake: admin scope **and** the IP allowlist (`src/http/auth.rs:L258`). With `AUTH_DISABLED=true` every socket is `dev-local` with admin (`src/http/auth.rs:L253-L255`).

### Client frames

| Frame | Effect |
|---|---|
| `{"op":"subscribe","topics":["player:<puuid>","patch"]}` | Adds topics; answered by `subscribed` |
| `{"op":"unsubscribe","topics":[…]}` | Removes topics; answered by `subscribed` |
| `{"op":"ping"}` | Answered by `{"op":"pong","at":<unix ms>}` |

Parsing is `src/ws/protocol.rs:L98-L137`. Invalid JSON gives `{"op":"error","error":{"code":"VALIDATION","message":"Invalid JSON"}}`; an unknown or missing `op` gives `Unknown op '<op>'` (`undefined` when absent). Non-string topics and topics over 200 characters are dropped silently. Binary frames are read as UTF-8 text (`src/ws/socket.rs:L84`). Errors never close the socket (`src/routes/ws.rs:L510`).

### Topics

| Topic | Admin only | Events |
|---|---|---|
| `player:<puuid>` (puuid must validate) | no | `game.started`, `game.ended`, `rank.changed`, `match.archived` |
| `patch` | no | `patch.new` |
| `ladder` | yes | `crawl.phase`, `ladder.crawl.completed`, `analytics.updated` |
| `metrics` | yes | `metrics.snapshot` every `METRICS_INTERVAL_S` (5 s), only while someone holds the topic (`src/ws/metrics.rs:L223-L237`) |
| `firehose` | yes | every event; a socket holding it skips its other topics so nothing arrives twice (`src/ws/socket.rs:L105-L109`) |

Topic parsing is `src/ws/protocol.rs:L47-L71`. An unknown topic is an error frame `VALIDATION` `Unknown topic '<raw>'`; an admin topic on a non-admin socket is `FORBIDDEN` `Topic '<t>' requires the admin scope` (`src/ws/socket.rs:L161-L170`). Duplicates are ignored and a socket holds at most 200 topics; further ones are ignored (`src/ws/socket.rs:L172-L178`). Every subscribe or unsubscribe is answered by `{"op":"subscribed","topics":[…every topic the socket now holds…]}`, so a refused topic is visible by its absence (`src/ws/socket.rs:L202-L206`). A captured session:

```text
<< {"op":"ready","consumer":"docs-read"}
>> {"op":"subscribe","topics":["player:AbC…","patch","metrics","nope"]}
<< {"op":"error","error":{"code":"FORBIDDEN","message":"Topic 'metrics' requires the admin scope"}}
<< {"op":"error","error":{"code":"VALIDATION","message":"Unknown topic 'nope'"}}
<< {"op":"subscribed","topics":["player:AbC…","patch"]}
>> {"op":"ping"}
<< {"op":"pong","at":1792365906183}
```

### Event frames

```json
{"op":"event","event":"rank.changed","topic":"player:AbC…","at":1792365906183,"data":{"puuid":"AbC…","queue":"RANKED_SOLO_5x5","before":{"tier":"GOLD","rank":"II","lp":64},"after":{"tier":"GOLD","rank":"I","lp":12}}}
```

The frame shape is `src/events.rs:L159-L172`; data fields per event are listed in [../architecture/data-flow.md](../architecture/data-flow.md) and `docs/_meta/scratch/jobs-and-observability.md` §5. One serialisation is shared by every socket (`src/ws/hub.rs:L5-L6`). Order is kept within a topic, not across topics (`src/ws/socket.rs:L9-L11`).

### Heartbeat, backpressure and close codes

- The server sends a WebSocket ping every 30 s; two unanswered pings drop the socket on the third tick, about 90 s of silence (`src/ws/protocol.rs:L23-L25`, `src/ws/socket.rs:L121-L130`). Client pings are answered by axum automatically.
- Each topic is a broadcast channel of capacity 256; a lagging socket receives `{"op":"resync","topic":"<t>","dropped":<n>}` and carries on. Re-read state over HTTP when you see it (`src/ws/hub.rs:L20`, `src/ws/socket.rs:L110-L116`).
- Close codes: `4401 "unauthorized"` (bad key at handshake), `4401 "key revoked"` (consumer revoked while connected, `src/ws/hub.rs:L188-L198`), `1001 "server shutting down"` (graceful shutdown, `src/ws/hub.rs:L200-L205`).
- Gauge `proxy_ws_connections`; counter `events_published_total{name}`.

---
**Sources:** `src/app.rs:L30-L35,L69-L119,L189-L195`, `src/routes/docs.rs:L86-L141,L374-L387`, `src/routes/health.rs:L21-L200`, `src/routes/ui.rs:L94-L233`, `src/routes/dev.rs:L33-L250`, `src/routes/ws.rs:L497-L572`, `src/ws/protocol.rs:L23-L147`, `src/ws/socket.rs:L38-L206`, `src/ws/hub.rs:L20-L205`, `src/http/auth.rs:L141-L353`, `src/http/quota.rs:L320-L431`, `src/http/request_id.rs:L287-L339`, `src/http/validate.rs:L21-L88`, `src/routes/passthrough.rs:L401-L476`, `src/fetcher.rs:L50-L69,L301-L367,L612-L650`, `src/riot/routing.rs:L46-L181`, `docs/_meta/scratch/routes.md`, `docs/design/12-object-storage-and-hosting.md:L904-L925`, `tests/snapshots/http__unknown_route_404.snap`, `tests/snapshots/ws_hub__ws_session.snap`, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-20 against commit `b7b363c`

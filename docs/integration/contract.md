# API contract for consumers

The canonical definition of what a consumer such as ninjagoldfinch.lol can rely on at the riot-proxy boundary: base URLs, authentication, the operations the site depends on, the response and error shapes, headers, versioning, and the guarantees behind them. A consumer can be implemented from this page and [../api/openapi.json](../api/openapi.json) alone.

## Base URL and transport

| Environment | Base URL | Notes |
|---|---|---|
| Production, same host | `http://127.0.0.1:8080` | plain HTTP over loopback; what the site uses |
| Production, public | `https://api.ninjagoldfinch.lol` | through the Cloudflare Tunnel; `/metrics`, `/dashboard*`, `/dev*` and `/v1/admin*` are blocked here by a WAF rule |
| Local development | `http://127.0.0.1:8080` | `just dev` |

Responses are JSON (`application/json; charset=utf-8` on proxy-built routes, `application/json` on health and error bodies). Request bodies are limited to 1 MiB. Compression is negotiated with `Accept-Encoding`.

## Authentication

Send `Authorization: Bearer rpx_…` on every `/v1/*` call. `?token=rpx_…` is also accepted and is intended for the WebSocket handshake. A key has scopes (`read`, `admin`) and a per-minute quota. Full details in [authentication.md](./authentication.md).

| Failure | Status and code |
|---|---|
| no key, unknown or revoked key | `401 UNAUTHORIZED`, `Missing or invalid API key` |
| key lacks the scope | `403 FORBIDDEN`, `This key lacks the 'read' scope` |
| quota exhausted | `429 QUOTA_EXCEEDED`, `Quota of 600/min exceeded`, with `Retry-After` |

## The error envelope

Every error from a proxy route has this body and status; `requestId` is the value of the `X-Request-Id` header and `retryAfter` (seconds) is present only on `QUOTA_EXCEEDED` and `RATE_LIMITED`, where it is also sent as `Retry-After`.

```json
{"error":{"code":"RATE_LIMITED","message":"Upstream rate limited","requestId":"01M58M8TRF7WWZ9NVNJX0QAFBE","retryAfter":2}}
```

| Code | Status | Consumer reaction |
|---|---|---|
| `VALIDATION` | 400 | fix the request; messages follow ajv wording such as `querystring must have required property 'platform'` |
| `BAD_REGION` | 400 | the platform or region value is wrong or in the wrong case (`EUW1` is refused, `euw1` accepted) |
| `UNAUTHORIZED` | 401 | the key is missing, unknown or revoked |
| `FORBIDDEN` | 403 | wrong scope, or an admin call from a disallowed address |
| `NOT_FOUND` | 404 | Riot has nothing (`X-Cache: HIT-NEG` when answered from the negative cache), or no such route |
| `QUOTA_EXCEEDED` | 429 | this consumer's quota; wait `retryAfter` |
| `INTERNAL` | 500 | report with the request id |
| `UPSTREAM_ERROR` | 502 | Riot failed or rejected the key; retrying soon rarely helps |
| `RATE_LIMITED` | 503 | the proxy could not get a Riot token within its budget or Riot answered 429; wait `retryAfter` |

Source: `src/http/error.rs:15-67,158-193`; the complete list with causes is [../api/errors.md](../api/errors.md).

## Headers a consumer may rely on

| Header | On | Meaning |
|---|---|---|
| `X-Request-Id` | every response | echo of an inbound id (≤128 chars of `[A-Za-z0-9-_.:]`) or a generated ULID |
| `X-RateLimit-Limit`, `-Remaining`, `-Reset` | every metered response after successful auth, and the 429 | the consumer quota window; not Riot's limits |
| `Retry-After` | `QUOTA_EXCEEDED`, `RATE_LIMITED` | seconds |
| `X-Cache` | passthrough, composites, static, analytics | `HIT`, `MISS`, `STALE`, `HIT-NEG`, `ARCHIVE`, `BYPASS` |
| `X-Cache-Age` | passthrough, composites, static (always `0`), champion pool | age of the content in whole seconds; never on analytics |
| `X-Cache-Fetched-Age` | passthrough and composites when known | seconds since the body was last fetched from Riot; absent on `ARCHIVE` |
| `ETag`, `Cache-Control: private, max-age=300` | analytics | weak ETag; `If-None-Match` answers 304 |

Source: `src/http/request_id.rs:287-338`, `src/http/quota.rs:332-336`, `src/routes/passthrough.rs:127-166`, `src/routes/analytics.rs:577-614`.

## Operations the site depends on

These thirteen operations are the contract the site is built against (its client is generated from the OpenAPI document; it calls nothing else). Each links to the reference entry.

| Operation | Key parameters | Response fields the site reads |
|---|---|---|
| `GET /v1/players/by-riot-id/{gameName}/{tagLine}/profile` | `refresh` (bool) | `puuid`, `platform`, `region`, `account`, `summoner`, `league`, `mastery`, `fetchedAgeSeconds`, `refreshed`, `refreshAvailableIn`, `warnings[]` |
| `GET /v1/players/{puuid}/matches` | `platform` required; `start` 0–10000; `count` 1–20; `queue`; `champion` (not with `type`); `refresh` | `matches[]` (`MatchSummary`), `hasMore`, `matchIdsFetchedAgeSeconds`, `archive.complete`, `warnings[]` |
| `GET /v1/static/versions` | none | `current`, `versions[]` |
| `GET /v1/static/{file}` | `file` in `champion, item, summoner, runesReforged, profileicon, map` (plus plural aliases); `version` | the Data Dragon JSON untouched |
| `GET /v1/static/queues` | none | Riot's queue table |
| `GET /v1/lol/matches/{region}/{matchId}` | `region` must match the id's prefix | Riot `MatchDto` untouched |
| `GET /v1/lol/matches/{region}/{matchId}/timeline` | as above | Riot timeline untouched |
| `GET /v1/lol/analytics/champions` | `queue`, `patch` (`all` or `major.minor`), `role`, `tier` or `minTier`, `side`, `minGames`, `limit` 1–500, `remakes` | `patch`, `totalGames`, `computedAt`, `champions[]` |
| `GET /v1/lol/analytics/patches` | `queue`, `championId`, `platform`, `side`, `remakes` | `patches[]` |
| `GET /v1/lol/analytics/champions/{championId}` | as champions plus `limit` 1–50 | `stats`, `byTier`, `matchups`, `items`, `runes`, `spells`, `sectionsComputedAt` |
| `GET /v1/lol/analytics/champions/{championId}/builds` | as above, `limit` 1–10, `opponentId` | `role`, `totalGames`, `builds[]` with `winRateLow` |
| `GET /v1/lol/analytics/champions/{championId}/matchups` | `limit` 1–200, lane roles | `matchups[]` |
| `GET /v1/lol/analytics/patches?championId=` | as patches | `patches[]` |

Validation rules and examples for each are in [../api/endpoints.md](../api/endpoints.md) and [../api/pagination-filtering-and-versioning.md](../api/pagination-filtering-and-versioning.md); the schemas are in the OpenAPI document under `components.schemas`.

## Guarantees and their limits

- **Freshness.** A `HIT` is at most the endpoint's soft TTL old (summoner 1 h, league entries 5 min, match ids 2 min, spectator 30 s, account 24 h); a `STALE` may be up to four times that and is being refreshed. Matches and timelines are immutable and served from the archive forever. Analytics answers change only when a rebuild finishes (after each completed crawl or an operator's recompute).
- **Partial answers.** Composite routes return `null` parts and a `warnings[]` entry instead of failing when one upstream call fails; only when every part fails is the profile a 404.
- **Refresh.** `?refresh=true` on `/v1/players/*` wins one refresh per part per player per 60 s; the losers are told `refreshed:false` and `refreshAvailableIn`. On passthrough routes `?refresh=true` is honoured only for admin-scoped keys and silently ignored otherwise.
- **Rate limits.** A consumer's own quota is independent of Riot's. Interactive requests wait at most 2 s for a Riot token; beyond that the answer is `503 RATE_LIMITED` or a `STALE` copy. The proxy never exposes Riot's limit headers.
- **Idempotency.** Every consumer-facing route is a GET; repeating it is safe. Admin POSTs that queue work return `already-queued` rather than failing when a duplicate is pending.
- **Stability.** Routes live under `/v1`. Codes, headers and metric names are v1's and change only by an owner decision recorded as an ADR; response bodies may gain fields in a minor release (additive JSON), which is how the site's SPD and VS series tracked 2.6.0 and 2.7.0.

## Versioning

The server reports its version in the OpenAPI document's `info.version` (`/openapi.json`, public unless `DOCS_UI=false`) and in `/v1/admin/info` (admin). The site regenerates its types from `riot-proxy spec` and records the version it was verified against (2.6.0 on its `origin/main`; the 2.7.0 additions `minTier`, `opponentId` and `winRateLow` are backwards compatible). A consumer that needs a field introduced in a given version should check `info.version` at build time, not at runtime.

> **TODO (unverified):** whether the site's unseen remote commits (after `b8d0b28`) regenerated against 2.7.0; the local clone of ninjagoldfinch-lol is behind its GitHub remote.

## WebSocket

`GET /v1/ws` with `?token=` or the bearer header. First frame `{"op":"ready","consumer":"…"}`; `{"op":"subscribe","topics":["player:<puuid>","patch"]}` answers `{"op":"subscribed","topics":[…]}`; events arrive as `{"op":"event","event":"game.started","topic":"player:…","at":1760000000000,"data":{…}}`. The site does not use it. Protocol details in [../api/endpoints.md](../api/endpoints.md#v1ws-the-realtime-socket).

---
**Sources:** `src/http/error.rs:L15-L67,L158-L193`, `src/http/request_id.rs:L287-L338`, `src/http/quota.rs:L332-L336`, `src/routes/passthrough.rs:L127-L166,L470-L476`, `src/routes/analytics.rs:L577-L614`, `src/routes/players.rs:L66-L147`, `src/routes/players/refresh.rs:L18-L71`, `src/riot/endpoints.rs:L88-L285`, `docs/api/openapi.json`, `docs/_meta/scratch/siblings.md` §1.5, `docs/_meta/scratch/routes.md` §1-§4
**Last verified:** 2026-10-19 against commit `52fa67d`

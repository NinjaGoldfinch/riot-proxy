# Errors

Every failure from a `/v1/*` route, the ops routes and the 404 fallback is one JSON envelope with a stable `code`. This page lists each code, its status, its exact messages, what raises it and how a client should react.

## The envelope

```json
{"error":{"code":"RATE_LIMITED","message":"Upstream rate limited","requestId":"01M58NAJ68D1A5HK99Q2PF0GCC","retryAfter":3}}
```

- `code` and `message` are always present. `requestId` is omitted only when no request is in scope (an error raised by a job); `retryAfter` (seconds) is present only on `QUOTA_EXCEEDED` and `RATE_LIMITED` (`src/http/error.rs:L158-L172`, `L187`).
- When `retryAfter` is set the response also carries a `Retry-After: <seconds>` header (`src/http/error.rs:L190-L193`).
- `Content-Type` is `application/json` (axum's `Json`), `X-Request-Id` is on every response, and `X-RateLimit-*` are present when the request got past authentication (`src/http/auth.rs:L332-L341`).
- 5xx are logged at `error`, 4xx at `warn`, and every envelope is pushed onto the errors ring visible at `GET /v1/admin/recent?kind=errors` (`src/http/error.rs:L176-L188`).

The codes, status lines and messages are byte-identical to v1's (`CLAUDE.md`, "Conventions").

## Codes

| Code | Status | Messages | Raised by |
|---|---|---|---|
| `UNAUTHORIZED` | 401 | `Missing or invalid API key` | No bearer or `token=`; unknown or revoked key (`src/http/auth.rs:L271-L275`, `src/http/error.rs:L106-L108`). No `X-RateLimit-*` headers. |
| `FORBIDDEN` | 403 | `This key lacks the 'read' scope`, `This key lacks the 'admin' scope`; `Admin access is not permitted from this address` | Scope missing (`src/http/auth.rs:L276-L281`); admin from an address outside `ADMIN_IP_ALLOWLIST` (`L282-L288`). |
| `QUOTA_EXCEEDED` | 429 | `Quota of {limit}/min exceeded` with `retryAfter` | The **consumer's** rolling 60 s quota is spent (`src/http/quota.rs:L429-L431`). |
| `NOT_FOUND` | 404 | `No route for {METHOD} {uri}`; `Resource not found upstream`; `Resource not found upstream (negative-cached)`; `No profile data available for this PUUID`; `Riot ID '{g}#{t}' did not resolve to a PUUID`; `The queue table has not been synced yet. Run the ddragon:sync job.`; `Static data '{name}' has not been synced yet. …`; admin: `No active consumer with that id`, `No consumer with that id and key hash`, `No such player for the current key scope`, `No such job`, `No such ladder crawl for the current key scope` | Router fallback (`src/app.rs:L189-L191`); Riot answered 404 (`src/riot/client.rs:L175`, `L303`); a cached 404 (`src/fetcher.rs:L321-L327`); composites (`src/routes/players.rs:L358-L360`, `L460-L468`); static data (`src/routes/statics.rs:L213`, `L289-L292`); admin lookups (`src/routes/admin.rs:L270`, `L312`, `L594`, `L1312`, `L1889-L1891`, `L2032`, `L2223-L2225`). |
| `UPSTREAM_ERROR` | 502 | `Upstream authentication failed`; `Upstream request failed` | Riot answered 401 or 403 (the proxy's key rejected) (`src/riot/client.rs:L176-L178`, `L304-L307`); Riot 5xx, a network error or timeout, or an unexpected status after the retries (`L183`, `L319-L327`; `src/fetcher.rs:L587-L609`); a composite part with a non-JSON body (`src/routes/players.rs:L136`); an unroutable active region (`L262`). |
| `RATE_LIMITED` | 503 | `Upstream rate limited` (Riot sent 429; `retryAfter` is Riot's `Retry-After` or 1); `Upstream rate limit budget exceeded` (the proxy's own limiter could not find a token inside the wait budget; `retryAfter` is the seconds until the next token, at least 1) | `src/riot/client.rs:L179-L182`, `L308-L318`; `src/riot/limiter/mod.rs:L75-L80`; `src/fetcher.rs:L108-L115`, `L460-L466`. |
| `BAD_REGION` | 400 | `params/region must be equal to one of the allowed values`; `params/platform must …`; `querystring/platform must …`; `body/platform must …`; `Match {id} belongs to region '{derived}', not '{declared}'`; `'{scope}' is neither a platform nor a region` | Any validation failure whose message mentions `platform` or `region` (`src/http/validate.rs:L21-L31`, `L67-L78`, `L194-L198`); match-id vs region mismatch (`src/routes/lol.rs:L53-L64`); the admin debug scope (`src/routes/admin.rs:L1084-L1086`). |
| `VALIDATION` | 400 | ajv-style, for example `params/puuid must NOT have fewer than 60 characters`, `querystring/count must be <= 100`, `querystring must have required property 'platform'`, `querystring/type must not be set with champion`, `querystring/minTier must NOT be sent with tier`, `params/file must be equal to one of the allowed values`, `params/id must match format "ulid"`, `body is not valid JSON`, `body must have required property 'pattern'`, `params <axum text>`, `Job {id} is {state}`, `Crawl {id} is already {status}` | Every other input rule (`src/http/validate.rs`, `src/http/body.rs:L367-L468`, `src/routes/riot.rs:L137-L139`, `src/routes/players.rs:L649-L653`, `src/routes/analytics.rs:L528-L534`, `L564-L572`, `src/routes/admin.rs:L1313-L1320`, `L1892-L1894`). |
| `INTERNAL` | 500 | `Internal server error` | Handler panics (`src/app.rs:L193-L195`); database failures during auth or in admin stores; analytics or champion-pool read failures; the version list unreachable before the first sync (`src/routes/statics.rs:L189-L192`); single-flight work that panicked (`src/http/error.rs:L123-L125`). |

Captured examples (scratch server, fake Riot key):

```text
401 {"error":{"code":"UNAUTHORIZED","message":"Missing or invalid API key","requestId":"01M58N9D5KGNAK11BF9Z0B2SP1"}}
403 {"error":{"code":"FORBIDDEN","message":"This key lacks the 'admin' scope","requestId":"01M58N9D5TA14QEYBQTCY9EM63"}}
400 {"error":{"code":"BAD_REGION","message":"params/platform must be equal to one of the allowed values","requestId":"01M58N9D6F8GENK3NHCFMDQ4TK"}}
400 {"error":{"code":"VALIDATION","message":"querystring must have required property 'platform'","requestId":"01M58N9D6PGC58Y7A9BZ3VXMY3"}}
404 {"error":{"code":"NOT_FOUND","message":"No route for GET /v1/nope?x=1","requestId":"01M58N9D62F2T9KP84MCX20YTS"}}
502 {"error":{"code":"UPSTREAM_ERROR","message":"Upstream authentication failed","requestId":"01M58N9D7MAGZXE2A432CA0EQ7"}}
429 {"error":{"code":"QUOTA_EXCEEDED","message":"Quota of 3/min exceeded","requestId":"01M58NAJ68D1A5HK99Q2PF0GCC","retryAfter":60}}
```

The Riot-side cases are pinned by `tests/snapshots/riot_client__riot_error_bodies.snap`: Riot 401 and 503 become `502 UPSTREAM_ERROR`, Riot 404 becomes `404 NOT_FOUND`, Riot 429 (typed or untyped) becomes `503 RATE_LIMITED` with `retryAfter` 3 or 1.

## Distinctions that matter

**`QUOTA_EXCEEDED` (429) vs `RATE_LIMITED` (503).** 429 is about *you*: your consumer key spent its `quotaPerMin`; other consumers are unaffected and the proxy made no Riot call. 503 is about *Riot*: the proxy's shared budget for that platform or region is exhausted or frozen by a Riot 429, so every consumer sees it. Over-quota is never reported as `RATE_LIMITED` (`src/http/quota.rs:L305-L306`). The status pair is v1's and the quota decision keeps the two codes deliberately distinct (ADR-034, `docs/DECISIONS.md:L226`); the site maps both to a 429 page for visitors (below).

**The two `RATE_LIMITED` messages.** `Upstream rate limited` means Riot itself answered 429 and the proxy gave up after its retries; `retryAfter` is Riot's `Retry-After` or 1 s. `Upstream rate limit budget exceeded` means no Riot call was made: the limiter's sliding windows (bootstrap `20:1,100:120` on a fresh scope, then whatever `X-App-Rate-Limit` says) had no token within the caller's wait budget (`CLIENT_WAIT_BUDGET_MS`, 2 000 ms for interactive requests). Before failing, both paths serve a cached copy inside its hard TTL as `X-Cache: STALE` if one exists (`src/fetcher.rs:L415-L428`, `L460-L466`, `L600-L606`).

**`BAD_REGION` vs `VALIDATION`.** Both are 400. `BAD_REGION` is picked whenever the lower-cased message contains `platform` or `region` (`src/http/validate.rs:L21-L31`), so it covers path enums, `?platform=`, body `platform` and the match-id mismatch. Note the corner case: `GET /v1/admin/limits/EUW1` is `VALIDATION`, because its message says `params/scope` (`src/http/validate.rs:L80-L88`). Region and platform values are case-sensitive: `EUW1` is refused, `euw1` accepted.

**`502 UPSTREAM_ERROR` for a rejected Riot key.** When Riot answers 401 or 403 the client sees only `Upstream authentication failed`; the proxy never says that *its* key is bad. The operator sees `RIOT KEY REJECTED` at `error` level in the log, with the reminder that development keys expire every 24 h (`src/riot/client.rs:L305`). Never retried (`L143`).

**`404` with `X-Cache: HIT-NEG`.** A Riot 404 is cached for the endpoint's negative TTL (30 s by default, 300 s for account-v1) and served from memory with the message `Resource not found upstream (negative-cached)` and the header `X-Cache: HIT-NEG` (`src/fetcher.rs:L321-L327`, `src/routes/passthrough.rs:L457-L464`, `src/config.rs:L714-L715`). Ladder, rotation and status endpoints are never negatively cached (`src/riot/endpoints.rs:L135-L174`, `L235-L256`).

## How to react

| Code | Retry? | Suggested handling |
|---|---|---|
| `UNAUTHORIZED` | no | Fix the key. If it was just minted, the auth cache remembers an unknown hash for 30 s (`src/http/auth.rs:L32`). |
| `FORBIDDEN` | no | Ask for the scope or the allowlist entry. |
| `QUOTA_EXCEEDED` | after `Retry-After` | Back off per key; `X-RateLimit-Reset` tells you when the oldest admission leaves the window. |
| `NOT_FOUND` | no (route) / after the negative TTL (Riot) | Treat as "does not exist". A `HIT-NEG` 404 will not change for up to 300 s even if the entity appears. |
| `BAD_REGION`, `VALIDATION` | no | A client bug; the message names the offending parameter in ajv form. |
| `RATE_LIMITED` | after `Retry-After` | Shared condition; do not hammer. Prefer showing stale data you already hold. |
| `UPSTREAM_ERROR` | once, after a short delay | The proxy already retried 5xx twice (`src/fetcher.rs:L34-L35`, `L587-L607`); a second client retry rarely helps. For `Upstream authentication failed`, nothing a client does helps: tell the operator. |
| `INTERNAL` | once | Report the `requestId`. |

What ninjagoldfinch.lol actually does (`docs/_meta/scratch/siblings.md` §1.4): HTTP 404, `NOT_FOUND` and `VALIDATION` become a 404 page; HTTP 429, `QUOTA_EXCEEDED` and `RATE_LIMITED` become a 429 page with `retryAfter` from the body or `Retry-After`; everything else (401, 403, 500, 502, 503 other than `RATE_LIMITED`, unreachable, timeout) becomes a 503 "Temporarily unavailable". It never retries and logs `proxy <status> <method> <path> request-id=<X-Request-Id>` on every non-2xx.

## WebSocket error frames

Errors on `/v1/ws` are frames, not HTTP responses, and never close the socket (`src/routes/ws.rs:L510`):

```json
{"op":"error","error":{"code":"UNAUTHORIZED","message":"Invalid key"}}
{"op":"error","error":{"code":"VALIDATION","message":"Invalid JSON"}}
{"op":"error","error":{"code":"VALIDATION","message":"Unknown op 'dance'"}}
{"op":"error","error":{"code":"VALIDATION","message":"Unknown topic 'plyer:typo'"}}
{"op":"error","error":{"code":"FORBIDDEN","message":"Topic 'metrics' requires the admin scope"}}
```

Only the `UNAUTHORIZED` frame is followed by a close (`4401 "unauthorized"`). An over-quota handshake is an ordinary HTTP `429 QUOTA_EXCEEDED` before the upgrade (`src/routes/ws.rs:L556-L563`). Protocol details are in [endpoints.md](endpoints.md#v1ws-the-realtime-socket).

## Responses that are not the envelope

| Case | Response |
|---|---|
| `/ddragon/*` unknown file or image | `404` with an empty body; CDN failure is `502` empty (`src/routes/statics.rs:L151-L156`) |
| `/readyz` not ready | `503` with the **readiness body**, not the envelope (`src/routes/health.rs:L87-L94`) |
| `/dev/jobs` | `302` redirect |
| Request body over 1 MiB | axum's extractor rejection |

> **TODO (unverified):** the status and body axum emits for a body over the 1 MiB `DefaultBodyLimit` (`src/app.rs:L30-L31`, `L105`); expected 413 with a plain-text body, but no test exercises it.

## The request id

Every response carries `X-Request-Id` and every envelope repeats it as `requestId`. The proxy accepts an inbound `X-Request-Id` (up to 128 chars of `[A-Za-z0-9-_.:]`) and echoes it; otherwise it generates a ULID (`src/http/request_id.rs:L287-L339`). The id is on the `request{request_id}` tracing span, in the JSON log line, in `GET /v1/admin/recent` and in the errors ring, so quoting it lets the operator find the exact request. See [../operations/troubleshooting.md](../operations/troubleshooting.md).

---
**Sources:** `src/http/error.rs:L15-L67,L106-L125,L158-L193`, `src/http/auth.rs:L30-L32,L271-L288,L332-L341`, `src/http/quota.rs:L305-L306,L429-L431`, `src/http/validate.rs:L21-L31,L67-L88,L194-L208`, `src/http/body.rs:L367-L468`, `src/http/request_id.rs:L287-L339`, `src/riot/client.rs:L143,L175-L183,L303-L327`, `src/riot/limiter/mod.rs:L75-L80`, `src/riot/endpoints.rs:L135-L174,L235-L256`, `src/fetcher.rs:L34-L35,L108-L115,L321-L327,L415-L428,L460-L466,L587-L609`, `src/routes/passthrough.rs:L457-L464`, `src/routes/ws.rs:L510,L542-L563`, `src/routes/statics.rs:L151-L156,L189-L192,L213,L289-L292`, `src/routes/health.rs:L87-L94`, `src/app.rs:L30-L31,L105,L189-L195`, `src/config.rs:L714-L715`, `tests/snapshots/auth__*.snap`, `tests/snapshots/http__unknown_route_404.snap`, `tests/snapshots/riot_client__riot_error_bodies.snap`, `tests/snapshots/ws_hub__ws_session.snap`, `docs/_meta/scratch/routes.md` §3, `docs/_meta/scratch/riot-and-cache.md` §1.5, `docs/_meta/scratch/siblings.md` §1.4, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-19 against commit `52fa67d`

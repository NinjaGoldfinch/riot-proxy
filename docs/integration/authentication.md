# Authentication and consumer keys

How riot-proxy identifies its consumers: how keys are issued, stored, rotated, revoked, scoped and rate-limited; how a consumer sends one; and how the no-auth development mode differs.

## The key

A consumer key is `rpx_` followed by 24 random bytes in unpadded base64url: 36 characters in total (`src/consumers.rs:12,95-103`). The plaintext is shown once, at creation; only `sha256(key)` is stored in `consumers.key_sha256` (`src/consumers.rs:105-107,154-157`). The bootstrap admin key printed on first boot has the same form.

## Issuing

| Way | Command or call | Notes |
|---|---|---|
| First boot | `riot-proxy serve` prints `bootstrap admin key (shown once): rpx_…` on stderr when the `consumers` table is empty | scopes `read,admin`, quota 600; `BOOTSTRAP_ADMIN_KEY` imports a chosen key instead and prints nothing (`src/cli/serve.rs:57-68,399-405`, `src/consumers.rs:308-333`) |
| CLI | `riot-proxy key create --name ninjagoldfinch-lol [--scopes read,admin] [--quota 600]` | prints id, name, scopes, quota and the key once (`src/cli/key.rs:46-57`) |
| Admin API | `POST /v1/admin/consumers {"name":"…","scopes":["read"],"quotaPerMin":600}` | 200 with `{id,name,scopes,quotaPerMin,key,warning}` (`src/routes/admin.rs:180-219`) |

Names are unique; a duplicate name or a duplicate key hash is a `VALIDATION` error (`src/consumers.rs:163-172`). The CLI needs the data directory and a valid `RIOT_API_KEY` in its environment because every command loads the configuration first (`src/cli/mod.rs:82-88`).

## Sending a key

| Form | Where accepted | Source |
|---|---|---|
| `Authorization: Bearer rpx_…` | every guarded HTTP route and the WebSocket handshake | `src/http/auth.rs:141-156` |
| `?token=rpx_…` | the same (documented for the WebSocket handshake only) | `src/http/auth.rs:302-315`, `src/routes/docs.rs:86-113` |

The scheme is matched case-insensitively; an empty token is refused. Prefer the header: a query-string key can be written to logs by anything in front of the proxy.

## Scopes

| Scope | Grants |
|---|---|
| `read` | `/v1/riot/*`, `/v1/lol/*` (analytics included), `/v1/players/*`, `/v1/static/*`, the `player:<puuid>` and `patch` WebSocket topics |
| `admin` | everything under `/v1/admin/*`, `/dev/reset`, the `metrics`, `ladder` and `firehose` topics, and `?refresh=true` on passthrough routes |

Routes are wrapped at registration in `require_read` or `require_admin` (`src/routes/docs.rs:118-141`); a missing scope is `403 FORBIDDEN` with `This key lacks the '<scope>' scope` (`src/http/auth.rs:276-281`). Unguarded: `/healthz`, `/readyz`, `/metrics`, `/ddragon/*`, `/docs`, `/openapi.json`, `/dev*` (except reset) and `/dashboard` (the page is inert without a key).

## Admin address check

Admin requests must also come from an address in `ADMIN_IP_ALLOWLIST` (IPs and CIDRs; empty allows every address). With `TRUST_PROXY=false` (default) the TCP peer is used; with `true` the leftmost `X-Forwarded-For` entry is (`src/http/auth.rs:116-139`). Behind the production tunnel the peer is always `cloudflared` on loopback, so the allowlist cannot narrow anything; it is left empty and Cloudflare Access on `admin.ninjagoldfinch.lol` is the gate (`docs/design/12-object-storage-and-hosting.md` §Admin hostname behind Access). Setting `ADMIN_IP_ALLOWLIST=127.0.0.1` together with `TRUST_PROXY=true` refuses every tunnelled admin call (ADM-01 notes).

## Quotas

Each consumer has `quota_per_min` (default 600; `AUTH_DISABLED`'s synthetic consumer 100 000). The quota is a sliding 60 s window of admissions keyed by consumer id; every metered response carries `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` (seconds), and exhaustion is `429 QUOTA_EXCEEDED` with `retryAfter` and `Retry-After` (`src/http/quota.rs:320-431`). 401 and 403 responses carry no quota headers. The WebSocket handshake counts as one request. Quotas are independent of Riot's limits, which surface as `503 RATE_LIMITED` instead.

## Caching of lookups

Key lookups are cached in memory for 60 s per found consumer and 30 s per unknown key, up to 10 000 entries (`src/http/auth.rs:29-32,206-209`). Revocation invalidates the entry immediately through the admin route; a revocation done by the CLI on the box is seen by the running server within 60 s.

## Rotation and revocation

1. Mint the replacement: `riot-proxy key create --name ninjagoldfinch-lol-2` (or the admin API).
2. Put the new key in the consumer's configuration (`/etc/ninjagoldfinch-lol/env` `PROXY_KEY` for the site) and restart it.
3. Revoke the old one: `riot-proxy key revoke ninjagoldfinch-lol`, or `DELETE /v1/admin/consumers/{id}`. The row is kept with `revoked_at` so the key can never be reissued; the admin route also drops the cache entry and closes that consumer's sockets with code 4401 (`src/consumers.rs:256-283`, `src/routes/admin.rs:248-270`).
4. Check `riot-proxy key list` shows the old name as `revoked`.

`POST /v1/admin/consumers/{id}/revoke-cache` with the key hash drops a cache entry without revoking, for the case where a key was rotated in the database by other means.

## The no-auth development mode

`AUTH_DISABLED=true` makes every guarded request and socket run as the synthetic consumer `dev-local` with both scopes and a 100 000/min quota; the IP allowlist is not consulted; the server logs a warning at start (`src/http/auth.rs:43-52,201-205,267-270`). Configuration refuses it when `ENV=production` (`src/config.rs:658-662`). It is how the showcase and the site's local development run against a local proxy, and how the dormant LAN dev VM was configured. `/dev/config.json` reports `authDisabled` so the dev explorer knows whether to ask for a key.

## Where keys live

| Key | At rest | In transit |
|---|---|---|
| Riot key | `/etc/riot-proxy/env` (production), `.env` (development); never logged, never in `/v1/admin/info` | `X-Riot-Token` to Riot only |
| Consumer keys | SHA-256 in SQLite; plaintext only with the consumer (`/etc/ninjagoldfinch-lol/env`, the operator's dashboard `localStorage`) | bearer header over loopback or the Cloudflare Tunnel |
| Object-storage keys | `/etc/riot-proxy/env`, `/etc/riot-proxy/rclone.conf` (0600) | S3 signatures |

The CI `test` job greps `src/`, `tests/` and `docs/` for `RGAPI-` key shapes and fails on a hit; fixtures are recorded with the key redacted (`.github/workflows/ci.yml:52-53`, `src/cli/record.rs:114-158`).

---
**Sources:** `src/consumers.rs:L12,L95-L107,L154-L172,L256-L333`, `src/http/auth.rs:L29-L52,L116-L156,L201-L209,L267-L343`, `src/http/quota.rs:L320-L431`, `src/routes/admin.rs:L180-L219,L248-L326`, `src/routes/docs.rs:L86-L141`, `src/cli/key.rs:L46-L83`, `src/cli/serve.rs:L57-L68,L399-L405`, `src/config.rs:L658-L662`, `.github/workflows/ci.yml:L52-L53`, `docs/design/12-object-storage-and-hosting.md` §Admin hostname behind Access
**Last verified:** 2026-10-19 against commit `52fa67d`

# Security

The threat model in brief: what the proxy protects (the Riot key, consumer keys, the bucket credentials, the archive), how each is kept, what the public hostnames expose, what is validated and rate-limited, how the processes are confined, and what one compromised process on the shared host can reach.

## Riot key custody

| Control | Detail | Evidence |
|---|---|---|
| One place on disk | `/etc/riot-proxy/env`, root-only, read by systemd before it drops to `riot-proxy`; streamed into the file through `umask 077`, never displayed | `docs/design/12-object-storage-and-hosting.md:594-601` |
| Never logged | the `Secret` wrapper prints `[REDACTED]`; the outgoing `X-Riot-Token` header is marked sensitive so `HeaderMap`'s `Debug` redacts it | `src/config.rs:135-159`; `src/riot/client.rs:234-236` |
| Not in the admin API | `GET /v1/admin/info` returns every variable in effect except `RIOT_API_KEY`, "not even its length" | `src/config.rs:397-404`; `src/routes/admin.rs:2489-2516` |
| Not in fixtures or docs | CI step "no leaked Riot keys" fails on `RGAPI-[0-9a-f-]{20,}` anywhere in `tests/`, `src/`, `docs/`; `riot record` replaces the key with `RGAPI-REDACTED` and refuses to keep a file that still contains one | `.github/workflows/ci.yml:52-53`; `src/cli/record.rs:29,114-158` |
| Key scope, not key | the 8-hex `KeyScope` (first 8 hex of sha256 of the key) is what appears in `/readyz` and the archive tables | `src/cache/keys.rs:34-37`; `src/routes/health.rs:79-97` |
| Key rejection is loud | a 401/403 from Riot logs `RIOT KEY REJECTED` at error level and the alert rule `RiotKeyRejected` fires | `src/riot/client.rs:305`; `ops/prometheus-alerts.yml:6-14` |

The key today is a personal key; Riot's policy forbids pooling keys to raise the limit, so there is one (`docs/design/12-object-storage-and-hosting.md:484`). The monitoring box holds no Riot key (`docs/design/12-object-storage-and-hosting.md:79-80`).

## Consumer key custody

| Control | Detail | Evidence |
|---|---|---|
| Format | `rpx_` + base64url of 24 random bytes from `getrandom`, 36 characters | `src/consumers.rs:12,95-103` |
| At rest | only `sha256(key)` in `consumers.key_sha256`; the plaintext is returned once from `key create` or `POST /v1/admin/consumers` and never again | `src/consumers.rs:105-107,154-162`; `src/cli/key.rs:46-57` |
| Revocation | soft: `revoked_at` is set and the hash row kept so the key can never be reissued; the admin API also drops the auth-cache entry, so it takes effect at once; without that call a cached lookup lives up to 60 s | `src/consumers.rs:228-283`; `src/http/auth.rs:30,263-265`; `src/routes/admin.rs:264,321` |
| Scopes | `read` and `admin`; the admin guard needs the scope **and** the IP allowlist | `src/consumers.rs:18-45`; `src/http/auth.rs:276-288` |
| Transport | `Authorization: Bearer`, or `?token=` in the query string, accepted on every guarded route and the WebSocket handshake | `src/http/auth.rs:141-156,302-315` |
| Where the site keeps its key | `PROXY_KEY` in `/etc/ninjagoldfinch-lol/env`, a read-scope key, sent on every server-side call; the browser never sees it | `docs/_meta/scratch/siblings.md:141,255-275` |

Note on `?token=`: the proxy's recent-request ring stores paths without their query string (`src/http/recent.rs:195-198`), but the `http` tracing span records `path` and tower-http's response line is the library's default (`src/app.rs:109-116`), and Cloudflare sees the full URL at the edge. Use the header on HTTP; keep `?token=` for the socket handshake, which is what the OpenAPI document declares it for (`src/routes/docs.rs:86-113`).

> **TODO (unverified):** whether the `path` recorded on the `http` span includes the query string, and therefore whether a `?token=` key can land in journald at `info`. `src/app.rs:109-116` was read only through the routes inventory; the extractor that fills `path` was not.

## Object-storage credentials

| Credential | Scope | Where | Evidence |
|---|---|---|---|
| Onidel access key | the proxy's key for bucket `riot-proxy-archive`; full read and write on that bucket, since the offload writes packs and snapshots; also used by the `[onidel]` rclone remote | `/etc/riot-proxy/env` (`ARCHIVE_OBJECT_ACCESS_KEY` / `_SECRET_KEY`, `[REDACTED]` in `/v1/admin/info`, boot logs bucket and endpoint only) and `/etc/riot-proxy/rclone.conf` (root, 0600) | `src/config.rs:458-469`; `src/archive/objects.rs:134,168-171`; `deploy/README.md:24-25,51-62` |
| R2 replica token | Object Read & Write on R2 bucket `riot-proxy-archive` only, so it cannot touch `ngf-backups`; IP-filtered to the box's addresses (session note, 2026-10-14) | `/etc/riot-proxy/rclone.conf`, handed to the replica unit as a systemd credential | `deploy/README.md:26-28`; `deploy/riot-proxy-replica.service:15` |
| R2 backups token | scoped to `ngf-backups` | `/etc/rclone/r2.conf`, used by the offsite timer | `docs/design/12-object-storage-and-hosting.md:815-818`; `deploy/README.md:151` |
| Restore-drill token | **read-only** (Object Read) on R2 `riot-proxy-archive`, typed into a 0600 scratch env file, so the drill's own boot-time `archive:offload` cannot write into the replica | `/var/tmp/restore-drill/env`, removed after the drill | `deploy/README.md:153-160,220` |

No bucket is public. Serving raw bodies from the bucket is a stated non-goal; every read goes through the proxy, which fetches byte ranges with its own credentials, checks each body's sha256 against `body_locations` and drops anything that fails (`docs/design/12-object-storage-and-hosting.md:52-54,100-101`; `src/archive/matches.rs:265-319`). The `object_store` client trusts only the bundled `webpki` roots, and `http://` endpoints are refused in production (`src/archive/objects.rs:329-338`; `src/config.rs:936-948`).

> **TODO (unverified):** the IP filter on the R2 replica token (box IPv4 plus the IPv6 /64) is a session note, not in the repository; and whether the Onidel key is bucket-scoped or account-wide depends on how it was created in the console (`docs/design/12-object-storage-and-hosting.md:545-546` asked for a scoped key).

## Authentication modes

- **Production**: every `/v1/*` route needs a consumer key; `AUTH_DISABLED=true` fails `Config::load` with `AUTH_DISABLED cannot be enabled when ENV=production` (`src/config.rs:676-680`).
- **Development**: `AUTH_DISABLED=true` makes every request and socket run as the synthetic `dev-local` consumer (scopes read and admin, quota 100 000/min, never persisted); the allowlist is not consulted; `serve` warns at boot (`src/http/auth.rs:43-52,201-205,267-270`). The dormant LAN dev VM ran this way (`README.md:27-58`).
- **Bootstrap**: on the first `serve` with an empty `consumers` table, `bootstrap-admin` is created with `BOOTSTRAP_ADMIN_KEY` or a generated key printed once to stderr; never again once any row exists (`src/consumers.rs:308-333`; `src/cli/serve.rs:57-68`).

## The admin surface

| Layer | What it does | Evidence |
|---|---|---|
| Admin scope | `/v1/admin/*` and the admin WebSocket topics (`metrics`, `firehose`, `ladder`) need a key with `admin`; a read key gets 403 `FORBIDDEN` | `src/http/auth.rs:276-281`; `src/ws/protocol.rs:247-250` |
| `ADMIN_IP_ALLOWLIST` | IPs or CIDRs; empty allows any source; non-empty refuses others with 403 `Admin access is not permitted from this address` | `src/http/auth.rs:59-77,116-126,282-288` |
| `TRUST_PROXY` | off by default (a change from v1); when on, the leftmost `X-Forwarded-For` entry is the client address. Production sets it `true` because every request arrives from `cloudflared` at `127.0.0.1`; an allowlist of `127.0.0.1` then refuses every admin call, so the list is left empty | `src/http/auth.rs:128-139`; `docs/DECISIONS.md:800-803` (ADR-068); `docs/design/12-object-storage-and-hosting.md:868-873` |
| Cloudflare Access on `admin.` | self-hosted application, One-time PIN only, policy "Owner only", session 24 h; the proxy does not read the Access JWT | `docs/design/12-object-storage-and-hosting.md:856-862,897-903` |
| WAF rule on `api.` | blocks `/metrics`, `/dashboard*`, `/dev*`, `/v1/admin*` (and `/docs`); `/v1/ws`, `/healthz`, `/readyz` stay open | `docs/design/12-object-storage-and-hosting.md:863-866,905-916` |
| `/metrics` | unauthenticated by design, for a scraper; public only on `admin.` behind Access, blocked on `api.` | `src/telemetry.rs:116`; `docs/design/07-deployment.md:221` |
| CORS | none unless `ADMIN_CORS_ORIGINS` lists exact origins; `*` is refused; applies only to `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz` | `src/http/cors.rs:22-43`; `src/config.rs:1035-1053` |
| `/dev` | never served in production, whatever `DEV_UI` says | `src/config.rs:682-684,784-785` |

Checks from outside after ADM-01: `api./metrics` 403, `api./v1/admin/stats` 403, `api./healthz` 200, `admin./dashboard` 302 to the Access login (`docs/design/12-object-storage-and-hosting.md:852-855`).

## Input validation and limits

| Control | Detail | Evidence |
|---|---|---|
| Request bodies | `DefaultBodyLimit` 1 MiB (v1's Fastify `bodyLimit`); larger bodies get axum's rejection, not the envelope | `src/app.rs:31,105` |
| JSON bodies | ajv-style coercion and messages: `body is not valid JSON`, `body must be object`, `body must have required property '<n>'` | `src/http/body.rs:341-391` |
| Ids | 26-char ULIDs, else `VALIDATION` `params/id must match format "ulid"` | `src/http/validate.rs:253-259` |
| Enums | case-sensitive where Riot's are: `LADDER_QUEUES`, scopes `read`/`admin`; platforms are case-insensitive on purpose | `src/config.rs:1163-1170`; `src/consumers.rs:25-35`; `src/riot/routing.rs:214-228` |
| Request id | an inbound `X-Request-Id` is honoured only if ≤ 128 chars of `[A-Za-z0-9-_.:]`, else a ULID is generated; echoed on every response | `src/http/request_id.rs:287,304-315,336-338` |
| Panics | caught and answered as 500 `INTERNAL` with the request id | `src/app.rs:117,193-195` |
| WebSocket | 200 topics per socket, 200-char topic names, unknown topics refused, heartbeat drop after 2 unanswered pings | `src/ws/protocol.rs:201-232` |

## What is rate-limited

| Surface | Limit | Evidence |
|---|---|---|
| Per consumer | `quota_per_min` admissions in a rolling 60 s window, default 600, keyed by consumer id; 429 `QUOTA_EXCEEDED` with `Retry-After` and `X-RateLimit-*` | `src/http/quota.rs:320,332-336,429-431`; `src/consumers.rs:14` |
| Towards Riot | the limiter's sliding-log windows per app and method scope, bootstrapped at 20/1 s and 100/120 s; bulk work capped at `BULK_USAGE_CEILING` while someone is active; an accountable 429 freezes the scope | `docs/_meta/scratch/riot-and-cache.md:188`; `src/riot/limiter/mod.rs:417-428` |
| Unmetered and unauthenticated | `/healthz`, `/readyz`, `/metrics`, `/ddragon/*` (mirror files and the image fill route, which fetches from Data Dragon on a miss), `/docs`, `/openapi.json`, `/openapi.yaml` | `docs/_meta/scratch/routes.md:32-38,116-126,207-215` |

`/ddragon/*` is the one unauthenticated route that can cause an outbound fetch (to Data Dragon, never to Riot's API, and only for files listed in a mirrored patch's JSON) (`docs/design/07-deployment.md:65-70`). Over quota is never confused with Riot's limit: `QUOTA_EXCEEDED` and `RATE_LIMITED` are distinct codes (`src/http/quota.rs:305-306`).

## TLS and transport

The proxy serves plain HTTP only; built-in TLS was removed before 2.0.0 and `TLS=true` refuses to start (`README.md:116-118`; `src/config.rs:686-691`; `docs/DECISIONS.md:792`, ADR-067). On production TLS terminates at Cloudflare's edge; the hop from Cloudflare to the box is the tunnel's outbound connection, and inside the box every hop is loopback (`docs/design/12-object-storage-and-hosting.md:532-537,806-807`). Outbound: HTTPS to Riot and Data Dragon with `rustls` and bundled roots (`docs/design/07-deployment.md:15`), HTTPS to the S3 endpoint (`http://` refused in production).

## Dependencies

`Cargo.lock` is committed and every build uses `--locked` (`Dockerfile:11,17`); the toolchain is pinned in `rust-toolchain.toml` and the Docker builder tag (`Dockerfile:3-4`). The dependency list is in [../reference/dependencies.md](../reference/dependencies.md).

> **TODO (unverified):** there is no Dependabot or Renovate configuration in `.github/` (it holds only `PULL_REQUEST_TEMPLATE.md` and `workflows/`), and no `cargo audit` or `cargo deny` step in `ci.yml`; how dependency updates and advisories are tracked is not recorded in the repository.

## Process confinement

The design's unit for the proxy (`docs/design/07-deployment.md:80-91`): `User=riot-proxy`, `ProtectSystem=strict` with `ReadWritePaths=/var/lib/riot-proxy`, `PrivateTmp=yes`, `NoNewPrivileges=yes`. The replica unit goes further (`deploy/riot-proxy-replica.service:17-33`): `DynamicUser=yes`, `LoadCredential` for the rclone config so the sync reads no other file, `ProtectHome`, `PrivateDevices`, `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectControlGroups`, `RestrictAddressFamilies=AF_INET AF_INET6`, `RestrictNamespaces`, `LockPersonality`, `MemoryDenyWriteExecute`, `SystemCallArchitectures=native`, plus `Nice=10` and idle I/O. `ninja-monitor` runs as its own user with `MemoryMax=64M` and a read-only view of everything (`docs/_meta/scratch/siblings.md:191`). The host allows inbound SSH only, key-only, root login off (`docs/design/12-object-storage-and-hosting.md:776-778`).

> **TODO (unverified):** the live `riot-proxy.service` is not in the repository; whether it carries exactly the design's four hardening lines, or more, was not checked. The site's unit and `cloudflared`'s are likewise unrecorded.

## Blast radius on the shared host

| If this is compromised | It holds | It can reach | It cannot reach | Evidence |
|---|---|---|---|---|
| the site (`ninjagoldfinch-lol`) | one read-scope consumer key | the proxy's read routes at the key's quota (600/min by default) | the Riot key, admin routes (no admin scope), the buckets, `/var/lib/riot-proxy` (different user) | `docs/_meta/scratch/siblings.md:141,255-275`; `src/http/auth.rs:276-281` |
| the proxy (`riot-proxy`) | the Riot key and the Onidel bucket key in its environment; the SQLite file | Riot's API as this key; read, write and delete on bucket `riot-proxy-archive`; every consumer hash | `/etc/riot-proxy/env` as a file (root-only; the process has the values in memory), the R2 tokens (`rclone.conf` is root-only, loaded only into the replica unit), `ngf-backups`, the site's checkout | `docs/design/12-object-storage-and-hosting.md:594-601`; `deploy/README.md:51`; `deploy/riot-proxy-replica.service:15` |
| the replica unit | the Onidel key and the R2 replica token for the sync's lifetime | both `riot-proxy-archive` buckets, including deletes on R2 (`sync` mirrors deletions) | `ngf-backups`, the database, the Riot key | `deploy/riot-proxy-replica.service:13-17`; `deploy/README.md:26-28` |
| the offsite timer | the R2 backups token | `ngf-backups` (14-day expiry limits what a deletion costs) | the archive buckets | `docs/design/12-object-storage-and-hosting.md:815-818` |
| `ninja-monitor` | a Discord webhook URL | Discord; read access to cgroups and, if granted, `/var/lib/riot-proxy` | anything else: its own user, `MemoryMax`, read-only | `docs/_meta/scratch/siblings.md:189-191,206-208` |
| `cloudflared` | the tunnel token | the two loopback ports it fronts | the Riot key, the database | `docs/design/12-object-storage-and-hosting.md:636-640` |

The separation that makes this hold is separate system users per service with root-only secret files, which the repository's runbooks already prescribe (`docs/design/12-object-storage-and-hosting.md:585-587,613-614`; `deploy/README.md:51`). Two things to keep that way: never put the Riot key in the site's environment (the site only ever needs a read key), and never give the proxy the R2 tokens (the replica is an ops job by design, `docs/design/12-object-storage-and-hosting.md:55-56`). A compromised proxy can delete packs on Onidel; the hourly `sync` would then propagate the deletion to R2 within the hour, so the replica protects against provider loss, not against a malicious delete. The 14-day `ngf-backups` expiry bounds how long a SQLite backup survives; the packs have no second line of defence beyond the replica.

> **TODO (unverified):** whether R2 object versioning or a deletion grace is enabled on the replica bucket; `deploy/README.md:29-36` lists only expiry and multipart-abort rules.

---
**Sources:** `src/config.rs:L135-L159,L397-L404,L458-L469,L676-L691,L784-L785,L936-L948,L1035-L1053,L1163-L1170`, `src/riot/client.rs:L234-L236,L305`, `src/riot/limiter/mod.rs:L417-L428`, `src/riot/routing.rs:L214-L228`, `src/cache/keys.rs:L34-L37`, `src/consumers.rs:L12-L45,L95-L107,L154-L162,L228-L283,L308-L333`, `src/cli/key.rs:L46-L57`, `src/cli/serve.rs:L57-L68`, `src/cli/record.rs:L29,L114-L158`, `src/http/auth.rs:L30,L43-L77,L116-L156,L201-L205,L263-L288,L302-L315`, `src/http/quota.rs:L305-L306,L320,L332-L336,L429-L431`, `src/http/cors.rs:L22-L43`, `src/http/body.rs:L341-L391`, `src/http/validate.rs:L253-L259`, `src/http/request_id.rs:L287,L304-L338`, `src/http/recent.rs:L195-L198`, `src/app.rs:L31,L105-L117,L193-L195`, `src/ws/protocol.rs:L201-L250`, `src/telemetry.rs:L116`, `src/archive/objects.rs:L134,L168-L171,L329-L338`, `src/archive/matches.rs:L265-L319`, `src/routes/admin.rs:L264,L321,L2489-L2516`, `src/routes/health.rs:L79-L97`, `src/routes/docs.rs:L86-L113`, `.github/workflows/ci.yml:L52-L53`, `Dockerfile:L3-L17`, `ops/prometheus-alerts.yml:L6-L14`, `deploy/README.md:L24-L36,L51-L62,L151-L160,L220`, `deploy/riot-proxy-replica.service:L13-L33`, `docs/design/07-deployment.md:L15,L65-L70,L80-L91,L221`, `docs/design/12-object-storage-and-hosting.md:L52-L56,L79-L80,L100-L101,L484,L532-L537,L545-L546,L585-L601,L613-L614,L636-L640,L776-L778,L806-L818,L852-L873,L897-L916`, `docs/DECISIONS.md:L792,L800-L803`, `README.md:L27-L58,L116-L118`, `docs/_meta/scratch/routes.md`, `docs/_meta/scratch/riot-and-cache.md:L188`, `docs/_meta/scratch/siblings.md`
**Last verified:** 2026-10-26 against commit `c9e1b33`

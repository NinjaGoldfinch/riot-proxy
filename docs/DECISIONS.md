# Architecture Decision Records

## ADR-001 — Single process, single binary (2026-09-16)
Accepted. A Riot key's rate limit bounds throughput; multiple processes only add coordination. See docs/design/03.

## ADR-002 — SQLite (WAL) as the only default store (2026-09-16)
Accepted. Postgres behind a feature flag for > ~50 GB archives. See docs/design/02, 04.

## ADR-003 — Rust / axum / tokio (2026-09-16)
Accepted. Go was the runner-up. See docs/design/02.

## ADR-004 — rusqlite (bundled) over sqlx (2026-09-23)
Accepted. The design already routes every query through one writer thread plus a reader pool (docs/design/03 §Cross-cutting decisions, 04 §SQLite configuration). That model is synchronous by nature, so rusqlite fits it without an async driver layered on top. `bundled` compiles SQLite into the binary, which the static musl build and the `FROM scratch` image need (07 §Option A). sqlx's compile-time query checking needs a live database or checked-in query metadata at build time. For a schema this small that costs more than it catches, and table-driven tests against a tempfile database cover the same ground.

## ADR-005 — refinery for migrations; rusqlite held at 0.39 (2026-09-23)
Accepted. refinery embeds `NNNN_*.sql` files with `embed_migrations!`, applies them on boot, and records applied versions in its own history table. That matches 04 §Schema ("Migrations are embedded in the binary … and applied on boot") and keeps migrations as plain SQL files that can be diffed against the v1 schema.
Consequence: refinery 0.9.2 accepts `rusqlite >=0.23, <=0.39`, but the latest rusqlite at the time of writing is 0.40. rusqlite is therefore held at 0.39. Only one `libsqlite3-sys` may be linked, so the two crates cannot use different versions. Revisit when refinery widens its range. `rusqlite_migration` 2.6 (which supports 0.40) is the fallback if refinery stalls.

## ADR-006 — CI required checks and toolchain pin (2026-09-23)
Accepted (owner decision on the checks and on deferring docker).
- Branch protection on `main` requires the four job checks `fmt`, `clippy`, `test` and `build-musl`, with `strict` (the branch must be up to date). The plan's §1 said to require `ci`, but GitHub reports status checks per job, never per workflow, so a required `ci` check would never arrive. Job ids must stay stable; renaming one means updating branch protection too.
- `build-musl` builds the static binary and asserts it is statically linked and under 20 MB (P0 exit check). The docker build and `healthcheck` steps from §4.4 are added in P0-07, when the Dockerfile exists.
- `rust-toolchain.toml` pins `1.98.1` (current stable on 2026-09-23). CI installs the toolchain from that file with `rustup toolchain install` instead of `dtolnay/rust-toolchain@stable`, so the version is set in exactly one place.
- The repo is public (owner decision), so branch protection works on the free plan.

## ADR-007 — reqwest 0.13 TLS features; embedded roots via `tls_certs_only` (2026-09-23)
Accepted. docs/design/09 lists `reqwest` with `rustls-tls`, and 07 §The artefact says "rustls + webpki-roots so there is not even a CA bundle to mount". reqwest 0.13 renamed `rustls-tls` to `rustls` (aws-lc-rs provider plus `rustls-platform-verifier`) and removed the `rustls-tls-webpki-roots` feature. On its own, the platform verifier would read the system CA store, which is empty in a `FROM scratch` image.
Decision: enable `rustls`, and in P1-03 build the client with `ClientBuilder::tls_certs_only(<webpki roots>)`. reqwest 0.13 bypasses the platform verifier when `tls_certs_only` is set (it uses `with_root_certificates` directly), so the binary carries its own roots as the design intends. P1-03 adds the roots crate and a test that the client builds with no system store.

## ADR-008 — Config layering and interpretation choices (2026-09-23)
Accepted.
- **Layering.** figment merges three string maps (`.env` < process env < CLI flags, over built-in defaults). A typed pass in `src/config.rs` then parses and validates the result, reporting every bad variable at once like v1's `parseEnv`. figment's `Env` provider is not used: it parses `"12345678"` into a number, and a number then fails to deserialize into a `String` field (`RIOT_API_KEY`, `HOST`). Its `Serialized` string maps also refuse to coerce `"8080"` into `u16`, so figment can only do the merging, not the typing. Only variables in `config::VARS` are read, so unrelated process env vars are ignored. Empty values count as absent (v1 behaviour).
- **`NODE_ENV` fallback.** design/07 says `ENV` replaces `NODE_ENV`. If `ENV` is unset and `NODE_ENV` is set, `NODE_ENV` is used. Otherwise a v1 `.env` with `NODE_ENV=production` would boot as development, re-enabling `AUTH_DISABLED` and the dev UI. `ENV` wins when both are set.
- **`DEV_UI`** keeps v1 semantics: unset means on everywhere except production, and an explicit value wins even in production. design/07's "production … turns `DEV_UI` off" is read as the default, which is how v1 behaves. *(Superseded by ADR-071: production now always turns it off.)*
- **`DDRAGON_DIR`** defaults to `$DATA_DIR/ddragon` ("now derived", design/07). It is still honoured when set, so v1 `.env` files port unchanged.
- **`RIOT_USER_AGENT`** defaults to `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)`. design/07 elides the URL (`riot-proxy/2.0 (+…)`).
- **Numeric bounds** are v1's TypeBox bounds verbatim. The one new numeric, `JOB_CONCURRENCY`, only requires ≥ 1.
- **Deferred validation.** `DEFAULT_PLATFORM`, `LADDER_PLATFORMS`, `LADDER_QUEUES`, `LADDER_TIER_FLOOR` and `CACHE_TTL_OVERRIDES` are kept as strings here. They are validated against the Riot enums by the modules that own them (P1-01, P1-02, P7-02) so config does not re-declare Riot semantics. v1 validated them at boot, and v2 will too once those modules exist.
- `DATABASE_URL=postgres://…` and `ROLE=api|worker` are refused until the `postgres` feature exists (P8-04).

## ADR-009 — Request ids, log shape, metrics exposition (2026-09-23)
Accepted.
- **v1 had no request id.** v1 (`c86e631`) sets no `X-Request-Id` header anywhere, and its error envelope is `{error:{code,message,retryAfter?}}`. design/03 and CLAUDE.md list `X-Request-Id` among headers "carried over verbatim from v1", but that is not what v1 does. The header is therefore a v2 **addition**. Adding a response header cannot break a v1 client. Whether `requestId` also goes into the error body is P0-05's decision and needs owner sign-off, because it changes v1's envelope.
- **Id source.** A ULID per request (design/03 §Cross-cutting decisions). An inbound `X-Request-Id` is honoured if it is 1–128 chars of `[A-Za-z0-9-_.:]`, so a caller can correlate its own logs with ours. Anything else is replaced, so a caller can never inject arbitrary text into our logs or headers.
- **Log shape.** JSON lines with event fields at the top level and the innermost span's fields under `span`. `request_id` comes from the outermost `request` span. `consumer`, `x_cache` and `upstream_ms` (design/07 §Observability) are added by the tasks that know them.
- **Exposition.** `metrics-exporter-prometheus` without its HTTP listener. `/metrics` is our own axum route, and `spawn_upkeep` drains histograms every 5 s. Histograms are configured with v1's exact buckets. Without that the exporter would emit summaries, and the dashboard's `histogram_quantile(… _bucket …)` queries would break.
- **Not ported:** `proxy_node_*` (prom-client Node.js runtime metrics; no Node runtime in v2, and neither the dashboard nor the alerts use them).

## ADR-010 — SQLite layer shape (2026-09-23)
Accepted. Implements design/04 §SQLite configuration with these specifics:
- **Migration file names** are `V0001__init.sql` rather than the plan's `0001_init.sql`. refinery only recognises `^[UV]\d+__\w+\.sql$`. Migrations run grouped (one transaction) on every open; applied versions are skipped.
- **Writer closures take `&mut Connection`** (the plan wrote `&Connection`), so they can use `Connection::transaction()`. A panic inside a closure is caught on the writer thread: the open transaction rolls back as it unwinds, the caller gets `DbError::WriterGone`, and the writer keeps serving.
- **Readers are opened `SQLITE_OPEN_READ_ONLY`**, so a write on the read path fails loudly instead of racing the writer. The pool is a `Mutex<Vec<Connection>>` gated by a `Semaphore` of the same size. No `r2d2` dependency.
- **`write`/`read` are generic over the closure's error type** `E: From<DbError>`, so callers can return their own errors (e.g. `ApiError`) from inside a closure.
- `journal_mode`, `synchronous` and `wal_autocheckpoint` are set on the writer. `busy_timeout`, `foreign_keys`, `cache_size`, `mmap_size` and `temp_store` are set on every connection. The values are design/04's, verbatim.

## ADR-011 — Error envelope, health bodies, HTTP layers (2026-09-23)
Accepted.
- **Error envelope** is `{error:{code,message,requestId,retryAfter?}}` (owner decision, 2026-09-23). Codes, default statuses and messages are v1's (`src/errors.ts`). `requestId` is a v2 addition. v1's OpenAPI `ErrorResponse` does not forbid extra properties, so this stays schema-compatible. `requestId` comes from a task-local set by the request-id layer, so `ApiError` needs no access to the request. It is omitted outside a request (unit tests, background jobs). `Retry-After` is sent as a header whenever `retryAfter` is set, as in v1.
- **404s:** unknown paths *and* known paths with the wrong method return `NOT_FOUND` with v1's message `No route for <METHOD> <path?query>`. Fastify does the same; axum would otherwise return 405 with an empty body.
- **Panics** in handlers become `INTERNAL` 500 envelopes (`CatchPanicLayer`) instead of dropping the connection.
- **`/healthz`** returns `{ok:true}` (v1). **`/readyz`** returns `{ok, sqlite}` with 200/503 and the same body both ways (v1 semantics). The probe takes and rolls back SQLite's write lock through the writer thread, with a 2 s bound. v1's `redis`/`postgres` booleans are gone with those backends. `keyScope` returns in P3-01 and a `limiter` flag in P2-06 (design/07: "SQLite writable and limiter restored").
- **Layers:** request id (outermost) → catch-panic → trace (one INFO line per response carrying `request_id`, `method`, `path`, `status`, `latency`) → compression (design/03) → 1 MiB body limit (v1 Fastify `bodyLimit`).
- **CORS: not added.** The plan lists `tower-http` cors for P0-05, but v1 registers no CORS plugin, and "v1's observed behaviour wins" (plan preamble). Adding it would allow cross-origin browser callers that v1 refused. Flagged for owner review; adding it later is a one-line layer once a policy is chosen.
- **Graceful shutdown:** SIGTERM/SIGINT handlers are installed before serving. On a signal the server stops accepting and drains for up to 10 s (`SHUTDOWN_GRACE`, Docker's default stop timeout), then exits even with requests still in flight. The final limiter checkpoint and WS close join this sequence in P2-06 and P6-07.
- `main` runs tokio with `min(cores, 4)` worker threads (design/03 §Cross-cutting decisions).

## ADR-012 — CLI and consumer keys (2026-09-23)
Accepted.
- **Key format is v1's**: `rpx_` + base64url(24 CSPRNG bytes), 36 chars. `consumers.key_sha256` stores the raw 32-byte sha256 (v1 stored hex text; design/04 says `BLOB`, and consumers are not migrated from v1 anyway, per P8-02). The plaintext exists only in the `Created` value returned to the caller that minted it.
- **New dependencies:** `getrandom` (CSPRNG) and `base64` (url-safe, no padding), needed to reproduce v1's key format. Neither was in P0-01's list.
- **Scopes** are validated to `read` and `admin` (design/03). Defaults are `read` and 600/min (v1 `DEFAULT_QUOTA_PER_MIN`). Names are `UNIQUE` (design/04 DDL), so `key revoke` accepts an id or a name. Revocation sets `revoked_at` and keeps the row, so the hash can never be reissued (v1 `disableConsumer`).
- **Bootstrap:** on `serve`, if `consumers` is empty, `bootstrap-admin` (read+admin, v1's name) is created in the same write transaction as the emptiness check. With `BOOTSTRAP_ADMIN_KEY` set, that key is imported (v1 semantics) and not echoed. Otherwise a key is generated and printed **once to stderr**, not to the JSON log stream on stdout, because log pipelines often ship stdout elsewhere. design/07's "First run" sketch shows it among the logs; the banner is on the same terminal, just not inside a JSON log record. Any existing consumer, even a revoked one, suppresses it, so it is shown at most once.
- **Flags** from `ConfigArgs` are global (`riot-proxy --data-dir x key list` or `riot-proxy key list --data-dir x`). Every command except `spec` loads the full config, so `RIOT_API_KEY` is required for `key …` too. v1's `key:create` had the same requirement.
- `healthcheck` connects to `HOST:PORT`, mapping wildcard binds to loopback, with a 3 s timeout (design/07's `HEALTHCHECK --timeout=3s`).
- `spec` prints an empty valid OpenAPI 3.1 document until P4-03.
- design/03's `main.rs` comment also lists `reset`. It is not in P0-06's task list, so it is not implemented. v1's `reset-cache`/`reset-db` scripts would be the reference if it's wanted later.

## ADR-013 — Dev tooling and container (2026-09-23)
Accepted.
- **Dockerfile** is design/07 §Option A with three adjustments. The builder is `rust:1.98.1-alpine` to match `rust-toolchain.toml` (which is dockerignored, so rustup inside the image doesn't fetch components). The dependency-cache stage stubs both `src/main.rs` and `src/lib.rs` (the crate is lib + bin). Builds use `--locked`. No CA bundle is copied: per ADR-007 the Riot client (P1-03) embeds webpki roots.
- **docker-compose.yml** is design/07's plus `build: .`, so `docker compose up` works before an image is published to GHCR (P8-05).
- **CI `build-musl`** now also builds the image, runs `--version`, starts a container, polls `docker exec … /riot-proxy healthcheck`, curls `/healthz` and `/readyz`, checks that the bootstrap key banner appears once in `docker logs`, stops the container with SIGTERM and asserts exit code 0, and runs `docker compose up` → `/healthz` 200. This resolves the docker part of ADR-006.
- **justfile** recipes: `dev`, `test`, `lint`, `cov`, `musl`, `docker`. `just acceptance` (named in CLAUDE.md) arrives with P8-01.
- The first CI run of the image confirmed ADR-007's premise: in `FROM scratch`, reqwest 0.13's default rustls setup fails at *client build time* ("No CA certificates were loaded from the system"), even for plain HTTP. `healthcheck` therefore builds its client with `tls_certs_only(empty)`. P1-03 must use `tls_certs_only(<webpki roots>)` for the same reason.

## ADR-014 — Owner decisions at the P0 gate (2026-09-24)
Accepted (owner).
- **License: MIT.** Added as `LICENSE`, and `license = "MIT"` in `Cargo.toml`.
- **CORS: deferred.** It stays off, as in v1 (ADR-011), until a later decision.
- **New v2 metrics use design/07's names verbatim**, without the `proxy_` prefix: `jobs_pending`, `limiter_bulk_waiters`, `sqlite_wal_bytes`, plus P2-05's `limiter_interactive_waiters`. v1 names keep `proxy_`. See docs/design/metrics.md.
- **Confirmed:** the bootstrap key is printed to stderr, outside the JSON logs (ADR-012). `NODE_ENV` is honoured as a fallback for `ENV` (ADR-008).

## ADR-015 — Routing (2026-09-24)
Accepted.
- **Sources.** The 16 platform and 4 regional host values match developer.riotgames.com/docs/lol "Routing Values" (fetched 2026-09-24). The portal page does not state the platform→region grouping or which regional host serves account-v1 for SEA, so those come from v1 (spec §5.1, `src/riot/routing.ts`): SEA platforms use `sea` for match-v5 and borrow `asia` for account-v1.
- **Error code.** An unknown platform or region is `BAD_REGION` (400), as in v1. The plan's `ApiError::BadRequest` is read as "a 400"; v1's code wins.
- **Config.** `DEFAULT_PLATFORM` and `LADDER_PLATFORMS` are now typed `Platform` and refused at boot when unknown (v1 behaviour), closing that part of ADR-008's deferred validation.

## ADR-016 — Endpoint registry (2026-09-24)
Accepted.
- **Source of truth.** Ids, paths, host families, soft TTLs, override keys and negative-cache classes are v1's `src/riot/endpoints.ts`. Hard TTL = soft × 4 with `STALE_WHILE_REVALIDATE` on, or = soft with it off (v1 `STALE_MULTIPLIER`, design/04 "Hard TTL (×4)"). `docs/contract/v1-endpoints.txt` pins the 16 v1 method ids, and `endpoints::parity` fails on any drift.
- **L2 membership** follows design/04's explicit table: account, summoner, the four ladder reads, and mastery. Rotations are *not* persisted, even though design/04's "TTL ≥ 1 h" rule of thumb would include them; the table is the more specific statement.
- **Negative TTLs** follow v1, not design/04's shorter summary. `NEG_TTL_ACCOUNT_SECONDS` applies to account; `NEG_TTL_SECONDS` to summoner, league entries, match ids, match, timeline, spectator and mastery; no negative caching for ladder reads, rotations or status.
- **Account routing is encoded in the registry** as `HostKind::Account`: regional, but resolved through `Region::account_region()`, so a `sea` caller always lands on `asia`. v1 enforced this with an `AccountRegion` type at compile time.
- **`CACHE_TTL_OVERRIDES`** is parsed exactly as v1 did (malformed pairs are skipped). Overrides never affect immutable endpoints (match, timeline). `TtlPolicy::ineffective_overrides()` lists unknown or no-op keys so `serve` can warn once the fetcher is wired (P3-05). v1 silently ignored them.
- `method_scope_key` equals the method id (v1 §9.2 buckets per method id).

## ADR-017 — Riot HTTP client (2026-09-24)
Accepted.
- **One call, no retries.** Per the plan, `RiotClient::send` performs exactly one GET and classifies the result. v1's client also retried, and that policy **must be ported into the fetcher in P3-05** so behaviour is unchanged: 5xx and network errors get 2 retries after 250/750 ms ±20 % jitter; an untyped (service) 429 gets up to 3 tries with 500 ms × 2ⁿ backoff capped at 8 s, ±20 %, and no bucket change; a typed 429 freezes the scope for `Retry-After` and re-acquires (v1 `src/riot/client.ts`, spec §5.5/§9.4).
- **Classification** (v1 §5.5): 404 → `NotFound`; 401/403 → `UpstreamAuth`, logged at **error** ("RIOT KEY REJECTED"); 429 → `RateLimited{limit_type, retry_after}`; 5xx and network → `UpstreamUnavailable`; anything else non-2xx → `UnexpectedStatus`. Errors carry the response headers, because the limiter observes every response (v1 §9.1). Client-facing mapping is v1's `toProxyError`, including never telling the caller the key was rejected, and `retryAfter` defaulting to 1.
- **Metrics:** `proxy_upstream_requests_total{region,method,status}` (`status="network"` on transport failure) and `proxy_upstream_latency_seconds` on every call, and `proxy_rl_429_total{region,type}` once per 429, where v1 counted it.
- **Transport:** reqwest with v1's pool shape (32 idle connections per host, 60 s keep-alive), a 10 s connect timeout and a 25 s total timeout (v1: 10 s headers + 15 s body), gzip, and `Accept: application/json`. TLS trusts **only** the embedded `webpki-root-certs` store via `tls_certs_only`, so no system CA bundle is needed (ADR-007). The `X-Riot-Token` header value is marked sensitive and redacted from `Debug`.
- **Log scrubbing:** v1 also regex-scrubbed `RGAPI-…`/`rpx_…` out of every log string. v2 prevents leaks at the source instead (the `Secret` type, sensitive header values, and the key never in URLs), and a test asserts the key is absent from client and error `Debug`/`Display` output. A tracing-layer scrubber can be added if a leak path is ever found.

## ADR-018 — Rate-limit header parsing (2026-09-24)
Accepted.
- v1 `parseLimitHeader` semantics: `limit:seconds` pairs, whitespace tolerated, malformed windows skipped, `limit > 0 && seconds > 0` required. Where v1 returned `[]`, v2 returns `None` and logs the skipped windows at warn. Count headers use the same pairs but allow `count = 0`: v1 dropped zero counts, which under `max(ours, riot)` sync changes nothing. Windows are sorted by length so header order is irrelevant.
- `RateLimitType` is `application | method | service`. Unknown values are `None` and logged. `Retry-After` is read as integer seconds only; an HTTP-date is ignored (Riot sends seconds; v1 used `Number()`, which yields NaN for dates).
- **Open, to raise at P2-04/P3-05:** design/05 §Observe says a service 429 is backed off by "client.rs" at `250ms × 2^attempt ± 25 %` for up to 3 attempts. v1 (`src/riot/client.ts`) uses 500 ms × 2ⁿ capped at 8 s, ±20 %, 3 tries, and the plan says the client does not retry at all (ADR-017). Where design and v1 disagree, v1 wins, but the plan says to ask first.

## ADR-019 — Dev CLI (2026-09-24)
Accepted.
- `riot-proxy riot get <endpoint> <platform|region> <params…> [-q k=v]…` exists only with `--features dev-cli` (default off, so it is never in release or musl builds). CI compiles it via `--all-features`.
- `<endpoint>` accepts the method id (`account.byRiotId`) or its slug (`account/by-riot-id`), derived mechanically from the id (`.` → `/`, camelCase → kebab-case). The P1 exit check uses the slug form.
- It calls `RiotClient::send` directly, bypassing cache and limiter, so it's for occasional manual calls only. The raw body goes to stdout and a status line to stderr. A hidden `--base-url` points it at a mock server for tests.

## ADR-020 — `.env` parsing follows node dotenv, not `dotenvy` (2026-09-24)
Accepted. Found during the P1 exit check: `dotenvy` rejects unquoted values containing spaces, such as `RIOT_USER_AGENT=riot-proxy (+https://…)`. v1's own `.env.example` is written that way, and node's `dotenv` accepts it. design/07 promises v1 `.env` files port mechanically, so `config::parse_dotenv` replaces `dotenvy` (the dependency is removed). Rules: `#` comment lines, optional `export`, unquoted values run to end of line (a ` #` starts an inline comment) and are trimmed, and `'…'`/`"…"`/`` `…` `` quoting (`\n` expanded in double quotes only). Lines without `=`, bad names and unterminated quotes are errors that name the line, where node would silently skip them; a typo in `.env` should stop the boot. `tests/fixtures/v1.env.example` (v1's file, key redacted) must parse. Its only rejected setting is `DATABASE_URL=postgres://…`, which v2 refuses by design.

## ADR-021 — Service-429 backoff uses v1's numbers (2026-09-24)
Accepted (owner). Resolves the open item in ADR-018. An untyped (service) 429 is retried up to 3 times with 500 ms × 2ⁿ backoff, capped at 8 s, ±20 % jitter, and no bucket change: v1's `src/riot/client.ts`, not design/05's 250 ms/±25 %. Per the plan (ADR-017) this lives in the fetcher (P3-05), not the client. design/05 §Observe should be read with this override.

## ADR-022 — Negative-cache hits are `X-Cache: HIT-NEG` (2026-09-24)
Accepted (owner). v1's `CacheState` value (`src/cache/store.ts`) is kept byte-identical: a negatively cached 404 is served with `X-Cache: HIT-NEG`, not the `NEG` written in design/03 and the plan's P3 exit check. That check therefore reads "`HIT`, `MISS`, `STALE`, `HIT-NEG`, `ARCHIVE`, `BYPASS`". The `HIT-` prefix also leaves room to tell future cache kinds apart.

## ADR-023 — Limiter windows are sliding logs, as in v1 (2026-09-25)
Accepted (owner). design/05 draws fixed windows (`count`, `reset_at`, reset on expiry), but v1 replaced that counter with a sliding log (a sorted set of admission timestamps) after it let up to 2× the limit through at a window boundary. That caused the accountable 429s at v1's Phase 2 gate (#17, test "never admits more than `limit` in any rolling window"). v2 keeps v1's algorithm: each window holds the admission instants from the last `seconds` (at most `limit` of them), a take succeeds only if fewer than `limit` remain after pruning, and the wait is until the oldest stamp ages out. Riot's `-Count` headers are absorbed by padding with entries stamped at sync time until the log is at least Riot's count. Own admissions keep their timestamps, and the count is never lowered. Everything else in design/05 (all-or-nothing across app ∪ method, mutex only for check-and-take, freeze, priorities, conservative restore) stands. Checkpoints (P2-06) must store stamps, or a representation that is never less conservative.

## ADR-024 — Observe and freeze details (2026-09-25)
Accepted.
- `observe` handles every response, errors included: limit headers reconfigure (and mark the app limits as known even when they equal the bootstrap values, per a v1 regression test), then count headers sync each window of the same length up to Riot's count. Method counts are ignored until that method's limits are known.
- **Freeze trigger follows v1**: any 429 carrying both `X-Rate-Limit-Type` and a numeric `Retry-After` freezes the whole scope (every method), `service` included. Only `application`/`method` are logged at error ("accountable 429"); `service` is logged at warn. design/05 only describes the `application|method` case and the untyped case; the typed-`service` case is v1's behaviour. With no type, or no `Retry-After`, nothing changes and the fetcher backs off (ADR-021).
- A freeze never shortens an existing longer one. `freeze` does not touch metrics; the client counts 429s (v1 test "freezes the whole scope").
- The four priority tests ported in P2-01 stay ignored until P2-05, which implements what they test. The plan's P2-04 acceptance ("all of P2-01's tests un-ignored") is met for every non-priority case.

## ADR-025 — Priorities (2026-09-25)
Accepted. design/05 §Priorities with v1 §9.3's rules:
- An interactive acquire that has to wait registers as a waiter on its scope, through a guard that unregisters on drop, so a cancelled acquire can't leave a phantom waiter (the in-process form of v1's leaked Redis waiter). While any interactive waiter is registered, bulk stands aside and is woken when the last one leaves.
- Bulk also stands aside while any app or method window holds `used ≥ BULK_USAGE_CEILING × limit`. With the default 0.80, 8 of 10 blocks bulk (v1 test), and bulk resumes once enough of the oldest admissions age out.
- **Budget semantics differ from v1:** v1 kept an interactive caller queued for its whole budget even when no token could arrive in time. v2 fails at once with `RATE_LIMITED` and the exact `retry_at` (design/05 flowchart). The two ported waiter tests use waits that fit their budget. Bulk callers pass a long budget, because the scheduler (P6) bounds how many wait.
- Waking uses `tokio::sync::Notify` (enabled before each check, so no wakeup is lost) plus `sleep_until` for computed times. Gauges `limiter_interactive_waiters` and `limiter_bulk_waiters` are process-wide totals (ADR-014 naming).

## ADR-026 — Limiter checkpoint format and restore (2026-09-25)
Accepted. design/05 §Persistence, adapted to sliding logs (ADR-023):
- **Rows:** `app:{scope}` (carries `frozen_until`) and `method:{scope}:{method}` in `limiter_state`. `windows` is JSON `{known?, windows:[{limit, seconds, stamps:[[unix_ms, n], …]}]}`, not design/04's `[{limit, seconds, count, reset_at}]`: a sliding log has no single `reset_at`. `known` preserves whether the app limits came from Riot or from the bootstrap.
- **Stamps are bucketed and rounded up** to `max(100 ms, seconds ms)`: at most ~1 000 buckets per window whatever the limit (a production 30000:600 window would otherwise be 30 000 stamps every 10 s). Rounding up means a restored stamp expires no earlier than the real one, so restore is never less conservative than the live state.
- **Restore:** expired stamps are dropped. A row older than 120 s restores every window **full**, stamped at restore time, so each is closed for one window length (design/05's conservative default). A future `frozen_until` is kept. Unreadable rows are skipped with a warning; a failed load starts from bootstrap limits, and Riot's first `-Count` headers correct any drift.
- **When:** `serve` restores before binding, checkpoints every 10 s (a missed tick is delayed, not burst), and writes a final checkpoint after the HTTP drain on SIGTERM/SIGINT. Checkpoint failures are logged, not fatal. `/readyz` gains `limiter` (true once restored) and is 503 until then.

## ADR-027 — Cache key format (2026-09-25)
Accepted (owner). Keys follow **design/04's shape**, not v1's hashed target: `{key_scope}:{method}:{host}:{param}:…[:{query hash}]`.
- Path parameters are kept readable (percent-encoded like `encodeURIComponent`, so a `:` inside a value becomes `%3A` and can't collide). The P5-05 admin purge can then target one player as well as v1's method-level globs. v1 hashed `path+query`, so a purge could only target a whole method or host.
- The query keeps v1's canonicalisation (sorted, empty values dropped, `encodeURIComponent` on keys and values). It is hashed with the first 16 hex characters of sha256; v1 used sha1, and no v1 keys are migrated.
- `key_scope` is v1's: the first 8 hex characters of sha256(`RIOT_API_KEY`). It is returned in `/readyz` as `keyScope` (v1).
- No `c:`/`neg:` prefixes: positive and negative entries share a key, and the stored status tells them apart (design/04). v1's negative-namespace test therefore becomes structural (P3-02). Purge patterns that already start with a key scope pass through unchanged, and anything else is prefixed with the current scope (v1 `scopedPurgePattern`). Derived reads use `{scope}:derived:{part}:{hash}` (v1 `derivedKey`).
- Keys use the **resolved** host, so an account lookup routed via `sea` shares the `asia` entry.

## ADR-028 — L1 cache (2026-09-25)
Accepted. design/04 §Cache tiers:
- `moka::future::Cache<String, Arc<CacheEntry>>`, weight-bounded by `CACHE_L1_MAX_MB` (new variable; default 128 per design/07 §Sizing). Weight = key + body + 128 bytes of overhead. An entry larger than the whole budget is simply not kept.
- **Freshness is ours, not moka's.** `get` compares the entry's `soft_expires`/`hard_expires` (tokio `Instant`) to now: Fresh, then Stale, then Miss (evicting on Miss). moka's per-entry expiry is set to the remaining hard TTL only to reclaim memory, because it runs on real time and can't be paused in tests.
- **`content_at` survives a byte-identical refresh** of the same status, so `X-Cache-Age` is content age (design/04). Any change of bytes or status resets it.
- **Negative entries** are status 404 with an empty body and `soft = hard = negative TTL`, so they are never served stale. Immutable endpoints (match, timeline) are never put in L1; they belong to the archive (P5).
- `invalidate_where(pred)` exists for the P5-05 admin purge.
Also fixed: the `.env.example` parity test ignored variable names containing digits.

## ADR-029 — L2 write-behind (2026-09-25)
Accepted. design/04 §Cache tiers:
- `ResponseCache` = L1 + optional `L2Writer`. Every put goes to L1. Endpoints with `persist_l2` (account, summoner, ladder, mastery; ADR-016) are also queued for L2, **negative entries included** (the `cache.status` column holds 200 or 404).
- The writer batches with one transaction per flush, at 500 rows or 2 s after the first row of a batch (design/04). The queue is bounded (10 000). The request path uses `try_send` and never waits; when full, the entry is dropped with a warning. `shutdown()` flushes what is queued and is idempotent. A crash loses at most one batch window of L2 writes, which is accepted, documented and not tested (plan P3-03).
- Rows store unix ms, converted through `crate::clock::Clock` (moved out of the limiter so both share it). `warm` loads rows with `hard_expires > now` into L1, keeping `content_at`, so `X-Cache-Age` survives a restart. `sweep` deletes expired rows at boot; the periodic sweep is P7-05's maintenance job. `delete_where` serves the P5-05 purge.
- Wiring into `serve` (warm at boot, flush on shutdown) comes with the fetcher in P3-05, the first code that reads the cache.

## ADR-030 — Single-flight (2026-09-25)
Accepted. design/03 `singleflight.rs`: `DashMap<K, Shared<BoxFuture<Result<T, E>>>>`, generic over key, value and error.
- The leader's work runs on its own `tokio::spawn`ed task, so it completes even if every waiting request is cancelled. The rate-limit token is spent by then, and the fetcher (P3-05) caches the result inside the work. A panic or abort becomes `E::from(WorkFailed)` for every waiter.
- The slot is removed when the work completes, success or failure, so errors are never shared with later callers (v1 "propagates failure … without leaving the slot occupied"). If no caller is waiting at completion, the slot is removed by the next caller to join, who receives that just-finished result.
- Each caller learns whether it did the work (`did_work`, v1's `didWork`), which the fetcher needs to count exactly one upstream call.
- v1's two cross-instance cases (Redis lock, the loser polling the winner's cache write) have no equivalent in one process. They are replaced by a cancelled-leader test.
- New dependency: `futures-util` (for `Shared`), without default features.

## ADR-031 — Fetcher (2026-09-25)
Accepted. design/03 §Request lifecycle with v1 `src/fetcher.ts` semantics:
- **`X-Cache` has six values** (owner decision): v1's `HIT`, `MISS`, `STALE`, `HIT-NEG` plus design/03's `ARCHIVE` (served from the match archive; v1 said `HIT`) and `BYPASS` (`?refresh=true`; v1 said `MISS`). A negative hit is a `NOT_FOUND` error that still carries `X-Cache: HIT-NEG` (v1 docs).
- **Stale on failure is labelled `STALE`.** When upstream 5xxs, or the rate-limit budget runs out, and a copy is still inside its hard TTL, v1 served it and said `MISS`. v2 says `STALE`, which is what the client received. `X-Cache-Age` is that copy's content age.
- **Retries** (v1 client policy, moved here per ADR-017/021): 5xx and network errors retry after 250 and 750 ms; a typed 429 lets `observe` freeze the scope and re-acquires; an untyped 429 backs off 500 ms × 2ⁿ up to 8 s, 3 tries. All waits get ±20 % jitter, with at most 8 attempts in total. 401/403, 404 and other 4xx never retry.
- **SWR:** a stale hit spawns a refresh at bulk priority (15-minute budget) through the same single-flight, so a concurrent miss and a refresh share one call. The limiter observes every response, errors included.
- **Archive** is a trait with `NoArchive` until P5-02. Immutable endpoints check it first and write to it after a miss; tests use an in-memory archive to reach `ARCHIVE`.
- `proxy_cache_reads_total{state}` uses v1's labels: `hit` (archive included, as v1), `miss` (bypass included), `neg`, `stale`.
- `serve` now warms L1 from L2, sweeps expired rows, warns on ineffective `CACHE_TTL_OVERRIDES`, puts the fetcher in `AppState`, and flushes L2 after the drain.

## ADR-032 — Replay fixtures (2026-09-25)
Accepted (owner). Plan P3-06 asked for ~200 recorded responses and a recorded "429 typed application" scenario:
- **Small real set.** 10 real exchanges (576 KB) for one cold lookup of `Hide on bush#KR1`: account, summoner, league entries, top-3 mastery, 5 match ids, and those 5 matches. 200 responses would have been ~20 MB of match JSON in a public repo for no extra coverage.
- **The 429 scenario is synthetic.** It is derived from the recorded summoner exchange by editing the status and headers, and labelled `"synthetic": true` in the fixture. A real application-typed 429 would mean deliberately exceeding the key's app limit, which is an accountable violation. The 429 body is empty rather than an invented Riot error body.
- **Format:** `NN-<method>.json` (request, status, the seven rate-limit and content-type headers the proxy reads) plus `NN-<method>.body` (verbatim bytes). `riot record` redacts the key and refuses to keep any file containing the key or a string matching CI's `RGAPI-` grep.
- **The replay snapshot** records X-Cache, cumulative upstream calls and limiter usage per step, for windows ≥ 10 s only so wall-clock run time cannot change it. Pass 2's matches read `MISS` until the archive (P5-02) turns them into `ARCHIVE`, and that snapshot change is expected and reviewed then.
- Observed while recording: the key in `.env` reports `X-App-Rate-Limit: 100:120,20:1`, which is the development-key default.

## ADR-033 — Authentication (2026-09-25)
Accepted. v1 `src/auth/plugin.ts` semantics, with these choices:
- **Key sources:** `Authorization: Bearer …` (scheme case-insensitive), else `?token=` on any route (v1 allowed it for WebSocket handshakes, which can't set headers in a browser). The plan's P6-07 says `?key=`; v1's name is `token` and wins.
- **Cache:** consumer lookups are cached in moka for **60 s** (plan P4-01; v1 used 300 s) and unknown keys for 30 s (v1). A CLI `key revoke` runs in another process and can't reach the server's cache, so the TTL bounds how long a revoked key keeps working. The admin API (P5-05) calls `Auth::invalidate` for immediate effect.
- **Scopes and messages** are v1's: 401 `Missing or invalid API key`; 403 `This key lacks the '<scope>' scope`; 403 `Admin access is not permitted from this address`.
- **Admin IP allowlist:** exact addresses and CIDR ranges, with IPv4-mapped IPv6 normalised (v1). **IPv6 CIDR ranges are also accepted**; v1 supported only IPv4 CIDR and required IPv6 hosts to be listed one by one. Unparseable entries are logged at warn and ignored.
- **Client IP** is the leftmost `X-Forwarded-For` entry when present, otherwise the TCP peer, matching v1's Fastify `trustProxy: true`. (Since ADR-068, only with `TRUST_PROXY=true`.) This assumes a trusted proxy in front (Caddy, design/07). Revisit with built-in TLS (P8-03), where the binary faces clients directly.
- **`AUTH_DISABLED`** runs every protected request as the synthetic `dev-local` consumer (read+admin, 100 000/min, no allowlist), and is refused in production (ADR-008).
- Routes opt in with `route_layer(from_fn_with_state(state, require_read | require_admin))`. The consumer is placed in request extensions and recorded on the request span as `consumer` (design/07 log fields).

## ADR-034 — Consumer quota (2026-09-25)
Accepted.
- **Sliding window** (design/03 and plan P4-02), not v1's fixed one-minute window (`@fastify/rate-limit` with a Redis store). It uses a sliding log per consumer, the limiter's `Window`, so no rolling minute ever admits more than `quota_per_min` and there is no double burst at a window boundary (the same reasoning as ADR-023). To go back to fixed windows, swap the window type; the headers wouldn't change.
- **Keyed by consumer id, never by address.** v1 had a regression test for this, and it runs *after* authentication. The quota follows `quota_per_min` changes immediately.
- **Headers** are v1's names: `X-RateLimit-Limit`, `X-RateLimit-Remaining` (after this request) and `X-RateLimit-Reset` (seconds until a slot frees), on every metered response including the 429. The 429 is `QUOTA_EXCEEDED`, `Quota of N/min exceeded`, with `Retry-After`/`retryAfter` (v1's message), and is distinct from upstream `RATE_LIMITED` (503).
- **Public routes** (`/healthz`, `/readyz`, `/metrics`, and later `/docs`) are not metered (v1's `allowList`). v1's anonymous 60/min quota only ever applied to requests that then failed authentication, so v2 doesn't meter anonymous requests; they get 401.
- Consumer windows are held in memory and are not checkpointed: a restart grants a fresh minute. Quotas protect the proxy, not Riot's budget; the limiter does that.

## ADR-035 — OpenAPI document and docs routes (2026-09-25)
Accepted.
- `utoipa` 6 + `utoipa-axum` 0.3: routes register through `OpenApiRouter` (`routes!(handler)`), so the document is built from the handlers that serve traffic and can't drift from them. `riot-proxy spec` prints the same document with no config or state needed.
- **Metadata mirrors v1's document:** the two servers (incl. the `{scheme}://{host}` variables), the `bearerAuth` and `tokenQuery` (`?token=`) security schemes, global `bearerAuth` security, v1's seven tags and its `x-tagGroups`. The ops routes declare `security: [{}]` (public). `info.version` is the crate version. The description is rewritten for v2 (no Redis/Postgres) and lists the six `X-Cache` values.
- **Docs routes:** `/openapi.json`, `/openapi.yaml` and `/docs` (Scalar via `utoipa-scalar`) share `DOCS_UI` (default on, production included) and need no key, as v1 did. The Scalar page loads its script from the jsdelivr CDN, as v1's `@scalar/fastify-api-reference` page did.
- **The compare script** (`scripts/compare-openapi.py`) identifies an operation as `METHOD /path` because **v1's document has no `operationId`s**. The P4 exit check's "zero missing operation ids" is read as zero missing operations under `/v1/riot/` and `/v1/lol/`. Baseline at P4-03: 16 of 16 missing there; the ops routes already match.

## ADR-036 — `/v1/riot/*` and shared route behaviour (2026-09-25)
Accepted.
- **Scope:** v1's `/v1/riot/*` is exactly account-v1: `accounts/by-riot-id/{region}/{gameName}/{tagLine}` and `accounts/by-puuid/{region}/{puuid}`. The plan's "one handler per endpoint id" covers the 16 routes across `/v1/riot/*` (2) and `/v1/lol/*` (14, P4-05), mirroring v1's contract.
- **Passthrough:** Riot's bytes are sent unchanged with `Content-Type: application/json; charset=utf-8`, plus `X-Cache` and `X-Cache-Age` (whole seconds, rounded, as v1). `HIT-NEG` is also sent on the cached 404. `X-Request-Id` and `X-RateLimit-*` come from the request-id and quota layers.
- **No `Cache-Control`.** The plan lists it, but neither v1 nor design/03's header list sends one, and no value is specified anywhere. Left out; easy to add once a policy is chosen.
- **Validation** follows v1's schemas: region and platform enums are case-sensitive (`BAD_REGION`); Riot ID lengths 1–16 and 1–5 in code points; PUUID 60–128 chars of `[A-Za-z0-9_-]`; match id `^[A-Za-z0-9]+_[0-9]+$`, 6–40 chars. Messages follow ajv's wording (`params/gameName must NOT have more than 16 characters`), so clients that surface them see v1-like text. `sea` is accepted for account routes and sent to `asia` (v1).
- **`?refresh=true`** is honoured (`X-Cache: BYPASS`) for admin keys only and ignored for others; v1 dropped unknown query params on these routes. This is design/03's admin-only bypass.
- **`proxy_requests_total{route,status,cache}`** is recorded for every request. `route` is the matched template in v1's Fastify form (`/v1/riot/accounts/by-puuid/:region/:puuid`), so existing dashboards group the same way. Unmatched requests are labelled `unmatched`; v1 used the raw URL, which made the label unbounded. `cache` uses v1's labels (`hit`, `miss`, `stale`, `neg`, `none`) plus `archive` and `bypass`.
- OpenAPI: a shared `ErrorResponse` component (v1's envelope plus `requestId`) is referenced by every error status, as v1 did after its #61.

## ADR-037 — `/v1/lol/*` routes (2026-09-26)
Accepted.
- **Eleven Riot-backed routes** exactly as v1's `routes/lol.ts`: summoner, league entries by PUUID, apex league, paged league entries, match ids, match, timeline, spectator, mastery (with `?top=N` → the top-N endpoint), rotations, status.
- **"Typed" means the request, not the response.** v1 passed Riot's bodies through unmodified (`PassthroughResponse`, deliberately unconstrained so that Riot adding a field never breaks the proxy), and so does v2. The plan's "typed serde structs derived from v1 TypeBox schemas" applies to v1's param and query schemas, which are enforced before any upstream call: ladder enums (`RANKED_QUEUES`, paged vs apex tiers, divisions, taken verbatim from v1 `riot/ladder.ts`), `MatchIdsQuery` (start 0–10 000, count 1–100, queue 0–5000, type enum, start/end time ≥ 0), `page` 1–100 000, `top` 1–200. Unknown query params are dropped (v1 ajv `removeAdditional`).
- **v1's defaults are applied**, so the upstream request and cache key match v1: match ids always send `start=0&count=20` and paged entries `page=1`. `?count=20&start=0` and no query share one cache entry.
- **Match routes** check that the match id's platform prefix agrees with the path region: `BAD_REGION`, "Match X belongs to region 'a', not 'b'" (v1 `resolveMatchRegion`).
- **Analytics deferred (owner decision):** v1's three `/v1/lol/analytics/*` routes read the aggregate tables and are implemented with them in P7-04. The P4 exit check is therefore "zero missing `/v1/riot/*` and `/v1/lol/*` operations except those three"; P7's exit check covers them.
- The OpenAPI document shares one `PassthroughResponses` set (200 plus v1's `upstreamErrors` statuses, each referencing `ErrorResponse`).

## ADR-038 — Dev UI and dashboard shells (2026-09-26)
Accepted. v1's `public/dev-ui.html` and `public/dashboard.html` are copied verbatim to `src/ui/` and embedded with `include_str!` (design/07: nothing to mount at runtime). Routes are v1's: `/dev` and `/dev/{*rest}` (the same document, for client-side routes), `/dev/config.json` (`authDisabled`, `defaultPlatform`, `regions`, `platforms[{value,label,region}]`), `/dashboard` and `/dashboard/config.json` (`authDisabled`). All are public, carry `Cache-Control: no-store` (v1), are unmetered, and are left out of the OpenAPI document (v1 `hide: true`). `DEV_UI` defaults off in production, where an explicit value wins; `DASHBOARD_UI` defaults on (ADR-008). The dashboard calls `/v1/admin/metrics`, `/metrics/history`, `/tracked-players` and `/v1/ws`, which arrive in P5–P7; P7-06 wires its data and adapts any v1-specific text. *(The `/dev` page, its catch-all and its config are superseded by ADR-071.)*

## ADR-039 — Archive schema (2026-09-26)
Accepted. `V0002__archive.sql` is design/04 §Schema verbatim for `players`, `matches`, `timelines`, `match_facts`, `champion_stats`, `champion_matchups`, `champion_builds`, `ladder_crawls`, `ladder_entries` and `crawl_match_ids`, with their indexes (plan P5-01: "exactly as design 04"). Existing V0001 databases upgrade in place (tested).
- **Known gap versus v1, to resolve in P7-04:** v1's schema also had `champion_items`, `champion_runes`, `champion_spells`, `champion_bans`, `analytics_slices`, `match_bans`, `match_participants` and `league_entries`. Design/04 replaces some (`match_participants` → `match_facts`, `league_entries` → `ladder_entries`, items/runes/spells → `champion_builds`) and drops others (bans, analytics slices). v1's analytics routes, deferred to P7-04 (ADR-037), read bans, runes, spells and slices, so P7-04 must either add them in a new migration or answer from design/04's tables. That is an owner question at P7-04.
- `timelines.match_id` references `matches` without `ON DELETE CASCADE` (design/04), so deleting an archived match requires deleting its timeline first. `match_facts` cascades.

## ADR-040 — Match archive (2026-09-26)
Accepted. `archive::matches` stores match-v5 bodies zstd-compressed (level 3) and returns them byte-identical; `archive::SqliteArchive` is the fetcher's `Archive`, so `match.byId` and `match.timeline` read the archive first (`X-Cache: ARCHIVE`) and archive after an upstream miss, before the response is returned.
- **Extracted columns:** `patch` is `major.minor` of `info.gameVersion` (v1 migration 0005), `queue_id` is `info.queueId`, `game_end_ms` is `info.gameEndTimestamp`. All three are `NOT NULL` in design/04, so a body missing any of them is **not archived** (warned; the caller still gets Riot's body) rather than stored with an invented value. v1 stored a null patch instead; v2's schema has no null to store.
- **Idempotent upsert**, as v1: archiving a match again rewrites its row and increments `proxy_archived_matches_total` (v1's "matches upserted").
- **Timelines** are stored only with `ARCHIVE_TIMELINES=true`, and only when their match is already archived (the `timelines → matches` foreign key; v1 likewise dropped a timeline that arrived before its match). Archived timelines are served whatever the flag says. v1 archived fetched timelines regardless of the flag, which only gated job fetches; design/04 marks the table "ARCHIVE_TIMELINES=true only".
- **Failures never fail a request:** read errors fall through to Riot, write errors are logged (v1 behaviour).
- `region` is the routing value the match was fetched through (`asia`, `europe`, …). `filter_unarchived` keeps input order and queries in chunks of 500 ids.
- The compression ratio is logged at debug per match.

## ADR-041 — Match facts (2026-09-26)
Accepted. `archive::facts::extract` is a pure function from a match-v5 body to one `match_facts` row per participant; `FACTS_VERSION = 1`. `matches::put` writes the match and its facts in one transaction (v1 `archiveMatch` did both), so `SqliteArchive` now carries the key scope.
- **Column shapes** (design/04 names the columns, not their JSON): `position` is `teamPosition`, with `""` or absent stored as `NULL` (Arena, ARAM, remakes). `items` is `[item0..item5]` in slot order with `0` for an empty slot; the trinket `item6` is excluded, as v1's item stats excluded it. `summoners` is `[summoner1Id, summoner2Id]` in slot order; v1 normalised the order when aggregating, not when storing. `runes` is `{primaryStyle, keystone, subStyle, perks[], statPerks[offense, flex, defense]}`, which is v1's keystone and sub-style pair plus the rest of the page, so build views need no re-extraction.
- **No invented values:** a participant without a puuid, team, champion or `win` (all `NOT NULL`) is skipped. v1 stored nulls there, and its metadata-only fallback rows are impossible under this schema. A puuid repeated within one match keeps its first row, because the key is `(match_id, puuid)`.
- **Remakes are recorded as Riot reports them**, as v1 did; v1's aggregates did not filter them either. `match_facts` has no remake flag and `matches` has no duration, so if P7's analytics should exclude remakes, that needs a schema addition. That is an owner question at P7.
- A body whose facts cannot be read is still archived, with no facts rows.
- Re-archiving replaces the key scope's facts for that match (`DELETE` then upsert), so a `facts_version` bump re-derives cleanly.
- **Fixtures:** `tests/fixtures/matches/` holds two real matches (ranked solo, Arena) and a remake derived by script from a real match, to avoid spending Riot calls hunting for a recorded one. The README lists every edit.

## ADR-042 — `/v1/players/*` composites (2026-09-30)
Accepted. The four routes are v1's `routes/players.ts`: `/{puuid}/profile`, `/by-riot-id/{gameName}/{tagLine}/profile`, `/{puuid}/matches` and `/{puuid}/champions`. Request rules, response shapes and field order follow v1's schemas (`ProfileBody`, `MatchPage`, `MatchSummary`, `PlayerChampions`).
- **Partial failure (v1):** the profile's four parts are fetched concurrently. A failed part is `null`, with `"<part> unavailable (<CODE>: <message>)"` in `warnings[]` and a null age; only every part failing is a 404. On the match page, a failed id lookup fails the page with its own error. A match that cannot be fetched, or holds no participant for the player, is left out and named in `warnings[]`.
- **Composite `X-Cache`** is `MISS` if any part went upstream (a miss, or a won refresh) and `HIT` otherwise, with `X-Cache-Age` the stalest part (v1 `summarise`, which knew only HIT and MISS).
- **The match page** reads the whole page from the archive in one query (`archive::matches::get_many`, v1 #54), counts those as cache hits, and fans out only for the rest. Summaries are v1's `match-summary.ts` projection, byte-for-byte in field names and order.
- **`?refresh=true`** is open to any consumer on these routes, metered once a minute per player and part (profile, matches), as v1 did; `proxy_refresh_claims_total{part,outcome}` counts it. v1 held the window in Redis; v2 holds it in memory (`RefreshWindows`), so a restart forgets running windows, allowing at most one early refresh per player. A refresh re-reads the match id list, never the immutable matches.
- **`players` rows:** a profile upserts the player with the account's Riot ID; the first page of match history (`start=0`, `LOOKUP_BACKFILL_LIMIT > 0`) upserts it without one. Stored columns are never blanked by a lookup (v1 `upsertPlayer`).
- **Backfill:** v1 queued a history walk on the first lookup and returned a `backfill` notice. That needs the job queue, so `backfill` is always `null` until P6-06.
- **Champion pool:** grouped at read time from `match_facts` joined to `matches`, cached in L1 for 300 s under a derived key (v1 `POOL_TTL_S`), never calling Riot. `championName` is absent until the Data Dragon mirror (P7-01), as v1 omitted names it could not resolve; and `csPerMin` needed CS and game length, which design/04 did not store; the owner chose to add them (ADR-043).
- Numbers the proxy computes print as JavaScript printed them (`winRate: 1`, not `1.0`); ISO timestamps match `toISOString`.

## ADR-043 — CS and game length in the archive (2026-09-30)
Accepted (owner decision: "CS and game length is important"). Design/04 dropped v1's `cs` and `game_duration` columns, which v1's champion pool used for `csPerMin`. `V0003__cs_and_duration.sql` adds them back, and design/04's schema block now shows them.
- `matches.game_duration` is `info.gameDuration` in seconds. Riot changed the field from milliseconds to seconds at patch 11.20, the same patch that introduced `gameEndTimestamp`. The archive already refuses bodies without `gameEndTimestamp` (ADR-040), so every archived duration is in seconds.
- `match_facts.cs` is `totalMinionsKilled + neutralMinionsKilled`, `NULL` only when both are absent (v1 `extractCs`). `FACTS_VERSION` is now 2.
- Both columns are nullable: rows written before V0003 have neither, and `facts:reextract` (P7-04) must re-derive them from the stored bodies, **including `matches.game_duration`**, not only the facts rows. No v2 archive exists outside development yet, and `migrate-v1` (P8) derives both from imported bodies.
- `csPerMin` = CS ÷ minutes, with both summed over only the games that have both values, so a game missing either can't skew the rate. It is absent when no game qualifies, as v1 omitted it; Arena games report their true 0 CS. v1 summed CS over every row and duration over rows with K/D/A, which could mix games from different sets.

## ADR-044 — Admin routes, data subset (2026-09-30)
Accepted. Twelve operations from v1's `admin.ts`, `health.ts` and `debug.ts`, all behind the admin scope and the admin IP allowlist, with quotas applying:
- `POST/GET /v1/admin/consumers`
- `DELETE /v1/admin/consumers/{id}`
- `POST /v1/admin/consumers/{id}/revoke-cache`
- `GET/POST /v1/admin/tracked-players`
- `DELETE /v1/admin/tracked-players/{puuid}`
- `POST /v1/admin/cache/purge`
- `GET /v1/admin/stats`
- `GET /v1/admin/limits/{scope}`
- `GET /v1/admin/debug/riot`
- `GET /v1/admin/debug/cache`

The OpenAPI compare shows the remaining 11 v1 admin operations missing (jobs, ladder, analytics, Data Dragon, metrics); each arrives with its feature.

- **Owner decisions:** the plan's `/v1/admin/archive/stats` is served at v1's path, `GET /v1/admin/stats`. It returns v1's `keyScope`, `archivedMatches` and `trackedPlayers`, plus `knownPlayers`, `archivedTimelines`, `archiveStoredBytes` and `archiveRawBytes`. `limits/{scope}`, `revoke-cache` and both debug routes, which no plan task named, are included.
- **Bodies** are read as v1's ajv did (`coerceTypes: 'array'`, unknown fields dropped), with ajv's messages (`http::body`). A body that is not JSON is a `VALIDATION` 400; v1 answered 500 because its error handler did not recognise Fastify's parse error.
- **Consumer ids are ULIDs**, so `{id}` is validated as one (`params/id must match format "ulid"`, where v1 said `"uuid"`). Timestamps are ISO strings as v1's were. A duplicate name is a `VALIDATION` 400; v2 names are `UNIQUE` (design/04 DDL, ADR-012).
- **Revoking a consumer drops its key from the auth cache immediately.** v1 left it working until the cache TTL and offered `revoke-cache` for that; `revoke-cache` remains, and still requires the key hash to belong to the consumer in the path, as v1's later fix did.
- **Tracking by PUUID keeps a stored Riot ID.** v1 wrote `null` names when a body had no `gameName`/`tagLine`, erasing names an earlier track by Riot ID had stored. Tracking by Riot ID resolves through account-v1 on the cached read path, and Riot's spelling wins; that is also how a player is re-resolved after a key rotation. A Riot ID with no PUUID in the answer is a 404 (v1 crashed). `backfill` is `null` and the `historyBackfill*` fields are `null` until the backfill job (P6-06) gives `players.backfill_state` its shape.
- **Purge** matches Redis `MATCH` globs (v1 used `SCAN MATCH`) against keys scoped to the current Riot key (`scoped_purge_pattern`), in L1 and L2. `deleted` counts distinct keys removed. An L2 write still queued (up to 2 s of write-behind) can land after a purge, and at worst comes back at the next boot's warm. The match archive is not a cache and is never purged.
- **Limits** reports every window the limiter holds for the bucket, including the bootstrap limits used before Riot has named any.
- **`debug/riot`** goes through the limiter and cache like any read. Without `method` it borrows v1's conservative `status.platformData` bucket and keys the cache on the whole path. With `method`, the path must be that endpoint's route (`Endpoint::parse_path`), so the request, cache key and any archive write are exactly the route's. v1 accepted any path under any method, which in v2 would have filed a debug body under the wrong cache key or archive id. Query parameters must be ones the endpoint takes. `debug/cache` reports `{key, present, ageSeconds, stale}` without fetching.
- **Not assigned to any task:** `POST /v1/admin/players/names/backfill` (a job) belongs with the jobs work in P6.

## ADR-045 — WebSocket protocol and hub (2026-09-30)
Accepted (owner decisions). Design/06 called its protocol "unchanged from v1 §11", but it differed from v1's code in its frames, topics, event names and `?key=`. The owner chose **v1's wire protocol plus four fixes**, and **v1's event names plus design/06's `crawl.phase`** (for P6-02). Design/06 §Protocol and §Events now say so.
- **Kept from v1:**
  - frames are `op`-tagged: `subscribe`, `unsubscribe` and `ping` from the client; `ready`, `subscribed` (listing everything the socket holds), `pong` and `error` from the server;
  - topics are `player:<puuid>`, `patch`, and the admin-only `metrics`, `firehose` and `ladder`, so a read key sees only the players it asks for;
  - browsers authenticate with `?token=`;
  - the server pings every 30 s and drops a socket after two unanswered pings; a socket holds at most 200 topics; over-long and non-string topics are dropped;
  - shutdown closes sockets with 1001.
- **Four fixes over v1:**
  1. `{"op":"resync","topic":…,"dropped":n}` when a socket falls more than a topic's buffer (256) behind.
  2. Topics are validated, so a typo is an error frame (`VALIDATION`, "Unknown topic 'x'") rather than a subscription that never fires.
  3. Sockets whose key is revoked are closed with 4401 "key revoked" (`Hub::close_consumer`, wired to revocation in P6-07); v1 left them open.
  4. Event frames carry `"op":"event"`, which is additive for v1 clients.
- **Hub (design/06):** one `broadcast` channel per topic, created on first subscribe and dropped when its last subscriber leaves, so per-player topics cost nothing when nobody follows that player. Event frames are serialised once and shared (`Utf8Bytes`). A socket holding the firehose skips its other topics, so no event arrives twice. **Order is kept within a topic** (and on the firehose) but not across topics, since receivers are polled fairly; v1's single relay happened to keep global order, which its protocol never promised.
- **Dependencies:** `tokio-stream` (with `sync`, for `StreamMap` and `BroadcastStream`) and, for tests, `tokio-tungstenite`, both already in the lock file through axum. `futures-util` gains its `sink` feature.
- **Acceptance:** the ignored soak (`tests/ws_soak.rs`) holds 1 000 sockets for 20 s with a 1 s heartbeat and a steady stream of events: 1.89 M frames delivered, 0 resyncs, resident memory +0.2 MiB (281.8 → 281.9 MiB, clients in the same process).
- **Event names (P6-02), owner's choice:** v1's `game.started`, `game.ended`, `rank.changed`, `match.archived`, `patch.new`, `ladder.crawl.completed`, `analytics.updated` and `metrics.snapshot`, plus design/06's `crawl.phase` (a crawl's stage changes, on the `ladder` topic) for the dashboard's progress panel. Design/06's renames (`metrics` for `metrics.snapshot`, and dropping `ladder.crawl.completed` and `analytics.updated`) are not taken: they added nothing, and would have broken the dashboard and existing listeners. Payload field names are v1's (`queueId`, `championId`), with design/06's extra fields added.

## ADR-046 — Event catalogue (2026-09-30)
Accepted. `events::Event` holds the nine events of ADR-045: v1's eight plus `crawl.phase`. It is an adjacently tagged enum (`event` + `data`, v1's frame keys) rather than the plan's `tag = "name"`, because v1's wire key is `event`. `publish()` builds the frame once, `{op:"event", event, topic, at, data}` in v1's key order, and increments `events_published_total{name}` (a v2 metric, ADR-014 naming), whether or not anyone holds the topic.
- **Payloads** are v1's, with its spellings (`gameId`, `queueId`, `championId`, `matchId`, `durationS`) and absent optional fields left out rather than nulled. Design/06's additions are optional extra fields: `platform`, `queueId` and `championId` on `game.ended`, and `patch` and `participants` on `match.archived`.
- **Topics** follow v1's publish calls: player events on `player:<puuid>`, `patch.new` on `patch`, the three ladder and analytics events on `ladder`, and `metrics.snapshot` on `metrics`. A `match.archived` with no player (a crawl archive) goes to the firehose alone; v1 published it to `player:`, which nobody could hold.
- `crawl.phase.stats` and the `metrics.snapshot` body are free-form JSON until P7-02 and P7-06 fix their shapes.

## ADR-047 — Durable job queue (2026-09-30)
Accepted. `jobs::Scheduler` is design/06 §Scheduler over the V0001 `jobs` table:
- `enqueue` is `INSERT OR IGNORE` against the partial unique index, and returns v1's `queued` / `already-queued`. `enqueue_on` does the same inside a caller's transaction, so a handler can fan out and record progress in one write.
- Claiming is design/06's `UPDATE … RETURNING`, ordered by priority, then `run_after`, then id (for determinism).
- `JOB_CONCURRENCY` workers wait on a `Notify` with a 1 s fallback.
- A retryable failure waits `2^attempts × 30 s ± 20 %`, with `attempts` counted at claim so the first retry is about 60 s; the fifth failure is final. A handler can also fail permanently (a bad payload, say). An unknown kind fails at once.
- **Resuming after a crash** (the plan's "restart resumes", which design/06 does not spell out): `recover()` on boot returns every `running` row to `pending`. The interrupted attempt still counts. Handlers are idempotent (design/06), so re-running the one unfinished job is safe. With a single process, any row still `running` at boot belongs to the previous one.
- **Panics** in a handler are contained (it runs in its own task) and retried with backoff. **Shutdown** stops claiming, lets running jobs finish within a grace period, then aborts them; their rows stay `running` for `recover`.
- **Metrics:** `proxy_jobs_total{job,status}` counts every attempt as `completed` or `failed`, as v1 did. `jobs_pending{kind}` (design/07) is sampled every 15 s, and a kind that empties reads 0.
- Handlers are a small object-safe trait returning a `BoxFuture`, rather than depending on `async-trait`.

## ADR-048 — Ticks (2026-09-30)
Accepted. `jobs::ticks` runs one interval loop per repeating job, and ticks only enqueue (design/06).
- **Schedule** (v1 `scheduleRepeatables`, config names v1's): `poll:live` every `TRACK_POLL_LIVE_S` (60), `poll:rank` every `TRACK_POLL_RANK_S` (600), `poll:matches` every `TRACK_POLL_MATCH_S` (300), `ddragon:sync` every `DDRAGON_SYNC_S` (3600), `maintenance` daily. The ladder-crawl and aggregate ticks arrive with P7-02 and P7-04. v1's daily `names:backfill` tick arrives with its job (ADR-044).
- **Firing:** the first tick is immediate, so a tick missed while the process was down fires on boot, as design/06 intends. A tick missed because the process stalled is skipped rather than burst.
- **Fan-out** (v1 `fanOut`): one job per tracked player of the current key scope, `{puuid, platform}`, deduped by PUUID and written in one transaction. A slow poll is therefore never queued twice, and once it has run the next tick re-queues it. `ddragon:sync` and `maintenance` are deduped on their kind.
- **Priorities:** polls use design/06's 10 000 band. Design/06 does not band `ddragon:sync`; it shares `maintenance`'s 30 000.
- **Tests:** the loop's timing is tested under paused time with a counting action, and the fan-out against a real database. The plan wanted both in one paused-time test, but tokio's paused clock auto-advances while SQLite's blocking reads are awaited, so that test would count phantom ticks.
- **Wiring:** ticks and workers start from `serve` once their handlers exist (P6-05 to P6-07); until then a tick would queue jobs that fail as unknown kinds.

## ADR-049 — Poll handlers (2026-09-30)
Accepted. `jobs::poll` ports v1's `pollLive`, `pollRank` and `pollMatches`. Every Riot call is `Priority::Bulk` (design/06).
- **State lives on the player's row**, where v1 kept Redis keys with a TTL: `players.in_game_id`, `last_rank` (a JSON snapshot by queue) and `last_seen_match_id`. It survives restarts, so a restart neither re-announces games in progress nor treats every rank as a new baseline.
- **`poll:live`:**
  - A game id different from the stored one publishes `game.started` with v1's payload (`queueId` from `gameQueueConfigId`, `championId` from the player's participant).
  - A 404 after a game publishes `game.ended` with `puuid`, `platform` and `gameId`. `queueId` and `championId` are not stored and so are absent. It also queues `poll:matches` a minute later, deduped with the tick's own poll (v1).
  - Other errors are retried, never read as "not in game". Going straight from one game to another publishes only the new start, as v1 did.
- **`poll:rank`:** the first snapshot is a baseline. After that, one `rank.changed` per queue whose `{tier, rank, lp}` moved, with `before: null` for a new queue. As in v1, a queue that disappears is not reported.
- **`poll:matches`** (v1 #46): pages from the newest match back to the cursor, 5 ids on the first page and 100 per page after it, up to `TRACK_CATCHUP_LIMIT`. Anything deeper hands over to `backfill:player` (`reason: "catchup"`, deduped per player).
  - Archive jobs are deduped by match id. Their priority follows design/06: the first page is the interactive band (0), and deeper matches get `100 + depth / 10`.
  - **Fix over v1:** v1 moved the cursor only when a new match still needed archiving, so a player whose new games had already been archived (by a lookup, say) was paged further back every tick. v2 moves the cursor to the newest id seen.
- `jobs::Queue`, the database plus the wake signal, is split out of `Scheduler` so handlers can enqueue while the scheduler holds them.

## ADR-050 — Archive and backfill jobs, and jobs in `serve` (2026-09-30)
Accepted. `jobs::archive` ports v1's `archiveMatchJob`, `backfillPlayer` and `match-walk.ts`, and `serve` now runs the scheduler.
- **`archive:match`** fetches the match at bulk priority. The fetcher's archive stores the body and its facts on the way through (P5-02/03). A 404 fails the job for good; other errors are retried. A timeline is fetched when the job asks (or by `ARCHIVE_TIMELINES`), and its failure never fails the archive (v1).
  - `match.archived` carries `puuid`, `matchId`, `patch` and `participants`, and is published only when the match was not already archived. v1 announced every run, including re-archives.
- **`backfill:player`** walks ids 100 at a time up to `limit` (v1's default 500) and queues the unarchived ones at `100 + depth / 10`, deduped by match id.
  - Its state is design/04's `players.backfill_state`: `{startedAt, doneAt, depth, cursor, limit}`, v1 #44's stamps plus a cursor, saved after every page.
  - **A walk that fails or is killed part-way resumes from its cursor** instead of re-reading from the top as v1 did. A history that grew meanwhile only means re-reading a few ids, never skipping one.
  - Completion follows v1 `walkIsComplete`: the (unfiltered) history ran out, or the walk was at least `LOOKUP_BACKFILL_LIMIT` deep. A shallow walk never stamps `doneAt`.
- **Queueing a walk** (`enqueue_backfill`) is deduped per player while pending or running, and counted in `proxy_backfills_queued_total{reason,status}` (v1).
  - The first page of `/v1/players/{puuid}/matches` queues one (`reason: lookup`) unless a completed walk has accounted for the player, and answers v1's `backfill` notice `{jobId, status, limit}`.
  - Tracking a player (`POST /v1/admin/tracked-players`) does the same (`reason: track`, v1 #46).
  - The admin player list's `historyBackfill*` fields now come from `backfill_state`.
- **`serve`:**
  - builds the handlers over one `Queue`, runs `recover()`, starts `JOB_CONCURRENCY` workers and the three poll ticks;
  - at shutdown, stops the ticks, gives running jobs 10 s, then closes WebSocket sockets before the final limiter checkpoint;
  - does not tick `ddragon:sync` and `maintenance` until their handlers exist (P7-01, P7-05).
  - `AppState` gains `jobs` (the queue) and `hub`.
- The full P6 exit-check scenario (track, spectator flip seen on `/v1/ws`, kill mid-backfill, resume) needs `/v1/ws` and runs at the P6 exit check. Here, resume without duplicate archive jobs is tested against the handler.

## ADR-051 — `/v1/ws` (2026-09-30)
Accepted. `routes::ws` wires the hub (ADR-045) to `/v1/ws`.
- **Handshake auth** (v1): any active key, sent as the bearer header or `?token=` (v1's name; the plan's `?key=` is not used, ADR-045). `AUTH_DISABLED` runs as `dev-local`. As in v1, a missing or unknown key still completes the upgrade, then gets `{"op":"error","error":{"code":"UNAUTHORIZED","message":"Invalid key"}}` and a 4401 close, so a browser can see why.
- **Admin topics** (`metrics`, `firehose`, `ladder`) need the admin scope *and* the `ADMIN_IP_ALLOWLIST`, decided once at the handshake (v1). An admin key from a refused address can connect, but only as a reader.
- **Quota** (design/06, which v1's public route did not apply): the handshake counts as one request against the consumer's quota. Over it, the upgrade is refused with the usual 429 envelope and `X-RateLimit-*` headers.
- **Revocation closes sockets:** `DELETE /v1/admin/consumers/{id}` closes that consumer's open sockets with 4401 "key revoked" (ADR-045 fix 3).
- **The `metrics` topic** (v1 `MetricsBroadcaster`) publishes `metrics.snapshot` every `METRICS_INTERVAL_S`, only while a socket holds the topic. The snapshot carries v1's `v`, `keyScope`, `totals` (archived matches, tracked and known players), `ws` and `events` sections; the rest of v1's document arrives with the dashboard in P7-06. v1's Redis lock for electing a publisher across API instances is not needed with one process.
- **Documented** in the OpenAPI document as `GET /v1/ws` under the `ws` tag, with the protocol, topics, events and close codes as prose (design/06). v1's document had no path for it, so the contract compare shows it as added.

## ADR-052 — Admin routes for jobs (2026-09-30)
Accepted. The job routes are v2's own: v1's queues were BullMQ's, with no admin API beyond `POST /v1/admin/backfill`. Design/06 says only that `/v1/admin/jobs` "lists and retries failed rows", so the shapes below are v2's.
- `GET /v1/admin/jobs?state=&kind=&limit=` lists rows newest first (limit 1–500, default 50). Each row is `{id, kind, dedupeKey, priority, state, attempts, runAfter, claimedAt, finishedAt, error, payload}`, with ISO timestamps as elsewhere.
- `GET /v1/admin/jobs/stats` returns row counts by kind and by state, plus totals.
- `POST /v1/admin/jobs/{id}/retry` puts a **failed** row back to `pending` now, with attempts reset. It is refused, with the id, when identical work (same kind and dedupe key) is already pending or running, so a retry can never run a match twice. Any other state is refused.
- `DELETE /v1/admin/jobs/{id}` cancels a **pending** row: it becomes `failed` with error `cancelled`, so it stays visible and retryable. A running job cannot be taken back from its worker (v1 said the same of BullMQ), so it is refused.
- Refusals are `VALIDATION` 400s ("Job X is running"), as v1's crawl cancel was; an unknown id is a 404.
- `POST /v1/admin/backfill` is v1's route, `{puuid, platform, limit? (1–10 000, default 500), fetchTimeline?}` → `{ok, jobId, status}`. It queues through `enqueue_backfill` (`reason: admin`).
- **Job ids** now come from one monotonic ULID generator, so ids made in the same millisecond still sort in creation order. The claim's tie-break (oldest first) and the list's order (newest first) therefore hold within a millisecond too.

## ADR-053 — Data Dragon mirror (2026-09-30)
Accepted. `ddragon:sync` (`jobs::ddragon`) fills `DDRAGON_DIR`; `r#static` reads it. Behaviour is v1's `static/ddragon.ts`, `static/champions.ts` and `routes/static.ts` unless listed here.
- **Sync** (v1):
  - reads `versions.json`, refreshes the un-versioned queue table (`meta/queues.json`) on every run, and downloads the six data files of the newest patch when it is not mirrored yet, or always with `force`;
  - a data file Riot does not serve is skipped, not fatal;
  - a queue-table failure keeps the copy on disk;
  - `patch.new {version}` is published when a patch is downloaded.
  - Data Dragon never goes through the limiter.
  - Errors reaching the version list are `Retry`s with the usual backoff.
- **"Mirrored" means `versions.json` exists** in the patch directory. The sync writes it last, and every file is written to a temp name and renamed into place.
  - v1 kept the current version in Redis and, when Redis was empty, took the newest directory on disk even if its sync had died half way. v2 takes the newest *complete* directory. A half-synced patch is finished by the next tick instead of being served incomplete.
  - The current version is remembered in memory once found.
  - Syncs are serialised: an admin `force` and the tick can overlap.
  - `versions.json` is the list fetched at the start of the run; v1 fetched it a second time at the end.
- **Module paths:** the design's `src/static/champions.rs` is used as written. `static` is a Rust keyword, so the module is `crate::r#static`. The job and the Data Dragon HTTP client (`Cdn`) live in `src/jobs/ddragon.rs`.
- **Routes** (v1 contract, not named in the plan task but part of "static serving"):
  - `GET /v1/static/versions` → `{current, versions}`. It is `X-Cache: HIT` from the mirror, or `MISS` when fetched live before the first sync.
  - `GET /v1/static/queues`.
  - `GET /v1/static/{file}?version=` with v1's aliases, validation messages and "has not been synced yet" 404s.
  - All three need a read key (v1).
  - `POST /v1/admin/ddragon/sync {force?}` → `{ok, jobId}`. Without `force` it joins the tick's queued job (dedupe `ddragon:sync`). A forced sync has its own dedupe key (`ddragon:sync:force`), so it is never swallowed by a pending tick job.
- **`/ddragon/*`** is `tower-http::ServeDir` over `DDRAGON_DIR`, without a key, as v1's Caddy served it (design/07 §Option B).
  - Found files carry v1's `Cache-Control: public, max-age=604800, immutable`.
  - Unlike Caddy's unconditional header, a 404 does not, so a patch requested before it syncs is not cached as missing for a week.
  - Caddy's `browse` directory listing is not reproduced.
- **Champion names** (v1 #111): id → name from the current patch's `champion.json`, parsed once per patch. `/v1/players/{puuid}/champions` now fills `championName` for ids the mirror knows and omits it otherwise (v1).
- **Ticks:** `serve` now ticks `ddragon:sync` every `DDRAGON_SYNC_S` (ADR-048). The first tick is immediate, so a fresh deployment syncs at boot. `ServeOptions::ddragon_urls` points it at a mock in tests.
- **Signal handling:** `serve` installs its SIGTERM/SIGINT handler before it binds and logs `listening`. Previously a signal in the moment after `listening` hit the default action; the extra client construction made that window visible in the CLI test.

## ADR-054 — Ladder schema and crawl enumeration (2026-10-03)
Accepted. Owner decisions at P7-02 start: the ladder in v1's shape, v1's four admin ladder routes, legs as rows, and v1's names job in P7-03.
- **Schema (`V0004__ladder.sql`)** replaces V0002's three ladder tables, which nothing had written. design/04 is updated to match.
  - `ladder_crawls` is v1's run log:
    - `key_scope`;
    - a `status` (`running | completed | failed | cancelled`) beside the `phase` (`enumerate | collect | archive`);
    - v1's six counters as columns, instead of design/04's `stats` JSON;
    - `legs_failed`;
    - v1's partial unique index: one running crawl per (key_scope, platform, queue).
  - `ladder_entries` is v1's `league_entries`:
    - latest state per (key_scope, platform, queue, puuid);
    - v1's `veteran` / `inactive` / `freshBlood` / `hotStreak` flags;
    - `first_seen_crawl_id` (set once) and `last_seen_crawl_id` (restamped).
    - It does not cascade from crawl rows, so P7-04 can port v1's tier slices.
  - `crawl_legs (crawl_id, leg, cursor)` replaces v1's Redis leg set and walk cursors.
  - Crawl ids are ULIDs (v1: UUIDs), as consumer ids are (ADR-044).
- **Stage ends.** A leg deletes its own `crawl_legs` row when it ends. In the same transaction, whoever deletes the last row moves the crawl on:
  - `failed` if any leg gave up (v1);
  - otherwise `enumerate → collect`, or straight to `completed` when `LADDER_BACKFILL_LIMIT=0` (v1);
  - then `collect → archive`, and `archive → completed`.
  - A re-run of a leg that already ended finds no row and changes nothing. This is why design/06's "decrement a counter" is not used: a counter is decremented twice by a job that crashes after its decrement but before it is marked done. design/06 is updated.
  - Finishing drops the crawl's remaining legs and match ids (v1 `clearCrawlState`). A crawl that is not running is never moved on or overwritten (v1's guard on `status = 'running'`).
- **`ladder:crawl`** (v1 `startCrawl`) inserts the crawl row, its legs and their jobs in one transaction. They exist together or not at all; v1 needed an ordering rule to approximate that. The fan-out is:
  - one `ladder:apex` per apex tier at or above the floor;
  - one `ladder:walk` per (paged tier, division).
  - Job dedupe keys are `<crawlId>:<leg>`.
  - A second start of a running ladder answers that crawl's id (`created: false`).
  - Validation messages are v1's `assertRankedQueue` / `assertTier`. A tier floor is case-insensitive here, as in v1's `assertTier`.
- **`ladder:apex` / `ladder:walk`** (v1):
  - Apex entries take the leg's tier, and division `I` when the entry has none.
  - A walk pages until the first empty page and checks every 10 pages that its crawl is still running.
  - Each non-empty page is written in one transaction: the entries, every player as an untracked player (a crawl never tracks anyone), the crawl's counters, and the walk's next-page cursor. A crash re-walks at most one page.
  - Empty pages are not counted.
  - Fetches are bulk priority. Its 15-minute wait budget already exceeds v1's 5-minute ladder budget.
  - A leg ends as failed on a non-retryable error or on its last attempt (v1 `isFinalAttempt`). Retries keep it outstanding.
- **Priorities** sit inside design/06's 20 000 band, in v1's order: crawl 20 000, apex 20 001, walk 20 002, collect 20 003, archive 20 004.
- **Events and metrics:**
  - `crawl.phase {crawlId, platform, queue, phase, stats}` is published on each stage change, and on the crawl's end with `phase` set to the final status. `stats` holds v1's counters.
  - `ladder.crawl.completed` (v1 fields) is published only for a clean run.
  - `proxy_ladder_pages_total` and `proxy_ladder_entries_total` count pages; `proxy_ladder_crawl_duration_seconds{platform,queue,status}` records each finished crawl.
- **Config:** `LADDER_QUEUES` (Riot's casing) and `LADDER_TIER_FLOOR` (trimmed, upper-cased) are now refused at boot when unknown, closing ADR-008's deferred validation.
- **Tick:** `ladder:crawl` per `LADDER_PLATFORMS` × `LADDER_QUEUES` every `LADDER_CRAWL_S`. It is off when 0, the default (v1), and deduped per ladder.
- **Routes** (v1):
  - `POST /v1/admin/ladder/crawl {platform?, queue?, tierFloor?}` answers 202 `{crawlId, status: started|already-running, platform, queue, legs}`. Defaults are `DEFAULT_PLATFORM`, the first `LADDER_QUEUES`, and `LADDER_TIER_FLOOR`. Body enums are exact, as ajv's were.
  - `GET /v1/admin/ladder/options` → `{platforms[{id,label}], queues, tiers, defaults{platform, queue, tierFloor, backfillLimit}}`.
  - The crawl list and cancel routes arrive with P7-03.

## ADR-055 — Crawl collect and archive, crawl list and cancel, names backfill (2026-10-03)
Accepted. Completes the crawl (v1 `jobs/ladder-crawl.ts`) and adds the routes and job the owner added to P7-03 (ADR-054).
- **Stage hand-overs commit with the stage end.** The transaction that deletes a stage's last leg also queues the next stage's legs and jobs:
  - enumerate → collect: batches of 25 players, `ladder:collect` legs named by offset;
  - collect → archive: the one `ladder:archive` leg.
  - A crash can therefore never leave a crawl in a stage with no jobs. v1 needed a "fan-out" sentinel leg to approximate this; v2 does not.
  - A collect stage with nobody to walk goes straight to archive (v1).
- **Collect candidates** (v1 `listCrawlBackfillCandidates`): entries this crawl stamped (`last_seen_crawl_id`), minus players whose walk started at or after the crawl did. They are ordered by league points then PUUID, descending. `backfills_enqueued` counts them.
  - All batches are queued in that one transaction. v1 paged the list with a keyset cursor because its jobs started while it was still paging; v2's are not visible until commit, so the paging hazard does not arise.
- **`ladder:collect`** (v1):
  - per player, it checks that the crawl is still running and stamps `backfill_state.startedAt`;
  - it walks match ids for the ladder's queue (420/440), 100 per page, up to `LADDER_BACKFILL_LIMIT`, into `crawl_match_ids`. `INSERT OR IGNORE` is the de-duplication. `match_ids_seen` counts new ids and survives a re-run, because re-inserted ids are not new.
  - A 404 skips the player; other errors retry the job.
  - A walk stamps `doneAt` only when it is at least `LOOKUP_BACKFILL_LIMIT` deep. A queue-filtered walk that runs out says nothing about the rest of the history (v1 `walkIsComplete`).
- **`ladder:archive`** (v1) drains the set 100 ids at a time. For each batch, one transaction holds:
  - an `archive:match` job per id `filter_unarchived` returns (deduped on the match id);
  - the batch's removal from the set;
  - `matches_queued`.
  - A crash re-reads at most one batch, and its jobs are deduped.
  - Priority **20 005**, after every lookup's depth-ranked archive jobs and after the polls. v1's `ARCHIVE_PRIORITY.ladder` (10 000) sat on its own queue; in v2's single queue, 10 000 would starve the polls behind a crawl's tens of thousands of matches.
- **Crawl end** queues `names:backfill` in the same transaction, for completed and failed crawls alike (v1: a name read from a match that did land is correct either way). It is not queued for cancelled crawls (v1).
  - `aggregate:analytics` on a clean end is added with its handler in P7-04, so that no job is queued that nothing can run.
- **`GET /v1/admin/ladder/crawls?platform=&queue=&limit=`** (1–100, default 20) returns v1's `LadderCrawlSummary` rows, newest first, with ISO timestamps and `pendingLegs`.
- **`DELETE /v1/admin/ladder/crawls/{id}`** (v1):
  - marks the crawl `cancelled` (never a finished one: "Crawl X is already completed" is a `VALIDATION` 400) and drops its working state;
  - cancels its queued legs as `failed`/`cancelled` job rows, so they stay visible, as `DELETE /v1/admin/jobs/{id}` leaves them;
  - answers `{ok, crawlId, status, droppedJobs}`.
  - Running legs stop at their next status check.
  - An unknown id is v1's 404. Ids are ULIDs. `crawl.phase` is published with phase `cancelled`.
- **`names:backfill`** (v1 `backfillNamesFromArchive`):
  - targets nameless players of the key scope, freshest first (limit 50 000);
  - reads each one's three most recent archived matches;
  - takes `riotIdGameName` (or `riotIdName`) and `riotIdTagline` from the newest match that carries them;
  - fills only null names, so account-v1 and admin names win;
  - makes no Riot call.
  - Bodies are decompressed once per batch of 500 players, since ten players share a match.
  - It runs in the 30 000 band, deduped to one at a time, with a daily tick (v1).
  - `POST /v1/admin/players/names/backfill` queues a pass and answers 202 `{ok, unnamed}`, counted before the pass (v1).

## ADR-056 — Analytics in v1's shape, with remakes kept apart (2026-10-03)
Accepted. Owner decisions at P7-04 start: v1's analytics tables and routes in place of design/04's three tables; remakes stored apart and excluded by default, with `?remakes=include` for v1's numbers. design/04 and design/06 are updated.
- **Facts (`V0005`, `FACTS_VERSION` 3):**
  - `match_facts` gains `gold`, `damage` and `vision` (Riot's `goldEarned`, `totalDamageDealtToChampions`, `visionScore`).
  - `match_bans` holds `info.teams[].bans`. A `-1` ban, or one without a team or pick turn, is left out (v1 `extractBans`).
  - `matches.remake` is 1 when any participant has `gameEndedInEarlySurrender`, Riot's remake flag.
  - `matches.facts_version` records the version that derived the match's rows; it is indexed.
  - Archiving writes all of these in the match's own transaction.
- **`facts:reextract`:**
  - Re-derives, from the stored body, the facts, bans, `remake` and `game_duration` of every match below `FACTS_VERSION`, `FACTS_REEXTRACT_BATCH` at a time, pausing 50 ms between batches (v1).
  - Facts keep the key scope they were written for.
  - It needs no cursor: a match it has done is not selected again. A body that no longer derives is stamped and skipped.
  - `serve` queues it at boot when stale matches exist; that covers ADR-043's note on filling `game_duration`.
  - v1's admin trigger swept the whole archive. v2's re-derives what is stale, because a version bump is what makes rows stale.
  - `POST /v1/admin/analytics/reextract` answers 202 `{ok, stale}`; `stale` is a v2 addition.
- **Aggregates** are v1's tables:
  - `analytics_slices`, `champion_stats`, `champion_bans`;
  - `champion_matchups` (no tier);
  - `champion_items`, `champion_runes`, `champion_spells`.
  - They are keyed by key scope, platform and queue, plus tier where v1 had it, and by patch.
  - Each also carries `remake` (0/1) as a key.
- **`aggregate:analytics`** (v1) rebuilds one ladder for the newest `AGGREGATE_PATCH_LIMIT` patches, numerically ordered, or for all when the limit is 0. Older patches keep their rows. It runs three steps, each its own transaction:
  - champions: slices, stats and bans;
  - matchups;
  - builds: items, runes and spells, a transaction each.
  - Tier comes from `ladder_entries`, joined at recompute time (ADR-054). Players the ladder does not hold are left out. A match counts in every tier it had a player in.
  - Stats, matchups and builds follow v1's queries:
    - `stated_games` and the duration count only rows with a K/D/A;
    - items skip empty slots and count a duplicate once;
    - spells are an unordered pair;
    - matchups need exactly one laner per team, exclude mirrors, and record the ladder player's side.
  - Metrics are v1's: `proxy_aggregate_duration_seconds{step}`, `proxy_aggregate_rows{table}` and `proxy_aggregate_runs_total{status}`. `analytics.updated {platform, queue, durationS, tables}` is published on success.
  - The job is deduped per ladder.
  - Triggers:
    - a clean crawl end, queued in the crawl's final transaction (ADR-055);
    - `AGGREGATE_INTERVAL_S` per `LADDER_PLATFORMS` × `LADDER_QUEUES`, off when 0 (v1);
    - `POST /v1/admin/analytics/recompute {platform?, queue?}`, which answers 202 `{ok, platform, queue}` (v1).
  - v1's 30-day Redis "last run" record is not kept; P7-06 decides whether the dashboard needs one.
- **Routes** are v1's three `/v1/lol/analytics/*`, with v1's:
  - query bounds, defaults and messages;
  - field names and order;
  - rounding to 4 places;
  - `pickRate` clamp, and absent averages without stated games;
  - newest-patch default;
  - empty 200 before any recompute;
  - `Cache-Control: private, max-age=300`;
  - weak `ETag` and `If-None-Match` handling, including lists and `*` → 304.
  - The ETag hashes v1's material with SHA-256 (the crate v2 has) rather than SHA-1. ETags are opaque, so no client depends on the hash.
- **Remakes.** `?remakes=exclude` is the default and `?remakes=include` adds the remake rows back. It applies to the three analytics routes and to `/v1/players/{puuid}/champions`, and is part of the ETag and the pool's cache key.
  - A match not yet re-extracted (`remake` NULL) counts as no remake.
  - The default changes `/v1/players/{puuid}/champions` too: remakes are no longer in a pool unless asked for. This is a v2 difference the acceptance suite must account for at P8-01.
- **Fixture:** `archive::analytics` tests rebuild a five-match fixture (a remake, an older patch, another queue, players off the ladder) and assert every table against values computed by hand (plan acceptance).

## ADR-057 — Maintenance and backups (2026-10-03)
Accepted. `maintenance` (`jobs::maintenance`) runs daily, from the first tick at boot, at the 30 000 band. Its steps run in design/07's order: the backup first, so the other steps can be undone from it.
1. **Backup:** `VACUUM INTO "$DATA_DIR/backups/riot-proxy-<UTC date>.db"`.
   - It runs on its own read-only connection, so the writer is not held for the copy.
   - The copy is written to `.riot-proxy-<date>.db.partial` and renamed, so a copy cut short by a crash is never taken for that day's backup.
   - One per UTC day: a restart the same day does not take another.
   - The newest 14 are kept. Only files named `riot-proxy-YYYY-MM-DD.db` are ever deleted.
   - design/04 said "weekly". design/06, design/07 and the plan say daily, and daily is used; design/04 is corrected.
2. **Trim:**
   - `done` jobs finished more than 7 days ago (design/06). `failed` rows stay, for `/v1/admin/jobs` to show and retry.
   - `metrics_history` older than 24 hours (design/04's 1440 points at 60 s).
3. **Sweep** expired L2 rows. This is the same sweep `serve` runs at boot.
4. **Checkpoint:** `PRAGMA wal_checkpoint(TRUNCATE)`.

A failure retries with the scheduler's usual backoff.

**`riot-proxy backup OUT`** runs the same `VACUUM INTO` on demand, safe while `serve` runs (design/07). Like SQLite, it refuses to overwrite `OUT`. It creates missing parent directories and prints the path and size.

With this, every tick in `schedule()` runs in `serve`: ADR-048's deferral is closed.

## ADR-058 — The dashboard's snapshot and history (2026-10-03)
Accepted. `stats::Stats` builds v1's `MetricsSnapshot` (`v: 1`), field for field and in v1's order. The same document goes to the `metrics` topic and to `GET /v1/admin/metrics` (v1). Where v1 read Redis, BullMQ or its separate worker, v2 reads what replaced them:
- **`queues`:** the `jobs` table, grouped by kind into v1's six queue names, so the dashboard's queue panel is unchanged:
  - `poll`, `archive`, `backfill`, `ddragon`, `ladder`;
  - `maintenance`, which also holds `aggregate:analytics`, `facts:reextract` and `names:backfill`, as in v1.
  - Counts: `active` = running; `waiting` = pending and due; `delayed` = pending in backoff; `failed` = failed in the last 24 h; `completed` = done in the last hour (v1's retention windows).
  - `prioritized` and `scheduled` are always 0: every v2 job has a priority, and ticks are timers, not parked jobs.
- **`limiter`:** the in-process limiter, covering every scope whose app or method limits Riot has taught it. Each entry has v1's kind, label, `frozenMs`, windows and methods.
- **`worker`:** `{alive: true, lastSeenMs: 0}`. The workers run in the process that answers, so there is no heartbeat to read.
- **`cache` and `flows`:** read back from the Prometheus exporter's rendering of `proxy_cache_reads_total`, `proxy_backfills_queued_total` and `proxy_refresh_claims_total`. They are cumulative per process, as in v1.
- **`ladder`:** running crawls, the last finished crawl (with `pendingLegs` from `crawl_legs`) and the entry count.
- **`analytics`:**
  - `lastRuns` comes from a new `analytics_runs` table (`V0006`): one row per ladder, written by every recompute, completed or failed (with the steps that finished). v1 kept it in Redis for 30 days.
  - `topChampions` lists the newest run's ladder's five most played on its latest patch, summed over tiers, excluding remakes (ADR-056), with names from the mirror.
- **`process`:** uptime since `serve` started, and RSS from `/proc/self/status` (0 where there is none).
- **`totals.activeConsumers`:** consumers that are not revoked.

**History:**
- A point in v1's `MetricsHistoryPoint` shape is sampled every `METRICS_HISTORY_INTERVAL_S` whether or not anyone watches. The first is taken after one interval.
- `queues.pending` sums waiting and delayed.
- Points are stored in `metrics_history`, keeping the newest 1440 (v1's cap), alongside maintenance's 24 h trim.
- `GET /v1/admin/metrics/history` → `{intervalS, maxPoints: 1440, points}`, oldest first, skipping points that do not parse (v1). It takes no parameters (v1).

**Dashboard:** v1's page, plus v2's `crawl.phase` event:
- it is described in the feed (`platform · queue → phase`);
- it and `ladder.crawl.completed` refresh the crawl panel at once instead of waiting for the 60 s poll.

**Manual acceptance:** screenshots of the overview and ladder tabs are in `docs/img/`. They were taken from the real `serve` binary, with Data Dragon synced live and representative rows seeded.

The OpenAPI compare against v1 now shows **no missing operations**.

## ADR-059 — The acceptance suite against v2 (2026-10-03)
Accepted. Owner decisions at P8-01: a Node mock Riot inside `acceptance/`, mock mode by default with v1's live mode kept as an opt-in, and a required `acceptance` CI job.
- **Mock Riot** (`acceptance/helpers/mock-riot.ts`, plain `node:http`) serves a small fixed world:
  - the player `Acceptance#MOCK` on `oc1` with 60 ranked games;
  - a second player, `Other#MOCK`;
  - a 30-player MASTER ladder sharing those games;
  - spectator state, which tests flip through `POST /__mock/state` (a game that ends adds its match to the player's history, as Riot would);
  - Data Dragon and the queue table.
  - Rate limits follow Riot's model: app windows per host and method windows per (host, method), `X-*-Rate-Limit[-Count]` headers on every answer, and an accountable typed 429 with `Retry-After` on overflow.
  - It forgives 50 ms of transport jitter at a window edge. The limiter keeps no more than `limit` admissions in any rolling window *as it admits them* (ADR-023), but requests arrive a few milliseconds off those instants. Real drift would still be caught.
- **v2 changes the suite needed:**
  - `RIOT_BASE_URL` and `DDRAGON_BASE_URL` point a real `serve` at a mock. They are refused when `ENV=production`. v1 had no such knob; its suite only ever ran against Riot.
  - With a fixed base URL, the Riot client sends `x-riot-host`: the host the request was for. The mock uses it to keep per-host buckets and to see routing.
  - **v1 was right:** `GET /v1/admin/limits/{scope}` now answers `frozenMs: 0` when not frozen. v1's `isFrozen` returned 0; v2 had returned null. Phase 2 caught it.
- **Harness** (`helpers/setup.ts`), in mock mode:
  - starts the mock, then `target/debug/riot-proxy serve` (or `RIOT_PROXY_BIN`) on a scratch `DATA_DIR`;
  - gives `serve` a clean environment: a fake key, a known `BOOTSTRAP_ADMIN_KEY`, fast poll intervals, `CACHE_TTL_OVERRIDES=spectator=5`, a MASTER ladder floor;
  - runs `serve` in that scratch directory, so the repo's `.env` (which holds a real key) is never read;
  - loads `.env` only in live mode.
  - Phases always run in file order, 1 to 7, because they share one server.
- **Live mode** (`ACCEPTANCE_LIVE=1`) is v1's: a real key, `ACCEPTANCE_RIOT_ID`, a running server or one started from `.env`, and the live game and crawl behind `ACCEPTANCE_LIVE_GAME=1` and `ACCEPTANCE_LADDER=1`. Manual only.
- **Test changes from v1** (no test is skipped in mock mode; 29 of 29 run):
  - Redis and BullMQ introspection (phases 5 and 6) becomes `GET /v1/admin/jobs` (ADR-052). BullMQ's custom job-id check becomes ULID ids deduped by PUUID (ADR-048).
  - Phase 6 injected a `game.started` through Redis. v2 has no Redis, so in mock mode the mock puts the player in a game, and delivery and topic isolation are asserted on real events.
  - The live-game check runs in mock mode too: `game.started`, `game.ended`, then `match.archived` for the game's own match. In live mode it is still opt-in, and the two injection-based checks do not run there.
  - Crawl ids are ULIDs, not UUIDs (ADR-054): the regex and the unknown-id DELETE use a well-formed ULID. A UUID would be a 400 for its format before it could be a 404.
  - "Ladder metrics are published": a labelled histogram has no sample before its first observation, in v1's prom-client as in v2's exporter. So v1's check passed only on deployments that had crawled. It now accepts absence when no crawl has finished, and the crawl checks assert the series afterwards.
  - The crawl runs whenever mock mode does (v1: `ACCEPTANCE_LADDER=1`). It is a few seconds' work against the mock's ladder.
- **CI:** an `acceptance` job builds the debug binary, runs `npm ci` and `npm test` in `acceptance/`, and becomes a fifth required check. `just acceptance` runs the same locally.

## ADR-060 — `migrate-v1` (2026-10-03)
Accepted. `riot-proxy migrate-v1 --from <path> [--key-scope <scope>]` (design/08 §Data migration).
- **Input** comes in either of two forms:
  - a `pg_dump -Fc` archive, recognised by its `PGDMP` magic and streamed through `pg_restore --data-only -f -`, which must be on `PATH`;
  - that command's output already written to a file: Postgres COPY text.
  - The parse runs on a blocking thread and hands rows to the async writer over a bounded channel, so memory is one batch, not the dump.
  - `pg_restore` must be at least the version of the `pg_dump` that wrote the archive. v1 ran Postgres 18, so a v1 production dump needs `pg_restore` 18 or later.
- **Matches:**
  - v1's `matches.data` goes through v2's own archive path (`matches::put`): zstd, then the derived columns, facts, bans and remake flag at the current `FACTS_VERSION`. That is design/08's "re-derives facts", and `facts_version` starts clean.
  - `archived_at` is v1's `fetched_at`.
  - `matches.timeline`, which in v1 was a column on `matches` and not its own table, goes to `timelines`.
  - Bodies are JSON-equal to v1's, not byte-equal to Riot's: v1 stored `jsonb`, which had already reordered them.
  - A body v2 cannot archive (for example, no `gameEndTimestamp`) is skipped and named in the report. It does not fail the run.
  - Batches of 64 matches are archived concurrently.
- **Facts key scope:** this deployment's, or `--key-scope`. v1's `match_participants` carried none. The two are equal when v2 runs with v1's Riot key, as the cut-over plans; the run warns when the players' stored scope differs.
- **Players** keep v1's own `key_scope`, platform, names, `tracked` and `last_seen_match_id`. v1's three backfill stamps become `backfill_state` `{startedAt, doneAt, depth}` (ADR-050).
- **Not read:** consumers (counted and reported: "mint new keys", design/08) and every v1 derived table (`match_participants`, `match_bans`, analytics, ladder). The archive regenerates them. Ladder entries come back with the next crawl.
- **Idempotent:** every write is an upsert, so a re-run changes nothing.
- **Fixture** (`tests/fixtures/v1-dump/`), built on Postgres 17.11 from v1's ten migrations:
  - 20 matches (ranked, Arena, a remake, one with a timeline, one malformed body, names with a tab, a backslash, Korean and a newline), 12 players, 2 consumers;
  - `v1.dump` (custom format) and `v1-data.sql` (its `pg_restore --data-only` text).
  - Tests import the text form always, and the dump whenever `pg_restore` exists.
- **Throughput:** 1 837 matches/s in release on the fixture loop (plan: ≥ 1 000), on the development machine. Design/08's "~5k/s" was an estimate.
- **Binary size (found on this PR):** the static musl binary reached 21.1 MB, over the 20 MB the P0 exit check set. The release profile moves from thin to fat LTO; `aws-lc-rs` is the only crypto backend linked, so there was no duplicate to drop.
- **CI's `pg_restore` is older than the fixture's `pg_dump`** (16 vs 17), and refuses the archive. The custom-format test runs only where `pg_restore` ≥ 17 exists, and the CLI's error names the version rule.

## ADR-061 — Built-in TLS (2026-10-03)
Superseded by ADR-067 (built-in TLS removed). Accepted. `serve --tls --domain <d> --acme-email <e>` (or `TLS`, `TLS_DOMAIN`, `ACME_EMAIL`) is design/07 §Option B.
- **Certificate:** `rustls-acme` 0.15 with the aws-lc-rs provider, already linked for reqwest, so there is no second crypto backend. It uses Let's Encrypt's production directory, caches the account and certificate in `$DATA_DIR/acme/`, and renews in-process. The challenge is TLS-ALPN-01, answered on the HTTPS port itself, so port 80 is never needed for issuance. ACME events are logged at info, failures at error.
- **Ports:** HTTPS goes on `TLS_PORT` (default 443). Every request on `TLS_REDIRECT_PORT` (default 80; 0 = no listener) gets a 308 to `https://<host>[:TLS_PORT]<path?query>`. Both bind `HOST`. `TLS_DOMAIN` and `ACME_EMAIL` are required when TLS is on.
- **Plain HTTP stays up on `127.0.0.1:PORT`** in TLS mode. That is where `riot-proxy healthcheck` connects in TLS mode (always IPv4 loopback) and where local Prometheus scrapes. It is never exposed.
- **`/metrics` and `/readyz` are private-only in TLS mode.** Loopback, RFC 1918, link-local, IPv6 ULA and IPv4-mapped forms of these pass. Anyone else gets the allowlist's 403 envelope (`FORBIDDEN`). Behind a reverse proxy (TLS off), nothing changes: the proxy in front decides.
- **One shutdown** drains HTTPS (axum-server's graceful shutdown, `SHUTDOWN_GRACE`), the redirect and the loopback listener together.
- **Tests:**
  - A self-signed `rcgen` certificate is passed through `ServeOptions.tls_pem`, which is not part of the operator-facing config. ACME itself needs a real domain and is the owner's manual check.
  - The integration test covers HTTPS 200, the 308 with host, port and query, and the loopback listener.
  - Unit tests cover the private ranges and the middleware with public and private peers.
- **Binary size (found on this PR):** rustls-acme brings its own HTTP client stack (async-web-client, futures-rustls, async-io), x509-parser, chrono and a second rcgen. These took the static musl binary to 22.2 MB, over the 20 MiB cap. The owner chose `opt-level = "s"` for every dependency, with this crate kept at 3.
  - Measured musl sizes:
    - cold crates only at "s": 22.0 MB;
    - all dependencies at "s": 17.9 MB;
    - everything at "s": 15.2 MB.
  - migrate-v1's archive benchmark drops from about 1 850 to about 1 730 matches/s (3 runs each), against a 1 000/s target.
  - `panic = "abort"` was not considered: it would defeat the catch-panic layer.

## ADR-062 — Postgres feature flag, compile-only (2026-10-04)
Accepted. Design/04 §Postgres compatibility asks for a `Store` trait with `SqliteStore` and `PgStore`. Plan P8-04 asks for the seam with no behaviour.
- **`src/db/store.rs`:** `trait Store { engine, migrate, claim_job }`.
  - The trait covers only the engine-specific operations; every other query is shared SQL through `Db`.
  - Design/06 names the job claim as the one engine-specific statement, and it differs only by Postgres's `FOR UPDATE SKIP LOCKED`.
  - `claim_sql(engine)` builds both forms from one text.
  - `$1` binds the same value at each use on both engines; SQLite treats it as one named parameter.
- **`SqliteStore`** wraps `Db`. `Scheduler::claim` now goes through it. The statement is unchanged apart from `?1` → `$1`, so behaviour is identical and the existing claim tests still cover it.
- **`PgStore`** exists only with `--features postgres`.
  - It adds no dependency.
  - It holds the URL as a `Secret`.
  - Every call returns `DbError::PostgresUnimplemented`.
- **Config is unchanged:** `DATABASE_URL=postgres://…` is still refused in every build, so the feature switches nothing on.
- **CI:** the clippy job also runs `cargo check --all-targets --features postgres` and the store's unit tests with the feature on. The job name and the required checks are unchanged.
- **Not done:** a real Postgres driver, Postgres migrations (the SQLite DDL uses SQLite types and partial indexes), and the `ROLE=api|worker` split. These belong to whoever implements `PgStore`.


## ADR-063 — Release pipeline (2026-10-04)
Accepted. `.github/workflows/release.yml` (plan P8-05, design/07).
- **Trigger:** a `v*` tag. The tag must equal `v` + Cargo.toml's `version`, and each binary's `--version` must print it. The crate moves to `2.0.0-rc.0` for the dry-run, so `--version` and the OpenAPI `info.version` say what the tag says.
- **Binaries** (each built natively, no cross-compilers):
  - `riot-proxy-linux-amd64`: `x86_64-unknown-linux-musl` on `ubuntu-latest`;
  - `riot-proxy-linux-arm64`: `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`;
  - `riot-proxy-darwin-arm64`: `aarch64-apple-darwin` on `macos-15`.
  - Both Linux binaries must be static and under 20 MB, as in CI.
- **Image:** `Dockerfile.release` puts the two Linux binaries into scratch images for `linux/amd64` and `linux/arm64`. It is the same layout as `Dockerfile` but copies the release's own binaries, so the image runs exactly what the release page offers and no arm64 build runs under emulation.
  - Pushed to `ghcr.io/ninjagoldfinch/riot-proxy` as `:<version>` and `:<major>`, with no `:latest`.
  - A pre-release tag (`v2.0.0-rc.0`) pushes only `:2.0.0-rc.0`, so `:2` never points at an rc.
  - The amd64 image is run and must pass its own healthcheck.
- **Release:**
  - `gh release create` with the three binaries and `sha256sums`.
  - The notes carry the image tag and digest.
  - A version with a `-` suffix is marked as a pre-release.
- **Pull requests** that touch the workflow or `Dockerfile.release` run every build and the image check, but neither log in to GHCR nor publish.
- **README.md** is written for v2: install, first run, a configuration table (`.env.example` remains the complete list), and operations (HTTPS, systemd, health and metrics, backups, migrating from v1).

## ADR-064 — Cut-over runbook (2026-10-04)
Accepted. `docs/CUTOVER.md` (plan P8-06) follows design/08 §Cut-over: deploy beside v1, import, move consumers one at a time, watch v1 reach zero, switch it off and keep its data for 14 days. Where the runbook departs from design/08's sketch:
- **The dump command.** v1 has no `timelines` table; timelines are a column on `matches` (ADR-060). The runbook dumps `-t matches -t players`.
  - It uses **plain SQL** (`pg_dump --data-only`), not `-Fc`. That avoids needing a `pg_restore` 18 on the v2 host.
  - Verified: a plain data-only dump of the P8-02 fixture imports exactly as the `pg_restore` text does (19 matches, 1 timeline, 12 players, one skip).
  - v1's nightly custom-format backups also work, given `pg_restore` 18 or later.
- **Import before the first boot**, so tracked players are polled from the start. Then **re-import once v1 is at zero** to pick up matches v1 archived during the overlap; upserts make this a no-op for everything already imported.
- **No ladder crawls during the overlap** (`LADDER_CRAWL_S=0`, no manual crawls). Both proxies spend one key's budget through separate limiters, so the runbook keeps bulk work out of the overlap. Interactive traffic only moves from one to the other.
- **Finding stragglers.** v1 does not log requests per consumer (request logging is off) and has no last-used column. The runbook therefore disables v1 keys one at a time through v1's `DELETE /v1/admin/consumers/:id`. v1 has no re-enable route, so the runbook gives the SQL that undoes a disable.

## ADR-065 — No default platform (2026-10-04)
Accepted (owner, task RC-01). v2 drops v1's `DEFAULT_PLATFORM`. The owner no longer uses v1, so v1 parity does not bind this change. A request that needs a platform must now name one. The proxy never picks one on the caller's behalf.
- **Removed:** the `DEFAULT_PLATFORM` variable, `Config::default_platform`, and the dev UI's `defaultPlatform`. A leftover `DEFAULT_PLATFORM` in a v1 `.env` is ignored, the same as `REDIS_URL`.
- **Required, `400 VALIDATION` when missing:**
  - `?platform=` on `/v1/players/{puuid}/profile`, `/v1/players/by-riot-id/{gameName}/{tagLine}/profile` and `/v1/players/{puuid}/matches`. The error reads "querystring must have required property 'platform'".
  - `platform` in the bodies of `POST /v1/admin/ladder/crawl` and `POST /v1/admin/analytics/recompute`. The error reads "body must have required property 'platform'".
- **A filter, not a default:**
  - `/v1/lol/analytics/champions*` read every platform's aggregates when `?platform=` is absent, and answer `"platform": null`. Every stored count adds up across platforms (match ids are platform-prefixed, so `matches_picked` and the slices do not double-count).
  - `/v1/players/{puuid}/champions` already treated `platform` as an optional filter and is unchanged.
- **Scheduled crawls:** an empty `LADDER_PLATFORMS` now schedules no crawl, instead of crawling `DEFAULT_PLATFORM`. `/v1/admin/ladder/options` reports the first of `LADDER_PLATFORMS` (or `null`) as `defaults.platform`. That value only preselects the dashboard's form, which always sends the platform it shows.
- **Not covered here:** account-v1 region selection (RC-02).


## ADR-066 — account-v1 picks its cluster (2026-10-04)
Accepted (owner, task RC-02). Riot serves every account from each account-v1 cluster: americas, asia and europe. The developer portal says so, and v1 already sent `sea` to asia on that basis. Each cluster has its own rate limits, so the proxy may choose a cluster instead of making the caller choose.
- **Routes** (owner decision):
  - New `GET /v1/riot/accounts/by-riot-id/{gameName}/{tagLine}` and `GET /v1/riot/accounts/by-puuid/{puuid}` pick the cluster.
  - The `/{region}/` routes stay as an explicit pin and never move: a pinned lookup on a frozen cluster is `RATE_LIMITED`, as before.
  - The two path shapes have different segment counts, so they do not collide.
- **Internal lookups pick too** (owner decision): the player profile's account part, `profile/by-riot-id`, and the admin track-by-Riot-ID lookup. The admin raw-debug route names its own host and stays pinned.
- **Order:** asia, then americas, then europe (`Region::ACCOUNT_PICK`, owner).
- **Failover** (owner decision: limiter-aware plus on 429). The fetcher's upstream leg:
  1. Takes a token from the first cluster with room now. A zero-budget `acquire` is a non-blocking try that also says when the cluster frees.
  2. If none has room, waits for whichever frees first, inside the usual wait budget. Past the budget it answers `RATE_LIMITED`, with `Retry-After` from the soonest cluster.
  3. On a typed 429, `observe` freezes that cluster. The next attempt picks another without waiting.
  4. On a service 429 (no `X-Rate-Limit-Type`), it moves to the next cluster at once. ADR-021's 500 ms × 2ⁿ backoff applies only once every cluster has answered one. The cluster that answered goes last for the rest of the request.
  - Bulk callers pick the same way, under the same ceiling and waiter rules.
- **One cache entry per account:** account-v1 cache keys use `account` in place of the host (`{scope}:account.byPuuid:account:{puuid}`), so a picked lookup and a pinned one, on any region, share the entry and its single-flight.
  - Existing account entries keyed by host are orphaned once: they are refetched on next use, and the L2 sweep drops them when they expire.
  - Other regional methods (match-v5) keep their host in the key.
- **Not changed:** the `/{region}/` routes' validation and `sea` handling; `X-Cache`, error codes and metric names.

## ADR-067 — Built-in TLS removed (2026-10-05)
Accepted (owner, task RC-03). Supersedes ADR-061 and design/07 §Option B. The proxy is never the public edge. It only takes plain HTTP from the owner's own services, so ACME, the 80→443 redirect and the private-only `/metrics` and `/readyz` were code nobody would run. The owner's real-domain check of P8-03 is dropped with it.
- **Removed:** `src/tls.rs`; the `--tls`, `--domain` and `--acme-email` flags; `TLS_DOMAIN`, `ACME_EMAIL`, `TLS_PORT` and `TLS_REDIRECT_PORT`; the `rustls-acme` and `axum-server` dependencies, the direct `rustls` one (reqwest still links it) and `rcgen`. `serve` binds `HOST:PORT` only, and `healthcheck` always follows `HOST`.
- **`TLS=true` refuses to boot** with `TLS: built-in TLS was removed; terminate HTTPS in a reverse proxy`, so an old `.env` cannot quietly come up as plain HTTP. `TLS=false` and the other old variables are ignored.
- **HTTPS, where needed, is a reverse proxy's job** (Caddy or nginx in front of `PORT`, design/07 §Option A).
- **Binary size:** the static musl binary drops from 17.9 to 16.6 MB. Building dependencies at opt-level 3 again would give 20.87 MB: under the 20 MiB cap by about 100 KB, inside the ~70 KB local-to-CI drift. ADR-061's `opt-level = "s"` for dependencies therefore stays.
- **Still open:** ADR-033 trusts the leftmost `X-Forwarded-For` for the admin allowlist. With no proxy in front, a caller can set it. That is unchanged here. (Resolved by ADR-068.)

## ADR-068 — `TRUST_PROXY`, off by default (2026-10-05)
Accepted (owner, task RC-04). Amends ADR-033's client-IP rule. The proxy has no reverse proxy in front (ADR-067), so a caller could put an allowlisted address in `X-Forwarded-For` and pass `ADMIN_IP_ALLOWLIST`. An admin key was still needed, but the allowlist was no defence on its own.
- **`TRUST_PROXY`** (bool, default `false`; the name is v1's Fastify option). Off: the client address is the TCP peer, and `X-Forwarded-For` is ignored. On: the leftmost `X-Forwarded-For` entry wins when it parses, otherwise the peer, exactly as before.
- **Default off** is a break from v1, which always trusted the header (owner: v1 parity is dropped). An operator who puts Caddy or nginx in front must set `TRUST_PROXY=true`. Otherwise every request appears to come from the proxy's address, and an allowlist that includes it would admit everyone.
- **Applies to** every use of the client address: the admin routes' allowlist and the `/v1/ws` admin-topic decision. Nothing else reads it.
- **Tests:** unit (`client_ip` with the setting on and off), integration (`tests/auth.rs`: a spoofed header is refused by default; through a trusted proxy, an outsider is refused and an allowlisted client is admitted), config default.

## ADR-069 — `:edge` image on every push to `main` (2026-10-05)
Accepted (owner, task OPS-01). The owner wants a dev VM on Proxmox (OPS-02) that follows `main` without building Rust on the VM.
- **`.github/workflows/edge.yml`** builds the static amd64 binary on every push to `main` and pushes `ghcr.io/ninjagoldfinch/riot-proxy:edge` and `:sha-<short>` from `Dockerfile.release`, after the image passes its own healthcheck. The OCI `revision` label carries the commit.
- **amd64 only.** The dev VM is amd64; one target keeps the run short. Releases stay multi-arch (ADR-063).
- **Not a release.** `:edge` never moves `:2` or a version tag, and nothing here creates a GitHub release. A pull request that touches the workflow builds and smoke-tests without pushing.
- **Not a required check.** Required checks stay as ADR-006 lists them; an `edge` failure does not block merges, it just leaves `:edge` on the last good commit.

## ADR-070 — Proxmox dev VM that follows `:edge` (2026-10-05)
Accepted (owner, task OPS-02). The owner wants a dev environment on their Proxmox host that updates itself when `main` moves.
- **`deploy/proxmox/create-vm.sh`**, run as root on the host, makes a Debian 13 cloud-image VM with `qm` (Proxmox VE 8+, `import-from`). Proxmox's own cloud-init sets the user, SSH keys and address; a vendor-data snippet (`cicustom vendor=`) installs Debian's `docker.io` + `docker-compose` (Compose v2), the stack in `/opt/riot-proxy` and the update timer. Using vendor-data, not user-data, keeps the Proxmox UI's cloud-init settings working.
- **Updates by polling, not webhooks.** `riot-proxy-update.timer` runs `docker compose pull && up -d` every 2 minutes against `:edge` (ADR-069). The VM needs no inbound access from GitHub and no Rust toolchain. `RIOT_PROXY_TAG` in `.env` pins a `sha-<short>` or release tag.
- **The Riot key never passes through the host.** The VM boots with an empty `RIOT_API_KEY`, and the stack stays down until the owner sets it in the VM's `.env` (mode 600).
- **Plain HTTP on the LAN, `ENV=development`.** As ADR-067: put Caddy in front if it ever needs HTTPS.
- **Tests:** `deploy/proxmox/test.sh` in a new CI job `ops` (not a required check): shellcheck, `cloud-init schema`, the embedded files byte-for-byte, the dry-run `qm` commands, and `riot-proxy-update` against a fake `docker`.

## ADR-071 — Dev explorer replaces v1's dev UI; never in production (2026-10-05)
Accepted (owner, task DEV-01). Supersedes the `DEV_UI` bullet of ADR-008 and the `/dev` part of the P4-06 decision. Design: design/10.
- **The page.** `src/ui/dev-ui.html` is rewritten as a dev explorer. It replaces v1's player viewer, which covered five reads and none of passthrough, analytics, admin, the WebSocket or health. It is one file with no external URLs and no build step. It has five hash-routed tabs:
  - **Explorer**: forms built from the OpenAPI document, plus a raw-request mode.
  - **Status**: `/healthz`, `/readyz` and `/v1/admin/metrics`, with per-method limiter windows.
  - **Player**: profile and recent matches rendered.
  - **Live**: `/v1/ws` topics and a frame log.
  - **History**: the last 50 requests in `localStorage`.
  Every view shares one response viewer: status, time, size, the proxy's headers, all headers, a Pretty or Raw body, the error envelope, and copy-as-curl with a `$RIOT_PROXY_KEY` placeholder.
- **The spec is the inventory.** The explorer reads `/dev/openapi.json`, the same finished document as `/openapi.json`. It is served even when `DOCS_UI=false`, so new routes appear in the explorer without UI work.
- **Never in production.** `dev_ui = ENV != production && DEV_UI (default true)`. An explicit `DEV_UI=true` no longer wins in production, because the explorer can send admin `POST`/`DELETE` calls. When that setting is ignored, `serve` logs a warning. `DEV_UI=false` still turns the page off elsewhere.
- **Routes:**
  - `/dev`, `/dev/config.json` and `/dev/openapi.json`. All are public, `no-store`, and left out of the OpenAPI document, as before.
  - `config.json` adds `version`, `env`, `docsUi` and `dashboardUi`.
  - **`/dev/{*rest}` is removed.** The page has no client-side paths, so `/dev/Name-TAG` is now a 404.
- **Safety in the page:**
  - A non-GET request asks for confirmation.
  - The key lives in `localStorage` (`rp.dev.key`) and is sent only as a Bearer header. The exception is the WebSocket handshake, which requires `?token=`.
- **Tests:**
  - config: never on in production, the ignored flag is reported, the default is on elsewhere.
  - integration (`tests/ui.rs`):
    - 404 for all three routes in production with and without `DEV_UI=true`, and with `DEV_UI=false`.
    - `/dev/openapi.json` equals `spec()` with `DOCS_UI=false`.
    - The `config.json` fields.
    - No catch-all.
    - The page is self-contained (no `http(s)://`, `<script src` or `<link>`).

## ADR-072 — Per-VM login keys for the Proxmox dev VM (2026-10-08)
Accepted (owner, task OPS-03). Amends ADR-070.
- **`create-vm.sh --generate-key`** runs `ssh-keygen -t ed25519 -N ''` on the host and writes `<key-dir>/<name>-<vmid>` (default `/root/.ssh/riot-proxy`, mode 700). The comment is `<user>@<name>-<vmid>`. Cloud-init gets only that public key, or that key plus `--ssh-keys` when both are given.
- **No passphrase.** The script runs unattended. The private key is root-only on the host, and the script prints how to `scp` it off the host and delete it.
- **Never overwritten.** An existing key file stops the run, so a rebuilt VM can't silently reuse an old key.
- **The key is made after the image download** and just before `qm create`, so an interrupted download leaves no key to block a retry. The existing-key check still runs before the download. Each step prints a `create-vm:` line on stderr, and the download shows a progress bar when stderr is a terminal. The first real run looked hung during a silent ~400 MB download (fix in #83).
- **`--name` must be a DNS name**, because it is now part of a file path.
- **Host keys are unchanged.** The Debian cloud image already generates unique host keys on first boot.
- **Tests:** the dry-run `ssh-keygen` and `--sshkeys` arguments, `--key-dir`/`--name`, the combined case, and two real keys that differ.

## ADR-073 — Dev explorer player tab: paging, filters, scoreboard, backfill status (2026-10-08)
Accepted (owner, task DEV-02). Extends ADR-071; design/10 §Player tab.
- **Page sizes 10, 25 and 50 are done in the page, not the API.** `/v1/players/{puuid}/matches` keeps its `count` cap of 20, because every id is its own upstream call. A page of 25 or 50 is 2–3 `start`/`count` calls joined in order. Only the first call carries `refresh`, which is allowed once a minute per player.
- **Queue labels and the queue filter use Riot's `queues.json`** through `/v1/static/queues`. The page holds no queue list of its own. Deprecated queues (their `notes` say so) are not offered as filters, but still label old matches.
- **Backfill status is read from what exists**: the player row (`/v1/admin/tracked-players`) and the job queue (`/v1/admin/jobs`, by `payload.puuid`). No new endpoint. Job lists are capped at 500, so counts at the cap show `500+`.
- **Testing a no-build page**: the pure helpers sit in one marked block that `tests/dev_ui.mjs` evaluates under `node --test`, which `cargo test` runs (skipped locally without node, required in CI). DOM wiring was checked with a throwaway jsdom smoke run. jsdom is not a project dependency.

## ADR-074 — Per-player archive endpoints; the dev page reads exact counts (2026-10-08)
Accepted (owner, task DEV-03). Supersedes ADR-073's "no new endpoint" and its capped job counts. The owner wants all backfilled data shown, not counts at a cap of 500.
- **`GET /v1/admin/players/{puuid}/archive`** returns, for the current `keyScope`:
  - the player's row, or null;
  - exact archive totals from `match_facts` joined to `matches`: matches, remakes, wins, timelines, oldest and newest game end, and a count per queue;
  - this player's `archive:match` and `backfill:player` jobs counted by state;
  - the newest job of each kind.

  Nothing is capped. Jobs are matched on `json_extract(payload, '$.puuid')`. That scans the job queue, which keeps `done` rows for seven days, so it is an admin read only: no expression index is added for it.
- **`GET /v1/admin/players/{puuid}/archive/matches`** returns the player's line in each archived match: `start` 0–1 000 000, `count` 1–100 (default 25), optional `queue`. Newest first, with `total` for the filter. It is served from `match_facts` and makes no Riot call. It is admin-scoped like the rest of `/v1/admin`, because it reads the whole archive for a PUUID.
- **The dev page** uses both: exact numbers in the History & backfill card, and an "Archive (all stored)" source for the match list.
- **Page tests are committed.** `tests/dom/` is a private npm package whose only dependency is `jsdom`. It drives `src/ui/dev-ui.html` against a fake API in the `test` CI job (`just ui-test`). The page itself still has no build step and no dependency.


## ADR-075 — Scoreboard layout in `/dev` (2026-10-08)
Accepted (owner, task DEV-04). Owner feedback on a DEV-03 screenshot: the match meta and buttons sat in the second team's header, so the two team headers didn't line up; the tables had different column widths; numbers were left-aligned; and the build string `16.20.824.8524` meant nothing at a glance.
- **Match meta moves to its own row** above both teams: patch, length, `X-Cache`, `raw`, ×. The patch is `info.gameVersion` cut to major.minor. The full string is kept on hover, because the build numbers are still useful for telling apart builds of the same patch.
- **Team blocks**: result, side, and team K/D/A and gold in the header. The side comes from match-v5 `teamId` (100 Blue, 200 Red). The tables use `table-layout: fixed` with one shared `colgroup`, so both teams line up. Numeric columns are right-aligned with tabular figures.
- Display only: no API, header or metric changes.

## ADR-076 — Data Dragon images are mirrored on first request (2026-10-08)
Accepted (owner, task DEV-05). Amends v1's §1 non-goal, "images stay on Riot's CDN" (design/07). The owner wants `/dev/showcase` to show champion, profile, item and summoner-spell icons without the page loading anything from another origin.
- **Filled on demand, not by `ddragon:sync`.** A patch has thousands of profile icons. Downloading all of them every patch would cost far more than the few dozen a page actually shows. `GET /ddragon/{version}/img/{kind}/{file}` serves from disk. On a miss it fetches Data Dragon's `/cdn/{version}/img/{kind}/{file}` once, writes it atomically beside the patch's JSON, and serves that copy from then on. Concurrent misses for one path share a single fetch.
- **Bounded, because it needs no key** (like the rest of `/ddragon/*`):
  - the patch must be mirrored;
  - `kind` must be one of `champion`, `profileicon`, `item`, `spell`;
  - the file must be an `image.full` that the patch's own data file lists (`champion`, `profileicon`, `item`, `summoner`).

  Anything else gets a 404 without a fetch. So an anonymous caller can make the proxy download only images Riot itself names, once each.
- **Checked before it is kept.** The body must start with the PNG signature and be at most 1 MiB. Otherwise the request is a 502 and nothing is written. A Riot 404 is passed on as a 404 and is not cached.
- **Not in scope:** rank emblems, which are not part of Data Dragon, and rune icons, which have nested paths. Pages show tiers as CSS badges.
- Like the JSON files, no request goes through the Riot limiter. Data Dragon is not rate limited (v1 §5.6).

## ADR-077 — Dev reset: wipe fetched data from `/dev`, keep keys and limits (2026-10-08)
Accepted (owner, task DEV-09). Extends ADR-071; design/10 §Reset tab.
- **Owner request:** "a reset option that fully deletes all the fetched data … only accessible from the dev endpoint", in its own tab.
- **Where:** `GET`/`POST /dev/reset`, merged into the app only when `dev_ui` is on, so it shares the explorer's gating: never in production, and absent with `DEV_UI=false`. It is not a `/v1/admin/*` route: those are in the OpenAPI document and would exist in production. It still needs an admin key (`require_admin`). The `POST` needs `{"confirm":"reset"}`, so a stray or replayed request without the body does nothing.
- **Scope (owner choice):** every fetched table, the L2 table and L1, all job rows and `metrics_history`. Kept: `consumers`, so the key that pressed the button still works (v1's `reset:db --keep-consumers`); `limiter_state`, the live rate-limit checkpoint for the key (v1 kept it in Redis, which `reset:db` never touched); refinery history; Data Dragon files.
- **An explicit list, not "every table but…"**. v1 truncated whatever `pg_tables` listed. Here `WIPED`/`KEPT` must between them cover the migrated schema, and a test enforces that, so a new operational table cannot be wiped by accident and a new data table cannot be missed.
- **Rows are deleted, not the file.** This needs no restart and keeps the writer thread and the reader pool. Freed pages stay in the file until SQLite reuses them; there is no `VACUUM`, because on a large archive that holds the writer for a long time.
- **Races accepted:** jobs that are mid-run can still write when they finish, and an L2 batch queued in the last 2 s can still land (`FLUSH_EVERY`). Both are reported (`runningJobs`) or show up on the next count refresh. Pausing the scheduler for a dev tool was not worth the coupling.

## ADR-078 — One page bar across `/dashboard` and `/dev`, rendered from config (2026-10-08)
Accepted (owner, task DEV-10). Extends ADR-071; design/10 §Page bar.
- **Owner request:** "a taskbar at the top that makes it easy to swap between the dashboard, dev and other pages".
- **Rendered on the server, once at startup**, into a `<!-- pagebar -->` marker in each embedded page. The alternative was JS in each page reading its `config.json`. That would duplicate the markup and the link logic in two files, and the dashboard's config would have to learn which other pages exist. Rendering it in Rust keeps one list (`routes::ui::pages`), and the links come from the same flags that mount the routes, so the bar cannot link to a 404. In production that means no `/dev` link.
- Links: Dashboard, Dev explorer, API docs (when `DOCS_UI`) and Metrics (`/metrics` is always mounted). `/docs` is Scalar's page and is left untouched.
- `/dev` and `/dashboard` responses are therefore no longer byte-identical to the embedded files: the marker is replaced. The tests compare against `render(…)`.
- **The planned `/dev/showcase` (DEV-06)** joins the bar by adding one entry to `pages()` behind the same `dev_ui` flag. That covers DEV-06's "links from `/dev` and the dashboard".

## ADR-079 — Faster CI: `build-musl` packages its own binary, the source `Dockerfile` builds in its own cached job (2026-10-08)
Accepted (owner request "improve build times on GitHub Actions", task OPS-04). Amends ADR-006 and the P0-07 docker steps.
- **Measured before** (runs 37733657692, 37734217148): `build-musl` took about 14.5 min and was the critical path. Of that, `docker build` took 10–10.5 min: it recompiled every dependency inside `rust:alpine` with fat LTO and no layer cache, straight after the same job had built the same static binary. The other jobs took 1–3.5 min.
- **`build-musl` now packages the binary it built** into `Dockerfile.release`'s scratch image. That is how `edge.yml` and `release.yml` already ship it, so the image under test is now the shipped one. The smoke test is unchanged: `--version`, healthcheck, `/healthz`, `/readyz`, the bootstrap banner and a clean SIGTERM exit.
- **A new `docker` job covers the source `Dockerfile`** that `docker compose up --build` uses. It runs in parallel with `build-musl`, builds with BuildKit layers cached in GitHub Actions (`type=gha`, scope `dockerfile`), and runs the compose `/healthz`, `/docs` and `/openapi.json` checks against that image (`up --no-build`). The dependency layer is reused until `Cargo.toml` or `Cargo.lock` change.
- **Caches are written from `main` only.** The repo was at 10.5 GB of Actions cache, over GitHub's 10 GB cap: every PR saved its own 0.3–0.8 GB Rust cache, which evicted `main`'s. Now `Swatinem/rust-cache` has `save-if: main`, the docker job writes `cache-to` only on `main`, and pull requests restore `main`'s caches.
- **Not changed:** the release profile (fat LTO, `codegen-units = 1`, deps at `opt-level = "s"`). It keeps the binary under the 20 MB cap (ADR-061), and it is what makes the final link slow.
- **Required checks:** `docker` is a new job id. Until the owner adds it to branch protection, a broken source `Dockerfile` does not block a merge. A broken shipped image still does, through `build-musl`.

## ADR-080 — `/dev/showcase`: an example frontend that must cover every read route (2026-10-08)
Accepted (owner, task DEV-06). Design/11. Uses ADR-076's local images and ADR-078's page bar.
- **Owner request:** "a visually pretty but still basic main page to show ladder information, player data and other useful stats … should always be changed when stuff is added to the proxy … only accessible when in dev mode and easily accessible from the dashboard and dev endpoint."
- **Its own page, not a tab in `/dev`** (owner's choice). The explorer is a debugging tool. The showcase is styled as a product, and keeping it a separate file keeps both pages simple. It is gated exactly like `/dev` (`dev_ui`, never in production) and joins the page bar behind the same flag, which is how the dashboard and `/dev` link to it.
- **Read routes only,** because that is what a consumer frontend's key allows. The page uses the any-cluster account route (ADR-066) and leaves the pinned `/{region}/` variants out.
- **"Always changed when stuff is added" is enforced by a test, not a habit.** The page lists every `GET` operation tagged `players`, `riot`, `lol` or `static` as either shown or left out with a reason, in a JSON block that `tests/ui.rs` checks against the OpenAPI document. A new read route fails CI until the page uses it or says why not. A JSON block rather than JS constants, so Rust parses it without evaluating the page. The tag list is the test's, so a new tag needs a deliberate edit there.
- **Analytics rows are per (champion, tier)**, so the home view sums games and wins per champion for its top-10 lists. It does not sum `pickRate`, which is a ratio per tier and does not add up.
- **Ladder names** are fetched for the 25 visible rows only, five at a time, and kept for the session: a whole Challenger league is 300 account calls, which a page should not spend on load.
- Work is split as planned: DEV-06 the shell, gating, coverage guard and home view; DEV-07 the player view; DEV-08 match detail and the champion view. Until then those routes are in `notShowcased` as `planned: DEV-0n`.

## ADR-081 — A lookup backfill walks the whole history by default (2026-10-08)
Accepted (owner, task DEV-11). Changes P6-06's limit; v1 parity is no longer required.
- **Owner request:** the first lookup of a profile should "grab all available match page data, instead of grabbing a certain amount of games", with the upper limit "just the integer limit, as it shouldn't stop".
- `LOOKUP_BACKFILL_LIMIT` now defaults to, and is capped at, `u32::MAX` (v1: default and maximum 10000). The walk already stops on an empty or short page from `match.idsByPuuid`, so in practice it ends where the player's history does; the limit only remains as an opt-in cap. `0` still disables lookup and tracking backfills.
- The walk's completion rule is unchanged (ran out of unfiltered history, or at least `LOOKUP_BACKFILL_LIMIT` deep). With the new default a walk only stamps `doneAt` by running out, so a player with more than 10000 ids is no longer marked complete at 10000.
- `.env.example` used to call 10000 "match-v5's ceiling". Nothing in `docs/design/` or v1 sources that, so the claim is dropped rather than repeated.
- `/dev` shows the uncapped walk as "full history" instead of the number.

## ADR-082 — Showcase player view: one profile call, the ranked-entries route left out (2026-10-09)
Accepted (task DEV-07). Design/11 §Page map. Builds on ADR-080.
- **The profile composite is the entry point.** Search and ladder rows go to `#/player/{gameName}/{tagLine}`, which calls `/v1/players/by-riot-id/…/profile` once; its PUUID drives every other card. The rank cards read the composite's `league` part, so `/v1/lol/league/entries/by-puuid/…` goes in `notShowcased` with that reason, as the summoner route already does.
- **Mastery is called separately** because the composite carries at most the top 20 (`topMastery`). The header uses `topMastery=3`; the mastery card shows the whole collection and its point total.
- **One queue filter (All, Solo/Duo 420, Flex 440)** drives the match history and the champion pool together, so both answer the same question. The ids are Riot's `queues.json` values, kept beside the league queue types in `QUEUES`.
- **Refresh is a button**, disabled for `refreshAvailableIn` seconds: it re-reads the profile and the match page with `refresh=true`, showing the metered refresh (P6) as a consumer would use it.
- **No runes.** The image mirror does not hold rune icons (ADR-076), and a name without an icon adds little, so match cards show champion, spells and items only.
- `gameDuration` is read as Riot documents match-v5: seconds, or milliseconds on matches without `gameEndTimestamp`. A live game's time comes from `gameStartTime`, or spectator-v5's `gameLength` while `gameStartTime` is still 0.

## ADR-083 — Showcase match detail and champion view (2026-10-09)
Accepted (task DEV-08). Design/11 §Page map. Completes the split in ADR-080.
- **Match detail is its own view, `#/match/{region}/{matchId}`,** opened from a player's match card. The region comes from the match page's `region` field, so the page never maps a platform to a region itself. Riot ID links on the scoreboard lead back to player views.
- **The gold graph is one diverging series** (blue minus red), not two team lines. The question is who is ahead and by how much, and a single line around 0 answers that. Blue `#3987e5` and red `#e05a5a` are the sides' colours, kept apart from the win/loss green and red. They pass the palette validator against the dark card surface. The graph has direct labels, a crosshair readout (pointer or arrow keys), and a table view.
- **Sources:** match-v5 `MatchDto` fields come from the replay fixture (`tests/fixtures/replay/cold-lookup/06-match.byId.body`), which the DOM tests use as is. Timeline fields (`info.frames[].timestamp`, `participantFrames[n].participantId`/`totalGold`) come from Riot's `TimelineDto`/`ParticipantFrameDto` on the developer portal. No local fixture carries frames. Sides are taken from the match's own `participantId` → `teamId`, not assumed as 1–5 and 6–10. Arena (`playerSubteamId`) is grouped by subteam and gets no graph.
- **The champion view uses both analytics routes:** the detail for rates by tier and builds, the matchups route for every lane matchup with lane tabs, because the detail carries only the top few (as with mastery, ADR-082). With no platform picked it sums every platform, which is what the route does without `platform`.
- **Rates are not summed across tiers;** games and wins are (ADR-080). Item and rune names come from `item.json` and `runesReforged.json`, fetched only by the champion view. Runes are names, not icons (ADR-082).

## ADR-084 — The showcase's layout is tested in headless Chromium (2026-10-09)
Accepted (owner request, task DEV-12). Design/11 §Principles.
- **Owner request:** "install chromium or another web UI framework and do testing on all the showcase elements, and fix a lot of the rendering and layout issues. Icon size is the big one."
- **Cause of the icon bug:** `img.icon { width: 100% }` came after `.icon { width: 48px }` and outranked it, so a loaded image filled its parent (122 px in match cards, 1106 px in the champion header). jsdom has no layout, and the earlier screenshots used images that 404'd and fell back to a fixed-size box, so neither showed it. The class now sets the size and the image only fills the box.
- **`playwright-core`, pinned (1.64.0), driven from `node:test`** like the jsdom suites, rather than `@playwright/test`: one runner, one fake API (`tests/dom/fake-api.mjs`, now shared), and no second config. Pinning fixes the browser revision. CI installs the headless shell with its system packages; locally `just ui-test` fetches it. The suite skips without a browser, except under `CI`, where it fails.
- **Real images at Data Dragon's sizes** (champion 120, item and spell 64, profile icon 128), generated in the test as PNGs. A layout bug that needs a loaded image can't hide behind a 404 again.
- **What is checked is geometry, not pixels:** icon sizes, overflow, line counts, overlap, cell display, tooltip bounds, at three widths. A screenshot diff would break on every font and patch change; these checks only break when the layout does. Screenshots are still uploaded as a CI artifact for a human look.

## ADR-085 — Fetched timelines are always archived (2026-10-09)
Accepted (owner request, task DEV-15). Amends ADR-040; design/04 §Schema.
- **Owner request:** "timeline data isn't being cached … it should be cached once it's fetched (it's immutable, like other match endpoints) so can be archived forever." With `ARCHIVE_TIMELINES` off (the default) a timeline was neither archived nor cached: immutable endpoints have no L1 TTL, so every request went to Riot (`X-Cache: MISS`).
- **Every timeline fetched from Riot is archived**, through the API or a job, whatever `ARCHIVE_TIMELINES` says. The flag now only decides whether archive jobs (`archive:match`, poll, ladder crawls) also fetch timelines, which is how v1 used it. Timelines are still not fetched automatically.
- **Unchanged:** a timeline is stored only once its match is archived (the `timelines → matches` foreign key, ADR-040). One fetched before its match is served but not stored, and is stored on the next fetch after the match is archived.

## ADR-086 — Rune icons are mirrored; the showcase shows runes and puts the level on the portrait (2026-10-09)
Accepted (owner request, task DEV-14). Design/07 §Data Dragon, design/11.
- **Owner request** (with a screenshot of the scoreboard): "Doubled up level and no runes are present. Fix this in the showcase and anywhere else this might occur."
- **Runes were missing because their icons were never mirrored.** ADR-076 served four kinds (`champion`, `profileicon`, `item`, `spell`), each listed by a data file's `image.full`. Rune icons are not listed that way. runesReforged.json gives each style and rune an `icon` path (`perk-images/Styles/Domination/Electrocute/Electrocute.png`), and Data Dragon serves it unversioned at `/cdn/img/<icon>`.
- **Served at `/ddragon/{version}/img/perk-images/…` and kept per patch.** Keeping it per patch matches every other image, and the existing `ServeDir` serves it from disk with no new code. Riot's copy is unversioned, so a rune that is redrawn shows the new icon on old patches until their mirrored copies are pruned. That is acceptable for icons.
- **Same guards as ADR-076.** Only a path the mirrored patch's runesReforged.json lists is fetched. Every segment must be a plain name (`[A-Za-z0-9_.-]`, not starting with `.`), and the first must be `perk-images`. A miss is fetched once; Riot's 404s and non-PNGs are not kept.
- **What the showcase shows:** keystone over secondary style, beside the spells, in the scoreboard and on match cards, as in the client. The scoreboard reads match-v5 `perks.styles` by `description` (`primaryStyle` / `subStyle`). Match cards read the proxy's `perks` summary (`keystone`, `subStyle`). Champion builds now show the pair's icons next to the names.
- **The level:** it was drawn once, as text beside the portrait. The fix draws it as a badge on the portrait's corner, as the client does. Match cards had no level at all and now get the same badge, from the summary's `champLevel`.

## ADR-087 — Dashboard crawl activity: the queue in claim order, and one view per crawl (2026-10-09)
Accepted (owner request, task DEV-13). Design/06 §Activity views.
- **Owner request:** "For the history for ladder crawls, and running events, allow it to be interactable and allow to see what is running currently and what is running next. Also show the status of it as well, where it is in the fetch and any other useful data."
- **One claim order, shared:** `CLAIM_ORDER` is now a constant used by `claim_sql` and by both views. "Up next" can't drift from what the workers do.
- **Progress comes from data that already exists.** No new columns. The enumerate total comes from the tier floor (one leg per apex tier, four per paged tier). Collect is `ceil(backfills_enqueued / 25)`. Archive is `match_ids_seen`. "Done" is the total minus the legs or ids still open. A stage that a cancelled or failed crawl stopped in shows `done: null`: once its legs are deleted, its progress can't be recovered.
- **Pace and ETA use a ten-minute window** of finished jobs. That reacts quickly to rate-limit stalls but doesn't jump on each job. There is no ETA when nothing finished in the window, so the view never says "∞" or invents a guess.
- **Downloads are counted per platform, not per crawl:** after the hand-off an `archive:match` payload names only its match. They are recognised by the crawl-match priority band and the job-id prefix, and labelled "every crawl on <platform>" in the UI.
- **Polling, not a new realtime topic:** the views scan `jobs`, and only an admin with the Ladder tab open needs them. Polling every 5 s, paused while the page is hidden, costs less than pushing every job transition over the `ladder` topic.
- **Tests:** Rust integration tests for both routes (a crawl followed through enumerate, collect and archive, then cancelled to `stopped`; claim order; ahead count; downloads; 404; reader keys refused). jsdom tests for the dashboard against a fake API (`tests/dom/fake-dashboard.mjs`). The headless-Chromium layout suite at three widths, as for the showcase (ADR-084).

## ADR-088 — Recompute analytics from the dashboard (2026-10-09)
Accepted (owner request, task DEV-16). Design/06 §Activity views.
- **Owner request:** "Also work on a way to run it manually as well" (analytics, after asking when it is filled in). `POST /v1/admin/analytics/recompute` already existed (v1), but it could only be called with curl, from `/docs`, or from `/dev`, and `/dev` is not served in production. The owner chose a dashboard button over changing the route.
- **The route is unchanged.** The button sends `{platform, queue}`. Platform stays required (ADR-065), so the button never sends an empty body, and there is no all-ladders option.
- **Pickers come from `GET /v1/admin/ladder/options`,** the same source as the crawl form, with this deployment's defaults preselected. A recompute rebuilds one ladder, so it offers the same ladders a crawl does.
- **The message says the job is queued, not done.** `aggregate:analytics` runs at maintenance priority (30 000), after every crawl download (20 005). After a crawl it can wait a long time, and the result appears in the panel's table when the job runs.
- **Tests:** jsdom tests against the fake API: the options fill the pickers and enable the button, the POST body matches the picked ladder, a refusal shows the API's message, and changing a picker clears the old message. The new controls are inside the panel that the headless-Chromium layout suite measures at three widths.

## ADR-089 — Rate-limit-aware job claims: lanes, yields, turns (2026-10-09)
Accepted (owner request, task SCH-01). Design/06 §Claiming, design/05 §Priorities and fairness.
- **Problem, seen live:** kr, euw1 and na1 crawls queued together collected match ids one platform at a time. All 8 workers held na1 `ladder:collect` jobs and waited inside `acquire` on americas' bulk ceiling (80/100), with asia and europe at 0 %. 74 733 `archive:match` jobs for oc1 (sea) waited too, because claims took rows strictly by `priority, run_after, id`.
- **Lane and method are derived, not passed.** `enqueue_on` stamps them from the kind and payload (`jobs::lanes::of`), so the eight producers of Riot-calling kinds can't disagree or forget. `assign_lanes` gives pending rows from before V0007 theirs at boot, so a queue that is already long benefits on the next restart.
- **The claim reads one head per lane.** The claim gets the open lanes as a JSON list (every platform and region less the blocked ones), not by scanning for distinct lanes. Each lane's best ready row is one seek on `jobs_lane_claim(lane, priority, run_after, id)`. Measured on 75 000 ready rows: about 0.1 ms, where sorting every ready row by band and running count took about 60 ms on the single writer. `+run_after` stops SQLite from choosing `jobs_claim` when `ANALYZE` has not run (325 ms in that case). A test checks that every lane `lanes::of` can produce is in the list, so no row can become unclaimable.
- **Bands for spreading:** 0–99, 100–9 999, then each 10 000 (design/06's band table). Inside a band the least-busy lane wins, so a crawl's collect (20 003) and another crawl's downloads (20 005) share the workers. Inside a lane, priority order is unchanged.
- **A yield is a limiter wait, not a Riot 429.** `FetchError::limited_until` is set only when our limiter's budget runs out (including a freeze after a typed 429 that outlasts the budget). Riot's own untyped 429 still maps to `RATE_LIMITED` with backoff and counts as an attempt, as before. A yield gives the attempt back, keeps the last real error and counts nothing in `jobs_total`, so no metric name or label changes.
- **Resume points:** a walk resumes from its page cursor; a collect is re-queued with only the players it has not done (`Yield.payload` replaces the row's payload). A backfill resumes from `backfill_state`. `archive:match` announces the match before it fetches the timeline, so a timeline yield can neither skip the timeline nor lose the event.
- **Take turns** every `CANCEL_CHECK_PAGES` pages instead of a per-kind cap, as the task specifies. The crawl-still-running check now runs once at the start of each turn, with the same 10-page spacing as v1.
- **Idle workers sleep until something can change:** the next delayed row's `run_after` or the first blocked lane's free time, whichever is sooner, falling back to 1 s. That is safe because SQLite only allows `ROLE=all`: every enqueue happens in this process and wakes a worker.
- **A single-flight caveat:** a job's fetch that leads a single-flight hands its 1 s budget to any interactive request coalesced onto it. That request gets `RATE_LIMITED` after about 1 s instead of waiting. Before this change it would have waited up to 15 min behind the bulk budget.
- **Tests:** limiter unit tests for `bulk_blocked`. Claim tests: a blocked lane skipped for an older row, a capped method blocking only its own jobs, a free lower band beating a blocked higher one, three crawls' first claims covering three platforms, yield bookkeeping, idle sleep, boot lane assignment, and only our own limiter yielding. Wiremock tests: a collect resumes without re-walking finished players, a walk resumes after a yield and takes turns every ten pages, three rate-limited crawls finish side by side, a poll behind a 40-page walk runs at its first turn, and a rate-limited timeline yields and comes back for only the timeline.

## ADR-090 — A manual analytics recompute runs ahead of the queue (2026-10-09)
Accepted (owner request, task DEV-18). Design/06 §Priority bands, §Activity views. Supersedes ADR-088's "runs at maintenance priority" for the manual path. (ADR-089 is held by SCH-01's work in progress.)
- **Owner request:** a manual recompute "should run in a separate worker when available (wait until one is freed up, and should run first)". The owner wants the analytics of the data archived now, not after a crawl's downloads finish weeks later. The rebuild reads the archive only and makes no Riot call, so it costs no rate limit.
- **Priority 0, not a reserved worker.** `POST /v1/admin/analytics/recompute` queues `aggregate:analytics` at `priority::INTERACTIVE`. The claim already takes the best band first, so the next worker that finishes claims it. No running job is interrupted, and no worker is kept idle for it.
- **A rebuild already queued is moved up, not duplicated.** The dedupe key (`platform:queue`) stays. The new `Queue::enqueue_or_promote` lowers a pending duplicate's priority and `run_after` to the request's (never raises them), in the same write as the enqueue. A running duplicate is left alone: it already reads what a second run would.
- **The automatic rebuilds are unchanged.** Crawl end and `AGGREGATE_INTERVAL_S` still queue at maintenance priority (30 000).
- **Tests:** a scheduler unit test (a pending duplicate is lifted and claimed first; a running one and a looser request change nothing; no duplicate queues fresh). An integration test: with archive and walk jobs queued and a crawl-end rebuild at 30 000, the route's call makes that same row the next claim, at priority 0.

## ADR-091 — Ladder tab: one card per running crawl, the rest folded (2026-10-09)
Accepted (owner request, task DEV-17). Design/06 §Activity views.
- **Owner request:** "Start cleaning up this page. It's ugly, lot of the data is repeated and makes no sense. Make sure that it's clean, nice to look at and makes sense. Most of the data should be expandable, not shown by default."
- **What repeated:** each running crawl appeared three times (its card, its history row, the job queue). The stage chain repeated the stage bars. The facts row repeated itself (players = entries = histories queued on a full crawl). The cancel button appeared twice. "Last completed" repeated the top history row. A collect stage in flight drew every open leg as a chip, hundreds of them, before anything else.
- **Running crawls are cards:** the head (ladder, floor, stage, start time, cancel), one bar per stage, and one totals line (players, match ids, matches queued, failed). Everything else from the activity view (the other counters, legs in flight, running/next/failed jobs, platform downloads, crawl id) sits in a closed **details** fold. The fold summary counts what it holds, and failures also show on the totals line, so nothing urgent is hidden. Open folds stay open across the 5 s refresh. The leg list scrolls inside a fixed height.
- **Past crawls list finished runs only,** ten at a time with "show more". A row opens into the same view with its details open. "Last completed" and the per-ladder "last crawled" line are gone: the table, newest first, already answers both.
- **The job queue and the analytics recompute are folded panels.** Their one-line summary (queue counts; last recompute) shows while closed. "Start a crawl" folds into the Crawls panel and opens by itself when nothing is running.
- **The running cards come from the snapshot or the crawl list,** whichever is newer, so a crawl started from the page shows up without waiting for a tick. A new card fetches its activity at once instead of at the next 5 s poll.
- **No API change.**
- **Tests:** the jsdom suite is rewritten for the new layout (a card per running crawl and not in the history, closed details with a counting summary, folds staying open across a refresh, paging past crawls, cancel from the card head, folded queue and recompute panels, the form open when idle). The fake API serves eleven older runs so that paging is tested. The headless-Chromium layout suite opens the card's details, a past run and both folded panels at three widths.

## ADR-092 — Live job activity and the /dev/jobs page (2026-10-09)
Accepted (owner request, task DEV-19). Design/06 §Live activity, design/10 §Jobs page.
- **Owner request:** "a dev page that shows all running jobs, what is in queue, what is happening with each and what is running right then and now", with "interactable tabs that show what is happening with that job". The owner picked a live activity feed over stored data only, `/dev/jobs` over a dashboard tab, and one closable tab per opened job.
- **In memory, not stored.** What a job is doing changes many times a second. Writing it to SQLite would put a write on the single writer for every Riot call. `jobs::activity::Activity` keeps it in the worker process instead: 200 events per trace, the 200 most recent finished traces. A restart loses it, which suits a dev view. The job's row, its attempts and its final error stay in `jobs` as before.
- **A task-local, not a parameter.** The handler runs with its job as the task's current job, so the fetcher and any helper can report without every signature carrying a context. The single-flight upstream leg runs on a spawned task, so it is wrapped with the caller's job. Code outside a job (interactive requests, ticks) reports nothing at no cost. A job that joins another caller's in-flight request logs only its own fetch line.
- **What is logged.** A fetch is one line: its `X-Cache` outcome (or error code) and duration. A failed Riot answer, a 429 backoff, a 5xx retry, and a rate-limit wait of 20 ms or more each get a line too. A successful Riot answer adds none, since the fetch line covers it. Waiting on the limiter and a call in flight set only the "now" line. Handlers add steps where they make progress: walk pages, apex leagues, collect players, archive batches, backfill id pages, match and timeline fetches, analytics steps.
- **Two admin reads.** `GET /v1/admin/jobs/activity` and `GET /v1/admin/jobs/{id}/activity?after=`. They sit under `/v1/admin` with the other job routes, and timestamps are ISO like theirs. They add no metric and change no existing response.
- **Single process.** A `ROLE=api` process shows no workers. Seeing a worker process's activity from another process is out of scope, as it is for SCH-01's limiter.
- **The page polls instead of using `/v1/ws`.** It polls once a second, for the tab in view only, and a job's tab asks only for new events. That keeps the page to the documented REST shapes, which the jsdom suite fakes. A hub topic per job would also need fan-out and cleanup for something one developer looks at.
- **Tests:** unit tests on `Activity` (steps and the now line, nothing outside a job, spawned tasks only when handed the job, finishing frees the worker and keeps the trace, bounds, a new attempt replaces the trace). An integration test runs a real worker against wiremock: the routes show the worker's job and now line while it runs, the miss and a 404 in the trace, `after` paging, the outcome after it ends, a failure's reason, a row with no trace, and a 404 for an unknown id. jsdom tests cover the page (overview, opening, following and closing tabs, reload and links, Retry/Cancel with confirm, an unknown job). `tests/ui.rs` checks that the page is served, gated, self-contained and in the page bar.

## ADR-093 — The dashboard job queue groups alike jobs (2026-10-09)
Accepted (owner request, task DEV-20). Design/06 §Activity views.
- **Owner request:** "Instead of showing all the ladder:collect tasks, group them based on queued jobs." A kr crawl filled the panel with a row per 25-player batch: six running, fifteen up next, all `ladder:collect · kr`.
- **One row per kind and platform,** in the order each first appears, so the claim order still reads top to bottom. A group of one keeps the job's own line. A group says how many jobs it holds and what they cover: a collect group gives the lowest and highest player of its batches (the batches need not be contiguous), an archive group its first match, a backfill group its player count. Running groups show the oldest claim; pending groups say `ready` or when the first comes due, and the highest try. The tooltip gives the kind and priority range.
- **Up next reads 100 jobs instead of 15.** Grouped, 100 fit in a few rows, and the counts cover more of the queue. The panel's header still gives the full ready and waiting counts. Counting the whole queue by kind was left out: `GET /v1/admin/jobs/stats` and `/dev/jobs` already do.
- **Only the queue panel groups.** A crawl card's own running/next/failed lists stay one row per job, since they are one crawl's jobs and the failures carry their own errors.
- **No API change.**
- **Tests:** the jsdom queue-panel test serves running and pending collects for two platforms, out of offset order, and checks the grouped rows, counts, ranges, times, tries and tooltip, and that the panel asks for 100 jobs.
## ADR-094 — Analytics over every patch, and a patch list (2026-10-09)
Accepted (owner request, task DEV-21). Design/11 §Page map.
- **Owner request:** the showcase's Top champions and champion page showed "No analytics yet" while the dashboard listed a finished recompute. Then: "when viewing on these patches, allow to change it. Also allow the region to be changed, but overall this data should show all data from all patches."
- **Why it was empty:** with no `patch`, the analytics routes read the newest patch that has any row. On the dev VM that was 16.20, with one match (10 games) against 406 on 16.19. The default `AGGREGATE_MIN_GAMES` of 10 then dropped every champion. na1 was empty for a different reason: its crawl had not reached the archive stage.
- **`patch=all` sums every aggregated patch** on `/v1/lol/analytics/champions`, `/champions/{championId}` and `/champions/{championId}/matchups`. Rows are grouped without the patch, `patch` reads `"all"` in the response and in each stat entry, and the slices and bans used for pick and ban rates are summed the same way. Patches older than `AGGREGATE_PATCH_LIMIT` keep their last rebuild's rows (ADR-056), so they count too. `minGames` applies to the summed rows.
- **The default stays the newest patch.** Clients that name no patch keep the behaviour they have. Only the showcase now asks for `all` by default. Changing the default would be one line.
- **`GET /v1/lol/analytics/patches?platform&queue&remakes`** lists the patches `champion_stats` holds for a ladder (or every platform), newest first by numeric order. Each entry has its games (participant rows, as `totalGames` counts them) and recompute time. It sends the same ETag and `Cache-Control` as the other analytics routes. It is a new v2 route: v1 had none (parity no longer applies, see RC-01).
- **The showcase** has a patch picker on Top champions and the champion page, defaulting to All patches and shared between them. The champion page also has a region picker: All regions or one platform. It starts on the platform picked in the header and does not change it.
- **Tests:** a unit test on the reads (every patch summed into one row labelled `all`, slices summed, the patch list newest first with and without remakes). An integration test on the routes (`patch=all` doubles a copied patch's games while pick rate stays put, on all three routes; the patch list, its ETag and 304, remakes, an unknown platform, validation). jsdom tests for both pickers, the calls they make and the "too few games" message. Pure-helper tests for the picker's choices and fallback.

## ADR-095 — Re-running the apex cap checks from /dev (2026-10-09)
Accepted (owner request, task LAD-03). Design/10 §Ladder tab.
- **Owner request:** after the 10,000 Master cap was found by hand (IMPLEMENTATION.md §Post-release — LAD), "tests that I can easily run through the dev portal". The owner picked a `/dev` panel that repeats those checks over a crawl-vs-crawl diff, since a crawl down to MASTER reads each apex league in one request and has no page churn to diff.
- **league-exp joins the registry.** The probe needs league-exp-v4, and LAD-02 may too. `league.expEntries` is v2's first method past v1's list: the parity test now checks v1's ids, then `V2_ADDED`. The route and its paging come from the owner's check on 2026-10-09, not from memory. Its own id gives it its own limiter bucket, as every method has. The `ladder` TTL key fits a ladder read. It is not persisted to L2, since only the probe reads it and the probe skips the cache read anyway. Calling it through the debug route's default bucket instead would have mixed it into `status.platformData`'s limits and cache key.
- **One synchronous admin POST, not a job.** It is about 55 calls and the answer is the point. Riot takes about a second per league-exp page, so pages go out ten at a time: a capped kr probe took 56 s one page at a time and 12 s batched with the production key. A batch may ask for up to nine pages past the end. It is POST because it bypasses the cache and costs Riot calls; a GET could be prefetched or replayed. It is under `/v1/admin/ladder` with the crawl routes, so the explorer lists it too, and is `admin`-tagged, so the showcase rule doesn't apply.
- **Interactive priority, cache read skipped.** The owner clicked Run and is waiting, and a cached Master list up to 2 minutes old would compare badly with fresh league-exp pages. Bodies are still written through to L1.
- **Verdicts, not pass/fail.** The cap is an observed value, so the probe reports what Riot does against what was seen: `confirmed`, `not-seen`, `changed`, `error`. `changed` is the one to act on: it means LAD-01/02's assumptions need re-checking.
- **Thresholds.** League-exp may have up to 10 players `masterleagues` lacks before it counts as a different list. The owner's check saw 1 and 2 of 10,000, from the ladder moving while it was paged. The walk stops after 100 pages (20,500 entries, twice the cap), so a longer list still shows as one.
- **Riot's 400 isn't visible.** The fetcher turns any non-2xx except 401/403/404/429 into `UPSTREAM_ERROR`, so `paged-refuses-apex` accepts any refusal and says that 400 was what Riot sent on 2026-10-09. Passing Riot's status through would change the error mapping for every route, which is out of scope.
- **Tests:** unit tests on `analyse` for every verdict; wiremock integration tests on a kr-shaped and an oc1-shaped ladder, plus auth and validation; node tests on the page's helpers and a jsdom test of the tab.

## ADR-096 — The dev reset stops running jobs first (2026-10-09)
Accepted (owner request, task DEV-22). Design/10 §Reset tab, design/06 §Halting.
- **Owner request:** "when using reset make sure to stop and clear all jobs first as well". Before this, the reset deleted the job rows, but jobs already running carried on. They could write players, matches or crawl rows into the emptied tables, and queue new jobs. The tab only warned about it.
- **Halt, wipe, resume.** The reset holds the workers (`Queue::halt`), aborts what they run, waits up to 10 s for them, and then clears L1 and every fetched table, `jobs` included. Then it lets them go. Workers are not shut down and restarted: `Workers` is owned by `serve` and shutting it down is final. A pause inside the claim loop also keeps the change to the scheduler small.
- **On the `Queue`.** The routes and the scheduler already share one `Queue` (`serve` builds the scheduler with `with_queue`), so the halt state lives there and no new handle goes into `AppState`.
- **Abort, not drain.** A crawl walk or backfill can run for minutes, and the owner asked for the jobs to be stopped. The abort lands at the handler's next `await`. A SQLite write already handed to the writer thread still commits, but it is queued ahead of the wipe's transaction, so the wipe deletes it.
- **Aborted jobs record nothing.** A Retry or Fail outcome would be wrong, since the job did nothing wrong, and its row is deleted a moment later anyway. Outside a reset, a row left `running` is what a crash leaves too, and `recover` handles it at boot.
- **The response** adds `stoppedJobs`. `runningJobs` keeps its meaning: job rows marked running when deleted.
- **Not covered:** workers in another process (`ROLE=worker`), an interactive Riot call in flight, and the last 2 s of L2 write-behind. design/10 lists them.
- **Tests:** a scheduler integration test (two running jobs aborted, nothing claimed while halted, claims resume after the drop; an idle halt settles at once) and a reset test with real workers (three running jobs stopped, the queued one cleared, no late write, new work runs afterwards). The jsdom test covers the tab's wording and the stopped count.

## ADR-097 — Saying when an apex league is cut off at Riot's cap (2026-10-09)
Accepted (owner request, task LAD-01). Design/06 §Job catalogue, design/11 §Page map.
- **Why:** kr, euw1 and na1 crawls down to MASTER each stored exactly 11,000 players, because Riot's `masterleagues` lists at most 10,000 (IMPLEMENTATION.md §Post-release — LAD). Nothing showed that the crawl was short. LAD-01 records it and shows it. LAD-02 is meant to find the missing players.
- **Where it is decided.** `write_page` takes the leg's apex tier (`Page.apex_tier`, `None` for a walk's page) and marks it in the same transaction that stores the list, as the task asks. A crash before the commit marks nothing, and the leg runs again. It counts the entries it stores. Riot sends a puuid on every entry, so that is Riot's count.
- **At or over the cap, for any apex tier.** "At least" `RIOT_APEX_LIST_CAP` counts as capped, so a list longer than 10,000 still reads as cut off; LAD-03's probe would report that as `changed`. The rule doesn't special-case MASTER. Challenger (300) and Grandmaster (700) never reach it, but if Riot changed those sizes, the crawl would say so rather than miss it. The unit tests cover both: Challenger and Grandmaster at their real sizes, and Grandmaster at the cap to check the ordering.
- **Shape.** `ladder_crawls.apex_capped` (V0008) is a JSON list in `TEXT` (repo convention), in `APEX_TIERS` order, and NULL for none. It is read back as `[]`, and so is a value this code did not write, so a bad row can't break the crawl routes. A tier stays marked: a retried leg that comes back one short is the ladder moving, not the cap lifting.
- **Surface.** `apexCapped` is a new v2 admin field on `LadderCrawlSummary`. That covers the crawl list, `GET /v1/admin/ladder/crawls/{id}` and the stats snapshot's `ladder.running`/`lastCompleted`, so the dashboard reads it from wherever it already gets the crawl. No v1 contract field changes. `GET /v1/lol/league/apex/...` stays a passthrough. The showcase counts the list it was given, because a read route can't see crawl rows.
- **One number, three copies.** The dashboard and the showcase have no build step, so they keep `APEX_LIST_CAP = 10000` in their own JS. `tests/ui.rs` checks that both match `RIOT_APEX_LIST_CAP`.
- **Tests:** unit tests on `write_page` (10,000 marks MASTER; 9,999, Challenger at 300, Grandmaster at 700 and a walk's page of 10,000 don't; a retry marks once and one short doesn't unmark; another crawl is untouched; tiers keep apex order). A wiremock integration test runs a crawl with a 10,000-player Master list (`apexCapped: ["MASTER"]` on the crawl route and in the list), then one a player short (`[]`). jsdom tests show the dashboard note on a capped finished crawl and none on the uncapped running card. The showcase note appears under a 10,000-player Master list on every page and not under Challenger or Grandmaster. Node tests cover `apexCapNote`.

## ADR-098 — Archive jobs fetch timelines by default (2026-10-09)
Accepted (owner request, task DEV-23). Amends ADR-085; design/04 §Schema, design/07 config table.
- **Owner request:** timelines "should be a default, and ARCHIVE_TIMELINES should be used to turn it off … having that data is useful, and although it takes a long time now, when I have a prod key it'll speed it up a lot." Until now `ARCHIVE_TIMELINES` defaulted to `false` (v1's default), so a ladder crawl archived matches without their timelines.
- **`ARCHIVE_TIMELINES` defaults to `true`.** Every archive job fetches the timeline after its match: `ladder:archive`'s `archive:match` jobs (which carry the flag), `poll:matches` and `backfill:player`. `ARCHIVE_TIMELINES=false` is how to turn it off. The flag keeps the meaning ADR-085 gave it; only the default changes.
- **The cost, accepted:** a second request per archived match, on `match.timeline`'s own method limit, and timelines are large on disk (design/07 §Sizing's "timelines on" row). With a development key this roughly doubles a crawl's archive stage; the owner expects a production key to absorb it. A timeline that cannot get a slot within `JOB_YIELD_BUDGET_MS` yields the job (the match is already archived and announced, so nothing is lost); any other timeline failure is logged and the match stays archived without it (v1).
- **THR-06b** (timeline sampling) was written for timelines off; it now needs the owner's call before it starts.
- **Tests:** config default and `ARCHIVE_TIMELINES=0` turning it off; a ladder crawl over the default config fetches and archives one timeline for each match it archives, none for one already archived, and none with `ARCHIVE_TIMELINES=false`.

## ADR-099 — The showcase and explorer revalidate every call (2026-10-09)
Accepted (owner report, task DEV-24). Design/11 §Principles, design/10 §One explorer request.
- **Owner report:** after a recompute on the dev VM (13,028 games in the run row), the showcase's Top champions still showed "All patches · 294 games", the numbers from before it. Ticking "Disable cache" in DevTools fixed it.
- **Cause:** the analytics routes send `Cache-Control: private, max-age=300` (v1's `ANALYTICS_CACHE_CONTROL`), and both pages used a plain `fetch`. The browser kept answering from its copy for up to five minutes, without asking the proxy. The server's numbers were already right.
- **Fix in the pages, not the header.** Both pages now pass `cache: 'no-cache'`, so the browser revalidates every call. The analytics routes answer an unchanged read with a 304 through their `ETag`, so this costs one round trip and no body. The header stays: a consumer's own frontend may well want the five minutes, and `max-age` is the right hint for one.
- **Both pages, every call.** The explorer exists to show what the proxy answers now. The showcase teaches frontends, and it is run next to the dashboard's Recompute now, where a stale copy reads as a server bug. The dashboard and `/dev/jobs` read admin routes, which send no `max-age`.
- **Tests:** jsdom tests that every `/v1/` call from the showcase and from the explorer asks for `no-cache`.

## ADR-100 — A champion's own patch list (2026-10-09)
Accepted (owner request, task DEV-25). Design/11 §Page map.
- **Owner request:** on the showcase's champion page, the patch picker read "All patches · 12,860 games" next to Jinx's 167: "make it so that it only shows the amount of games based on the champion (instead of all of them)". It showed the ladder's games because `GET /v1/lol/analytics/patches` had no way to ask for one champion.
- **`championId` on `/v1/lol/analytics/patches`** (≥ 1, optional). With it, the list holds only the patches `champion_stats` has rows for that champion on, and each entry's `games` is that champion's games, summed over tiers and roles, remakes excluded unless `remakes=include`. These are the rows the champion detail's `totalGames` sums, but the detail drops tiers under `minGames` first. So the picker's "All patches" can be a little above the page's total, and the difference is the games in thinly played tiers. A champion is in a match at most once, so its games are also its matches. The response echoes `championId` (`null` without one), and it is part of the `ETag`.
- **Not `minGames`-filtered.** The list shows every patch the champion was played on, including those under `AGGREGATE_MIN_GAMES`. Picking one of those shows the existing "Not enough … games on patch …" message, which says why the page is empty.
- **The showcase** passes the champion on its champion page. Home's Top champions keeps the ladder's list. The picked patch is still shared between the two: when the champion was never played on it, the champion page falls back to All patches, as it already did for a patch the ladder lacked. A champion with no rows at all now reads "No {champion} games in the analytics yet", because an empty champion list can't tell "no analytics" apart from "never played".
- **Tests:** a unit test on the read (a champion's patches and games, an unplayed champion's empty list). An integration test on the route (one game per patch for the most played champion, the `ETag` differs, an unplayed id gives `[]`, `championId=0` is a 400). jsdom tests that the champion page asks with `championId` and labels the champion's games, while home asks without it.

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
- **`DEV_UI`** keeps v1 semantics: unset means on everywhere except production, and an explicit value wins even in production. design/07's "production … turns `DEV_UI` off" is read as the default, which is how v1 behaves.
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

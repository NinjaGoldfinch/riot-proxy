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
- **Client IP** is the leftmost `X-Forwarded-For` entry when present, otherwise the TCP peer, matching v1's Fastify `trustProxy: true`. This assumes a trusted proxy in front (Caddy, design/07). Revisit with built-in TLS (P8-03), where the binary faces clients directly.
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

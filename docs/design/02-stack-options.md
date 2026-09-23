# 02 — Stack options

Two independent decisions, evaluated separately:

1. **Language / runtime** — what the binary is written in.
2. **Storage & coordination** — what holds cache, limiter state, jobs and the archive.

The second matters more than the first. Most of v2's simplification comes from collapsing storage to SQLite + process memory, and that works in any of the languages below.

## Evaluation criteria

Weighted for *this* project: a solo-maintained hobby-infrastructure service that must stay up for months untouched.

| Criterion | Weight | Why |
|---|---|---|
| Deploy simplicity (single artefact, no runtime install) | 5 | The stated v2 goal |
| Memory / CPU at idle | 4 | Runs beside other things on a small box |
| Reliability under long uptime (no leaks, no GC pauses that matter, no runtime version churn) | 4 | "Reliable" in the brief |
| Concurrency model fit (thousands of idle WS + bursty upstream I/O) | 3 | The actual workload |
| Ecosystem for the specific needs (HTTP, WS, SQLite, OpenAPI, Prometheus) | 3 | Must not hand-roll infrastructure |
| Author familiarity / reuse | 3 | You already write Rust + SQLite for `ninja-recorder` v2 |
| Type-sharing with downstream TS projects | 2 | Nice, but OpenAPI codegen solves it in any language |
| Iteration speed | 2 | The design is settled; this is a port, not exploration |

## Language options

### A. Rust — `axum` + `tokio` + `reqwest` + `sqlx`/`rusqlite`

| | |
|---|---|
| Binary | Single static (musl) binary, ~10–15 MB, `FROM scratch` image |
| Idle RSS | ~15–30 MB |
| Concurrency | `tokio` — native async, zero-cost tasks; ideal for many idle sockets |
| HTTP | `axum` (on `hyper`), first-class WS via `axum::extract::ws` |
| OpenAPI | `utoipa` derives the document from handler + type annotations; `utoipa-scalar` serves the same Scalar UI v1 uses at `/docs` |
| Validation | `serde` + `validator`/`garde` |
| Cache | `moka` (TinyLFU, async-aware, per-entry TTL) |
| Limiter | hand-written ~150 lines (the algorithm is v1's; see [05](05-rate-limiter.md)) — `governor` exists but does not model Riot's header-driven, multi-window semantics |
| SQLite | `rusqlite` (bundled, sync, simplest) or `sqlx` (async, compile-time checked queries, migrations built in) |
| Jobs | `tokio` tasks + a durable `jobs` table; `tokio-cron-scheduler` for cron-style ticks |
| Metrics | `metrics` + `metrics-exporter-prometheus` |
| Logging | `tracing` + `tracing-subscriber` (JSON) |
| TLS | `axum-server` with `rustls`; `rustls-acme` for built-in Let's Encrypt |
| Risks | Slowest to write; compile times; async lifetimes around WS + broadcast take a day to get right the first time |

**Fit:** highest on every weighted criterion except iteration speed. The `ninja-recorder` v2 rewrite is already Rust + SQLite, so the two projects share crates, patterns and a mental model.

### B. Go — `net/http` (1.22+ router) or `chi`

| | |
|---|---|
| Binary | Single static binary, ~12–20 MB |
| Idle RSS | ~20–40 MB |
| Concurrency | goroutines — excellent; simpler mental model than Rust async |
| HTTP | stdlib `net/http` is enough since 1.22's method+pattern routing; `coder/websocket` or `gorilla/websocket` |
| OpenAPI | `huma` generates from handler types and serves Scalar/Stoplight; `oapi-codegen` for the spec-first direction |
| Cache | `otter` or `ristretto`; both need a small wrapper for soft/hard TTL |
| SQLite | `modernc.org/sqlite` (pure Go, no cgo → still static) or `mattn/go-sqlite3` (cgo, faster) |
| Jobs | goroutines + `time.Ticker` + jobs table; `river` if you ever want Postgres-backed queues |
| Metrics | `prometheus/client_golang` |
| Logging | `log/slog` |
| Risks | GC is fine at this scale; error handling is verbose; generics are enough for typed caches but not as expressive |

**Fit:** ~90% of Rust's score with meaningfully faster development. The correct choice **if** Rust's learning curve or compile times would stall the project. The v1 spec already listed Go as the alternative stack.

### C. Bun — `Hono` + `bun:sqlite`

| | |
|---|---|
| Binary | `bun build --compile` produces a ~90 MB single executable (embeds the runtime) |
| Idle RSS | ~50–80 MB |
| Concurrency | JS event loop; fine for I/O, single-threaded CPU |
| HTTP | `Hono` (or Elysia); native WS in `Bun.serve` |
| OpenAPI | `@hono/zod-openapi` — closest to v1's TypeBox flow |
| Cache | `lru-cache` or hand-rolled Map + timers |
| SQLite | `bun:sqlite` — built in, very fast, sync |
| Risks | Bun still moves fast; Node-compat gaps surface in less-common libraries; the "single binary" is large; you keep the JS runtime churn that v1 suffers from |

**Fit:** the fastest port from v1 — much of the TypeScript reuses verbatim. But it keeps a JS runtime in the loop and produces the heaviest artefact. Worth it only if type sharing with TS consumers is more important than the deploy goal.

### D. Keep Node, trim the stack

Replace Redis with in-process cache, Postgres with `better-sqlite3`, BullMQ with a jobs table, merge `worker` into `api`. Keep Fastify.

| | |
|---|---|
| Deploy | Still needs a Node install or a ~200 MB `node:alpine` image; `pkg`/SEA single executables exist but are second-class |
| Idle RSS | ~80–100 MB |
| Effort | Lowest — mostly deletions |
| Risks | Doesn't hit the "lightweight" goal; still pinned to a Node major; `better-sqlite3` is a native addon that must be rebuilt per Node version |

**Fit:** a legitimate *intermediate* step (see [08-migration-plan](08-migration-plan.md#option-b-strangler-path)), not the destination.

### Scoring

Scores 1–5 per criterion, multiplied by weight. Highest possible 130.

| Criterion (weight) | Rust | Go | Bun | Node-trim |
|---|---|---|---|---|
| Deploy simplicity (5) | 5 · 25 | 5 · 25 | 3 · 15 | 2 · 10 |
| Idle footprint (4) | 5 · 20 | 4 · 16 | 3 · 12 | 2 · 8 |
| Long-uptime reliability (4) | 5 · 20 | 5 · 20 | 3 · 12 | 3 · 12 |
| Concurrency fit (3) | 5 · 15 | 5 · 15 | 3 · 9 | 3 · 9 |
| Ecosystem fit (3) | 4 · 12 | 4 · 12 | 4 · 12 | 5 · 15 |
| Author reuse (3) | 5 · 15 | 2 · 6 | 3 · 9 | 4 · 12 |
| TS type sharing (2) | 3 · 6 | 3 · 6 | 5 · 10 | 5 · 10 |
| Iteration speed (2) | 2 · 4 | 4 · 8 | 5 · 10 | 5 · 10 |
| **Total** | **117** | **108** | **89** | **86** |

The "TS type sharing" row is scored 3 for Rust/Go because `openapi-typescript` regenerates a typed client from `/openapi.json` in one command — the v1 approach of hand-sharing TypeBox types was never actually used by a downstream project.

## Storage & coordination options

### Cache

| Option | Verdict |
|---|---|
| Redis | ❌ Only justified by multiple processes |
| In-process only (`moka`) | ✅ For everything with a TTL ≤ 24 h. Lost on restart — acceptable; a cold cache just costs one round of upstream calls, and the limiter (below) survives restarts so it cannot cause a 429 storm |
| In-process + SQLite write-behind | ✅ For the expensive-to-rebuild tiers: `account` (24 h), `summoner`, ladder pages. A `cache` table with `(key, body, soft_expires, hard_expires)`; warmed into `moka` on boot. Cheap and makes restarts invisible to callers |

### Rate limiter state

| Option | Verdict |
|---|---|
| Redis (v1) | ❌ |
| Memory only | ⚠️ Correct while running; a restart forgets in-flight window usage → the exact 429 storm the README warns about |
| Memory + checkpoint to SQLite every N seconds and on shutdown | ✅ On boot, restore buckets and treat every window as full until its `reset_at` — conservative, never over-commits. Riot's `-Count` headers re-sync the truth on the first response anyway |

### Archive & relational data

| Option | Verdict |
|---|---|
| PostgreSQL | ⚠️ Still the right answer past ~50 GB of match JSON or if a second *writer* process ever appears. Keep it as `--database postgres://…` behind a cargo feature, **not** the default |
| SQLite, WAL mode, single writer | ✅ Match JSON as `BLOB` (zstd-compressed — match-v5 payloads compress ~8×), facts and analytics as normal tables. A 1 M-match archive is ~10–15 GB compressed and SQLite handles that comfortably. One writer connection, a pool of readers, `PRAGMA synchronous=NORMAL` |
| Embedded KV (`redb`, `sled`, `fjall`) | ❌ Analytics (`champion_stats`, matchups, builds) are relational queries; keep SQL |
| DuckDB for analytics | 💡 Optional later: point DuckDB at the SQLite file for the `aggregate:analytics` recompute if it ever gets slow. Not v2.0 |

### Job queue

| Option | Verdict |
|---|---|
| BullMQ / Redis | ❌ |
| In-memory priority heap | ⚠️ Loses queued backfills on restart |
| SQLite `jobs` table + in-memory heap | ✅ Durable, ordered, idempotent by `(kind, dedupe_key)`. The worker claims rows with `UPDATE … WHERE id = (SELECT … LIMIT 1) RETURNING *` — SQLite's single-writer rule makes this atomic for free |

### Realtime fan-out

| Option | Verdict |
|---|---|
| Redis pub/sub | ❌ |
| `tokio::sync::broadcast` per topic | ✅ Publisher and subscribers are in the same process. Slow consumers get `Lagged` and a resync, matching v1's `firehose` semantics |

## Recommendation

**Rust, single binary, SQLite + in-process state.** Go if you want to ship in half the time and accept ~10 MB more RSS.

Everything from [03](03-architecture.md) onward is written for Rust but calls out the Go equivalent where it differs.

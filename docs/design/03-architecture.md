# 03 — Architecture

## System context

```mermaid
flowchart LR
    subgraph clients["Downstream projects"]
        web[Website]
        bot[Discord bot]
        rec[ninja-recorder]
    end

    subgraph host["One host · one container · one volume"]
        rp["riot-proxy v2<br/><i>single Rust binary</i>"]
        db[("data/riot-proxy.db<br/>SQLite · WAL")]
        dd["data/ddragon/<br/>static mirror"]
        rp <--> db
        rp --> dd
    end

    riot[(Riot API)]
    cdn[(Data Dragon CDN)]

    clients -->|"HTTPS · Bearer rpx_…<br/>REST + WebSocket"| rp
    rp -->|"X-Riot-Token<br/>~20 regional hosts"| riot
    rp -->|hourly| cdn
    prom[Prometheus] -.->|/metrics| rp
```

Optional pieces, each off by default: Caddy in front (if you already run one), Postgres instead of SQLite (`--features postgres`), external Prometheus/Grafana.

## Why one process is not a compromise

A Riot production key gives *one* application-level rate limit shared by every request made with it. Two proxy replicas do not double your budget; they split it and then need Redis to agree on the split. The proxy's throughput ceiling is Riot's, not the host's. A single tokio runtime on one core can service far more requests per second than a Riot key allows, so scale-out buys nothing.

What you *do* want from multiple processes — availability during a deploy — v2 gets from a sub-100 ms restart, a persisted limiter and a persisted cache: a redeploy is a blip shorter than one Riot round-trip.

## Component diagram

```mermaid
flowchart TB
    subgraph bin["riot-proxy binary"]
        direction TB
        http["HTTP layer<br/>axum · tower middleware<br/>auth · quota · tracing · metrics"]
        ws["WebSocket hub<br/>topics → broadcast channels"]
        routes["Routes<br/>/v1/riot · /v1/lol · /v1/players<br/>/v1/admin · /docs · /dashboard"]
        fetcher["Fetcher<br/>the single funnel every read takes"]
        cache["Cache<br/>moka L1 · SQLite L2<br/>negative cache · SWR"]
        sf["Single-flight<br/>in-flight map of futures"]
        limiter["Limiter<br/>header-driven buckets<br/>interactive / bulk"]
        client["Riot client<br/>reqwest · host routing<br/>error policy"]
        sched["Scheduler<br/>ticks · durable jobs · priority heap"]
        jobs["Job handlers<br/>poll · archive · backfill<br/>crawl · ddragon · aggregate"]
        archive["Archive<br/>matches · facts · analytics"]
        events["Event bus<br/>tokio broadcast"]
        store[("SQLite<br/>one writer · N readers")]
    end

    riot[(Riot API)]

    http --> routes --> fetcher
    http --> ws
    fetcher --> cache --> sf --> limiter --> client --> riot
    fetcher --> archive
    sched --> jobs --> fetcher
    jobs --> archive
    jobs --> events --> ws
    cache --> store
    limiter -->|checkpoint| store
    sched --> store
    archive --> store
```

Every box is a Rust module (`src/<name>/`), every arrow is a plain function call or channel — no network hop inside the binary.

## Request lifecycle (read path)

Identical in *policy* to v1 `src/fetcher.ts`; different in *mechanism* — every step is a memory access, not a Redis round-trip.

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant A as Auth+Quota<br/>(tower layer)
    participant F as Fetcher
    participant AR as Archive<br/>(SQLite)
    participant L1 as Cache L1<br/>(moka)
    participant SF as Single-flight
    participant RL as Limiter
    participant R as Riot

    C->>A: GET /v1/lol/summoner/… (Bearer)
    A->>A: key hash lookup (moka), scope, per-minute quota
    A->>F: fetch(endpoint, params, Interactive)
    F->>AR: immutable? (match, timeline) → SELECT
    AR-->>F: hit → 200 X-Cache: ARCHIVE
    F->>L1: negative cache? → 404 X-Cache: NEG
    F->>L1: get(key)
    alt fresh
        L1-->>C: 200 X-Cache: HIT
    else stale (soft < now < hard)
        L1-->>C: 200 X-Cache: STALE
        F-)SF: spawn refresh @ Bulk
    else miss
        F->>SF: join_or_start(key)
        SF->>RL: acquire(scope, method, Interactive, budget 2 s)
        RL-->>SF: token | RATE_LIMITED
        SF->>R: GET … X-Riot-Token
        R-->>SF: 200 + X-App-Rate-Limit* headers
        SF->>RL: observe(headers) → reconfigure + sync counts
        SF->>L1: put(key, body, soft, hard)
        SF->>AR: archive if immutable
        SF-->>C: 200 X-Cache: MISS
    end
```

`X-Cache` values, headers (`X-Cache-Age`, `X-RateLimit-*`, `X-Request-Id`) and error codes (`QUOTA_EXCEEDED` vs `RATE_LIMITED`) are carried over verbatim from v1 so downstream projects need no changes.

## Process model

One binary, one flag.

```mermaid
flowchart LR
    subgraph all["--role all  (default, single host)"]
        a1[HTTP + WS] --- s1[Scheduler + jobs]
        a1 --- d1[(SQLite)]
        s1 --- d1
    end

    subgraph split["--role api / --role worker  (only if you ever want it)"]
        a2[HTTP + WS] -.->|"requires --database postgres://"| d2[(Postgres)]
        s2[Scheduler + jobs] -.-> d2
    end
```

`--role all` is the design target and the only mode with SQLite. The split mode exists so the door to Postgres + separate worker stays open without a rewrite — but it is a **feature flag**, not the default, and v2.0 can ship without it.

Inside the process:

```mermaid
flowchart TB
    main["main()"] --> cfg[load config<br/>env + .env + flags]
    cfg --> db[open SQLite<br/>run embedded migrations]
    db --> warm[warm L1 from L2<br/>restore limiter checkpoint]
    warm --> spawn
    subgraph spawn["tokio::spawn"]
        srv[axum server]
        sch[scheduler loop]
        ck[checkpoint loop<br/>limiter + L2 flush every 10 s]
        hb[metrics history<br/>every 60 s]
    end
    spawn --> sig[SIGTERM / SIGINT]
    sig --> drain[graceful drain<br/>finish in-flight · final checkpoint · close WS]
```

## Module layout

```
src/
├── main.rs              CLI (clap): serve, key create, migrate, reset, spec
├── config.rs            typed config; refuses AUTH_DISABLED in production
├── app.rs               builds the axum Router + state
├── http/
│   ├── auth.rs          Bearer → consumer (moka-cached sha256 lookup), scopes, IP allowlist
│   ├── quota.rs         per-consumer sliding window
│   ├── error.rs         ApiError → JSON body + status (same codes as v1)
│   └── headers.rs       X-Cache, X-Cache-Age, X-RateLimit-*, X-Request-Id
├── routes/
│   ├── riot.rs          /v1/riot/* passthrough
│   ├── lol.rs           /v1/lol/*
│   ├── players.rs       /v1/players/* composites (fan-out with join_all)
│   ├── admin.rs         /v1/admin/*
│   ├── health.rs        /healthz /readyz /metrics
│   ├── docs.rs          /openapi.json via utoipa, /docs via utoipa-scalar
│   └── ui.rs            /dev /dashboard — static HTML embedded with include_str!
├── ws/
│   ├── hub.rs           topic → broadcast::Sender; subscribe/unsubscribe protocol
│   └── protocol.rs      frames, same shape as v1 §11
├── fetcher.rs           the funnel (see sequence diagram)
├── cache/
│   ├── l1.rs            moka wrapper: soft/hard TTL, negative entries
│   ├── l2.rs            SQLite cache table, write-behind, boot warm
│   └── keys.rs          key_scope + canonical key
├── singleflight.rs      DashMap<Key, Shared<BoxFuture>>
├── riot/
│   ├── client.rs        reqwest with pool, UA, X-Riot-Token, error policy
│   ├── routing.rs       platform ↔ region, sea→asia for account-v1
│   ├── endpoints.rs     the 9 endpoint groups: path, method-scope, TTLs
│   └── limiter/
│       ├── bucket.rs    window state
│       ├── mod.rs       acquire / observe / freeze / checkpoint
│       └── priority.rs  interactive vs bulk, ceiling
├── jobs/
│   ├── scheduler.rs     ticks, durable table, claim loop, priority heap
│   ├── poll.rs          live / rank / matches
│   ├── archive.rs       archive:match, backfill:player
│   ├── ladder.rs        crawl → apex/walk → collect → archive
│   ├── ddragon.rs       sync + mirror to disk
│   ├── analytics.rs     facts extraction, aggregate:analytics
│   └── maintenance.rs   L2 sweep, WAL checkpoint, backup
├── archive/
│   ├── matches.rs       zstd blobs, filter_unarchived
│   ├── facts.rs         per-participant rows
│   └── analytics.rs     champion stats, matchups, builds
├── db/
│   ├── mod.rs           writer + reader pool, pragmas
│   └── migrations/      NNNN_*.sql embedded via sqlx::migrate! or refinery
├── events.rs            topics enum, publish()
├── metrics.rs           counters/histograms; same names as v1 for the Grafana board
└── static/champions.rs  champion id ↔ name from ddragon mirror
```

Rough size: 6–8k lines of Rust for feature parity, ~half of v1, mostly because the Redis/Postgres/BullMQ glue and the two `docker-compose` files disappear.

## Cross-cutting decisions

| Decision | Choice | Notes |
|---|---|---|
| Async runtime | `tokio` multi-thread, `worker_threads = min(cores, 4)` | The workload is I/O; 4 is plenty |
| SQLite access | `rusqlite` on a dedicated writer thread via `tokio::task::spawn_blocking` + channel; `r2d2`-style reader pool | Keeps async code free of blocking calls; single writer is a SQLite requirement anyway |
| JSON | `serde_json`; match bodies stored as raw bytes, never re-serialised | Passthrough routes forward Riot's bytes untouched |
| Compression | `zstd` level 3 for archive blobs; `tower-http` `CompressionLayer` for responses | |
| IDs | `ulid` for request ids and job ids | Sortable, no coordination |
| Config | `figment` (env + `.env` + `--flag`) or plain `envy` | Same variable names as v1 where they still apply |
| Errors | one `ApiError` enum, `IntoResponse` | Codes unchanged from v1 |
| Time | `jiff` or `time` | Not `chrono` for new code |

## Go equivalents (if you pick Go)

| Rust | Go |
|---|---|
| `axum` + `tower` | `net/http` + middleware funcs, or `chi` |
| `tokio::sync::broadcast` | one `chan` per subscriber in a hub map, or `nhooyr`/`coder` websocket + fan-out goroutine |
| `moka` | `otter` |
| `rusqlite` writer thread | one `*sql.DB` with `SetMaxOpenConns(1)` for writes, a second for reads |
| `utoipa` | `huma` |
| `metrics-exporter-prometheus` | `client_golang` |
| `tracing` | `log/slog` |

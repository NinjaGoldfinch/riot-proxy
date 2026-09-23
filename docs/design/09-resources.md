# 09 — Resources

Verify versions on crates.io / pkg.go.dev before pinning; this list is about *which* library, not *which release*.

## Rust crates

| Need | Crate | Docs |
|---|---|---|
| HTTP server + WS | `axum` (`ws` feature), `tower-http` (compression, cors, serve-dir, trace) | https://docs.rs/axum · https://docs.rs/tower-http |
| Async runtime | `tokio` (`full`) | https://tokio.rs |
| HTTP client | `reqwest` (`rustls-tls`, `json`, `gzip`) | https://docs.rs/reqwest |
| SQLite | `rusqlite` (`bundled`) **or** `sqlx` (`sqlite`, `runtime-tokio`, `migrate`) | https://docs.rs/rusqlite · https://docs.rs/sqlx |
| Migrations (if rusqlite) | `refinery` | https://docs.rs/refinery |
| In-process cache | `moka` (`future`) | https://docs.rs/moka |
| Concurrent map (single-flight) | `dashmap` | https://docs.rs/dashmap |
| OpenAPI | `utoipa`, `utoipa-axum`, `utoipa-scalar` | https://docs.rs/utoipa |
| Validation | `garde` or `validator` | https://docs.rs/garde |
| Serialisation | `serde`, `serde_json` | https://serde.rs |
| Compression | `zstd` | https://docs.rs/zstd |
| Metrics | `metrics`, `metrics-exporter-prometheus` | https://docs.rs/metrics |
| Logging | `tracing`, `tracing-subscriber` (`json`, `env-filter`) | https://docs.rs/tracing |
| Config | `figment` or `envy` + `dotenvy` | https://docs.rs/figment |
| CLI | `clap` (`derive`) | https://docs.rs/clap |
| TLS + ACME | `axum-server` (`tls-rustls`), `rustls-acme` | https://docs.rs/rustls-acme |
| Hashing / ids | `sha2`, `hex`, `ulid` | |
| Time | `jiff` | https://docs.rs/jiff |
| Cron-style ticks | `tokio-cron-scheduler` (optional; `tokio::time::interval` is enough) | |
| Test doubles | `wiremock`, `tokio::time::pause` | https://docs.rs/wiremock |
| Static builds | `cross`, or `rustup target add x86_64-unknown-linux-musl` | https://github.com/cross-rs/cross |

## Go equivalents

| Need | Package |
|---|---|
| Router | `net/http` (Go ≥ 1.22 patterns) or `github.com/go-chi/chi/v5` |
| WS | `github.com/coder/websocket` |
| SQLite | `modernc.org/sqlite` (pure Go) or `github.com/mattn/go-sqlite3` |
| Cache | `github.com/maypok86/otter` |
| OpenAPI | `github.com/danielgtaylor/huma/v2` |
| Metrics | `github.com/prometheus/client_golang` |
| Migrations | `github.com/pressly/goose/v3` |
| Config | `github.com/caarlos0/env` |
| Compression | `github.com/klauspost/compress/zstd` |

## Riot

| | |
|---|---|
| Developer portal + API reference | https://developer.riotgames.com |
| Rate limiting (headers, types) | https://developer.riotgames.com/docs/portal#web-apis_rate-limiting |
| Routing values (platform vs regional) | https://developer.riotgames.com/docs/lol#routing-values |
| Data Dragon | https://developer.riotgames.com/docs/lol#data-dragon |
| Policies (key hygiene, ToS) | https://developer.riotgames.com/policies/general |

## SQLite operations

| | |
|---|---|
| WAL mode | https://www.sqlite.org/wal.html |
| `VACUUM INTO` (online backup) | https://www.sqlite.org/lang_vacuum.html#vacuuminto |
| Pragmas | https://www.sqlite.org/pragma.html |
| "Appropriate uses" — when SQLite is the right server DB | https://www.sqlite.org/whentouse.html |
| Litestream (streaming SQLite replication, if you ever want off-box continuous backup) | https://litestream.io |

## Reference implementations worth reading

- **Riot rate limiters in the wild** — `@fightmegg/riot-rate-limiter` (TS, cited in the v1 spec), `Riven` (Rust Riot client with its own header-driven limiter — the closest prior art to [05](05-rate-limiter.md); read its `rate_limit` module), `pantheon` (Python).
- **Single-binary + SQLite services** — `Gitea`, `Pocketbase`, `Miniflux`: the operational model v2 copies.
- **Durable jobs on SQL** — `river` (Go/Postgres) and `Oban` (Elixir) for claim-loop and dedupe patterns; both map cleanly to the `jobs` table in [06](06-jobs-and-realtime.md).

## Internal

- v1 spec: `docs/riot-proxy-spec.md` — still the authority on Riot semantics (§5), TTL rationale (§8), 429 policy (§9.4), WS protocol (§11).
- v1 README "How it works" — the prose explanation of the crawl phases and archive ordering.
- `ops/grafana/riot-proxy-dashboard.json`, `ops/prometheus-alerts.yml` — reused unchanged.

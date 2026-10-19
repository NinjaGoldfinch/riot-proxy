# Dependencies

The third-party crates riot-proxy builds on, what each is for, and why some are pinned where they are. Versions are the resolved ones in `Cargo.lock` at the verified commit.

## Policy

- `Cargo.toml` pins each dependency to its current minor; `Cargo.lock` pins the exact patch and is committed. Release and edge builds use `--locked`.
- `rust-toolchain.toml` pins the toolchain to `1.98.1` with `rustfmt`, `clippy` and the `x86_64-unknown-linux-musl` target. Bump it deliberately; the builder tag in `Dockerfile` (`rust:1.98.1-alpine`) must move with it.
- The static musl binary must stay under 24 MiB (CI, ADR-162). Dependencies compile at `opt-level = "s"` and the crate itself at `3`; `lto = "fat"`, `codegen-units = 1`, `strip = true` (ADR-061).
- Dev and test builds carry line tables only, and dependencies no debug info at all (`[profile.dev]`, CI-05, ADR-177), which keeps `target/` and link times small.
- Every reqwest client is built with explicit TLS roots from `webpki-root-certs`, because the `FROM scratch` image has no CA bundle (ADR-007, ADR-013).

## Runtime dependencies

| Crate | Version | Purpose |
|---|---|---|
| `tokio` | 1.53.1 | Async runtime; `worker_threads = min(cores, 4)` (`src/cli/mod.rs`) |
| `axum` | 0.8.9 | HTTP router and handlers; `ws` feature for `/v1/ws` |
| `tower`, `tower-http` | 0.5.3, 0.7.1 | Middleware: catch-panic, compression, CORS, static files (`fs`), body limit, request tracing |
| `reqwest` | 0.13.5 | Riot and Data Dragon client: `json`, `gzip`, `rustls`, `http2`; default features off |
| `webpki-root-certs` | 1.0.9 | TLS roots compiled in, so no CA bundle is needed at runtime |
| `rusqlite` | 0.39.0 | SQLite with the `bundled` engine (libsqlite3-sys 0.37). Held at 0.39 because `refinery` 0.9 accepts rusqlite up to 0.39 (ADR-005) |
| `refinery` | 0.9.2 | Embedded SQL migrations (`src/db/migrations/V*.sql`), applied on boot and by `riot-proxy migrate` |
| `moka` | 0.12.16 | In-memory L1 response cache (weight-bounded by `CACHE_L1_MAX_MB`) and the consumer-key lookup cache |
| `dashmap` | 6.2.1 | Concurrent maps (single-flight in-flight table and similar) |
| `object_store` | 0.14.2 | S3-compatible object storage for packed bodies (`aws` feature) and the `file://` local store; shares reqwest and aws-lc-rs with the proxy (OBJ-01, ADR-146) |
| `zstd` | 0.14.0 | Compression of archived match and timeline bodies and of pack indexes |
| `serde`, `serde_json` | 1.0.229, 1.0.151 | JSON; `raw_value` lets passthrough routes forward Riot's bytes untouched |
| `figment` | 0.10.19 | Configuration from flags, environment and `.env` |
| `clap` | 4.6.7 | CLI with `derive` and `env` features |
| `utoipa`, `utoipa-axum`, `utoipa-scalar` | 6.0.0, 0.3.0, 0.4.0 | OpenAPI document generated from handlers; `/docs` reference UI |
| `metrics`, `metrics-exporter-prometheus` | 0.24.6, 0.18.3 | Metrics with v1's names; the exporter's own HTTP listener is off and `/metrics` is an axum route |
| `tracing`, `tracing-subscriber` | 0.1.44, 0.3.23 | Structured logs, JSON or pretty, filtered by `LOG_LEVEL` |
| `jiff` | 0.2.37 | Dates and times (not `chrono`, by design/03) |
| `ulid` | 3.0.0 | Request ids, job ids, pack ids |
| `sha2`, `hex`, `base64` | 0.11.0, 0.4.3, 0.23.1 | Consumer-key hashing, pack checksums, encodings |
| `getrandom` | 0.4.3 | Key generation |
| `thiserror`, `anyhow` | 2.0.20, 1.0.104 | Error types in the library; `anyhow` at the CLI edge |
| `bytes`, `futures-util`, `tokio-stream` | 1.12.1, 0.3.34, 0.1.19 | Byte buffers, stream and sink helpers (WebSocket, broadcast) |
| `libc` | 0.2.189 | Disk free-space probe for `data_dir_free_bytes` and the backup guard |

## Development dependencies

| Crate | Version | Purpose |
|---|---|---|
| `wiremock` | 0.6.5 | Mock Riot upstream in integration tests |
| `tempfile` | 3.27.0 | Per-test SQLite files and data directories |
| `insta` | 1.48.0 | Snapshots (`tests/snapshots/`): OpenAPI document, error bodies, WS frames |
| `proptest` | 1.11.0 | Property tests for the limiter (`proptest-regressions/`) |
| `tokio-test`, `tokio-tungstenite` | 0.4.5, 0.29.0 | Async test helpers; WebSocket client for `/v1/ws` tests |
| `async-trait` | 0.1.92 | Test wrappers around an `object_store` back end |

## Feature flags

| Feature | Effect |
|---|---|
| `dev-cli` | Adds `riot-proxy riot get` and `riot-proxy riot record` for raw upstream calls and fixture recording. Never in release builds |
| `postgres` | A compile-only `Store` stub for Postgres (P8-04, ADR-062); CI checks it builds and its unit tests pass |

## Node tooling (tests only)

- `tests/dom/`: jsdom and `playwright-core` drive the three embedded pages in CI's `ui` job; Node 22.
- `acceptance/`: v1's vitest suite against a mock Riot; Node 22.

> **TODO (unverified):** the package lists in `tests/dom/package.json` and `acceptance/package.json` are not reproduced here; read the lockfiles.

---
**Sources:** `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `Dockerfile`, `src/cli/mod.rs:L139-L142`, `docs/design/03-architecture.md#cross-cutting-decisions`
**Last verified:** 2026-10-19 against commit `52fa67d`

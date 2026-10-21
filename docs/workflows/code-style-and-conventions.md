# Code style and conventions

The formatting, lint, naming, error-handling, logging and commit rules the repository enforces, and where each one is configured.

## Formatting and lints

| Rule | Where | Enforced by |
|---|---|---|
| `rustfmt` with `edition = "2024"`, `max_width = 110`, `use_field_init_shorthand = true` | `rustfmt.toml` | `cargo fmt --all --check` (CI `fmt`) |
| Clippy with warnings as errors on all targets and features | `justfile` `lint`, CI `clippy` | `cargo clippy --all-targets --all-features -- -D warnings` |
| `unwrap_used`, `expect_used`, `panic` warn (so `-D warnings` fails them) | `Cargo.toml` `[lints.clippy]` | clippy |
| The three are allowed in tests; functions may take up to 8 arguments | `clippy.toml` | clippy |
| `RUSTFLAGS=-D warnings` for every CI build | `.github/workflows/ci.yml` | CI |
| Rust 2024 edition, `rust-version = "1.98"`, toolchain `1.98.1` | `Cargo.toml`, `rust-toolchain.toml` | cargo |

Run `just lint` before pushing; it is exactly what CI runs.

## Module layout and naming

- One module per concern, following `docs/design/03-architecture.md#module-layout`: `http/` (middleware, auth, errors), `routes/` (handlers), `riot/` (client, routing, endpoints, limiter), `cache/`, `fetcher.rs`, `jobs/`, `archive/`, `db/`, `ws/`, `events/`, `metrics.rs`, `telemetry.rs`, `cli/`.
- Unit tests live beside the code in `src/<module>/tests.rs` or `#[cfg(test)] mod tests`; integration tests in `tests/*.rs`, each a module of the single `tests/it.rs` binary (`autotests = false`, CI-04, ADR-176), sharing `tests/common/`; a new file must be added to `tests/it.rs` or a guard test fails. `tests/telemetry.rs` keeps its own binary because it owns the process-global metrics recorder.
- JSON field names on the wire are camelCase (`requestId`, `gameName`, `sqliteReadersFree`); Rust fields are snake_case with `#[serde(rename_all = "camelCase")]`.
- Configuration variables are SCREAMING_SNAKE environment names, mirrored one-to-one by `--kebab-case` flags where a flag exists.
- Job kinds are `noun:verb` strings (`archive:match`, `aggregate:analytics`, `archive:offload`). Event topics are dotted (`crawl.phase`, `analytics.updated`).
- Metric names are v1's, with new metrics using design names without the `proxy_` prefix (owner decision at the P0 gate, PROGRESS.md). Use the constants in `src/metrics.rs`, never string literals.

## Errors

- Library code returns `Result` with `thiserror` enums; `anyhow` only at the CLI edge (`src/cli/`). Prefer `?` over `unwrap`/`expect`, which clippy flags.
- HTTP handlers return `http::ApiError`. Its codes, statuses and the envelope `{"error":{"code","message","requestId","retryAfter?"}}` are byte-identical to v1 (ADR-011); see [../api/errors.md](../api/errors.md). `requestId` is filled in by the middleware.
- An unknown route or wrong method is `404 NOT_FOUND` with `No route for <METHOD> <url>`, as in v1's Fastify handler (`src/app.rs`).
- A panic in a handler becomes `500 INTERNAL` through `CatchPanicLayer` (`src/app.rs`).
- A failing archive or object store never fails a request: the fetcher falls through to Riot (design/12 §Goals).

## Async and SQLite

- No blocking calls in async functions. SQLite goes through `Db::write` and `Db::read` (a dedicated writer thread and a reader pool, `src/db/mod.rs`); the limiter's `std::sync::Mutex` is never held across an `.await`.
- Timestamps are unix milliseconds in `INTEGER` columns; JSON bodies are `TEXT`; match and timeline bodies are zstd `BLOB`s.
- Migrations are forward-only SQL files `src/db/migrations/V<nnnn>__<name>.sql` (refinery naming, ADR-010); see [adding-a-migration.md](./adding-a-migration.md).

## Logging

- `tracing` with JSON output by default (`pretty` when stdout is a terminal), filtered by `LOG_LEVEL` (`src/telemetry.rs`).
- Every request runs inside an `http` span carrying `request_id`, `method`, `path` and, once authenticated, `consumer`; the id is echoed as `X-Request-Id`.
- Log at `info` for lifecycle events, `warn` for recoverable failures (a request error, a store read that fell back), `error` for things an operator must act on. Never log a key: `RIOT_API_KEY` is omitted from `/v1/admin/info` and the object-storage keys are `[REDACTED]`.
- The bootstrap admin key banner goes to stderr, not the JSON log (PROGRESS.md notes).

## Riot semantics

Never invent them. Every header, route, status code and TTL comes from `docs/design/`, the v1 repository (`../riot-proxy-v1`, reference only) or the Riot developer portal. Unknown means stop and ask (CLAUDE.md).

## Secrets

`RIOT_API_KEY`, consumer keys and object-storage keys live only in `.env` (gitignored), `/etc/riot-proxy/env` on the host, and never in CI. Fixtures are redacted; CI's `test` job greps for `RGAPI-` and fails on a hit.

## Commits, branches and pull requests

- Conventional Commits, scoped to the module, with the task id in the footer:

  ```
  feat(limiter): observe X-App-Rate-Limit headers and reconfigure windows

  Task: P2-03
  ```

- Branch names: `<type>/<task-id>-<slug>`, type in `feat | fix | test | chore | docs | ci`.
- One task, one branch, one pull request, squash-merged. Never commit to `main`. Base every branch on `origin/main`; stacked pull requests have been merged into dead branches before.
- Every pull request ships tests for the behaviour it adds or changes, and fills `.github/PULL_REQUEST_TEMPLATE.md` (task, what changed, how tested, design conformance, checklist).
- A new or reshaped read route updates `src/ui/showcase.html` (or its `notShowcased` list) and `docs/design/11-showcase.md`; `tests/ui.rs` fails otherwise.
- Record non-trivial decisions as `ADR-nnn` in `docs/DECISIONS.md` and tick the task in `PROGRESS.md` with the pull request number.
- Commit messages and pull request bodies are plain: no tooling attribution, no generated-by lines.

## Documentation

- Design docs under `docs/design/` are the authority; this `docs/` set describes what is built and links to them.
- British/NZ spelling, present tense, second person for procedures.
- Every file here ends with a `Sources` and `Last verified` footer; see [../_meta/doc-maintenance.md](../_meta/doc-maintenance.md).

---
**Sources:** `CLAUDE.md`, `docs/IMPLEMENTATION.md` §0.1–0.6, `rustfmt.toml`, `clippy.toml`, `Cargo.toml` `[lints.clippy]`, `.github/workflows/ci.yml`, `src/app.rs:L190-L201`, `.github/PULL_REQUEST_TEMPLATE.md`, `PROGRESS.md` (Owner review at the P0 gate; Notes for the next task)
**Last verified:** 2026-10-21 against commit `f8384be`

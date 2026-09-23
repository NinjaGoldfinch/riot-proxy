# CLAUDE.md — riot-proxy v2

Single-binary Rust proxy in front of the Riot Games API. Design lives in `docs/design/`; the task list and working rules live in `docs/IMPLEMENTATION.md`. Read both before doing anything.

## Non-negotiables

- **One task → one branch → one PR → squash-merge.** Never commit to `main`. Never batch tasks.
- **Every PR ships tests** for the behaviour it adds or changes. No exceptions.
- **CI green before done:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, musl build.
- **Never invent Riot semantics.** Source every header, route, status code and TTL from `docs/design/`, `../riot-proxy-v1`, or the Riot developer portal. Unknown → ask.
- **v1 is reference, not source.** Port its tests and contract artefacts; do not translate its files.
- **No secrets.** `RIOT_API_KEY` only in `.env` / CI secrets. Fixtures are redacted (CI greps for `RGAPI-`).
- **Stop and ask** on ambiguity, unmet acceptance criteria, or a dependency that doesn't match the plan.
- **Log decisions** in `docs/DECISIONS.md` (ADR-nnn). **Tick tasks** in `PROGRESS.md` with the PR number.

## Layout

```
src/            see docs/design/03-architecture.md#module-layout
tests/          integration (wiremock + tempfile SQLite), snapshots (insta), replay fixtures
acceptance/     v1's black-box suite (vitest), run with `just acceptance`
docs/design/    the v2 design docs — the authority
docs/contract/  v1 openapi.json + endpoint list used for parity checks
```

## Commands

```
just dev        # cargo run -- serve with .env
just test       # cargo test
just lint       # fmt + clippy
just cov        # cargo llvm-cov
just musl       # static build
just acceptance # start server + wiremock, run acceptance/
```

## Conventions

- Conventional Commits, module-scoped: `feat(limiter): …`, footer `Task: P2-03`.
- Branch: `<type>/<task-id>-<slug>`.
- Unix-ms `INTEGER` timestamps everywhere; JSON in `TEXT`; match bodies as zstd `BLOB`.
- `ApiError` codes, headers (`X-Cache`, `X-Cache-Age`, `X-RateLimit-*`, `X-Request-Id`) and metric names are **byte-identical to v1**.
- No blocking calls in async fns — SQLite goes through `Db::write` / `Db::read`.
- Prefer `?` + `thiserror` over `unwrap`/`expect` (clippy warns on both).

## Phase gates

After a phase's exit check passes: paste the result into `PROGRESS.md`, tag `phase-P<n>`, and **pause for owner review** before the next phase.

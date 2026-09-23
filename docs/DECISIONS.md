# Architecture Decision Records

## ADR-001 — Single process, single binary (2026-09-16)
Accepted. A Riot key's rate limit bounds throughput; multiple processes only add coordination. See docs/design/03.

## ADR-002 — SQLite (WAL) as the only default store (2026-09-16)
Accepted. Postgres behind a feature flag for > ~50 GB archives. See docs/design/02, 04.

## ADR-003 — Rust / axum / tokio (2026-09-16)
Accepted. Go was the runner-up. See docs/design/02.

## ADR-004 — rusqlite vs sqlx
Pending — decide in P0-01. Default recommendation: rusqlite (bundled) + refinery; sqlx if compile-time query checking proves worth the async complexity.

## ADR-005 — Migrations tool
Pending — decide in P0-01.

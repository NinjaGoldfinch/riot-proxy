# ADR: one process, one static binary

**Context.** v1 was five containers, two databases and a queue broker guarding a single Riot key. A Riot key has one application-level rate limit shared by every request made with it, so a second proxy replica does not add throughput; it splits the budget and then needs coordination to agree on the split (`docs/design/03-architecture.md#why-one-process-is-not-a-compromise`).

**Decision.** riot-proxy is a single Rust binary (axum on tokio) that serves HTTP, runs the limiter, the caches, the scheduler and every job in one process, with all state in one SQLite file and one data directory. Rust was chosen over Go after a scored comparison (`docs/design/02-stack-options.md`). Availability during deploys comes from a sub-second restart with a persisted limiter checkpoint and an L2-warmed cache, not from replicas.

**Consequences.** No Redis, no broker, no cross-process single-flight. Vertical limits are the host's RAM and disk, not CPU. The process must never block its async runtime, so SQLite goes through a writer thread and a reader pool (`src/db/mod.rs:152-183`). A deploy is a restart; running jobs resume from the `jobs` table on the next boot (`src/jobs/scheduler.rs:963-973`). Horizontal scaling is an explicit non-goal (`docs/design/README.md#non-goals-unchanged-from-v1`).

**Status.** Accepted, in force (ADR-001, ADR-003, 2026-09-16).

**Evidence.** `docs/DECISIONS.md` ADR-001 and ADR-003; `src/main.rs`; `src/cli/serve.rs:43-394` (everything spawned from one `serve`); `Cargo.toml` (`[profile.release]` static, stripped); CI `build-musl` job.

---
**Sources:** `docs/DECISIONS.md:L3-L14`, `docs/design/03-architecture.md`, `src/cli/serve.rs:L43-L394`, `src/db/mod.rs:L152-L183`, `src/jobs/scheduler.rs:L963-L973`
**Last verified:** 2026-10-19 against commit `52fa67d`

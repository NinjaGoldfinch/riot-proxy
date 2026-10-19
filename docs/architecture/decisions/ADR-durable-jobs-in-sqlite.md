# ADR: a durable job table instead of a queue broker

**Context.** v1 used BullMQ on Redis. With one process the broker only adds a dependency; what the jobs need is durability across restarts and awareness of the Riot limiter.

**Decision.** Jobs are rows in the SQLite `jobs` table with a kind, a JSON payload, a priority, a dedupe key enforced by a partial unique index while pending or running, and a lane (ADR-047). There is no in-memory mirror; the dashboard counts read an index (ADR-137). Periodic work comes from in-process ticks whose first firing is immediate at boot (ADR-048). Workers claim with one `UPDATE … RETURNING` that takes the best ready row per rate-limit lane and skips lanes the limiter reports blocked; a job whose fetch would wait longer than `JOB_YIELD_BUDGET_MS` yields its worker and re-queues itself (ADR-089). Kinds that never call Riot can be capped per kind with `JOB_KIND_LIMITS`, by default one analytics rebuild and one offload at a time (ADR-134). Failures retry five times with exponential backoff; `done` rows are trimmed after seven days.

**Consequences.** A crash loses nothing: `running` rows go back to `pending` on boot. Large backlogs are fine (production held 1.19 million pending crawl jobs) but purging them needed a chunked admin route (ADR-137). Everything a job does is visible in `/v1/admin/jobs*` and the dashboard, and traced in memory per job for the activity views.

**Status.** Accepted, in force (ADR-047, ADR-048 2026-09-30; ADR-089 2026-10-09; ADR-134, ADR-137 2026-10-13).

**Evidence.** `src/jobs/scheduler.rs`, `src/jobs/lanes.rs`, `src/jobs/ticks.rs`, `src/db/store.rs:98-133`, `src/db/migrations/V0001__init.sql:33-45`, `V0007__job_lane.sql`, `V0022__jobs_state_kind.sql`, `tests/jobs.rs`, `tests/lanes.rs`.

---
**Sources:** `docs/DECISIONS.md:L349-L368,L1001-L1013,L1453-L1482`, `src/jobs/scheduler.rs:L40-L42,L225-L269,L963-L1025`, `src/db/store.rs:L98-L133`, `src/db/migrations/V0001__init.sql:L33-L45`
**Last verified:** 2026-10-19 against commit `52fa67d`

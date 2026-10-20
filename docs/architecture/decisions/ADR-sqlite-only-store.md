# ADR: SQLite as the only default store

**Context.** With one process there is no coordination problem for a database to solve. The archive grows by tens of kilobytes per match; everything else is small.

**Decision.** One SQLite file in WAL mode with `synchronous=NORMAL`, accessed through `rusqlite` (bundled engine) with a dedicated writer thread and a reader pool; migrations embedded with `refinery` and applied on boot (`src/db/mod.rs:26-42,148-155,343-345`). Postgres exists only as a compile-only `Store` seam behind the `postgres` feature so the door stays open without a rewrite; `postgres://` is refused at runtime in every build (`src/config.rs:628-633`, `src/db/store.rs:172-206`).

**Consequences.** All writes serialise on one lock, so CPU-heavy jobs that write (analytics rebuilds, re-extraction) are capped to one at a time by default (`JOB_KIND_LIMITS`). Backups are a `VACUUM INTO` copy, which needs free space of about the database's size (ADR-136). The file never shrinks on its own; reclaiming space is a copy-and-swap with strict rules about the WAL (`deploy/README.md` §Reclaiming). When raw bodies pushed the file past 50 GB, the answer was object storage (see [ADR-packs-in-object-storage.md](./ADR-packs-in-object-storage.md)), not Postgres. `rusqlite` is held at 0.39 because `refinery` 0.9 accepts no newer version (ADR-005).

**Status.** Accepted, in force (ADR-002 2026-09-16; ADR-004, ADR-005 2026-09-23; ADR-062 for the seam).

**Evidence.** `docs/DECISIONS.md` ADR-002, 004, 005, 062; `src/db/mod.rs`; `src/db/store.rs`; `Cargo.toml` (`rusqlite = "0.39"` with the comment); CI `clippy` job's Postgres check.

---
**Sources:** `docs/DECISIONS.md:L6-L18,L714-L730`, `src/db/mod.rs:L26-L42,L148-L155,L343-L345`, `src/db/store.rs:L172-L206`, `src/config.rs:L628-L633`, `Cargo.toml`
**Last verified:** 2026-10-20 against commit `b7b363c`

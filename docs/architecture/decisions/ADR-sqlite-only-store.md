# ADR: SQLite as the only default store

**Context.** With one process there is no coordination problem for a database to solve. The archive grows by tens of kilobytes per match; everything else is small.

**Decision.** One SQLite file in WAL mode with `synchronous=NORMAL`, accessed through `rusqlite` (bundled engine) with a dedicated writer thread and a reader pool; migrations embedded with `refinery` and applied on boot (`src/db/mod.rs:53-69,194-202,533-535`). Postgres exists only as a compile-only `Store` seam behind the `postgres` feature so the door stays open without a rewrite; `postgres://` is refused at runtime in every build (`src/config.rs:719-724`, `src/db/store.rs:213-247`).

**Consequences.** All writes serialise on one lock, so CPU-heavy jobs that write (analytics rebuilds, re-extraction) are capped to one at a time by default (`JOB_KIND_LIMITS`). Backups are a `VACUUM INTO` copy, which needs free space of about the database's size (ADR-136). The file never shrinks on its own; reclaiming space is a copy-and-swap with strict rules about the WAL (`deploy/README.md` §Reclaiming). When raw bodies pushed the file past 50 GB, the answer was object storage (see [ADR-packs-in-object-storage.md](./ADR-packs-in-object-storage.md)), not Postgres. `rusqlite` is held at 0.39 because `refinery` 0.9 accepts no newer version (ADR-005).

**Status.** Accepted, in force (ADR-002 2026-09-16; ADR-004, ADR-005 2026-09-23; ADR-062 for the seam).

**Evidence.** `docs/DECISIONS.md` ADR-002, 004, 005, 062; `src/db/mod.rs`; `src/db/store.rs`; `Cargo.toml` (`rusqlite = "0.39"` with the comment); CI `clippy` job's Postgres check.

---
**Sources:** `docs/DECISIONS.md:L6-L18,L714-L730`, `src/db/mod.rs:L27-L44,L152-L171,L465-L467`, `src/db/store.rs:L213-L247`, `src/config.rs:L719-L724`, `Cargo.toml`
**Last verified:** 2026-11-09 against commit `7531561`

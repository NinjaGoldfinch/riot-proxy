# ADR: timelines are fetched and archived with their match

**Context.** v1 archived timelines only on request. Build analytics (skill order, item order) need timelines, and a timeline fetched once through the API is immutable and worth keeping.

**Decision.** Every timeline fetched through the API is archived (ADR-085). Archive jobs (polls, backfills, ladder crawls) fetch each match's timeline too unless `ARCHIVE_TIMELINES=false` (ADR-098; default `true`, `src/config.rs:740`). A match and its timeline are archived together: storing one half queues the other as an `archive:match` job at priority 0 (ADR-118, `src/archive/mod.rs:88-110`); `timelines` lost its foreign key to `matches` so either half may arrive first (migration `V0013`). Missing timelines of already-archived ranked matches are backfilled at the lowest priority, bounded by `TIMELINE_BACKFILL_PATCHES` (ADR-133, default 0).

**Consequences.** One more Riot request per match and a body five to six times larger (about 62 KB against 11 KB compressed, `docs/design/12-object-storage-and-hosting.md` §Why). This is what made the SQLite file double in a day on the crawl and drove the move to object storage. The site's match page can show gold and build data without a second fetch.

**Status.** Accepted, in force (ADR-085 2026-10-08; ADR-098, ADR-118, ADR-133 2026-10-10 to 2026-10-11).

**Evidence.** `src/jobs/archive.rs:263-307`, `src/archive/mod.rs:88-110`, `src/jobs/timelines.rs`, `src/db/migrations/V0013__timelines_standalone.sql`, `tests/archive.rs`, `tests/timelines.rs`.

---
**Sources:** `docs/DECISIONS.md:L968-L973,L1094-L1101,L1274-L1283,L1443-L1452`, `src/config.rs:L740`, `src/archive/mod.rs:L88-L110`, `src/jobs/archive.rs:L263-L307`, `docs/design/12-object-storage-and-hosting.md` §Why
**Last verified:** 2026-10-26 against commit `220cdef`

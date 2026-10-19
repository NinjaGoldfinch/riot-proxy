# ADR: raw bodies packed into an S3 bucket, R2 as the replica

**Context.** By 2026-10-13 the production SQLite file was 20 GB, the disk was at 97 %, and the crawl had a million matches queued. The bulk of the file was immutable zstd bodies the proxy reads rarely. Onidel's object storage caps a bucket at 3 million objects by default, which two objects per match would pass within months.

**Decision.** Bodies stay in SQLite for a 14-day local window and are packed, thousands to a 64 MiB object sorted by match id, into one Onidel bucket in the same data centre (`packs/<kind>/<yyyy>/<mm>/<ulid>.pack`), matches and timelines separately (ADR-146). The `object_store` crate is the client (ADR-162). SQLite keeps the map (`body_packs`, `body_locations`) and a freed row keeps an empty blob (ADR-163). Packing happens as soon as a pack of a kind is full; freeing only when the window ends (ADR-164). The map is also snapshotted daily to `index/` and written at the end of every pack, so it can be rebuilt without a backup (ADR-165). Cloudflare R2 holds an hourly `rclone sync` replica the proxy never reads; restoring is repointing `ARCHIVE_OBJECT_URL`'s endpoint. SQLite backups go to a separate R2 bucket every six hours.

**Consequences.** A read outside the window costs a ranged GET of a few milliseconds; a failure falls back to Riot. A bucket outage delays packing but loses nothing. The SQLite file stops growing by bodies and the nightly backup shrinks from 22 GB to a few. Disk pressure is now governed by `ARCHIVE_BODY_LOCAL_DAYS`. Compaction of packs with deleted bodies (OBJ-06) is designed but not built. Two providers exist because Onidel is the cheap same-DC primary and R2 is the off-vendor copy for the day the Onidel account is gone.

**Status.** Accepted, in production since v2.4.0 (2026-10-13 to 2026-10-14) (ADR-146, ADR-162 to ADR-165).

**Evidence.** `docs/design/12-object-storage-and-hosting.md`, `deploy/README.md`, `src/archive/{pack,objects,offload,recovery}.rs`, `src/jobs/offload.rs`, `src/db/migrations/V0023__body_packs.sql`, `tests/offload.rs`.

---
**Sources:** `docs/DECISIONS.md:L1555-L1562,L1696-L1750`, `docs/design/12-object-storage-and-hosting.md`, `deploy/README.md`, `src/archive/pack.rs:L1-L11`, `src/jobs/offload.rs:L146-L195`
**Last verified:** 2026-10-19 against commit `52fa67d`

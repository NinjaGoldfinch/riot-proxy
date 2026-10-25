# Migrations

How schema migrations are written, applied and recorded, the complete list V0001 to V0029, and what to expect from the expensive ones in production. The resulting tables are in [schema.md](schema.md) and [schema-analytics.md](schema-analytics.md); the procedure for adding one is in [../workflows/adding-a-migration.md](../workflows/adding-a-migration.md).

## How they run

- **Embedded at compile time.** `refinery::embed_migrations!("src/db/migrations")` compiles every `V####__name.sql` into the binary (`src/db/mod.rs:22-24`), so a release binary carries exactly the migrations its schema expects. refinery only recognises the `^[UV]\d+__\w+\.sql$` naming (ADR-010, `docs/DECISIONS.md:51`).
- **On every open.** `Db::open` applies the pragmas to the writer connection, runs `migrate`, then `PRAGMA optimize`, and only then opens the reader pool and starts the writer thread (`src/db/mod.rs:147-171`). `riot-proxy serve` and every CLI command that touches the database go through `Db::open`, so all of them migrate (`src/cli/serve.rs:51-54`, `src/cli/mod.rs:136-141`).
- **`riot-proxy migrate`.** Creates the parent directory, opens a raw `rusqlite::Connection` without the pragmas, runs the same `migrate` and prints `<path>: up to date` or `<path>: applied V…` (`src/cli/mod.rs:144-168`). Use it to migrate ahead of a restart when a migration is known to be slow.
- **Grouped, one transaction.** `runner().set_grouped(true).run(conn)`: every pending migration runs in a single transaction, so a failure part-way leaves the database at the previous version (`src/db/mod.rs:361-365`). Already-applied versions are skipped, which is what makes migrating on every boot safe.
- **Forward only.** There are no down migrations. A schema change SQLite cannot express as `ALTER` is done as copy into `<table>_vNN`, `DROP`, `RENAME` (V0013, V0017, V0020, V0021, V0025, V0029 follow this pattern). Rollback is restoring a backup, below.
- **History.** refinery records each version in `refinery_schema_history(version, name, applied_on, checksum)`, one row per applied file; `applied_on` is an ISO timestamp (dev database `sqlite_master`). The `checksum` column is what lets refinery notice an edited file, so never edit a merged migration; add a new one.

> **TODO (unverified):** what refinery does when an applied file's checksum no longer matches (abort or warn) is the crate's default behaviour; `src/db/mod.rs:361-365` sets only `set_grouped(true)` and the repo has no test for a divergent file.

- **Postgres.** `PgStore::migrate` returns `DbError::PostgresUnimplemented` (`src/db/store.rs:172-206`); the SQL is SQLite-only (partial indexes, `WITHOUT ROWID`).

## The list

PR numbers from `PROGRESS.md`; dates are the accepting ADR's in `docs/DECISIONS.md`.

| File | Date, PR | What it does | Task, ADR |
|---|---|---|---|
| [`V0001__init.sql`](../../src/db/migrations/V0001__init.sql) | 2026-09-23, #4 | `consumers`, `cache` + `cache_hard`, `limiter_state`, `jobs` + `jobs_dedupe`, `jobs_claim`, `metrics_history` | P0-04, ADR-010 |
| [`V0002__archive.sql`](../../src/db/migrations/V0002__archive.sql) | 2026-09-26, #40 | `players`, `matches`, `timelines` (FK to `matches`), `match_facts`, first analytics and ladder tables (all later replaced) | P5-01, ADR-039 |
| [`V0003__cs_and_duration.sql`](../../src/db/migrations/V0003__cs_and_duration.sql) | 2026-09-30, #43 | `matches.game_duration`, `match_facts.cs` | P5-04, ADR-043 |
| [`V0004__ladder.sql`](../../src/db/migrations/V0004__ladder.sql) | 2026-10-03, #56 | drops V0002's ladder tables; `ladder_crawls`, `ladder_entries`, `crawl_legs`, `crawl_match_ids` in v1's shape | P7-02, ADR-054 |
| [`V0005__analytics.sql`](../../src/db/migrations/V0005__analytics.sql) | 2026-10-03, #58 | `matches.remake`, `matches.facts_version`; `match_facts.gold/damage/vision`; `match_bans`; drops V0002's analytics; `analytics_slices`, `champion_stats`, `champion_bans`, `champion_matchups`, `champion_items`, `champion_runes`, `champion_spells` | P7-04, ADR-056 |
| [`V0006__analytics_runs.sql`](../../src/db/migrations/V0006__analytics_runs.sql) | 2026-10-03, #60 | `analytics_runs` | P7-06, ADR-058 |
| [`V0007__job_lane.sql`](../../src/db/migrations/V0007__job_lane.sql) | 2026-10-09, #107 | `jobs.lane`, `jobs.method`, `jobs_lane_claim`, `jobs_lane_running` | SCH-01, ADR-089 |
| [`V0008__apex_capped.sql`](../../src/db/migrations/V0008__apex_capped.sql) | 2026-10-09, #117 | `ladder_crawls.apex_capped` | LAD-01, ADR-097 |
| [`V0009__cache_fetched_at.sql`](../../src/db/migrations/V0009__cache_fetched_at.sql) | 2026-10-09, #125 | `cache.fetched_at` | SITE-01, ADR-103 |
| [`V0010__player_ranks.sql`](../../src/db/migrations/V0010__player_ranks.sql) | 2026-10-09, #127 | `player_ranks` | DEV-28, ADR-105 |
| [`V0011__rank_lookups.sql`](../../src/db/migrations/V0011__rank_lookups.sql) | 2026-10-09, #134 | `rank_lookups` (seeded from `player_ranks`), `rank_lookup_queue` | DEV-29, ADR-111 |
| [`V0012__match_builds.sql`](../../src/db/migrations/V0012__match_builds.sql) | 2026-10-09, #142 | `match_builds` | BLD-01, ADR-117 |
| [`V0013__timelines_standalone.sql`](../../src/db/migrations/V0013__timelines_standalone.sql) | 2026-10-10, #144 | rebuilds `timelines` without its FK, so a timeline can arrive before its match | TL-01, ADR-118 |
| [`V0014__champion_builds.sql`](../../src/db/migrations/V0014__champion_builds.sql) | 2026-10-10, #145 | `champion_builds`, `champion_build_parts` | BLD-02, ADR-120 |
| [`V0015__analytics_totals.sql`](../../src/db/migrations/V0015__analytics_totals.sql) | 2026-10-10, #148 | `analytics_match_totals`, `champion_ban_totals`, backfilled from existing slices | SITE-08, ADR-123 |
| [`V0016__match_tiers.sql`](../../src/db/migrations/V0016__match_tiers.sql) | 2026-10-10, #153 | `match_tiers` (dropped again by V0019) | THR-02, ADR-127 |
| [`V0017__tier_side_keys.sql`](../../src/db/migrations/V0017__tier_side_keys.sql) | 2026-10-10, #155 | `analytics_unsplit`; rebuilds seven analytics tables with `tier` and `side` keys, existing rows copied with `''` | FLT-01, ADR-129 |
| [`V0018__timeline_gaps.sql`](../../src/db/migrations/V0018__timeline_gaps.sql) | 2026-10-11, #161 | `timeline_gaps` | TL-02, ADR-133 |
| [`V0019__tier_on_facts.sql`](../../src/db/migrations/V0019__tier_on_facts.sql) | 2026-10-13, #166 | `match_facts.tier` copied from `match_tiers`; drops `match_tiers` | DSK-04, ADR-138 |
| [`V0020__without_rowid_derived.sql`](../../src/db/migrations/V0020__without_rowid_derived.sql) | 2026-10-13, #167 | rebuilds `match_builds`, `match_bans` and eleven analytics tables `WITHOUT ROWID`, copied in key order | DSK-06, ADR-140 |
| [`V0021__facts_without_rowid.sql`](../../src/db/migrations/V0021__facts_without_rowid.sql) | 2026-10-13, #170 | rebuilds `match_facts` `WITHOUT ROWID`; recreates `facts_player`; drops `facts_champ` | DSK-07, ADR-141 |
| [`V0022__jobs_state_kind.sql`](../../src/db/migrations/V0022__jobs_state_kind.sql) | 2026-10-13, #178 | `jobs_state_kind` covering index | ADM-02, ADR-145 |
| [`V0023__body_packs.sql`](../../src/db/migrations/V0023__body_packs.sql) | 2026-10-14, #197 | `body_packs`, `body_locations`; `packed_at`, `offloaded_at` on `matches` and `timelines`; four partial indexes | OBJ-02, ADR-163 |
| [`V0024__matches_archived.sql`](../../src/db/migrations/V0024__matches_archived.sql) | 2026-10-14, #198 | `matches_archived (match_id, archived_at)` | OBJ-03, ADR-164 |
| [`V0025__champion_stats_totals.sql`](../../src/db/migrations/V0025__champion_stats_totals.sql) | 2026-10-15, #212 | `champion_stats_totals` filled from `champion_stats`; drops `champion_stats_slice` | PERF-02, ADR-168 |
| [`V0026__floor_totals.sql`](../../src/db/migrations/V0026__floor_totals.sql) | 2026-10-15, #217 | `analytics_floor_totals`, `champion_ban_floor_totals`, backfilled through a TEMP table | FLT-03, ADR-170 |
| [`V0027__matchup_builds.sql`](../../src/db/migrations/V0027__matchup_builds.sql) | 2026-10-19, #218 | `champion_matchup_builds`, `champion_matchup_build_parts`, not backfilled | FLT-04, ADR-171 |
| [`V0028__jobs_crawl_matches.sql`](../../src/db/migrations/V0028__jobs_crawl_matches.sql) | 2026-10-20, WR-01 | `jobs_crawl_matches` partial covering index on `archive:match` | WR-01, ADR-179 |
| [`V0029__analytics_counted.sql`](../../src/db/migrations/V0029__analytics_counted.sql) | 2026-10-21, THR-03 | `analytics_counted` + partial index `analytics_uncounted`, `analytics_retract`, `analytics_stamped`, trigger `match_facts_tier_moved`; `analytics_runs` rebuilt with `kind` in its primary key | THR-03, ADR-182 |

## Expensive migrations

Every migration runs in the boot transaction before the listener opens, so a slow one is a slow start, not a half-migrated service. What has been measured:

| Migration | Cost | Where measured |
|---|---|---|
| V0017 | a copy of each analytics table, linear in rows: 12 s on a synthetic 4.6 M-row database | `docs/DECISIONS.md:1388` |
| V0019, V0020, V0021 (the DSK rebuilds) | each copies a whole table in key order; V0020 rebuilt 460 MiB of automatic indexes away on the VPS, V0021 a 253 MiB one; V0021 was held back to v2.3.0 until DSK-05's `facts:reextract` had finished on production so the copy would be dense | `V0020:1-11`, `V0021:1-8`, `PROGRESS.md:200-201` |
| V0023 | building `matches_to_pack` and `matches_to_free` reads every match body once, about 2.5 GB on production, "a few minutes at most"; the `ALTER TABLE … ADD COLUMN`s themselves are cheap on a 20 GB file | `docs/DECISIONS.md:1700`, `docs/design/12-object-storage-and-hosting.md:161-162` |
| V0024 | reads `archived_at` from every match row once, about 40 bytes a match of index | `docs/DECISIONS.md:1714` |
| V0025 | 1.4–1.9 s on a copy; 3 s on production (about 1.6–2× slower there) | `docs/DECISIONS.md:1776`, `docs/flt/FLT-03-plan.md:573` |
| V0026 | 1.36 s at 200 000 facts, 3.70 s at 600 000; estimated 25–30 s for production's 2.3 M facts | `docs/DECISIONS.md:1841` |
| V0028 | builds one index over `archive:match` rows: 1.3 s for 1.18 M jobs on a synthetic queue | ADR-179 |
| V0029 | pure DDL on empty tables plus a copy of `analytics_runs`' eight rows at most; reads no other data (6–25 ms in the plan's runs; about 10 ms at boot with the release binary on a synthetic 100 000-match archive, 2026-10-21). The cost comes after it: with `AGGREGATE_DELTA_S` > 0 the first boot queues one full rebuild of every configured ladder, one at a time, before its deltas start. By the 2026-10-20 production runs (oc1 532 s, na1 970 s, kr 1012 s, euw1 980 s) that is about 58 minutes of writer time | `V0029:1-4`, `docs/thr/THR-03-plan.md` |

> **TODO (unverified):** the actual V0026 and V0027 timings on production were not found in `docs/DECISIONS.md`, `PROGRESS.md` or `docs/flt/`; the v2.7.0 release line in `PROGRESS.md` was ticked by #226 (`52fa67d`) without recording them.

Rebuild-style migrations put the old table's pages on the free list; the file does not shrink until a reclaim (`deploy/README.md:222-270`).

## Rollback

There is no down migration. To return to an older binary whose schema predates a migration, restore the newest backup taken before the upgrade (`$DATA_DIR/backups/riot-proxy-<date>.db`, or the copy in R2 `ngf-backups`) and lose what was written since; see `docs/design/07-deployment.md` and [../deployment/backup-and-restore.md](../deployment/backup-and-restore.md). Offloaded bodies are unaffected by a database rollback as long as the restored file is newer than V0023; a file older than the pack map can be repaired with `riot-proxy packs restore-index` ([../integration/object-storage.md](../integration/object-storage.md#recovery)).

## Writing a new one

1. Name it `V00NN__<slug>.sql` with the next number; check `origin/main` first, other branches may have taken it (`docs/DECISIONS.md:51`).
2. Open with a comment naming the task and ADR, as every existing file does (`V0023:1`).
3. Prefer `ALTER TABLE … ADD COLUMN` and `CREATE INDEX`; added columns are absent from existing records and read as NULL without touching the row (`V0023:32-36`). Rebuild only when SQLite cannot alter (a constraint, `WITHOUT ROWID`), copying in key order.
4. Backfill derived rows in the migration only when a rebuild would not (V0015, V0026), and test the backfill equals a rebuild (`src/db/tests.rs`).
5. Add a `src/db/tests.rs` case seeded at the previous version, and note the production cost in the ADR.

The full procedure, with the test and ADR steps, is in [../workflows/adding-a-migration.md](../workflows/adding-a-migration.md).

---
**Sources:** `src/db/mod.rs:L22-L24`, `src/db/mod.rs:L147-L171`, `src/db/mod.rs:L361-L365`, `src/db/store.rs:L172-L206`, `src/cli/mod.rs:L136-L168`, `src/cli/serve.rs:L51-L54`, `src/db/migrations/V0001__init.sql` to `V0029__analytics_counted.sql`, `docs/thr/THR-03-plan.md`, `docs/DECISIONS.md:L15-L17`, `docs/DECISIONS.md:L51`, `docs/DECISIONS.md:L1388`, `docs/DECISIONS.md:L1700`, `docs/DECISIONS.md:L1714`, `docs/DECISIONS.md:L1776`, `docs/DECISIONS.md:L1841`, `docs/flt/FLT-03-plan.md:L573`, `PROGRESS.md:L10-L365`, `docs/design/12-object-storage-and-hosting.md:L159-L162`, `deploy/README.md:L222-L270`, `docs/_meta/scratch/persistence.md`
**Last verified:** 2026-10-26 against commit `220cdef`

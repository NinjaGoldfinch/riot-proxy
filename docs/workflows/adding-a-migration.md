# Adding a migration

How schema changes ship: one forward-only refinery SQL file under `src/db/migrations/`, embedded in the binary and applied on boot, with the SQLite rules this archive has learned the hard way and the tests and documents a new file must update.

## How migrations run

| Aspect | Behaviour |
|---|---|
| Engine | refinery, embedded at compile time by `refinery::embed_migrations!("src/db/migrations")` (`src/db/mod.rs:18-20`) |
| Naming | `V<nnnn>__<name>.sql`; refinery only recognises `^[UV]\d+__\w+\.sql$`, and the four-digit padding is this repository's convention (ADR-010, `docs/DECISIONS.md:51`). refinery logs them without the padding (`V27__matchup_builds`) |
| When | `Db::open` applies pending migrations on the writer connection, after the pragmas and before the readers open (`src/db/mod.rs:131-147`). `serve` opens that way, and so does every CLI command that touches the database (`src/cli/mod.rs:136-141`). `riot-proxy migrate` applies them on a raw connection and prints `<path>: up to date` or `<path>: applied V…` (`src/cli/mod.rs:144-165`) |
| Transaction | all pending files in **one** transaction (`set_grouped(true)`, `src/db/mod.rs:294-296`); applied versions are skipped, so running on every boot is safe |
| History | `refinery_schema_history(version, name, applied_on, checksum)`; a fresh database has 28 rows after V0028 (`src/db/tests.rs:895`) |
| Direction | forward only. There are no down migrations: every file is `V…`, and a change that must be undone is undone by restoring the previous backup (`README.md:142`; [../deployment/upgrade-and-rollback.md](../deployment/upgrade-and-rollback.md)) |

Observed on 2026-10-19: a first `serve` logged `going to batch apply 27 migrations in single transaction.` naming `V1__init` to `V27__matchup_builds`; a second one `going to batch apply 0 migrations`; `riot-proxy migrate` printed `…/riot-proxy.db: up to date`.

> **TODO (unverified):** what refinery 0.9 does when an already-applied file is edited (its `checksum` no longer matches the history row). The safe rule is the usual one: never edit a migration that has been applied anywhere, add a new file.

## Numbering

The files are `V0001__init.sql` to `V0028__jobs_crawl_matches.sql` (`src/db/migrations/`); the next free number is **V0029**. Because several sessions work on this clone and branches are always based on `origin/main`, check there before picking a number:

```bash
git fetch origin && git ls-tree --name-only origin/main src/db/migrations/ | tail -1
```

On 2026-10-19 that printed `src/db/migrations/V0027__matchup_builds.sql` at `origin/main` `e669f38`. Two branches with the same number cannot both merge; the second one renumbers and re-checks its tests.

## Rules for the SQL

### Conventions every table follows

- Timestamps are unix milliseconds in `INTEGER` columns; JSON is `TEXT`; match bodies are zstd `BLOB`s (`src/db/migrations/V0001__init.sql:1-2`; CLAUDE.md conventions). Name a timestamp `<what>_at` (`archived_at`, `packed_at`, `created_at`).
- Rows holding an encrypted Riot id carry `key_scope TEXT NOT NULL` (`docs/design/04-data-and-cache.md:63`, `417-423`).
- `PRAGMA foreign_keys = ON` on every connection (`src/db/mod.rs:27`), so a declared `REFERENCES … ON DELETE CASCADE` is enforced; declare one only where the parent really owns the rows (`timelines` lost its FK to `matches` in V0013 for that reason, ADR-118).
- Header comment: task id, ADR, and why, as every file has (`src/db/migrations/V0023__body_packs.sql:1-5`).
- The DDL in `docs/design/04-data-and-cache.md` §Schema is the authority and V0001 copied it verbatim (`V0001__init.sql:1-2`); keep design/04 in step.

### Cheap and expensive changes in SQLite

| Change | Cost | Evidence |
|---|---|---|
| `ALTER TABLE … ADD COLUMN` (nullable or with a constant default) | cheap: added columns are absent from existing rows' records and read as `NULL` without touching the row (`V0023__body_packs.sql:32-33`, `37-40`) | V0003, V0008, V0009, V0019, V0023 |
| `CREATE INDEX` on a big table | a full scan once; partial indexes are small | V0022, V0024 |
| Rebuilding a table (new PK, `WITHOUT ROWID`, dropping a column) | copy into `<table>_vNN`, `DROP`, `RENAME`, recreate indexes (`V0021__facts_without_rowid.sql:9-33`). On production this was 0.5 GiB of rows for "a minute or three of boot" (V0020, ADR-140, `docs/DECISIONS.md:1495`) and 0.7 GiB plus an index for 3 to 8 minutes, about 0.9 GiB of temporary space and as much WAL (V0021, ADR-141, `:1504`), on a database then around 20 to 22 GB (ADR-135 `:1455`, ADR-142 `:1510`). The old pages go to the free list and the file does not shrink until a `VACUUM INTO` and swap (`docs/design/12-object-storage-and-hosting.md:219-221`) | V0013, V0017, V0020, V0021 |
| A migration that touches `matches`/`timelines` bodies | the dominant cost: those rows hold the blobs; design/12 deferred dropping `body_zstd` to OBJ-05 "when the column is near-empty and the rebuild is cheap" (`docs/design/12-object-storage-and-hosting.md:212-217`) | avoid |

So: prefer `ADD COLUMN` plus a backfill job over a rebuild; when a rebuild is unavoidable, copy in key order so the new b-tree is dense (`V0021__facts_without_rowid.sql:3-4`, `30`), say the expected boot time in the ADR, and schedule the deploy with disk headroom (the backup step needs 1.2 × the file, ADR-136).

### `WITHOUT ROWID`

Derived tables keyed by most of their row are `WITHOUT ROWID`, so the key is stored once instead of in the row and again in an automatic index (ADR-140, `docs/DECISIONS.md:1490-1496`). The rule: tables with no secondary index, or whose secondary indexes already contain the key, because in a `WITHOUT ROWID` table a secondary index carries the whole primary key instead of an 8-byte rowid (`:1494`; ADR-141 dropped `facts_champ` for that reason, `:1501`). `matches`, `timelines` and `jobs` keep their rowids (large blobs; a lane index). The analytics tables put `tier` and `side` last in the key so a read for one champion seeks through the rest (`V0027__matchup_builds.sql:5-7`, `18`).

### Partial indexes end with their state columns

A partial index whose `WHERE` names state columns still lists those columns last: SQLite checks a query's terms against the row unless the index holds the columns, and in `matches` and `timelines` the state columns sit after the body, so a count would otherwise read every body (`V0023__body_packs.sql:32-36`, `41-48`; `docs/design/12-object-storage-and-hosting.md:196-202`). Tests assert the counts run as `SCAN … USING COVERING INDEX matches_to_pack` / `matches_to_free` (`src/archive/offload/tests.rs:501-514`; `src/jobs/offload/tests.rs:497-512`), so a new state column on a body table gets the same treatment.

## Tests to update

Every integration test opens a fresh tempfile database through `Db::open` (`tests/common/mod.rs:32-33`), so a new migration runs in all of them; the ones that assert the schema itself are in `src/db/tests.rs`:

| Test | What to change | Lines |
|---|---|---|
| `migrations_create_exactly_the_design_04_tables` | add the new table to the sorted list | `src/db/tests.rs:92-149` |
| `migrations_apply_once_across_reopens` | the history count, 27 → 28 | `:895`, `:899` |
| a seeded upgrade test, for anything beyond `CREATE TABLE` | run the runner with `.set_target(refinery::Target::Version(27))`, seed rows, open with `Db::open`, assert rows survive with their values and the DDL says what it should: `v0020_rebuilds_…` and `v0021_rebuilds_…` are the models | `:398-450`, `:539-562` |
| `archive_indexes_exist` / `archive_foreign_keys_behave_as_designed` | if the migration adds an index or an FK | `:167`, `:731` |

Outside `src/db/`:

- **The dev reset lists.** `routes::dev::WIPED` and `KEPT` must together name every table; a test fails otherwise, "so a new migration has to choose" (`src/routes/dev.rs:38-83`; `tests/dev_reset.rs`). Derived and fetched data go in `WIPED`, children before parents; only consumers, the limiter checkpoint and refinery's history are `KEPT`.
- **A `/v1/admin/stats` or dashboard count** over the new table needs its own `EXPLAIN QUERY PLAN` test, as the pack counts (`src/archive/offload/tests.rs:501-514`) and the job counts (`src/db/tests.rs:1218`) have.
- Run `cargo test --lib db::` then `cargo test --test it dev_reset::` and the lot ([testing.md](./testing.md)).

## Documents to update

- `docs/design/04-data-and-cache.md` §Schema (the DDL) and the ADR in `docs/DECISIONS.md` that explains the shape and the cost.
- This set: [../data/schema.md](../data/schema.md) (the table) and [../data/migrations.md](../data/migrations.md) (the list, with the version, task and ADR); [../data/models.md](../data/models.md) if a new entity.
- `PROGRESS.md` tick with the pull request number.
- If production needs preparation (disk, a long boot), a line in [../deployment/upgrade-and-rollback.md](../deployment/upgrade-and-rollback.md) and the release notes ([release-process.md](./release-process.md)).

## Worked example (hypothetical)

A `clash_teams` table for a Clash feature: one row per (key scope, platform, team id), small rows, read by key only, so `WITHOUT ROWID`; a partial index for rows still to be refreshed, ending with its state column.

```sql
-- src/db/migrations/V0029__clash_teams.sql (hypothetical)
-- CLASH-01 (ADR-NNN): Clash teams a tracked player belongs to, refreshed by
-- clash:sync. Keyed by the whole row, so WITHOUT ROWID (ADR-140); read by key
-- and by what is due, never by rowid.
CREATE TABLE clash_teams (
  key_scope    TEXT NOT NULL,
  platform     TEXT NOT NULL,
  team_id      TEXT NOT NULL,
  puuid        TEXT NOT NULL,
  body         TEXT NOT NULL,                    -- JSON, Riot's TeamDto as received
  fetched_at   INTEGER NOT NULL,                 -- unix ms
  refresh_after INTEGER,                         -- unix ms; NULL once the tournament is over
  PRIMARY KEY (key_scope, platform, team_id)
) WITHOUT ROWID;
-- Rows due for a refresh; the state column ends the index (design/12 §As built).
CREATE INDEX clash_teams_due ON clash_teams(platform, refresh_after)
  WHERE refresh_after IS NOT NULL;
```

Then: add `"clash_teams"` to the sorted list in `migrations_create_exactly_the_design_04_tables` and to `routes::dev::WIPED` (it is derived from Riot), bump the history count to 28, add the DDL to design/04 §Schema, write the ADR, and document the table in `docs/data/schema.md` and the file in `docs/data/migrations.md`.

> **TODO (unverified):** the example's columns are illustrative; a real Clash table takes its fields from the developer portal's `TeamDto`, not from this page.

## Checklist

1. `git ls-tree origin/main src/db/migrations/` for the next number; `V0029__<name>.sql` with a header comment.
2. Conventions: unix-ms `INTEGER`, JSON `TEXT`, `key_scope`, FKs only where owned, `WITHOUT ROWID` by the rule, state columns last in partial indexes.
3. Cost stated: `ADD COLUMN` where possible; a rebuild's boot time, temp space and WAL in the ADR.
4. `src/db/tests.rs`: table list, history count, a seeded upgrade test for anything but a new table.
5. `routes::dev::WIPED` or `KEPT`.
6. design/04 §Schema, ADR, `docs/data/schema.md`, `docs/data/migrations.md`, `PROGRESS.md`.
7. `just lint`, `just test`; a backup before the first production boot ([../deployment/backup-and-restore.md](../deployment/backup-and-restore.md)).

---
**Sources:** `src/db/mod.rs:L18-L27`, `L131-L147`, `L294-L296`, `src/cli/mod.rs:L136-L165`, `src/db/migrations/V0001__init.sql:L1-L2`, `V0003__cs_and_duration.sql`, `V0021__facts_without_rowid.sql:L1-L33`, `V0023__body_packs.sql:L1-L48`, `V0027__matchup_builds.sql:L1-L19`, `src/db/tests.rs:L92-L149`, `L167`, `L398-L450`, `L539-L562`, `L731`, `L873-L915`, `L1218`, `src/archive/offload/tests.rs:L501-L514`, `src/jobs/offload/tests.rs:L497-L512`, `src/routes/dev.rs:L38-L83`, `tests/common/mod.rs:L32-L33`, `docs/design/04-data-and-cache.md:L61-L63`, `L417-L423`, `docs/design/12-object-storage-and-hosting.md:L196-L221`, `docs/DECISIONS.md:L49-L51` (ADR-010), `L1455` (ADR-135), `L1460-L1462` (ADR-136), `L1490-L1505` (ADR-140, ADR-141), `L1510` (ADR-142), `README.md:L142`, `docs/_meta/scratch/persistence.md` §1
**Last verified:** 2026-10-20 against commit `f0087f7`

# Persistence inventory — riot-proxy v2

Evidence snapshot at branch `docs/DOC-01-documentation-set`, HEAD `7491ff6`. Every claim cites `file:line`. Line numbers are those of the files as read on 2026-10-19. The local dev database at `data/riot-proxy.db` was opened read-only; it is **stale** (refinery history stops at V0006 `analytics_runs`, applied 2026-10-03), so the schema below is derived from the migration files, not from the dev file, except where the dev dump is used to confirm DDL that the migrations create verbatim (`refinery_schema_history`, V0001–V0006 tables).

---

## 1. Migrations

### How they run

| Aspect | Evidence |
|---|---|
| Engine | `refinery`, embedded at compile time: `refinery::embed_migrations!("src/db/migrations")` — `src/db/mod.rs:18-20`. |
| Runner | `embedded::migrations::runner().set_grouped(true).run(conn)` — `src/db/mod.rs:294-296`. `set_grouped(true)` = all pending migrations in **one transaction**. Already-applied versions are skipped, "safe on every boot" (`src/db/mod.rs:292-293`). |
| On boot | `Db::open` runs `migrate(&mut writer)` on the writer connection after applying pragmas and before opening readers / spawning the writer thread — `src/db/mod.rs:144-149`. `serve` opens via `Db::open_async(path, Db::default_readers())` — `src/cli/serve.rs:51-54`. Every other CLI command that touches the DB goes through `cli::open_db` → `Db::open_async(path, 1)` — `src/cli/mod.rs:136-141`, so they also migrate. |
| `riot-proxy migrate` | `src/cli/mod.rs:144-165`: creates the parent dir, opens a **raw** `rusqlite::Connection` (no pragmas), calls `crate::db::migrate`, prints `<path>: up to date` or `applied V…`. Also exposed via the `Store` trait: `SqliteStore::migrate` → `db.write(|c| super::migrate(c))` — `src/db/store.rs:152-154`. |
| Direction | Forward-only. No down migrations exist (`src/db/migrations/` holds only `V####__name.sql` files). Rebuild-style migrations copy into `_vNN` tables, `DROP`, `RENAME` (V0013, V0017, V0020, V0021). |
| History table | `refinery_schema_history(version int4 PRIMARY KEY, name VARCHAR(255), applied_on VARCHAR(255), checksum VARCHAR(255))` — dev DB `sqlite_master` dump. Dev rows: versions 1–6, `applied_on` ISO strings like `2026-10-03T16:55:07.222627298Z`. |
| Postgres | `PgStore` is a stub behind the `postgres` feature; `migrate` returns `DbError::PostgresUnimplemented` — `src/db/store.rs:172-206`, `src/db/mod.rs:62-64`. Config still refuses `postgres://` (`src/db/store.rs:6-8`). |

### Migration list

| File | Creates / alters | Cites |
|---|---|---|
| `V0001__init.sql` | `consumers`, `cache` (+ `cache_hard`), `limiter_state`, `jobs` (+ `jobs_dedupe` UNIQUE partial, `jobs_claim` partial), `metrics_history` | P0-04; "DDL verbatim from docs/design/04 §Schema" (`V0001:1-2`) |
| `V0002__archive.sql` | `players` (+ `players_tracked` partial), `matches` (+ `matches_patch_queue`, `matches_end`), `timelines` (FK → matches), `match_facts` (+ `facts_player`, `facts_champ`), first `champion_stats`/`champion_matchups`/`champion_builds`, first `ladder_crawls`/`ladder_entries`/`crawl_match_ids` | P5-01 (`V0002:1`) |
| `V0003__cs_and_duration.sql` | `matches.game_duration INTEGER`, `match_facts.cs INTEGER` | P5-04, ADR-043 (`V0003:1`) |
| `V0004__ladder.sql` | DROPs V0002's three ladder tables; creates `ladder_crawls` (+ `ladder_crawls_live` UNIQUE partial, `ladder_crawls_recent`), `ladder_entries` (+ `ladder_entries_ladder`, `ladder_entries_last_seen`, `ladder_entries_puuid`), `crawl_legs`, `crawl_match_ids` | P7-02, ADR-054 (`V0004:1`) |
| `V0005__analytics.sql` | `matches.remake`, `matches.facts_version` (+ `matches_facts_version`), `match_facts.gold/damage/vision`, `match_bans`; DROPs V0002 analytics; creates `analytics_slices`, `champion_stats` (+ `champion_stats_slice`), `champion_bans`, `champion_matchups`, `champion_items`, `champion_runes`, `champion_spells` | P7-04, ADR-056 (`V0005:1`) |
| `V0006__analytics_runs.sql` | `analytics_runs` | P7-06, ADR-058 (`V0006:1`) |
| `V0007__job_lane.sql` | `jobs.lane`, `jobs.method`, `jobs_lane_claim` partial, `jobs_lane_running` partial | SCH-01, ADR-089 (`V0007:1`) |
| `V0008__apex_capped.sql` | `ladder_crawls.apex_capped TEXT` | LAD-01, ADR-097 (`V0008:1`) |
| `V0009__cache_fetched_at.sql` | `cache.fetched_at INTEGER` | SITE-01 (`V0009:1`) |
| `V0010__player_ranks.sql` | `player_ranks` | DEV-28, ADR-105 (`V0010:1`) |
| `V0011__rank_lookups.sql` | `rank_lookups` (backfilled from `player_ranks`), `rank_lookup_queue` (+ `rank_lookup_queue_next`) | DEV-29, ADR-111 (`V0011:1`) |
| `V0012__match_builds.sql` | `match_builds` | BLD-01, ADR-117 (`V0012:1`) |
| `V0013__timelines_standalone.sql` | Rebuilds `timelines` **without** the FK to `matches` | TL-01, ADR-118 (`V0013:1`) |
| `V0014__champion_builds.sql` | `champion_builds`, `champion_build_parts` | BLD-02, ADR-120 (`V0014:1`) |
| `V0015__analytics_totals.sql` | `analytics_match_totals`, `champion_ban_totals`, backfilled from existing slices | SITE-08, ADR-123 (`V0015:1`) |
| `V0016__match_tiers.sql` | `match_tiers` (+2 indexes) | THR-02, ADR-127 (`V0016:1`) |
| `V0017__tier_side_keys.sql` | `analytics_unsplit`; rebuilds `champion_stats`, `champion_matchups`, `champion_items`, `champion_runes`, `champion_spells`, `champion_builds`, `champion_build_parts` with `tier`, `side` key columns (`''` = not split) | FLT-01, ADR-129 (`V0017:1`) |
| `V0018__timeline_gaps.sql` | `timeline_gaps` | TL-02, ADR-133 (`V0018:1`) |
| `V0019__tier_on_facts.sql` | `match_facts.tier TEXT` (copied from `match_tiers`), DROPs `match_tiers` | DSK-04, ADR-138 amends ADR-127 (`V0019:1`) |
| `V0020__without_rowid_derived.sql` | Rebuilds `match_builds`, `match_bans`, `analytics_slices`, `analytics_match_totals`, `champion_stats` (+ recreates `champion_stats_slice`), `champion_bans`, `champion_ban_totals`, `champion_matchups`, `champion_items`, `champion_runes`, `champion_spells`, `champion_builds`, `champion_build_parts` as `WITHOUT ROWID` | DSK-06, ADR-140 (`V0020:1`) |
| `V0021__facts_without_rowid.sql` | Rebuilds `match_facts` `WITHOUT ROWID`; recreates `facts_player`; **drops `facts_champ`** (not recreated, `V0021:6-8`) | DSK-07, ADR-141 (`V0021:1`) |
| `V0022__jobs_state_kind.sql` | `jobs_state_kind` index | ADM-02 (`V0022:1`) |
| `V0023__body_packs.sql` | `body_packs`, `body_locations` (+ `body_locations_pack`), `matches.packed_at/offloaded_at`, `timelines.packed_at/offloaded_at`, partial indexes `matches_to_pack`, `matches_to_free`, `timelines_to_pack`, `timelines_to_free` | OBJ-02, ADR-146, ADR-163 (`V0023:1`) |
| `V0024__matches_archived.sql` | `matches_archived ON matches(match_id, archived_at)` | OBJ-03, ADR-164 (`V0024:1`) |
| `V0025__champion_stats_totals.sql` | `champion_stats_totals` (filled from `champion_stats`); **drops `champion_stats_slice`** (`V0025:40`) | PERF-02, ADR-168 (`V0025:1`) |
| `V0026__floor_totals.sql` | `analytics_floor_totals`, `champion_ban_floor_totals`, backfilled via a TEMP table | FLT-03, ADR-170 (`V0026:1`) |
| `V0027__matchup_builds.sql` | `champion_matchup_builds`, `champion_matchup_build_parts` | FLT-04, ADR-171 (`V0027:1`) |

---

## 2. Tables after V0027 (40 tables)

Conventions that apply to every table (design/04 §Schema, repeated in `V0001:2`, `V0002:3`): timestamps are **unix ms `INTEGER`**; JSON lives in **`TEXT`**; bodies are **zstd `BLOB`s** (level 3, `src/archive/matches.rs:33`). `key_scope` is an 8-hex-char identifier derived from the Riot key (`KeyScope::from_key`, `src/cli/migrate_v1.rs:384`). The 40 tables = 37 in `routes/dev.rs` `WIPED` (`src/routes/dev.rs:41-78`) + `consumers`, `limiter_state`, `refinery_schema_history` ("every table in the schema is in exactly one of the two lists (tested)", `src/routes/dev.rs:80-81`).

Declared FKs are only: `match_facts.match_id`, `match_bans.match_id`, `match_builds.match_id` → `matches(match_id) ON DELETE CASCADE`; `crawl_legs.crawl_id`, `crawl_match_ids.crawl_id` → `ladder_crawls(id) ON DELETE CASCADE`; `body_locations.pack_id` → `body_packs(pack_id)`. `PRAGMA foreign_keys = ON` on every connection (`src/db/mod.rs:27`).

### 2.1 Operational (V0001)

#### `consumers`
- Columns: `id TEXT PK` (ulid), `name TEXT NOT NULL UNIQUE`, `key_sha256 BLOB NOT NULL UNIQUE`, `scopes TEXT NOT NULL` (JSON array `["read"]|["read","admin"]`), `quota_per_min INTEGER NOT NULL DEFAULT 600`, `created_at INTEGER NOT NULL`, `revoked_at INTEGER` — `V0001:5-13`.
- Indexes: autoindexes for PK + two UNIQUEs (dev dump `sqlite_autoindex_consumers_1..3`).
- Writes: `src/consumers.rs:155` INSERT (create / `bootstrap_admin`, called at `serve` boot `src/cli/serve.rs:62`); `src/consumers.rs:245,275` `UPDATE … SET revoked_at`.
- Reads: `src/consumers.rs:204,217,235,262,297,316,376`; `src/stats.rs:571`.
- Retention: **never deleted** (revocation is a timestamp). Not in dev `WIPED`.
- Relationships: none declared; the plaintext key is hashed (`key_sha256`), never stored (`V0001:8`).

#### `cache` (L2)
- Columns: `key TEXT PK` (includes key_scope), `body BLOB NOT NULL`, `status INTEGER NOT NULL` (200 or 404), `content_at INTEGER NOT NULL`, `soft_expires INTEGER NOT NULL`, `hard_expires INTEGER NOT NULL` — `V0001:16-23`; `fetched_at INTEGER` nullable — `V0009:4` (rows older than V0009 read `COALESCE(fetched_at, content_at)` `src/cache/l2.rs:192-194`).
- Index: `cache_hard ON cache(hard_expires)` — `V0001:24`.
- Writes: write-behind `L2Writer`: bounded queue 10 000 (`src/cache/l2.rs:28`), flush every 2 s or 500 rows (`l2.rs:24-26`), `INSERT OR REPLACE` in one tx (`l2.rs:159-183`); full queue drops the row + `cache_l2_dropped_total` (`l2.rs:102-112`). Shutdown flushes (`l2.rs:115-123`, `serve.rs:390`).
- Reads: `warm` loads every `hard_expires > now` row into L1 at boot (`l2.rs:186-216`, `serve.rs:86`); admin purge `keys_where`/`delete_where` (`l2.rs:227-261`).
- Retention: `sweep` deletes `hard_expires <= now` at boot (`serve.rs:90`) and in daily maintenance step 3 (`src/jobs/maintenance.rs:251`); admin purge by predicate (`l2.rs:241-261`); dev reset wipes.
- Units: `body` is the raw response bytes (not zstd) — `l2.rs:170`.

#### `limiter_state`
- Columns: `scope TEXT PK` (`app:euw1` | `method:euw1:match.byId`), `windows TEXT NOT NULL` (JSON `StoredScope {known?, windows:[{limit,seconds,stamps:[[unix_ms,count]…]}]}` `src/riot/limiter/persist.rs:37-51`), `frozen_until INTEGER`, `updated_at INTEGER NOT NULL` — `V0001:26-31`.
- Writes: `persist::save` = `DELETE FROM limiter_state` then re-INSERT every row in one tx (`persist.rs:178-194`); every 10 s (`CHECKPOINT_EVERY`, `persist.rs:25`; `spawn_checkpoints` `serve.rs:82`) and at shutdown (`serve.rs:389`).
- Reads: `persist::load` at boot (`persist.rs:196-213`, `serve.rs:76`); a row older than 120 s (`STALE_AFTER`, `persist.rs:23`) restores every window as full (`persist.rs:92-94,147-148`).
- Retention: whole table replaced on each checkpoint. Not in dev `WIPED`.
- Units: stamps are unix-ms buckets rounded **up** to `max(100 ms, seconds)` ms (`persist.rs:53-70`).

#### `jobs`
- Columns: `id TEXT PK` (ulid), `kind TEXT NOT NULL`, `dedupe_key TEXT`, `priority INTEGER NOT NULL`, `payload TEXT NOT NULL` (JSON), `state TEXT NOT NULL DEFAULT 'pending'` (pending|running|done|failed), `attempts INTEGER NOT NULL DEFAULT 0`, `run_after INTEGER NOT NULL`, `claimed_at INTEGER`, `finished_at INTEGER`, `error TEXT` — `V0001:33-43`; `lane TEXT`, `method TEXT` (NULL for kinds that never call Riot) — `V0007:5-6`.
- Indexes: `jobs_dedupe UNIQUE (kind, dedupe_key) WHERE state IN ('pending','running')` (`V0001:44`), `jobs_claim (state, run_after, priority) WHERE state='pending'` (`V0001:45`), `jobs_lane_claim (lane, priority, run_after, id) WHERE state='pending'` (`V0007:8`), `jobs_lane_running (lane) WHERE state='running'` (`V0007:9`), `jobs_state_kind (state, kind, run_after, finished_at)` (`V0022:6`).
- Writes: enqueue `INSERT OR IGNORE` with lane/method (`src/jobs/scheduler.rs:225-241`), promote (`:263`), claim `UPDATE … RETURNING` through `Store::claim_job` (`src/db/store.rs:98-133,156-169`), finish/retry/fail (`scheduler.rs:1099-1113`), cancel (`:215,:775`), retry failed (`:751`), boot `recover` running→pending (`:960-972`), boot `assign_lanes` (`:1035-1050`), `jobs/archive.rs:154` payload `json_set`.
- Reads: scheduler (`scheduler.rs:246,610-862`), `stats.rs:460`, `jobs/ladder/store.rs:746-787`, `jobs/ladder/ranks.rs:154`, `jobs/archive.rs:188`, `routes/dev.rs:124`.
- Retention: daily maintenance deletes `state='done' AND finished_at < now − 7 d` (`JOB_RETENTION_MS`, `maintenance.rs:36,237-240`). **`failed` rows are never auto-deleted**; admin purge deletes pending/failed by kind+priority in 5 000-row chunks (`scheduler.rs:797-840`, `PURGE_CHUNK` `:512`); dev reset wipes.
- Relationships: `dedupe_key` of `archive:match` is the match id (`jobs/archive.rs:186-191`); crawl legs carry `crawlId` in payload (`scheduler.rs:200-222`).

#### `metrics_history`
- Columns: `at INTEGER PK`, `point TEXT NOT NULL` (JSON `HistoryPoint`) — `V0001:47-50`.
- Writes: `INSERT OR REPLACE` every `METRICS_HISTORY_INTERVAL_S` (default 60, range 10–3600, `src/config.rs:722`) — `src/stats.rs:965-981`.
- Reads: `stats.rs:985-995` (`history()`), `:974-975`.
- Retention: two rules — on every write keep only the newest `HISTORY_MAX_POINTS = 1440` rows (`stats.rs:43,974-976`); daily maintenance deletes `at < now − 24 h` (`HISTORY_RETENTION_MS`, `maintenance.rs:38,241-244`).

### 2.2 Players & archive (V0002 +)

#### `players`
- Columns: `key_scope TEXT NOT NULL`, `puuid TEXT NOT NULL`, `platform TEXT NOT NULL`, `game_name TEXT`, `tag_line TEXT`, `tracked INTEGER NOT NULL DEFAULT 0`, `last_seen_match_id TEXT` (poll cursor), `backfill_state TEXT` (JSON; v2 shape `{startedAt, doneAt, depth}` ADR-050 per `migrate_v1.rs:300-301,325-332`), `last_rank TEXT` (JSON), `in_game_id INTEGER`, `updated_at INTEGER NOT NULL`; `PRIMARY KEY (key_scope, puuid)` — `V0002:6-18`.
- Index: `players_tracked ON players(tracked) WHERE tracked = 1` — `V0002:19`.
- Writes: `src/players.rs:63-75` upsert, `:134` tracked, `:171` poll columns (`in_game_id`/`last_rank`/`last_seen_match_id`); ladder crawl upsert with `tracked=0` (`src/jobs/ladder/store.rs:246-250`), backfill stamps (`store.rs:542,562`); `jobs/archive.rs:299` `backfill_state`; `jobs/names.rs:170` names; `cli/migrate_v1.rs:336-346`.
- Reads: `players.rs:85,101,115,184`; `jobs/names.rs:45,100`; `stats.rs:569-570`; `ladder/store.rs:445` (LEFT JOIN).
- Retention: never deleted (dev reset only).
- Relationships (undeclared): `(key_scope, puuid)` ↔ `match_facts`, `ladder_entries`, `player_ranks`, `rank_lookups`, `rank_lookup_queue`.

#### `matches`
- Columns: `match_id TEXT PK` (`OC1_123…`), `region TEXT NOT NULL` (routing value), `patch TEXT NOT NULL` (`"14.18"`), `queue_id INTEGER NOT NULL`, `game_end_ms INTEGER NOT NULL`, `body_zstd BLOB NOT NULL`, `body_size INTEGER NOT NULL` (uncompressed), `archived_at INTEGER NOT NULL` — `V0002:22-31`; `game_duration INTEGER` seconds (`V0003:5`); `remake INTEGER`, `facts_version INTEGER` (`V0005:12-16`); `packed_at INTEGER`, `offloaded_at INTEGER` unix ms (`V0023:37-38`).
- Indexes: `matches_patch_queue (patch, queue_id)`, `matches_end (game_end_ms)` (`V0002:32-33`); `matches_facts_version (facts_version)` (`V0005:17`); `matches_to_pack (archived_at, packed_at) WHERE packed_at IS NULL`, `matches_to_free (archived_at, packed_at, offloaded_at) WHERE packed_at IS NOT NULL AND offloaded_at IS NULL` (`V0023:41-44`); `matches_archived (match_id, archived_at)` (`V0024:7`).
- Writes: `matches::put_stored` upsert incl. `offloaded_at = NULL` on re-archive (`src/archive/matches.rs:375-396`); `write_derived` sets `game_duration, remake, facts_version` (`matches.rs:143-151`); `facts:reextract` stamps `facts_version` for bodies that no longer derive (`src/jobs/analytics.rs:457`); `pack::locate_on` sets `packed_at` (`src/archive/pack.rs:337-339,351`); `offload::free` sets `body_zstd = X'', offloaded_at` (`src/archive/offload.rs:155-158`); `recovery::replace_tables` sets `packed_at` (`recovery.rs:432-440`).
- Reads: `matches.rs:187-189` (body + LEFT JOIN `body_locations`), `:491-495` stats, `:527` filter_unarchived; `pool.rs:54`; `player.rs:30,48,100,146`; `tiers.rs:89,121-123`; `ranks.rs:171-173`; `offload.rs:193-199,236-245,270-286`; `jobs/timelines.rs:139-143,223`; `jobs/archive.rs:178`; analytics `ladder_facts` (`analytics.rs`, joins `matches`), `recent_patches` (`analytics.rs:46-57`); V0015/V0026 backfills.
- Retention: **never deleted** by any job (dev reset only). Offloaded rows keep a row with the empty blob (`V0023:3-5`).
- Units: body state = local (`packed_at` NULL) → packed (`packed_at` set, body still local) → offloaded (`offloaded_at` set, `body_zstd = X''`) — `V0023:2-5`, `matches.rs:437-446`.

#### `timelines`
- Columns (after V0013 rebuild, no FK): `match_id TEXT PK`, `body_zstd BLOB NOT NULL` — `V0013:6-9`; `packed_at INTEGER`, `offloaded_at INTEGER` — `V0023:39-40`. Rowid table.
- Indexes: `timelines_to_pack (match_id, packed_at) WHERE packed_at IS NULL`, `timelines_to_free (match_id, packed_at, offloaded_at) WHERE packed_at IS NOT NULL AND offloaded_at IS NULL` — `V0023:45-48`.
- Writes: `put_timeline_stored` upsert, `offloaded_at = NULL` (`matches.rs:425-431`); `locate_on` (`pack.rs:337-339`); `offload::free` (`offload.rs:155`); `recovery.rs:432`.
- Reads: `matches.rs:187-189` (kind=timeline); `jobs/analytics.rs:516` (builds:extract); `jobs/archive.rs:178-185`; `jobs/timelines.rs:141,222-223`; `player.rs:32`; `offload.rs:197-199,242-244,284-285`; `matches.rs:495`.
- Retention: never deleted (dev reset only).
- Relationship (undeclared since V0013): `match_id` → `matches.match_id`; a timeline may exist before its match (TL-01, `V0013:1-5`).

#### `match_facts` (WITHOUT ROWID since V0021)
- Columns: `match_id TEXT NOT NULL REFERENCES matches ON DELETE CASCADE`, `key_scope TEXT NOT NULL`, `puuid TEXT NOT NULL`, `team_id INTEGER NOT NULL`, `position TEXT`, `champion_id INTEGER NOT NULL`, `win INTEGER NOT NULL`, `kills`, `deaths`, `assists INTEGER`, `items TEXT` (JSON `[int×6]`), `runes TEXT` (JSON positional `[primaryStyle, keystone, subStyle, [perks], [stat perks]|null]` `V0021:19`, `src/archive/facts.rs:53-57`), `summoners TEXT` (JSON `[int,int]`), `facts_version INTEGER NOT NULL`, `cs`, `gold`, `damage`, `vision INTEGER`, `tier TEXT` (NULL unstamped, else Riot tier or `'UNKNOWN'`); `PRIMARY KEY (match_id, puuid)` — `V0021:9-25`.
- Index: `facts_player (key_scope, puuid, match_id)` — `V0021:33`. (`facts_champ` dropped at V0021.)
- Writes: `facts::write` — delete participants not in body for this key_scope, then upsert without touching `tier` (`facts.rs:329-379`); called from `write_derived` in `put_stored`'s tx and from `facts:reextract` (`matches.rs:137-154`, `jobs/analytics.rs:440-466`); `tiers::stamp` (`tiers.rs:37-57`) in the archive tx (`matches.rs:398-399`) and `tiers:backfill`; `tiers::late_stamp` (`tiers.rs:65-90`) from `ranks::record` (`ranks.rs:65`). `FACTS_VERSION = 4` (`facts.rs:19`).
- Reads: `pool.rs:54`, `player.rs:30,48,100,146`, `ranks.rs:171`, `tiers.rs:123`, `names.rs:118`, `jobs/analytics.rs:446,525`, analytics `ladder_facts` (`analytics.rs:100-110` region, `TIER = f.tier`, `SIDE` from `team_id`).
- Retention: rows only disappear with `matches` cascade (never) or `facts::write` dropping a participant; dev reset wipes.

#### `match_bans` (WITHOUT ROWID since V0020)
- Columns: `match_id TEXT NOT NULL REFERENCES matches ON DELETE CASCADE`, `team_id`, `pick_turn`, `champion_id INTEGER NOT NULL`; `PK (match_id, team_id, pick_turn)` — `V0020:31-37`.
- Writes: `facts::write_bans` — `DELETE` per match then `INSERT OR IGNORE` (`facts.rs:292-301`), inside `write_derived`.
- Reads: analytics ban aggregates (`analytics.rs:207-217,284`), V0015/V0026 backfills.
- Retention: replaced per match; never trimmed.

#### `match_builds` (WITHOUT ROWID since V0020)
- Columns: `match_id TEXT NOT NULL REFERENCES matches ON DELETE CASCADE`, `key_scope`, `puuid TEXT NOT NULL`, `starter TEXT NOT NULL` (JSON), `boots INTEGER`, `items TEXT NOT NULL` (JSON ≤ 6), `skills TEXT NOT NULL` (`"QWEQQRQ…"`), `skill_order TEXT`, `builds_version INTEGER NOT NULL`; `PK (match_id, key_scope, puuid)` — `V0020:13-24`.
- Writes: `builds::write` — `DELETE` per match, INSERT with `BUILDS_VERSION = 3` (`src/archive/builds.rs:24,381-407`), driven by `builds:extract` (`jobs/analytics.rs:505-530`).
- Reads: `jobs/analytics.rs:519` (what is still to extract), analytics `rebuild_builds` (`analytics.rs:461-540` region).
- Retention: replaced per match; never trimmed.

#### `timeline_gaps` (V0018)
- Columns: `match_id TEXT PK`, `reason TEXT NOT NULL` (`"not_found"`), `marked_at INTEGER NOT NULL` — `V0018:4-8`. No FK.
- Writes: `jobs/timelines.rs:164-171` upsert.
- Reads: `jobs/timelines.rs:142,224`.
- Retention: never trimmed (dev reset only).

### 2.3 Ladder (V0004, V0008)

#### `ladder_crawls`
- Columns: `id TEXT PK` (ULID), `key_scope`, `platform`, `queue`, `tier_floor TEXT NOT NULL`, `status TEXT NOT NULL DEFAULT 'running'` (running|completed|failed|cancelled), `phase TEXT NOT NULL DEFAULT 'enumerate'` (enumerate|collect|archive), `started_at INTEGER NOT NULL`, `finished_at INTEGER`, counters `pages_fetched`, `entries_seen`, `players_discovered`, `backfills_enqueued`, `match_ids_seen`, `matches_queued`, `legs_failed INTEGER NOT NULL DEFAULT 0` — `V0004:14-31`; `apex_capped TEXT` (JSON list of tiers) — `V0008:4`.
- Indexes: `ladder_crawls_live UNIQUE (key_scope, platform, queue) WHERE status='running'` (`V0004:33-34`), `ladder_crawls_recent (key_scope, platform, queue, started_at)` (`V0004:35`).
- Writes: `src/jobs/ladder/store.rs:97-103` INSERT … ON CONFLICT DO NOTHING RETURNING; counters `:277,:466`; `apex_capped :319`; `legs_failed :368`; `phase :397`; `finish :420-423`.
- Reads: `store.rs:118,131,303,377,583,621,713`; `stats.rs:632,635`.
- Retention: **never deleted** — a run log (`V0004:5-6`); dev reset only.

#### `ladder_entries`
- Columns: `key_scope`, `platform`, `queue`, `puuid TEXT NOT NULL`, `tier`, `division TEXT NOT NULL`, `league_points`, `wins`, `losses INTEGER NOT NULL`, `veteran`, `inactive`, `fresh_blood`, `hot_streak INTEGER NOT NULL DEFAULT 0`, `first_seen_crawl_id TEXT NOT NULL`, `last_seen_crawl_id TEXT NOT NULL`, `updated_at INTEGER NOT NULL`; `PK (key_scope, platform, queue, puuid)` — `V0004:37-55`.
- Indexes: `ladder_entries_ladder (key_scope, platform, queue, tier, division, league_points)`, `ladder_entries_last_seen (last_seen_crawl_id)`, `ladder_entries_puuid (key_scope, puuid)` — `V0004:56-59`.
- Writes: `store.rs:230-241` upsert (`first_seen_crawl_id` set once, `last_seen_crawl_id` restamped).
- Reads: `tiers.rs:41-42` (STAMP), `ranks.rs:176-177` (PLAN), `store.rs:445`, `stats.rs:639`.
- Retention: never deleted; "outlives the crawl rows" (`V0004:7`).
- Relationships (undeclared): `first/last_seen_crawl_id` → `ladder_crawls.id`; `(key_scope, puuid)` → `players`.

#### `crawl_legs`
- Columns: `crawl_id TEXT NOT NULL REFERENCES ladder_crawls(id) ON DELETE CASCADE`, `leg TEXT NOT NULL` (`ladder:walk:DIAMOND:I`), `cursor INTEGER`; `PK (crawl_id, leg)` — `V0004:61-66`.
- Writes: `store.rs:143` INSERT OR IGNORE; `:284` cursor; `:360` delete own leg; `:429` delete all at finish.
- Reads: `store.rs:156,168,722`.
- Retention: deleted when its job finishes or the crawl finishes (`store.rs:360,429`).

#### `crawl_match_ids`
- Columns: `crawl_id … ON DELETE CASCADE`, `match_id TEXT NOT NULL`; `PK (crawl_id, match_id)` — `V0004:68-72`.
- Writes: `store.rs:500` INSERT OR IGNORE; `:529` delete one; `:430` delete all at finish.
- Reads: `store.rs:518,731`.
- Retention: as `crawl_legs`.

### 2.4 Ranks (V0010, V0011)

#### `player_ranks`
- Columns: `key_scope`, `platform`, `queue TEXT NOT NULL` (league-v4 queueType), `puuid`, `tier`, `division TEXT NOT NULL`, `league_points INTEGER NOT NULL`, `fetched_at INTEGER NOT NULL`; `PK (key_scope, platform, queue, puuid)` — `V0010:7-17`.
- Writes: `ranks::record` — DELETE the player's rows on the platform, INSERT one per ranked queue (`src/archive/ranks.rs:53-66`); fed by `SqliteArchive::observe` on `league.entriesByPuuid` bodies (`src/archive/mod.rs:180-198`).
- Reads: `tiers.rs:43-44` (STAMP); V0011 backfill.
- Retention: replaced per lookup; never trimmed.

#### `rank_lookups`
- Columns: `key_scope`, `platform`, `puuid TEXT NOT NULL`, `looked_up_at INTEGER NOT NULL`; `PK (key_scope, platform, puuid)` — `V0011:7-13`.
- Writes: `ranks.rs:74-81` upsert (`stamp_on`), from `record` and `stamp` (404s).
- Reads: `ranks.rs:178-179` (PLAN, with `RANK_LOOKUP_RECHECK_S`).
- Retention: never trimmed.

#### `rank_lookup_queue`
- Columns: `key_scope`, `platform`, `queue`, `puuid TEXT NOT NULL`, `games INTEGER NOT NULL`; `PK (key_scope, platform, queue, puuid)` — `V0011:21-28`. Index `rank_lookup_queue_next (key_scope, platform, queue, games DESC)` — `V0011:29`.
- Writes: `plan` INSERT OR IGNORE (`ranks.rs:144-157`), `take` DELETE (`:186-208`), `put_back` (`:211-222`).
- Reads: `ranks.rs:107,192`.
- Retention: rows are consumed by `ranks:lookup`.

### 2.5 Analytics (recomputed; all WITHOUT ROWID since V0020 except `analytics_runs`, `analytics_unsplit`)

Common writer: `archive::analytics::Scope::clear` deletes a table's rows for `(key_scope, platform, queue)` and the rebuilt patches (`?4 IS NULL OR patch IN json_each(?4)`), then `Scope::insert` runs an `INSERT … SELECT` binding `?1`–`?6` = key_scope, platform, queue, queue_id, patches JSON, now — `src/archive/analytics.rs:70-98`. Patches = newest `AGGREGATE_PATCH_LIMIT` (default 4, 0 = all; `src/config.rs:753`; `recent_patches` `analytics.rs:46-57`); a ladder on `analytics_unsplit` rebuilds every patch (`analytics.rs:570-597`). Common reader: `archive::analytics::Read` (`analytics.rs:1100-1500`, e.g. `:1167,1183,1203,1218,1237,1253,1434`), used by `/v1/lol/analytics/*` (`src/routes/analytics.rs:1-3,33-35`) and `/dev`/admin. Retention: rows for patches outside the rebuilt window are **kept** (`analytics.rs:15-17`); nothing trims by age; dev reset wipes all.

| Table | Key columns (PK in order) | Value columns | Created / shape |
|---|---|---|---|
| `analytics_slices` | key_scope, platform, queue, tier, patch, remake | matches, computed_at | V0005:36-41, V0020:44-54 |
| `analytics_match_totals` | key_scope, platform, queue, patch, remake | matches, computed_at | V0015:7-13, V0020:56-67 |
| `analytics_floor_totals` | key_scope, platform, queue, floor_tier, patch, remake | matches, computed_at | V0026:10-17 |
| `champion_stats` | key_scope, platform, queue, tier, patch, champion_id, role, remake, side | games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at | V0005:43-57 → V0017:30-54 (+side) → V0020:69-89; `champion_stats_slice` dropped V0025:40 |
| `champion_stats_totals` | key_scope, platform, queue, patch, champion_id, role, remake, side | same sums as `champion_stats`, computed_at = max | V0025:16-39 |
| `champion_bans` | key_scope, platform, queue, tier, patch, champion_id, remake | bans, computed_at | V0005:59-65, V0020:91-102 |
| `champion_ban_totals` | key_scope, platform, queue, patch, champion_id, remake | bans, computed_at | V0015:15-21, V0020:104-115 |
| `champion_ban_floor_totals` | key_scope, platform, queue, floor_tier, patch, champion_id, remake | bans, computed_at | V0026:19-25 |
| `champion_matchups` | key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side | games, wins, computed_at | V0005:68-73 → V0017:58-70 → V0020:117-128 |
| `champion_items` | …, champion_id, role, item_id, remake, tier, side | games, wins, computed_at | V0020:130-141 |
| `champion_runes` | …, champion_id, role, keystone_id, sub_style_id, remake, tier, side | games, wins, computed_at | V0020:143-155 |
| `champion_spells` | …, champion_id, role, spell_a, spell_b (a ≤ b), remake, tier, side | games, wins, computed_at | V0020:157-170 |
| `champion_builds` | …, champion_id, role, remake, core (JSON `[a,b]`), tier, side | games, wins, computed_at | V0014:5-11 → V0020:172-184 |
| `champion_build_parts` | …, champion_id, role, remake, core, part (`item3|item4|item5|starter|boots|skill_order|runes|spells`), value, tier, side | games, wins, computed_at | V0014:14-22 → V0020:186-200 |
| `champion_matchup_builds` | …, champion_id, role, opponent_id, remake, core, tier, side | games, wins, computed_at | V0027:10-19 |
| `champion_matchup_build_parts` | …, champion_id, role, opponent_id, remake, core, part (`item3|boots|runes|spells`), value, tier, side | games, wins, computed_at | V0027:21-30 |

Writers by table (INSERT … SELECT): `analytics_slices` `analytics.rs:153`; `analytics_match_totals` `:164`; `champion_stats` `:173`; `champion_stats_totals` `:191` (same tx as `champion_stats`, `V0025:9-10`); `champion_bans` `:207`; `champion_ban_totals` `:217`; `analytics_floor_totals` `:267`; `champion_ban_floor_totals` `:284`; `champion_matchups` `:311`; `champion_items` `:388`; `champion_runes` `:408`; `champion_spells` `:429`; `champion_builds` `:461`; `champion_build_parts` `:479`; `champion_matchup_builds` `:518`; `champion_matchup_build_parts` `:533`. `tier` is `match_facts.tier`, `side` is `CASE team_id WHEN 100 THEN 'blue' ELSE 'red'` (`analytics.rs:104-110`); `''` in either = V0017 copy not yet split (`V0017:9-15`).

#### `analytics_runs` (rowid)
- Columns: `key_scope`, `platform`, `queue TEXT NOT NULL`, `at INTEGER NOT NULL`, `status TEXT NOT NULL` (completed|failed), `ms INTEGER NOT NULL`, `steps TEXT NOT NULL` (JSON `{step: seconds}`), `rows TEXT NOT NULL` (JSON `{table: rows}`), `games INTEGER NOT NULL`; `PK (key_scope, platform, queue)` — `V0006:3-12`.
- Writes: `INSERT OR REPLACE` per ladder after each run (`src/jobs/analytics.rs:165-183`). Reads: `:189-200`, dashboard. Retention: one row per ladder, replaced.

#### `analytics_unsplit` (rowid)
- Columns: `key_scope`, `platform`, `queue`; `PK (key_scope, platform, queue)` — `V0017:17-20`, seeded from existing analytics rows (`V0017:21-28`).
- Writes: only the V0017 INSERT; `mark_split` DELETE after a full rebuild (`analytics.rs:581-590`). Reads: `:570-579,:593-600`; boot `split_if_needed` (`serve.rs:300-308`).
- Retention: drains itself.

### 2.6 Object-storage map (V0023)

#### `body_packs` (rowid)
- Columns: `pack_id TEXT PK` (ULID), `kind TEXT NOT NULL` (`match|timeline`), `bytes INTEGER NOT NULL` (object size), `bodies INTEGER NOT NULL`, `live_bytes INTEGER NOT NULL`, `sha256 BLOB NOT NULL` (32 bytes, whole object), `created_at INTEGER NOT NULL` — `V0023:10-18`.
- Writes: `pack::record_on` INSERT (`src/archive/pack.rs:287-311`) inside `offload::write_pack`'s tx (`offload.rs:75-81`); `locate_on` lowers `live_bytes` of a pack a body moved out of (`pack.rs:326-330`); `recovery::reindex` INSERT … ON CONFLICT DO NOTHING + recompute `live_bytes` (`recovery.rs:546-567`); `recovery::replace_tables` DELETE all + re-INSERT (`recovery.rs:397-414`).
- Reads: `matches.rs:496-500` (stats), `jobs/offload.rs:212` (`archive_packs` gauge), `recovery.rs:116-118` (snapshot), `recovery.rs:433` (join).
- Retention: never deleted by any job (OBJ-06 compaction not present — see §7); dev reset wipes.

#### `body_locations` (WITHOUT ROWID)
- Columns: `match_id TEXT NOT NULL`, `kind TEXT NOT NULL`, `pack_id TEXT NOT NULL REFERENCES body_packs(pack_id)`, `offset INTEGER NOT NULL`, `len INTEGER NOT NULL`, `sha256 BLOB NOT NULL` (of the body's zstd bytes); `PK (match_id, kind)` — `V0023:21-29`. Index `body_locations_pack (pack_id)` — `V0023:30`.
- Writes: `pack::locate_on` upsert (`pack.rs:331-336,343-350`); `recovery::replace_tables` (`recovery.rs:415-428`).
- Reads: `matches.rs:187-189` (LEFT JOIN on body read), `offload.rs:157-158,238,244` (free / next_free guards), `recovery.rs:131-133` (snapshot), `pack.rs:327-328`, `recovery.rs:433-436,564`.
- Retention: never deleted by a job; dev reset wipes. ~60 bytes/row (design/12 `§SQLite`).
- Relationships (undeclared): `(match_id, kind)` → `matches.match_id` / `timelines.match_id`.

### 2.7 `refinery_schema_history`
`version int4 PRIMARY KEY, name VARCHAR(255), applied_on VARCHAR(255), checksum VARCHAR(255)` (dev dump). Written only by refinery (`src/db/mod.rs:294-296`). Not in `WIPED`.

### ER summary (undeclared edges in italics)

```
consumers                      limiter_state        metrics_history        refinery_schema_history
players (key_scope,puuid) ─┬─< match_facts (match_id,puuid) >── matches (match_id) ──< match_bans
                           │          │ tier                        │                  match_builds
                           │          │                             ├── *timelines* (match_id, no FK)
                           ├─< ladder_entries ──*crawl_id*──> ladder_crawls ──< crawl_legs, crawl_match_ids
                           ├─< player_ranks       (feed match_facts.tier via tiers::stamp)
                           ├─< rank_lookups
                           └─< rank_lookup_queue
matches/timelines (match_id,kind) ── *body_locations* >── body_packs (pack_id)   (object key = packs/<table>/<yyyy>/<mm>/<pack_id>.pack)
matches ── *timeline_gaps* (match_id)
match_facts + matches + match_bans + match_builds ──rebuild──> analytics_* / champion_* (keyed by key_scope,platform,queue[,tier],patch[,…],remake[,side])
jobs (dedupe_key = match id for archive:match; payload.crawlId → ladder_crawls.id)
```

---

## 3. SQLite setup

### Pragmas (`src/db/mod.rs:22-35`, matching design/04 §SQLite configuration lines 40-48)
- Writer only (`WRITER_PRAGMAS`): `journal_mode = WAL`, `synchronous = NORMAL`, `wal_autocheckpoint = 1000` — `mod.rs:31-35`.
- Every connection (`CONNECTION_PRAGMAS`): `busy_timeout = 5000`, `foreign_keys = ON`, `cache_size = -65536` (64 MiB), `mmap_size = 268435456` (256 MiB), `temp_store = MEMORY` — `mod.rs:24-30`. Applied via `pragma_update_and_check` tolerating `QueryReturnedNoRows` (`mod.rs:279-290`).
- `PRAGMA optimize = 0x10002` on the writer at open, before each analytics rebuild and in maintenance (`mod.rs:259-270`, `jobs/analytics.rs:255`, `maintenance.rs:252`; ADR-102).
- Page size: default 4096 (asserted `src/db/disk.rs:146`; dev file reports 4096).

### Writer thread + reader pool (`src/db/mod.rs`)
- One writer `Connection` on a dedicated OS thread `sqlite-writer`, fed by `tokio::mpsc` of boxed closures, queue depth `WRITE_QUEUE = 1024` (`mod.rs:37-38,152-156`). `Db::write(f)` sends the closure and awaits a oneshot (`mod.rs:213-230`); a panicking closure yields `DbError::WriterGone` and the thread continues (`mod.rs:298-308`). Writes are serialised by construction → no `SQLITE_BUSY` between writers.
- Readers: `readers.max(1)` read-only connections (`SQLITE_OPEN_READ_ONLY | NO_MUTEX | URI`, `mod.rs:272-278`) in a `Mutex<Vec<Connection>>` guarded by a `Semaphore` of the same count (`mod.rs:80-87,157-170`). `Db::read(f)` acquires a permit, pops a connection, runs `f` under `spawn_blocking` with `catch_unwind`; `ReaderLoan::drop` returns the connection **before** releasing the permit so a cancelled future cannot strand a permit (`mod.rs:103-121,236-267`; INC-01, ADR-113). Pool size: `serve` uses `Db::default_readers()` = `available_parallelism()` (fallback 4) (`mod.rs:180-183`, `serve.rs:52`); one-shot CLI commands use 1 (`cli/mod.rs:136-141`).
- Gauges/counters: `sqlite_readers_free` set on every loan/return (`mod.rs:123-125`); `Db::reads()` counts `read` calls (`mod.rs:92-94,200-202`).
- `Db::attach_objects` (OnceLock, first call wins) stores the bucket handle for offloaded reads (`mod.rs:84-86,173-181`).

### WAL checkpoint cadence
- Automatic: `wal_autocheckpoint = 1000` pages (`mod.rs:34`).
- Explicit: `PRAGMA wal_checkpoint(TRUNCATE)` as the last maintenance step, logging `(busy, log, checkpointed)` (`maintenance.rs:254-269`).
- Disk gauges `sqlite_db_bytes`, `sqlite_wal_bytes`, `sqlite_freelist_bytes`, `data_dir_free_bytes` sampled with each history point (`src/db/disk.rs:40-74`, `src/metrics.rs:55-58`).

### `/readyz` probe (`src/routes/health.rs`)
`state.db.write(|c| c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;"))` under `READY_TIMEOUT = 2 s` (`health.rs:19,83-87`); `ok = sqlite && limiter_restored` (`health.rs:88-89`); body carries `sqliteReaders` and `sqliteReadersFree` (informational, `health.rs:102-104`, `:60-63`). 503 carries the same body (`health.rs:72-75`).

### Backups (`src/jobs/maintenance.rs`)
- Statement: `VACUUM INTO ?1` from a **separate read-only connection** on `spawn_blocking` so the writer isn't held (`maintenance.rs:57-76`). Target must not pre-exist (`:60-62`).
- Path pattern: `$DATA_DIR/backups/riot-proxy-<YYYY-MM-DD>.db` (UTC day) — `PREFIX`/`SUFFIX` `maintenance.rs:40-41`, `backup_name :78-84`; backup dir = `config.data_dir.join("backups")` (`serve.rs:208-212`). Written to `backups/.riot-proxy-<date>.db.partial` then renamed (`:287,307-308`); a leftover partial is removed first (`:289-291`) and on failure (`:317-321`).
- One per UTC day: skipped if today's exists (`:280-283`).
- `BACKUP_KEEP` (default 14, 0–365; `config.rs:729`; `KEEP_BACKUPS` `maintenance.rs:30`): 0 = take none, prune none (`:219-220`); otherwise delete every file beyond the newest `keep` whose name matches `riot-proxy-DDDD-DD-DD.db` exactly (`:86-105,223-231`).
- Free-space guard: `need = (db_bytes + wal_bytes) × BACKUP_FREE_FACTOR (1.2)`; if `statvfs(f_bavail)` of the backup dir < need → skipped with warning and `report.backup_skipped` (`maintenance.rs:34,294-306`; `disk.rs:95-123`). A failed copy is logged and does not stop the run (`:314-322`; ADR-136).
- On-demand: `riot-proxy backup OUT` runs the same `vacuum_into` (`src/cli/backup.rs:10-21`).
- Last run is exposed on `GET /v1/admin/backups` (`LastRun`, `maintenance.rs:127-139,180-183`).

### Daily `maintenance` steps, in order (`maintenance.rs:1-17,215-271`)
1. Backup (above), then prune old backups (`:219-232`).
2. One write tx: `DELETE FROM jobs WHERE state='done' AND finished_at < now − 7 d`; `DELETE FROM metrics_history WHERE at < now − 24 h` (`:234-250`).
3. `l2::sweep` (`:251`).
4. `PRAGMA optimize` (`:252`).
5. `PRAGMA wal_checkpoint(TRUNCATE)` (`:254-262`).
6. After the steps, whatever their result: enqueue `archive:offload` and `archive:index-snapshot` when a bucket is configured (`:204-211`, `then_bucket_jobs` `serve.rs:213-216`).
Schedule: ticked every 86 400 s as a singleton job at `priority::MAINTENANCE` (`src/jobs/ticks.rs:116-117,193`); the first tick fires at boot (`ticks.rs:3`).

---

## 4. Object storage

### Configuration (`src/config.rs`, `src/archive/objects.rs`)
| Var | Default / range | Evidence |
|---|---|---|
| `ARCHIVE_OBJECT_URL` | unset → everything stays in SQLite, no offload/snapshot kinds registered (`serve.rs:248-249`, `jobs/offload.rs:9-10`). `s3://<bucket>` (bucket 3–63 chars `[a-z0-9.-]`, no path) or `file:///abs/dir` | `config.rs:884-911` |
| `ARCHIVE_OBJECT_ENDPOINT` | required with `s3://`; `scheme://host[:port]`, trailing `/` stripped; `http://` refused in production | `config.rs:914-926,987-989` |
| `ARCHIVE_OBJECT_REGION` | `auto` | `config.rs:732` |
| `ARCHIVE_OBJECT_ACCESS_KEY` / `_SECRET_KEY` | both or neither (`config.rs:981-984`); required in production with `s3://` (`config.rs:990-995`) | `config.rs:979-995` |
| `ARCHIVE_OBJECT_CONCURRENCY` | 16 (1–256) | `config.rs:735`, `objects.rs:38-39` |
| `ARCHIVE_BODY_LOCAL_DAYS` | 14 (0–365) | `config.rs:736` |
| `ARCHIVE_PACK_TARGET_BYTES` | 64 MiB (1 MiB–1 GiB) | `config.rs:737` |
| `ARCHIVE_OFFLOAD_PACE_MS` | 50 (0–60 000) | `config.rs:738`, `serve.rs:256` |
| `file://` in production | refused | `config.rs:997-999` |

Client build (`objects.rs:99-139`): `s3://` → `object_store::aws::AmazonS3Builder` with bucket, region, endpoint, `with_virtual_hosted_style_request(virtual_hosted(endpoint, bucket))`, `allow_http` iff endpoint starts `http://`, credentials only if both keys set, `ClientOptions` = bundled `webpki_root_certs` only, `with_no_system_certificates(true)` (`objects.rs:329-338`). Virtual-hosted iff the endpoint host starts with `<bucket>.` (`objects.rs:319-326`); otherwise path style. `file://` → `LocalFileSystem::new_with_prefix(dir)` after `create_dir_all`; **keeps no user metadata** (`objects.rs:102-113,8-9`). `InMemory` for tests (`:142-144`). **No explicit timeouts or retry policy are configured** — `object_store`'s defaults apply (see §7). Boot builds the client without a request and logs `describe()` = `s3://<bucket> at <endpoint>` or `file://<dir>` (never keys) (`objects.rs:134,168-171`, `serve.rs:101-104`).

Operations (`objects.rs:173-276`): `put` (single `put_opts` ≤ 8 MiB; else multipart `WriteMultipart` with 8 MiB parts, 4 in flight, abort on failure — `:33-41,176-205`), `head` (`get_opts(head: true)`, NotFound → `None`), `get_range` / `get_ranges` (short read → `ObjectsError::ShortRead`, `:294-308`), `list(prefix)` sorted by key, `delete` (NotFound ok). Metadata attributes: `Content-Type: application/octet-stream` + `Attribute::Metadata(name)` per pair; object_store maps these to `x-amz-meta-<name>` on S3 (`:62-63,278-291`).

### Bucket key scheme
- Packs: `packs/<kind.table()>/<yyyy>/<mm>/<ulid>.pack` = `packs/matches/2026/10/01J….pack` or `packs/timelines/…`; year/month are UTC from the ULID's timestamp — `src/archive/pack.rs:124-134`; parsed back by `parse_key` (`:356-367`). Listing prefix `packs/` (`recovery.rs:30`).
- Index snapshots: `index/packs-<yyyy-mm-dd>.jsonl.zst`, `index/locations-<yyyy-mm-dd>.jsonl.zst` (UTC date) — `recovery.rs:29,179-192`.

### Pack binary format (`src/archive/pack.rs:1-11,140-193`)
```
[body 0][body 1] … [body n-1][index (zstd level 3)][index length: u64 LE][magic "RPXPACK1"]
```
- One `Kind` per pack (`match` | `timeline`, `pack.rs:29-53`). Bodies sorted by match id, each exactly the `body_zstd` frame from SQLite (no re-encode). `build` refuses empty input, duplicate ids and an empty body (`:143-158`).
- Index: JSON lines `{"kind","matchId","offset","len","sha256"(hex)}` (`:88-97,166-177`), zstd level 3 (`:25,181`). Trailer = 16 bytes (`TRAILER_LEN`, `:23`).
- Digests: sha256 per body (`Entry.sha256`) and of the whole object (`Pack.sha256`, `:189`). Object metadata: `sha256` (hex) and `bodies` (count) → `x-amz-meta-sha256`, `x-amz-meta-bodies` (`:115-121`).
- Readers: `index_range(size, trailer)` (`:195-213`), `parse_index` validates kind, bounds, order, non-overlap, non-empty (`:218-262`), `read` for an in-memory pack (`:266-274`), `verify` checks every body digest (`:277-282`).
- Pack id: `Ulid::from_parts(now_ms, getrandom u128)` (`offload.rs:64,300-306`).
- Target size: `ARCHIVE_PACK_TARGET_BYTES` (64 MiB default); a single larger body is a pack of its own (`offload.rs:176-181,210-218`).

### `body_packs` / `body_locations` semantics
`body_packs` = one row per uploaded, verified pack (size, body count, live bytes, whole-object sha256, `created_at` = run's `now_ms` or, on reindex, the ULID's time — `pack.rs:287-311`, `recovery.rs:544-558`). `body_locations` = where each `(match_id, kind)` body lives: pack id, byte `offset`, `len`, body sha256; a body relocated to a newer pack moves its row and the old pack's `live_bytes` drops (`pack.rs:318-354`). `packed_at` is set on the body row in the same tx (`:337-339,351`).

### Offload job (`src/jobs/offload.rs`, `src/archive/offload.rs`)
- Kind `archive:offload` (`jobs/mod.rs:41`), priority `MAINTENANCE = 30_000` (`jobs/mod.rs:82`), dedupe key = kind (at most one pending/running, `jobs/offload.rs:41-49`), **no lane** (calls no Riot route, `jobs/offload.rs:10-11`); `JOB_KIND_LIMITS` default runs one at a time (design/12 as built).
- Triggers: (a) `Pending` in-memory counter of unpacked zstd bytes per kind, raised by every `SqliteArchive::put` (`src/archive/mod.rs:168-171`, `jobs/offload.rs:89-111`); when a kind reaches `target` it enqueues, re-asking at most every `RETRY_ABSORBED = 60 s` if absorbed (`:51-53,96-110`); counter is **not seeded at boot** — it starts at 0 and (b) boot enqueues a run (`serve.rs:312-318`, `jobs/offload.rs:55-58`); (c) daily after maintenance (`maintenance.rs:204-211`); a run resets the counter to the measured backlog (`jobs/offload.rs:192,197-202`).
- One run (`jobs/offload.rs:146-195`), `cutoff = now − ARCHIVE_BODY_LOCAL_DAYS` days:
  1. **Pack**, for `Match` then `Timeline`: loop `next_pack` → ids of unpacked bodies, oldest `archived_at` first (timelines in match-id order, window from the match's `archived_at`, no match = new), stop at `target_bytes`; fewer bytes are packed only if one is older than cutoff (`archive/offload.rs:175-223`). `write_pack`: read local bodies (`length(body_zstd) > 0`, chunks of 500, `:120-141`), build on `spawn_blocking`, `objects.put` with metadata, `verify` (HEAD: size must match and `sha256` metadata must equal; no metadata → read whole object back and hash) (`:89-118`), then one tx `record_on` (`:72-81`). Sleep `pace` between packs (`jobs/offload.rs:169`). Metrics: `archive_offload_bytes_total{kind}` (`:164-165`).
  2. **Free**, per kind: `next_free` up to `FREE_BATCH = 2 000` packed, located, local bodies whose match `archived_at < cutoff` (`archive/offload.rs:173,227-255`); `free` = `UPDATE … SET body_zstd = X'', offloaded_at = ?1 WHERE … packed_at IS NOT NULL AND offloaded_at IS NULL AND EXISTS body_locations` in one tx (`:147-170`); sleep `pace`; stop when a batch frees 0.
  3. **Measure**: `backlog` per kind → `archive_offload_pending{kind,step=pack|free}` gauges, `Pending` counter reset, `archive_packs` gauge from `count(*) FROM body_packs` (`jobs/offload.rs:197-217`, `archive/offload.rs:257-298`).
- Failure: any store/DB/pack error → `JobError::Retry` (`jobs/offload.rs:147`), scheduler backoff `2^attempts × 30 s ± 20 %`, fifth failure final (`scheduler.rs:13,39`). Nothing unverified is recorded; a crash between PUT and the tx leaves an orphan object the rows don't reference (`archive/offload.rs:6-9`).
- `ARCHIVE_BODY_LOCAL_DAYS=0` frees every packed body on the next run (cutoff = now).
- `matches::put` / `put_timeline` called outside `SqliteArchive` (e.g. `migrate_v1.rs:273-289`) do not raise `Pending`; those bodies are packed by the boot/daily run because `packed_at IS NULL`.

### Reads of offloaded bodies (`src/archive/matches.rs:176-313`)
`read_rows` LEFT JOINs `body_locations`; an **empty `body_zstd`** alone means offloaded (`offloaded_at` is not consulted, `:196-207`). Offloaded ids are grouped by `pack_id`, each pack fetched with **one `get_ranges`** call (object_store merges close ranges) with `buffer_unordered(objects.concurrency())` = `ARCHIVE_OBJECT_CONCURRENCY` (`:250-254`); each part is sha256-checked against `body_locations.sha256`, then decompressed on `spawn_blocking` (`:262-313`). Failures (no location, no bucket attached, pack-id not a ULID, GET error, digest mismatch, zstd error) log one `warn` and **drop the body**, so `Archive::get` returns `None` and the fetcher goes to Riot (`:217-219,234-248,268-281,288-292,299-304`; `src/archive/mod.rs:128-144`). A body read back is **not** re-written to SQLite; a re-archive (`put_stored`) makes it local again with `offloaded_at = NULL` (`:384,428`). No metric is emitted for read failures (none found in `matches.rs`; `metrics.rs:60-62` lists only the three archive metrics).

### Index snapshots (`src/archive/recovery.rs`, `src/jobs/index_snapshot.rs`)
- Kind `archive:index-snapshot`, priority MAINTENANCE, dedupe = kind (`index_snapshot.rs:17-24`), handler `IndexSnapshotHandler` → `recovery::write_snapshot`, errors → `JobError::Retry` (`index_snapshot.rs:26-47`); registered only with a bucket (`serve.rs:248-269`); enqueued after each maintenance run (`maintenance.rs:208-210`) → daily.
- `write_snapshot`: one read transaction (`unchecked_transaction`) streams `body_packs` (ORDER BY pack_id) then `body_locations` (ORDER BY match_id, kind) as JSON lines (`PackRow`/`LocationRow`, camelCase, sha256 hex — `recovery.rs:63-88,109-146`), zstd level 3 **with frame checksum** (`:32,194-199`); PUTs `locations-<date>` first, then `packs-<date>`, metadata `last-pack` (newest pack id) and `rows` (`:207-271`). A second run the same day overwrites.
- Lifecycle (ops, not code): Onidel `index/` expire 14 d; R2 `index/` 21 d; R2 abort-incomplete-multipart 1 d; **no expiry on `packs/`** (`deploy/README.md:29-46`, design/12 §Replica).

### `riot-proxy packs …` (`src/cli/packs.rs`)
Needs `ARCHIVE_OBJECT_URL` (`packs.rs:29`); opens the DB with 1 reader (`:30`).
- `reindex [--since <ulid>]` → `recovery::reindex`: lists `packs/`, skips non-pack keys, filters `id > since`, sorts by ULID, reads each pack with `concurrency` in flight (HEAD for `sha256` metadata then two ranged GETs: trailer, index; a store without metadata reads the whole object and hashes it — `recovery.rs:468-493`), records each pack in **its own tx** (`body_packs` INSERT … ON CONFLICT DO NOTHING with `created_at` = ULID time, `locate_on`, `live_bytes` recomputed — `:542-571`). Unreadable packs are listed in `failed`; the command exits non-zero after recording the rest (`packs.rs:70-83`). Idempotent.
- `restore-index` → `recovery::restore_index`: newest date having **both** objects (`snapshot_dates` `:273-309`), `load_snapshot` validates (ULIDs unique, 32-byte digests, every location in a listed pack of its kind, `len > 0`, `offset ≥ 0`, each pack's `live_bytes` == sum of its locations — `:333-390`), `replace_tables` wipes and reloads both tables in one tx and sets `packed_at` on located rows lacking it (`:392-447`), then `reindex(since = max pack_id in the snapshot)` (`:597-612`). A bad snapshot is skipped for the previous date; none → full reindex (`:613-621`). Doc says run with the service stopped (`:595-596`, `deploy/README.md`).
- `snapshot` → `write_snapshot` now (`packs.rs:55-66`).

### System of record vs replica; R2
- **SQLite (`riot-proxy.db`) is the system of record** for everything except packed bodies: facts, analytics, jobs, players, consumers, limiter, and the pack map (`body_packs`, `body_locations`). Its copies: local `backups/` (`VACUUM INTO`) and an ops-side rclone copy to R2 `ngf-backups` (design/12 topology lines 60-77; `deploy/README.md:151-152`).
- **The configured bucket (`ARCHIVE_OBJECT_URL`, Onidel `riot-proxy-archive` in production) is the system of record for offloaded bodies**: once `free` runs, SQLite holds `X''` and the only copies are the pack and its replica (`V0023:3-5`; design/12 §Rollback: unsetting the URL makes offloaded rows read `None` and re-fetch from Riot). The pack map has three copies: SQLite, `index/` snapshots, and each pack's own index (`recovery.rs:1-15`).
- **R2 is a replica only**, produced by `rclone sync` from the box hourly via `deploy/riot-proxy-replica.{service,timer}` (`deploy/README.md:6,15-16`); design/12 §Replica says the proxy never reads it. **There is no R2-specific code**: the only mentions of R2 in `src/` are comments (`objects.rs:5-6,35-36`; `cli/packs.rs:3`); the `ws/hub.rs:244` hit is a variable named `r2`. Failover is purely configuration: point `ARCHIVE_OBJECT_ENDPOINT` at `https://<account id>.r2.cloudflarestorage.com` with `ARCHIVE_OBJECT_REGION=auto` and restart (`deploy/README.md:142-145`, `packs.rs:3`). The R2 multipart note (`objects.rs:35-36`, equal 8 MiB parts) is the only R2 accommodation in code.

### Where code differs from design/12 and design/04
1. design/12 §Offload says the job runs in the "bulk lane"; code gives it **no lane** (`jobs/offload.rs:10-11`, `enqueue_on` lanes come from `lanes::of(kind, payload)` `scheduler.rs:227`).
2. design/12 §Offload says the unpacked-bytes counter is "seeded at boot from the `*_to_pack` indexes"; code starts at 0 and relies on the boot run + `measure()` after it (`jobs/offload.rs:55-58,197-202`; `serve.rs:312-318`).
3. design/12 §Offload says a store error ends the turn with the job `failed`; code returns `JobError::Retry` (`jobs/offload.rs:147`), i.e. retried with backoff, final only after five attempts (`scheduler.rs:13,39`).
4. design/12 §Read path says an empty body **and** `offloaded_at` set triggers the location lookup; code keys on the empty blob alone (`matches.rs:196-207`).
5. design/12 §Configuration lists `ARCHIVE_COMPACT_BELOW` (OBJ-06); no such variable exists in `config.rs` (the `ARCHIVE_*` list at `config.rs:55-63` ends at `ARCHIVE_OFFLOAD_PACE_MS`). No compaction or orphan-sweep code was found in `src/archive/` or `src/jobs/` (see §7).
6. design/12 §Replica says `--since` is "recorded … in the object's metadata (`x-amz-meta-last-pack`)"; as built, `restore_index` takes `since` from the snapshot's own max `packId` (`recovery.rs:602-605`) because a local-directory store keeps no metadata (design/12 "as built" paragraph agrees).
7. design/04 §What lives where shows `data/ddragon/` as "patch JSON + images"; the sync writes JSON only — images are mirrored on first request by `/ddragon/*` (`src/static/mod.rs:5-7`, `src/static/images.rs:1-8`).
8. design/04 §SQLite configuration "Reader pool of `n_cores`" holds for `serve` (`mod.rs:181-183`); one-shot commands (`migrate-v1`, `packs`, `key`, …) open 1 reader (`cli/mod.rs:136-141`), and `riot-proxy migrate` opens a raw connection **without** the pragmas (`cli/mod.rs:144-153`).
9. design/12 §Storage layout says snapshot metadata is `x-amz-meta-last-pack`; code sets metadata names `last-pack` and `rows` and relies on object_store to add the `x-amz-meta-` prefix (`recovery.rs:255,262`; `objects.rs:62-63`). Equivalent on S3; absent on `file://`.
10. design/12 topology says SQLite backups are copied to R2 "every 6 h"; `deploy/` contains only the hourly archive replica timer (`deploy/README.md:6`); the backup-copy timer is not in this repo (§7).

---

## 5. Local file storage under `DATA_DIR`

`DATA_DIR` default `./data` (`src/config.rs:622`), created at load (`config.rs:589,792-797`). Observed dev tree: `data/backups/`, `data/ddragon/`, `data/riot-proxy.db`, `-shm`, `-wal`.

```
$DATA_DIR/
├── riot-proxy.db                      DB_FILE_NAME (config.rs:101,624); WAL mode
├── riot-proxy.db-wal                  SQLite WAL (disk.rs:76-81 wal_path); 0 bytes after TRUNCATE checkpoint
├── riot-proxy.db-shm                  SQLite shared memory (present in dev listing)
├── backups/                           config.data_dir.join("backups") (serve.rs:210)
│   ├── riot-proxy-YYYY-MM-DD.db       VACUUM INTO, one per UTC day, newest BACKUP_KEEP kept (maintenance.rs:40-41,78-84)
│   └── .riot-proxy-YYYY-MM-DD.db.partial   transient during a copy (maintenance.rs:287)
└── ddragon/                           DDRAGON_DIR, default data_dir/ddragon (config.rs:703-705)
    ├── <version>/                     e.g. 16.19.1 — a patch dir counts as mirrored once versions.json exists (static/mod.rs:9-12)
    │   ├── champion.json, item.json, runesReforged.json, summoner.json, profileicon.json, map.json   DATA_FILES (static/mod.rs:28-35; jobs/ddragon.rs:243-244)
    │   ├── versions.json              written last (jobs/ddragon.rs:256; static/mod.rs:48)
    │   └── img/<kind>/<file>          images fetched on first request (static/images.rs:7-8); rune icons img/perk-images/… (images.rs:10-13)
    └── meta/
        └── queues.json                META_DIR/META_FILES (static/mod.rs:51-54; jobs/ddragon.rs:272-274)
```
A `file:///<dir>` object store (dev/tests only) lives wherever the URL points, holding `packs/…` and `index/…` exactly as a bucket would (`objects.rs:102-113`). Files are written atomically via `write_atomic` (`jobs/ddragon.rs:204`).

---

## 6. `riot-proxy migrate-v1 --from <dump> [--key-scope <8hex>]` (`src/cli/migrate_v1.rs`)

- **Reads**: a `pg_dump -Fc` archive (magic `PGDMP`) streamed through `pg_restore --data-only --table=matches --table=players --table=consumers -f -` (must be on PATH; must be ≥ the dump's PG version, v1 ran PG 18 — `:353-377,388-395`), or a file already in COPY text format. Parses `COPY <schema>.<table> (cols) FROM stdin;` blocks on a blocking thread (`:142-153,180-234`), unescaping Postgres text escapes (`:68-126`) and `timestamptz` → unix ms (`:130-140`). Only `matches` (`match_id, region, data, timeline, fetched_at`) and `players` rows are consumed; every other table (incl. `consumers`, v1's derived `match_participants`, `match_bans`, analytics) is counted as ignored (`:9-14,212-232`).
- **Writes**: `matches` → `matches::put(db, id, region, facts_scope, body, fetched_ms)` in batches of 64 concurrent puts (`BATCH`, `:37,270-282`), i.e. the normal archive path: zstd body, `matches` upsert, `match_facts` (fresh `facts_version`), `match_bans`, `remake`/`game_duration`, tier stamps. A v1 `timeline` column → `matches::put_timeline` (`:287-290`). Bodies that fail `extract` (`NotAMatch`) are reported as skipped, not fatal (`:292-294`). `players` → `INSERT … ON CONFLICT (key_scope, puuid) DO UPDATE` keeping v1's own `key_scope`, `platform`, names, `tracked`, `last_seen_match_id`, `updated_at`, and v1's three backfill stamps folded into `backfill_state` JSON `{startedAt, doneAt, depth}` (`:300-350`). `consumers` are **not migrated** ("mint new keys", `:14,406-408`).
- Facts key scope: `--key-scope` or this deployment's `KeyScope::from_key(RIOT_API_KEY)` (`:379-384`); a note is printed if players carry other scopes (`:409-421`).
- Opens the DB via `open_db` (1 reader, migrations applied) (`:380`). Does not raise the offload `Pending` counter (uses `matches::put` directly, `:273`).

---

## 7. TODO (unverified)

- `TODO (unverified)`: `object_store` client timeouts/retry counts — code sets only root certificates (`objects.rs:330-338`); the effective request timeout, connect timeout and retry policy are the crate's defaults for the version pinned in `Cargo.lock` (not read).
- `TODO (unverified)`: OBJ-06 (orphan-pack sweep, compaction, `ARCHIVE_COMPACT_BELOW`) — no implementation found in `src/archive/*.rs`, `src/jobs/*.rs` or `src/config.rs`; `Objects::delete` (`objects.rs:271-276`) has no call site in the files read. `PROGRESS.md`/`docs/IMPLEMENTATION.md` not consulted for its status.
- `TODO (unverified)`: the ops timer that copies `backups/` to R2 `ngf-backups` ("every 6 h" in design/12 topology) — not in `deploy/`; only `riot-proxy-replica.{service,timer}` (hourly archive sync) is. `deploy/README.md:100-175` references `/etc/rclone/r2.conf` for the backups bucket, so it exists on the box outside this repo.
- `TODO (unverified)`: exact `ClaimFilter`/lane behaviour for `archive:offload` under `JOB_KIND_LIMITS` default — design/12 "as built" says `archive:offload=1`; `config.rs` default for `job_kind_limits` was not read.
- `TODO (unverified)`: whether any route reads `body_packs`/`body_locations` directly beyond `stats.rs` (`archive::matches::stats`, `matches.rs:488-514`) — `src/routes/admin.rs` matched a grep for table names but its lines were not inspected.
- `TODO (unverified)`: `ladder_entries` rows for players who drop off a ladder are never removed by code read here; a "stale entry" rule may exist in `src/jobs/ladder/*.rs` beyond `store.rs` (`ranks.rs`, `walk`) — not read in full.
- `TODO (unverified)`: the dev DB on this box is at V0006 (last applied 2026-10-03T16:55Z); V0007–V0027 will apply on the next `serve`/`migrate` against it. Not a code fact, an environment observation.
- `TODO (unverified)`: design/12 says packs are "verified on Onidel" before free; `verify` compares the HEAD size and the `sha256` **metadata the uploader itself set**, not a server-computed checksum (`offload.rs:89-118`) — whether `object_store` checks an ETag/CRC on upload depends on crate defaults.

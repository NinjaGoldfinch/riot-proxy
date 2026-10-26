# LAD-02 + THR-05 plan: the Master players Riot's 10,000 cap leaves out

Written 2026-10-27 against `origin/main` **b7ab034** (v2.9.0). Every `file:line` is at b7ab034. The query timings in §1 come from the probe in §10, run in this session. Nothing under `src/` or `tests/` was edited before approval.

**Preflight.**
- `git fetch origin` gives b7ab034 (tag v2.9.0). `gh pr list` shows no open PRs. The open branches are `feat/thr-01-archive-batch` (a7b9ce5, docs only), `feat/WR-04-checkpoint-off-the-writer` (0a0a94f, wip), and three branches already merged or docs only (`feat/THR-07-delta-batch-memory`, `feat/TL-03-catch-up-after-collect`, `docs/THR-03-plan`). None of them adds a file under `src/db/migrations/` (`git diff --stat origin/main...origin/<b> -- src/db/migrations`). The latest migration is `V0030__collect_cursor.sql`, so this task takes **V0031**. It moves to V0032 if THR-01 lands its index first.
- The latest ADR is ADR-185 (`docs/DECISIONS.md:2060`). `grep -rn 'ADR-18[6-9]'` over `docs`, `PROGRESS.md` and every open branch is empty, and WR-03 holds ADR-181. So this task takes **ADR-186**. Both numbers are checked again before merging.
- Branch `feat/LAD-02-discover`, cut from b7ab034. This plan comes first. The code follows on the same branch once the plan is approved, in one PR with the footer `Task: LAD-02`.
- There is no Riot key in this session and no dev VM. Everything below is wiremock or arithmetic, and §9 is the owner's.

---

**The plan in one paragraph.**
- **No new stage, no new job.** Discover becomes a step of `ranks:lookup`. That job already runs at the archive hand-off, already runs alongside the crawl's `archive:match` downloads (§3 (f)), and already asks Riot about exactly the players LAD-02 wants, most games first. What it doesn't do is put a Master answer on the ladder.
- **Three changes close the gap:**
  1. When the ladder's latest completed crawl capped MASTER, a lookup whose answer is Master or above for the job's queue **upserts the player into `ladder_entries`**. The row carries that crawl's ids and a new `discovered_at`.
  2. The same run first **re-checks the ladder's apex players that this crawl's enumerate didn't list**: players found by earlier runs, and listed players who fell out of the top 10,000. That keeps their wins and losses current, which THR-04's cursor needs.
  3. **The collect candidates include players discovered within `RANK_LOOKUP_RECHECK_S`.** Today they are only `last_seen_crawl_id = this crawl` (`store.rs:457`), and an upsert alone would never be collected (§2, row 1).
- **New schema and config.** One migration of four `ADD COLUMN`s and no new config.
- **Cost.** The re-checks count against `RANK_LOOKUP_LIMIT`, so a run makes at most as many lookups as today's. Crawl N's time is unchanged by construction. Crawl N+1's collect grows by the discovered players whose totals moved, which is the point of the task (Q1).

---

## 1. Measurements

There is no key, so this section measures what the design adds to the database and works out the request cost from the code. It doesn't measure production.

**The candidate query** (`store::collect_candidates`, run inside `advance()`'s write transaction, `src/jobs/ladder.rs:944`). The probe (§10) built a kr ladder of the shape LAD-02 will leave behind:
- 11,000 rows listed by crawl M;
- 20,000 Master rows discovered by crawl N a day earlier;
- 5,000 Master rows last updated 30 days ago;
- 150,000 Diamond rows on five other platforms.

It used Python's SQLite 3.46.1. rusqlite 0.39 bundles a newer SQLite, so the plans may differ slightly, but not in kind.

| query | rows | time | plan |
|---|---|---|---|
| today's (`last_seen_crawl_id = M`) | 11,000 | 9.2 ms | primary-key prefix `(key_scope, platform, queue)`, then a temp B-tree for the order |
| proposed (`… OR discovered_at >= ?since`) | 31,000 | 19.8 ms | the same seek: today's query doesn't use `ladder_entries_last_seen` either |
| proposed, as a `UNION` with a partial index on `discovered_at` | 31,000 | 53.8 ms | two seeks and a de-duplicating temp B-tree, which is slower |
| the re-check plan (§3 (b)) | 20,000 | 15.4 ms | `ladder_entries_ladder` `(key_scope, platform, queue, tier)`, one seek per apex tier |

So the `OR` goes in as is and **V0031 adds no index**. The extra 10 ms is spent once per crawl, on the writer, well under THR-03's 250 ms bar. The re-check plan runs on a reader, as `ranks::plan` already does (`src/archive/ranks.rs:120-142`).

**Lookups per run.** `ranks:lookup` is one job per ladder. It does at most `RANK_LOOKUP_LIMIT` (50,000) lookups, one `league.entriesByPuuid` at a time on one worker (`src/jobs/ladder/ranks.rs:98-139`).
- The re-checks come out of the same 50,000. A run's request count and its worker time can't exceed today's.
- On kr, the re-check set settles at about the Master players past the cap: ~20,000 (LAD intro: ~30K Master, 10K listed), plus the daily churn at the bottom of the list. That leaves ~30,000 for new candidates.
- The method limit is not the constraint. The recorded production headers for this endpoint read `X-Method-Rate-Limit: 20000:10,1200000:600` (`tests/fixtures/replay/cold-lookup/03-league.entriesByPuuid.json`).
- The run is paced by the round trip, or by the platform's app limit when that comes first. At 100–250 ms a request, 50,000 lookups take 1.4–3.5 h on one worker. That is already the case today.

**Crawl N+1's extra collect.** Each discovered player is a THR-04 first sight at their first collect (one `match.idsByPuuid`, newest 100). After that, it's a cursor walk only if their total moved. On kr, the first wave is up to ~20,000 extra first-sight requests on asia's match-v5 scope, on top of today's ~11,000 candidates at most. Later crawls cost one request per discovered player who played. That share is unmeasured, as THR-04's Q4 was.

## 2. What the brief and the rows say that the code doesn't

The code wins in each case. The plan follows the right-hand column.

| brief or row says | b7ab034 | effect |
|---|---|---|
| "THR-05's discovered players get their cursor on their first collect … as long as discover upserts their `ladder_entries` row before the next crawl's collect" (`PROGRESS.md:401`) | `collect_candidates` takes only rows with `last_seen_crawl_id = ?crawl` (`src/jobs/ladder/store.rs:457`). Enumerate restamps only the players Riot lists (`store.rs:244`), and a discovered player is the one Riot doesn't list. So an upserted row is **never a candidate** in crawl N+1 | the candidate query gains `OR le.discovered_at >= ?since` (§3 (d)) |
| the row's (c): an upserted player is then collected through THR-04 | THR-04 skips a player whose `wins + losses = collected_games` (`store.rs:453`). Only enumerate writes `wins`/`losses`, and it never sees a discovered player again. After one collect they would be "unchanged" forever | the run re-checks known apex players each crawl, writing fresh totals (§3 (b)) |
| "ranks:lookup … skips anyone looked up within 7 days" serves as discover's recheck | `PLAN` also skips **every player with a ladder row** for the queue (`src/archive/ranks.rs:176-177`). A discovered player, or a listed one who drops out of the top 10,000, is never looked up again | the re-check set is planned separately and isn't filtered by `rank_lookups` (§3 (b)) |
| "Today `ranks:lookup` queues after the crawl's recompute" (brief, THR-05 (b)) | it is queued in the same transaction as the counting job (`ladder.rs:852-860`), at the archive hand-off, when the crawl completes. The recompute has no lane and the lookup has kr's, so both start at once. The crawl's `archive:match` jobs are queued just before, in that same leg (`ladder.rs:775-805`) | THR-05 (b) needs no move (§3 (f)) |
| ADR-111: "priority `BACKFILL + 6`, after the crawl's `archive:match` jobs" | 20,005 and 20,006 are one band (design/06 `:94`, bands of 10,000). The claim then takes the lane with the fewest running jobs first (design/06 `:46`, `CLAIM_ORDER`). An idle kr lane beats asia's next download, so the lookup takes a worker beside the downloads. "After" holds only within a lane | pinned by a new lanes test (§7, test 11). ADR-186 corrects the wording |
| the row's crawl card "`discover` stage bar" in `src/ui/dashboard.html` | there is no `dashboard.html`. The card is `activityView` in `src/ui/admin.html:1790-1816`, with the totals at `:1803` and the counters at `:1809`. `admin.html` is 159,886 bytes against a 163,840-byte budget (`tests/ui.rs:203`) | a single line in the details fold, about 300 bytes (§3 (g)). The budget isn't raised |
| a fourth stage after archive | the crawl completes at the hand-off (`next_stage`, `ladder.rs:921-928`), long before its matches are downloaded. A stage there would hold the crawl open for the lookups' 1.4–3.5 h, and `finished_at − started_at` would grow by that much | no new stage. `next_stage`/`advance` keep their four stages (§3 (f), Q2) |
| `apex_capped` at about line 1798 of `admin.html`, the crawl view at about `admin.rs:1797` | `admin.html:1798`. `LadderCrawlSummary` is `src/routes/admin.rs:1764-1798`, its `new` `:1800-1822` | anchors only |
| the row's metric `ladder_discovered_total` | its neighbours are `proxy_ladder_*_total` (`src/metrics.rs:35-40`, `docs/design/metrics.md:21-26`) | `proxy_ladder_discovered_total{platform,queue}` (Q4) |

## 3. Design

**(a) When discover is on.** A `ranks:lookup` run is in **discover mode** when, at planning time, the ladder's latest *completed* crawl (`store::latest_completed`, new: `ladder_crawls_recent`, `V0004__ladder.sql:35`) has `MASTER` in `apex_capped`.
- The run stores that crawl's id and floor in its payload: `RankLookup.discover: Option<{crawlId, tierFloor}>`, `#[serde(default)]`. This sits beside `planned` (`src/jobs/ladder/ranks.rs:34-43`).
- The crawl is resolved at planning time, not at enqueue, because of dedupe. A crawl that completes while the ladder's last lookup is still pending has its own job deduped into that one (`ranks.rs:57`). The pending job then plans for the newer crawl.
- A queued payload from before the upgrade has no `discover`. If it is already `planned`, it finishes its list as today.
- An uncapped crawl (oc1, or any ladder whose Master list is short) is unchanged. Riot listed every Master player there, so a Master player who isn't on the ladder can't exist.
- `RANK_LOOKUP_LIMIT=0` still turns the job off (`ranks.rs:63`), discover included.

**(b) What it plans.** In `lookup_ranks`'s planning branch (`ranks.rs:75-93`), when the waiting list is empty:
1. **The re-checks, first.** These are rows of this ladder (`key_scope, platform, queue`) with `tier IN APEX_TIERS`, `last_seen_crawl_id <> ?crawl` and `updated_at >= now − RANK_LOOKUP_RECHECK_S`. It covers two kinds of player:
   - players discovered by earlier runs (their `updated_at` is the last lookup that kept them);
   - listed players this crawl's enumerate didn't list: the bottom of the 10,000, or someone who left the tier.

   It has no `rank_lookups` filter: they are re-checked every crawl. They go into `rank_lookup_queue` with `games = REFRESH_GAMES` (2⁴⁰), so `take`'s `ORDER BY games DESC` (`src/archive/ranks.rs:192-193`) hands them out before any planned player. The column comment in V0031 says so. A row older than the window has gone a week without a lookup that kept it, and it is left alone (G4).
2. **Then `PLAN` as today**, with `limit − re-checks` as its limit. It skips ladder players and recent lookups, and puts the most games first (`ranks.rs:168-181`).

The planned count is bumped into `ladder_crawls.discover_planned`.

**(c) What a lookup does in discover mode.** The fetch already returns the body (`LadderContext::get`, `ladder.rs:400-411`) and `Archive::observe` records it first (`src/fetcher.rs:517`, `src/archive/mod.rs:180-197`). That writes `player_ranks`, the stamp in `rank_lookups`, and the late stamps (§3 (e)).

On `Ok(body)` (`ranks.rs:110`), a pure function `discovered(body, queue, floor) -> Option<Entry>` reads league-v4's `LeagueEntryDTO`. The fields and their names are the portal's (`src/routes/riot_schemas.rs:50-75`, ADR-110) and the recorded body's (`03-league.entriesByPuuid.body`): `queueType`, `tier`, `rank`, `leaguePoints`, `wins`, `losses`, `veteran`, `inactive`, `freshBlood`, `hotStreak`, `puuid`. The ladder's `RiotEntry` (`ladder.rs:355-366`) gains `queue_type` and `tier`, and `to_entry` (`:375`) is reused, so a missing `puuid` falls back to the job's player. The function returns the entry for the job's queue when its tier is in `APEX_TIERS` and in `tiers_at_or_above(floor)`.
- **Some(entry):** `store::discover_entry` does one write transaction:
  - the same upsert as `write_page` (`store.rs:233-246`), with `first_seen_crawl_id` set once and `last_seen_crawl_id` = the run's crawl, plus `discovered_at = now`;
  - the untracked `players` row (`:249-254`);
  - `ladder_crawls.discover_found + 1`.

  `collected_games`/`collected_at` aren't touched, so a re-checked player keeps their cursor. Then `proxy_ladder_discovered_total{platform,queue}` is incremented.
- **Below the floor, unranked in this queue, or another queue only:** if the row has a `discovered_at`, it is set to `NULL`, and nothing else changes. A listed player who dropped to Diamond keeps a stale `MASTER` row, as rows do today. The tier stamp prefers the newer `player_ranks` (`src/archive/tiers.rs:36-46`), so the stale row doesn't mislabel their games.
- **404:** stamped as today (`ranks.rs:111-121`). The row is left alone and ages out of the window.
- **A yield:** put back as today (`ranks.rs:122-133`).

`discover_done` is bumped once per turn of 25 by the count done, in one write, and also when a turn ends early on a yield.

**(d) Collect in the next crawl** (owner answer LAD-02 (e)). `collect_candidates` (`store.rs:446-472`) changes one predicate:

```sql
  AND (le.last_seen_crawl_id = ?4 OR le.discovered_at >= ?6)   -- ?6 = crawl.started_at − RANK_LOOKUP_RECHECK_S·1000
```

- Everything else stays: the `startedAt` exclusion, the order, and the unchanged partition.
- `advance()` gets the recheck from `end_leg`'s closure, as it gets `backfill_limit` (`ladder.rs:826-842`).
- A discovered player's first collect is THR-04's first sight. Its stamp sets `collected_games` to the total discover wrote.
- That crawl's own run re-checks them and writes the new total, so the crawl after it walks them only if they played: one `startTime` request, or none.

**(e) Tiers and analytics: nothing new.**
- The lookup's `ranks::record` late-stamps the player's `UNKNOWN` facts from the last `TIER_LATE_STAMP_DAYS` (`src/archive/ranks.rs:65`, ADR-127).
- Trigger `match_facts_tier_moved` (`V0029__analytics_counted.sql:40-49`) writes `analytics_retract` for the steps that already counted them.
- `ranks_done` queues `aggregate:delta` for a stamped ladder (`ranks.rs:147-168`), which subtracts and recounts (ADR-182).
- Later archives stamp the player from `player_ranks` or the ladder row, whichever is newer (`tiers.rs:36-46`).

So owner Q5 stands: no retract step and no weekly rebuild. §7 test 10 pins the totals.

**(f) THR-05 (b): alongside archive, with no move.**
- The lookup is queued in the archive leg's own transaction (`ladder.rs:852-860`), just after the leg queued every `archive:match` (`:775-805`).
- It is laned on the platform (`src/jobs/lanes.rs:48`) and the downloads on the region (`:52-55`). They are in one band, so the next free worker takes the lookup (§2, row 5).
- It uses at most one worker, as today. The crawl's four stages, `next_stage` (`ladder.rs:921-928`) and `advance` (`:934-988`) keep their shape. Only the candidate query in `advance` changes (d).

**(g) Visibility.**
- `ladder_crawls` gains three counters: `discover_planned`, `discover_done`, `discover_found`.
- They join `Counters`, `COLUMNS` and `crawl_row` (`store.rs:37-77`), plus three `Counter` arms (`:533-551`).
- `LadderCrawlSummary` gains `discoverPlanned`, `discoverDone` and `discoverFound` (`admin.rs:1764-1822`, OpenAPI snapshot), each documented.
- `crawl.phase` frames carry them through `json!(crawl.counters)` (`ladder.rs:1021`). That is additive. They are only bumped after the crawl's last phase frame, so the card reads them from the crawl route.
- The card's details fold gets one line after "Match downloads", only when `apexCapped` includes MASTER: `<h3>Master discovery</h3><p class="muted">1,234 of 20,000 looked up · 812 Master found</p>`, or "not started" while `discoverPlanned` is 0. Past crawls open the same view, so the line stays on the crawl it belongs to.
- No stage bar: discover isn't a stage (Q2).

**(h) Removal is out of scope, and nothing needs it.**
- No code deletes `ladder_entries` rows (`grep 'DELETE FROM ladder_entries' src` is empty).
- A discovered player who drops below Master has `discovered_at` cleared on their next lookup (c), or ages out of the window after `RANK_LOOKUP_RECHECK_S`. Either way they stop being a collect candidate, which is the "aging" a listed player gets from `last_seen_crawl_id` today.
- A listed player who drops is unchanged from today.

## 4. Gaps (each settled in ADR-186)

- **G1: a stale crawl id.** A run that is still working its list when the next crawl completes keeps the older crawl's id, because the newer crawl's job deduped into it. Its upserts carry that id in `last_seen_crawl_id` and their counts go to the older crawl's row. Collect doesn't care, since (d) reads `discovered_at`, not the id. The next run, planned for the newest crawl, re-checks them. With `LADDER_CRAWL_S=86400` and a run of at most 3.5 h, this only happens with manual back-to-back crawls.
- **G2: re-check vs collect race.** A crawl N+1 can start while crawl N's run is still re-checking. A re-check that writes a newer total *during* N+1's walk of the same player makes the walk's stamp record a total that includes a game the walk didn't list. That player is then "unchanged" until they next play. That next walk's `startTime` is `collected_at − overlap`, from before the game, so it lists the game then. The game is late, not lost: the remake case of ADR-185.
- **G3: the limit is shared.** Re-checks first means that on kr about 20,000 of the 50,000 lookups go to players who are already placed, not to `UNKNOWN` ones (ADR-111's purpose). The alternative is re-checks outside the limit, at up to 70,000 lookups a run (Q3).
- **G4: the re-check window.** `RANK_LOOKUP_RECHECK_S` (7 days) bounds both the re-check set (b) and the collect clause (d), so one knob says how long a player we can't confirm stays on the ladder. A player missed by a full list (G3), a failed crawl or a 404 is kept for a week, not dropped at once. A row last confirmed more than a week ago isn't re-checked, but `PLAN` doesn't take it back either, because it still has a ladder row. Such a player comes back only if the list or a profile lookup sees them. ADR-186 records this as accepted.
- **G5: GM and Challenger.** Their lists are under the cap and complete, so a lookup that says GM or Challenger for an unlisted player is a promotion since the list was read. Upserting them is correct and rare. The crawl's floor is always MASTER or lower when MASTER is capped, so (c)'s floor check never rejects an apex tier. It stays as a guard.
- **G6: counts.** `entries_seen`/`players_discovered` stay what enumerate saw (11,000 on kr), as LAD-01's card reads them. "Master players stored" is `discover_found` plus the listed Master, and the accept query (§9) counts rows.
- **G7: a cached answer.** `league.entriesByPuuid` is kept 300 s (`src/riot/endpoints.rs:129`). A cache hit doesn't call `observe`, but (c) parses the body it gets back, so a cached answer upserts too. Only `rank_lookups`' stamp is skipped, and a player answered from the cache was stamped when it was fetched.

## 5. V0031

```sql
-- LAD-02 / THR-05 (ADR-186): Master players past Riot's 10,000 cap, found
-- among archived matches' players by ranks:lookup. discovered_at is when a
-- lookup last placed the player at Master or above on this ladder; a crawl
-- collects such players for RANK_LOOKUP_RECHECK_S. The counters are that
-- run's. rank_lookup_queue.games is 2^40 for a re-check, so it is taken first.
ALTER TABLE ladder_entries ADD COLUMN discovered_at INTEGER;                       -- unix ms
ALTER TABLE ladder_crawls ADD COLUMN discover_planned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ladder_crawls ADD COLUMN discover_done INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ladder_crawls ADD COLUMN discover_found INTEGER NOT NULL DEFAULT 0;
```

All four are `ADD COLUMN`s that are nullable or have a constant default, so no row is rewritten and no index is built (§1). The first capped crawl after the upgrade discovers. The crawl after that is the first to collect anyone.

## 6. Files

- `src/db/migrations/V0031__ladder_discover.sql`.
- `src/jobs/ladder/ranks.rs`: `RankLookup.discover`, planning the re-checks, the per-lookup upsert or clear, turn counters, `REFRESH_GAMES`.
- `src/archive/ranks.rs`: `plan_rechecks` beside `plan`, which takes the remaining limit. `take`, `put_back` and `record` are unchanged.
- `src/jobs/ladder/store.rs`: `latest_completed`, `discover_entry`, `clear_discovered`, the `collect_candidates` predicate, and the counters (`Counters`, `COLUMNS`, `crawl_row`, `Counter`).
- `src/jobs/ladder.rs`: `RiotEntry`'s two fields, `discovered()` (pure), and the recheck passed to `advance()`.
- `src/metrics.rs`: `LADDER_DISCOVERED_TOTAL` and its catalogue entry.
- `src/routes/admin.rs`: the three summary fields.
- `src/ui/admin.html`: the discovery line. `tests/dom/fake-admin.mjs` and `tests/dom/admin-ladder.test.mjs`.
- `tests/snapshots/it__openapi__openapi_document.snap` and `docs/api/openapi.json`.
- `src/db/tests.rs`: the 30 → 31 count and a seeded upgrade test.
- `tests/ladder.rs`, `tests/lanes.rs` and `tests/analytics.rs` (§7).
- No config, `.env.example` or `src/cli/serve.rs` change. `LadderContext.rank_lookup_recheck_s` already exists (`ladder.rs:89`).

## 7. Tests

**Unit**
1. `src/jobs/ladder/tests.rs`, `collect_candidates`:
   - a row discovered inside the window is a candidate;
   - one outside it isn't, and nor is one whose `discovered_at` was cleared;
   - a discovered player whose total equals their cursor is counted as skipped;
   - a listed row is still a candidate whatever its `discovered_at`.
2. `src/archive/ranks.rs`, `plan_rechecks`:
   - an apex row not seen by the crawl and updated inside the window is planned with `games = REFRESH_GAMES`, and `take` hands it out before a 500-game player;
   - a row seen by this crawl isn't planned, and nor is a Diamond row, a row outside the window, or another queue's or platform's row;
   - `PLAN` gets the limit less the re-checks.
3. `src/jobs/ladder/tests.rs`, `discovered()`: solo MASTER, GRANDMASTER and CHALLENGER give `Some` with wins and losses read. DIAMOND, `[]`, flex only, a malformed body, and an apex tier below a CHALLENGER floor give `None`. A missing `puuid` takes the job's.
4. `store::discover_entry`:
   - an insert sets `first_seen`/`last_seen`/`discovered_at` and an untracked `players` row;
   - an update keeps `first_seen_crawl_id`, `collected_games` and `collected_at`, and moves `last_seen`, `wins`, `losses` and `discovered_at`;
   - `discover_found` is bumped.
5. A below-Master answer clears `discovered_at`. A 404 leaves it.
6. `RankLookup` with the old shape (`{platform, queue, planned: true}`) deserialises with `discover: None` and upserts nothing.

**Integration (`tests/ladder.rs`, wiremock)**

7. `a_capped_crawl_puts_the_master_players_it_finds_on_the_ladder`:
   - the setup: the real cap (10,000 Master, as `a_crawl_whose_master_list_is_at_the_cap_reports_it`, `:1225`), `LADDER_BACKFILL_LIMIT=0` so the crawl completes at enumeration, and two archived matches whose outsiders answer MASTER, DIAMOND, `[]` and 404;
   - after the drain: 10,001 MASTER rows with `last_seen_crawl_id` = the crawl, the DIAMOND player not stored, and the crawl route's `discoverPlanned 4 · discoverDone 4 · discoverFound 1`;
   - the metric is 1.
8. `an_uncapped_crawl_records_ranks_but_stores_no_one`: today's ADR-111 fixture with a MASTER outsider. `player_ranks` has them, `ladder_entries` doesn't, and the counters are 0.
9. **The wave**, `a_discovered_player_is_collected_by_the_next_crawl_and_then_only_when_they_play`. The floor is MASTER on a short list. `apex_capped` is seeded to `["MASTER"]` right after each `start()`: `mark_capped` only adds (`store.rs:304-327`), so this stands in for 10,000 entries without 11,000 collect walks. Each crawl runs in its own `drain_jobs`, so each gets a fresh scheduler, handlers and fetcher (`tests/ladder.rs:821-838`) over the same file: a restart between crawls.
   - **Crawl N:** outsider X (two archived games) is discovered. No `match.idsByPuuid` asks for X.
   - **Crawl N+1:** X is a candidate. One request for X at `start=0&count=100`, with no `startTime`. X's new game is archived. X's cursor is stamped with the total discover wrote, and the re-check sets `last_seen_crawl_id = N+1`.
   - **Crawl N+2, X idle:** X is in `playersSkipped`, with no request for X.
   - **Crawl N+3, after X's mock gains a win:** one request carrying `startTime`.
   - **A last re-check answers DIAMOND:** `discovered_at` is NULL, and crawl N+4 doesn't walk X.
10. **No totals change**, `tests/analytics.rs`, `a_discovered_players_late_stamps_are_retracted_and_a_rebuild_agrees`:
   - the setup: the delta on, the ladder stamped by a first rebuild, and X's game of yesterday counted under `UNKNOWN`;
   - the discover run late-stamps it at MASTER, leaves `analytics_retract` rows, and queues `aggregate:delta`, which moves the game from `UNKNOWN` to `MASTER`;
   - then every analytics table is snapshotted (rows less `computed_at`), `aggregate:analytics` is run, and the tables are snapshotted again. The two snapshots are identical. The snapshot reads the table list from `STEPS` in the same way `src/archive/analytics/delta/tests.rs:181` does.

**`tests/lanes.rs`**

11. `a_rank_lookup_is_claimed_beside_a_regions_download_backlog`: 100 pending `archive:match` on asia, one running, and a `ranks:lookup` on kr. The next claim takes the lookup (THR-05 (b), §3 (f)).

**`src/db/tests.rs`:** 30 → 31, and `v0031_adds_discovery` (seeded at V0030: a ladder row reads `discovered_at` NULL, and a crawl row reads its counters as 0).

**`tests/openapi.rs`** (the snapshot) and **`tests/dom/admin-ladder.test.mjs`**:
- 12: the line shows on a capped crawl, with "of" and "Master found" numbers, and "not started" at 0 planned;
- 13: it is absent on an uncapped one.

The `admin.html` size test stays green.

**Mutation checks.** Each is applied alone, then reverted. Each must fail a test, and not by a compile error:

| # | mutation | must be caught by |
|---|---|---|
| 1 | drop `OR discovered_at >= ?6` | 9 (N+1 sends no request for X), 1 |
| 2 | `discovered_at IS NOT NULL` for `>= ?6` (no window) | 1 |
| 3 | upsert any ranked answer (drop the apex check) | 3, 7 (the DIAMOND player stored) |
| 4 | discover on whatever `apex_capped` says | 8 |
| 5 | skip the re-check plan | 9 (N+3 skips X: their total never moved on the ladder) |
| 6 | re-check rows this crawl listed | 2 |
| 7 | no clear on a below-Master answer | 5, 9 (N+4) |
| 8 | the upsert resets `collected_games`/`collected_at` | 4, 9 (N+2 walks X) |
| 9 | `discover_found` not bumped | 4, 7 |
| 10 | re-checks put in with their real games, not `REFRESH_GAMES` | 2 |

## 8. Docs in the same PR

- `docs/design/06-jobs-and-realtime.md`:
  - the `ranks:lookup` catalogue row (`:150`): the re-checks, the upsert in discover mode, and the counters;
  - the `ladder:collect` row: discovered players are candidates;
  - the priority note (`:94`): it runs beside the downloads (§2, row 5);
  - the crawl view paragraph: the discovery line.
- `docs/design/04-data-and-cache.md` §Schema (the four columns) and `docs/data/schema.md`.
- `docs/data/migrations.md` (to V0031), `docs/workflows/adding-a-migration.md` §Numbering (V0032 next, 31 in the history) and `docs/_meta/inventory.md` (31 migrations).
- `docs/design/metrics.md`: `proxy_ladder_discovered_total`.
- `docs/design/07-deployment.md`: no row (no config). The `RANK_LOOKUP_*` descriptions in `docs/deployment/configuration.md` say they also bound discover.
- `docs/api/openapi.json` regenerated, and `docs/api/endpoints*.md` if they list the summary's fields.
- `docs/DECISIONS.md` ADR-186, in ADR-185's layout: Why, What, the gaps, Measured (§1), Observed (the owner's drill), Rejected (a fourth stage, a hook in `ranks::record`, `ladder_discover_checked`/`LADDER_DISCOVER_*`), Tests. It also corrects ADR-111's "after the downloads" (§2) and amends the PROGRESS note on THR-04 (§2, row 1).
- `docs/IMPLEMENTATION.md:340,359`: the LAD-02 and THR-05 rows' Files and Tests refreshed, and a note that they were superseded by this plan. `PROGRESS.md:265,272` ticked with the PR number. §Notes for the next task: discovered players, the window, G3, and THR-01/owner Q7 as next.
- `docs/_meta/changelog.md` and each touched page's *Last verified* footer.
- Before pushing: `linkcheck.py docs`, `docs-guard.py origin/main HEAD` and `mermaidcheck.sh docs`.

## 9. Accept (owner, dev VM, production key)

Two builds, the same ladder (kr, `RANKED_SOLO_5x5`, floor MASTER), a day apart as `LADDER_CRAWL_S` runs them:
- **Baseline:** v2.9.0, so `ranks:lookup` without discover.
- **This build:** crawls N and N+1.

For each crawl, time two spans:
- the crawl's own time, `finished_at − started_at` (`/v1/admin/ladder/crawls`);
- the crawl's work, from `started_at` to the last `archive:match` of the platform's ids that its archive leg queued, and the ladder's `ranks:lookup`, both `done`.

```sql
SELECT max(finished_at) FROM jobs WHERE kind IN ('archive:match', 'ranks:lookup')
   AND finished_at >= :started_at AND (kind = 'ranks:lookup' OR json_extract(payload, '$.matchId') LIKE 'KR\_%' ESCAPE '\');
```

Then these checks:

```sql
-- after crawl N's ranks:lookup is done: more than 10,000 Master players on the ladder from crawl N
SELECT count(*) FROM ladder_entries WHERE platform = 'kr' AND queue = 'RANKED_SOLO_5x5'
   AND tier = 'MASTER' AND last_seen_crawl_id = :crawl_n;
-- crawl N+1 walked them: its candidates include discovered players
SELECT count(*) FROM ladder_entries WHERE platform = 'kr' AND queue = 'RANKED_SOLO_5x5'
   AND discovered_at IS NOT NULL AND collected_at >= :n1_started_at;
```

- **Pass:**
  - the first count is above 10,000;
  - the second is above 0 and close to crawl N's `discoverFound`;
  - crawl N's two spans are within 10 % of the baseline's;
  - a "Recompute now" after N+1's delta has run leaves every `champion_stats` total, and the `analytics_slices` games per tier, unchanged. The same before/after query as THR-03's accept does this.
- **Crawl N+1's spans aren't held to the 10 %:** it collects and downloads up to ~20,000 more players' games (Q1). Record them for ADR-186's *Observed* block anyway.

## 10. The probe (scratchpad, not in the repo)

`probe.py` (session scratchpad `lad02/`) builds the V0001–V0004 schema plus the V0030 and V0031 ladder columns in memory. It seeds the ladder of §1 and times each query five times on SQLite 3.46.1, with `EXPLAIN QUERY PLAN`. Its output is §1's table (`probe.txt`). The `players` table is empty there, so the `startedAt` join costs a probe per row and nothing more. The real join is the same seek.

## 11. Questions for the owner

The brief's answers to Q5, Q6, LAD-02 (e) and Q7 are taken as given and aren't asked again.

**Q1. What "within 10 %" measures.** Crawl N's discover can't slow crawl N: its lookups are bounded as today's are (G3), and they run on another scope beside the downloads. Crawl N+1, though, collects up to ~20,000 more players on kr and downloads their games, so its time grows with what discover found. That is the purpose of the task, not a cost of the discover step. *Recommended: hold crawl N to 10 % of the baseline, and record N+1's growth without gating on it (§9).* The alternative is a gate on N+1, which would need the discovered players' collect paced or capped. That would be a new setting, and the brief rules new settings out.

**Q2. A step of `ranks:lookup`, not a crawl stage.** A fourth stage after archive would hold the crawl open for 1.4–3.5 h of lookups and add that to `finished_at`. A stage before collect would add it to the crawl's critical path. A hook in `ranks::record` would let any profile lookup write ladder rows, and the archive layer would need to know about crawls. The cost of the step is no stage bar: the card shows a line in its details instead. *Recommended: the step.*

**Q3. Re-checks inside `RANK_LOOKUP_LIMIT`** (G3). Inside it, a run costs what it does today, and on kr about 30,000 lookups a day are left for `UNKNOWN` players. Outside it, kr runs up to ~70,000 lookups a day, about 40 % more platform-scope worker time. *Recommended: inside.* Lift it only if `UNKNOWN` grows on the champion page after the release.

**Q4. The metric's name.** The row says `ladder_discovered_total`. Its neighbours are `proxy_ladder_pages_total` through `proxy_rank_lookups_total`. *Recommended: `proxy_ladder_discovered_total`.*

**Q5. No LAD-03-style check of the cap first.** The plan relies on `apex_capped` (LAD-01) and doesn't re-probe Riot. *Recommended: no extra step.* LAD-03's `/dev` probe already confirms the cap when the owner wants to.

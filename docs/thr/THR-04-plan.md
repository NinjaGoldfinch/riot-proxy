# THR-04 plan: collect only what is new (the collect cursor)

Written 2026-10-26 against `origin/main` **c9e1b33** (6eda84e v2.8.0, plus #235 THR-07 and #236 TL-03). Every `file:line` is at c9e1b33. Every number below was produced in this session by the probe in §11. Nothing under `src/` or `tests/` was edited before approval: the prototype that §1 measures lives in a scratch worktree outside the repository.

**Preflight.**
- `git fetch origin main` gives c9e1b33. There are no open PRs. #235 and #236 changed none of `src/jobs/ladder.rs`, `src/jobs/ladder/`, `src/db/migrations/`, `src/routes/`, `src/riot/`, `src/ui/`, `src/config*`, `tests/lanes.rs` or `tests/dom/` (`git diff --stat 6eda84e c9e1b33`), so the brief's anchors in those files hold. `tests/ladder.rs` gained a line, so its anchors moved by one (§2).
- **`main`'s CI for c9e1b33 is red, but not in Rust.** In run 40226768474, `fmt`, `clippy`, `test`, `coverage`, `build-musl`, `docs`, `ui`, `docker` and `acceptance` pass. `ops` fails in `deploy/test.sh` (`FAIL the dry run leaves the pack both have alone`, an rclone dry-run check). The run for 220cdef (#235) also failed in `ops` alone, but on at least eight *other* `deploy/test.sh` cases (the healthcheck and rollback ones). Two runs failing on different cases point to a flaky ops suite, not a broken `main`. This branch touches nothing under `deploy/` (Q5).
- The latest migration is `V0029__analytics_counted.sql`. `feat/thr-01-archive-batch` (a7b9ce5, docs only) reserves V0030 only if its Q7 index is taken, and `feat/WR-04-checkpoint-off-the-writer` (0a0a94f, wip) adds none. So this task's migration is **V0030** (V0031 if THR-01 lands an index first).
- The latest ADR is ADR-184 (`docs/DECISIONS.md:2053`). WR-03 holds ADR-181. THR-01's plan names ADR-183, which #235 has since taken, so THR-01 renumbers when it merges. Nothing names ADR-185 (`grep -rn 'ADR-18[5-9]' docs PROGRESS.md` is empty), so this task takes **ADR-185**. Both numbers are re-checked before merging, as THR-03 did.
- Branch `feat/THR-04-collect-cursor`, cut from c9e1b33: this plan first, then the code on the same branch once the plan is approved, in one PR.
- There is no Riot key in this session (no `.env`, no `RGAPI-` in the environment) and no browser. §3 says what that means for the `startTime` check.

---

**The plan in one paragraph.**
- **The cursor.** Two columns on `ladder_entries`, `collected_games` and `collected_at`, stamped when a player's match-id walk finishes. The candidate query drops players whose `wins + losses` equals `collected_games`.
- **The walk.** A player with a cursor is asked only for games since `collected_at − LADDER_COLLECT_OVERLAP_S` (7,200 s), paged 100 at a time with no depth limit. A player without one is walked as today.
- **The numbers** (a mock Riot, 1,000 Challenger players, the prototype against today's code, §1):
  - a second crawl where 112 players' totals moved sends **114** `match.idsByPuuid` requests, not 1,000, and collects every new id;
  - a player with 250 new games gets all 250, where today's walk stops at 100;
  - an idle ladder sends **0**, and a ladder where everyone played sends 1,002 against today's 1,000.
- **Production.** Its saving is the share of Master+ players idle between two daily crawls. That share isn't measured yet (Q4).
- **`startTime`.** The portal doesn't say which timestamp it filters on (§3). The 7,200 s overlap covers both readings, ADR-185 records the reading as an assumption, and the owner runs the empirical check (§10).
- **Departures from the row:** the cursor is read from the row instead of carried in the payload (Q1), and a cursor older than 14 days is ignored (Q2).

---

## 1. Measurements

There is no key here, so this measures the mechanism, not production (owner answer 4: enough to approve Phase B). The method was THR-01's (§1.2 on `feat/thr-01-archive-batch`):
- **The probe.** One test module, `thr04_probe` (§11), run with `cargo test --release --test it thr04_probe -- --ignored --nocapture` in two scratch worktrees of c9e1b33: `base` (today's code) and `proto` (§4 (a)–(f) as a quick prototype, with the cursor carried in the payload as row (c) has it).
- **Riot.** wiremock serves one Challenger league of **1,000 players** and `match.idsByPuuid` from a model of each player's games. That responder filters on `startTime` against the game's *start* (the mock's choice, the safe reading of §3) and pages by `start`/`count`. Each id request takes **100 ms**. `X-App-Rate-Limit: 100000:10,6000000:600` and a matching method limit keep the limiter out of the way. Without them the limiter assumes dev-key limits and the collect yields.
- **The server.** 8 workers, `LADDER_BACKFILL_LIMIT=100`, a fresh fetcher per crawl (`match.idsByPuuid` is cached for 120 s, `src/riot/endpoints.rs:181`, so crawls close together would otherwise share answers), and a tempfile SQLite. Each crawl runs to the archive hand-off. The ids it collected are read from `crawl_match_ids`, and then the crawl is closed by hand.
- **The players.** Each starts with 40 ranked games, 3 h apart, all counted in wins and losses.

Between the crawls:
- *crawl 2*: players 0–99 play 3 games. Players 100–109 each have a game that **started 30 minutes before crawl 1's collect** and is listed only now (in progress then). Player 110 plays **250**. Player 111 plays a **remake** (listed, totals unchanged). Player 112's totals **drop** to 5 with 5 new games (a season reset).
- *crawl 3*: player 111 plays one real game.
- *crawl 4*: nobody plays.
- *crawl 5*: everyone plays one game.

| crawl | today: candidates | today: id requests | prototype: candidates | prototype: id requests (with `startTime`) | prototype: collect stage |
|---|---|---|---|---|---|
| 1, first sight | 1,000 | 1,000 | 1,000 | 1,000 (0) | 13.8 s, as today |
| 2, 112 moved | 1,000 | 1,000 | **112** | **114** (114) | **3.5 s** (today 13.8 s) |
| 3, one player moved | 1,000 | 1,000 | 1 | 1 (1) | 0.1 s |
| 4, idle | 1,000 | 1,000 | **0** | **0** | none: straight to archive |
| 5, everyone moved | 1,000 | 1,000 | 1,000 | 1,002 (1,002) | 14.0 s |

What each path collected:
- **Crawl 2, today:** all 315 ids of the +3, in-progress and reset players, but only **100 of player 110's 250**. The 100-game ceiling misses 150.
- **Crawl 2, prototype:** all 315, and **all 250** (three pages: 100, 100, 50, which is the 114 − 112). The ten in-progress games were found: the overlap reached back past their start. The remake wasn't collected, since its player was skipped.
- **Crawl 3, prototype:** one request collected player 111's real game **and the remake** from before crawl 2. Its `startTime` came from crawl 1's cursor (row (g)).
- **Crawl 5:** both paths collected all 1,000 new ids. The prototype's two extra requests were player 110 again. Its 250 games from crawl 2 all started inside crawl 2's 2-hour overlap, so they were listed a second time (251 ids, three pages).

What this shows:
- **Requests scale with players whose totals moved,** plus one per extra hundred games, and not with the ladder's size. An idle ladder costs nothing past its enumerate.
- **The overlap re-lists recent games.** A cursor walk lists everything from `collected_at − 2 h` on, so each walked player's games from the last two hours before the previous collect come back. They are dropped by the set and by `filter_unarchived`, so they cost no `match.byId`. They cost an extra page only when that window plus the new games pass 100 ids, which takes a two-hour run of more than 99 games and so happens only in this synthetic test. They do show up in `match_ids_seen` for the crawl (1,567 in crawl 5 for 1,000 new matches), which counts ids new to the crawl's set, not new to the archive. `matches_queued` stays the true count. ADR-185 says so (G11).
- **The ceiling goes,** as the row says.
- **The collect stage's time follows the request count** (8 workers at 100 ms a request). With Riot's limits, the requests saved are bulk room handed to the archive stage on the same regional host (match-v5).

**Production's idle share is the open number.** The saving on production is `1 − (moved players / players)` per crawl. With `LADDER_CRAWL_S=86400` on Master+, many of those players play every day, so the share may be well below the mock's 89 %. That is a guess until it is measured (Q4, §10). Whatever the share, the `startTime` half removes the ceiling.

## 2. What the brief says that the code doesn't

The code wins in each case. The plan follows the right-hand column.

| brief or row says | c9e1b33 | effect |
|---|---|---|
| the THR-04 row is `docs/IMPLEMENTATION.md:338`, the preamble `:305-336` | THR-07's row went in at `:338`. THR-04 is `:339`, the group heading `:306`, the preamble `:306-333` | anchors only |
| the row's Files: `src/ui/dashboard.html` | there is no such file. The crawl card is in `src/ui/admin.html` (Ladder tab, DEV-17, ADR-091), with its counters at `:1809` | `admin.html` changes. It is 159,842 bytes against a budget of 163,840 (`tests/ui.rs:202`), 4 KB of room, and one counter is about 45 bytes. `PROGRESS.md:400`'s "about 154 KB" is stale and gets refreshed |
| `tests/dashboard.rs:565` reads the counters | `the_dashboard_follows_crawl_phases` only checks that the page redraws on `crawl.phase` (`tests/dashboard.rs:565-569`). The counters text is asserted in `tests/dom/admin-ladder.test.mjs:39` (`histories queued 1,982`) on `tests/dom/fake-admin.mjs:205` | the DOM test gets the new counter. `tests/dashboard.rs` is unchanged |
| `LadderCrawlSummary` at `src/routes/admin.rs:1775-1812` | `:1764-1795` (struct), `:1797-1819` (`impl` with `new`) | anchors only |
| `a_crawl_runs_every_stage…` at `tests/ladder.rs:873`, `a_crawls_activity_shows_each_stage…` at `:1371` | `:874`, `:1372` (#236 added a line) | anchors only |
| `store::Crawl.counters` at `store.rs:43-66` | `Crawl` `:19-34`, `Counters` `:37-46`, `COLUMNS` `:48-50`, `crawl_row` `:52-73` | anchors only |
| `docs/design/07-deployment.md`'s config table gets the variable | 07 lists no `LADDER_*` row. They are lumped in "everything else … as v1" (`:151`) | `LADDER_COLLECT_OVERLAP_S` is a v2 addition, so it gets its own row in 07, as `LOOKUP_BACKFILL_LIMIT` has (`:136`) |
| `backfills_enqueued` is read by the card and the summary | it is also read by the crawl view's collect total, `(backfills_enqueued + 24) / 25` batches (`src/routes/admin.rs:2136`), and printed by `tests/p7_exit.rs:269,302` | owner answer 3 keeps that meaning right: the candidate count is still the number of players in collect jobs, so the stage total stays exact |
| `mark_walk_started` is part of the first-sight walk | it is called in `collect_batch` for **every** player before `collect_one` (`src/jobs/ladder.rs:606-609`) | owner answer 2 moves the call under the first-sight branch (§4 (e)) |

## 3. `startTime`: what the portal says (brief §2)

**The portal is not explicit, so the safe reading stands as an assumption (brief §2, method 3). The owner runs the empirical check (owner answer 5) with the commands in §10.**

The portal's page is a JavaScript app, but its data endpoint answers a plain fetch. `GET https://developer.riotgames.com/api-details/match-v5` returned 200 with 168,191 bytes of JSON on 2026-10-26 (sha256 `e98cf514af94…5e0f`, kept as `/home/dev/thr04/portal/api-details-match-v5.json`). The parameter table of `GET /lol/match/v5/matches/by-puuid/{puuid}/ids`, as it reads there:

> **startTime** optional long: Epoch timestamp in seconds. The matchlist started storing timestamps on June 16th, 2021. Any matches played before June 16th, 2021 won't be included in the results if the startTime filter is set.
> **endTime** optional long: Epoch timestamp in seconds.
> **start** optional int: Defaults to 0. Start index.
> **count** optional int: Defaults to 20. Valid values: 0 to 100. Number of match ids to return.

The page names no match timestamp for the comparison. The same page defines three that it could be: `InfoDto.gameCreation` ("when the game is created on the game server (i.e., the loading screen)"), `gameStartTimestamp` ("when match starts on the game server") and `gameEndTimestamp` ("when match ends on the game server … can occasionally be significantly longer than when the match 'ends'"). Nothing here says which one the filter uses. Two things the text does settle: the unit is seconds, and "matches *played* before" a date are excluded, which fits either reading.

What the plan does with that:
- **The overlap covers both readings** (owner answer 1: 7,200 s, not resized). A game not yet listed at the last collect is one that hadn't ended by then, or had ended but wasn't indexed yet.
  - *Under a start (or creation) reading*, it is found if it started at or after `collected_at − 7,200 s`. That holds for any game whose length plus Riot's listing lag is under two hours. The longest ranked games are about an hour.
  - *Under an end reading*, it ended after `collected_at − lag`, which is well inside the window.
- **ADR-185 records the reading as an assumption,** with the arithmetic above and the owner's empirical result in its *Observed* block once it is in.
- If the check shows an end reading, nothing changes. A smaller overlap would do, but owner answer 1 fixes 7,200 s regardless.

## 4. Design (the row's (a)–(h), against the code)

**(a) Columns.** `ladder_entries` gains `collected_games INTEGER` and `collected_at INTEGER` (unix ms), both nullable. Its key is `(key_scope, platform, queue, puuid)` (`V0004__ladder.sql:37-57`), so the cursor is per ladder: a player on solo and flex has two (G6). `ladder_crawls` gains `players_skipped INTEGER NOT NULL DEFAULT 0` (h). §6 has the DDL.

**(b) Candidates.** `store::collect_candidates` (`src/jobs/ladder/store.rs:439-459`) keeps its join, its `startedAt` exclusion and its order (`league_points DESC, puuid DESC`). It reads one more column and partitions in Rust (G1):

```sql
SELECT le.puuid, le.collected_at,
       le.collected_games IS NOT NULL AND le.collected_games = le.wins + le.losses AS unchanged
  FROM ladder_entries le LEFT JOIN players p ON …            -- as now
 WHERE …                                                       -- as now, startedAt exclusion included
 ORDER BY le.league_points DESC, le.puuid DESC
```

`unchanged` is the negation of the row's `collected_games IS NULL OR collected_games <> wins + losses`: `<>`, not `<`, so a season reset still collects. The unchanged rows are counted into `players_skipped`. The rest are the candidates, counted into `backfills_enqueued` as now (owner answer 3). Both bumps happen in `advance()`'s transaction (`src/jobs/ladder.rs:885-937`), the same one that queues the collect jobs.

**(c) What a collect job carries** (Q1). The row has `CollectJob` carry `(puuid, wins + losses, collected_at)` per player. **Recommended instead: the payload stays `{crawlId, platform, queue, puuids, offset}`, and each walk reads its cursor from its own `ladder_entries` row when it starts the player.** The stamp then writes `collected_games = wins + losses` in SQL. That is the same value the candidate query read: `ladder_entries.wins/losses` is written only by this ladder's enumerate (`store.rs:229-241`), and `ladder_crawls_live` allows one running crawl per ladder (`V0004__ladder.sql:33-34`). It also removes three problems:
- **Restarts.** A queued `ladder:collect` row from before the upgrade is the same shape, so it needs no `#[serde(default)]` branch, and it gets cursor walks too.
- **Yields.** The yield path (`ladder.rs:612-622`) slices one vector, not two kept in step.
- **THR-05** collects discovered players through this path. They will have `ladder_entries` rows once discover upserts them, so they need no payload of their own.

The cost is one indexed `Db::read` (a primary-key lookup) per player, beside the request each player costs anyway. Test 8 (§8) still runs an old-shape payload, to pin the format.

**(d) A walk with a cursor.** In `collect_one` (`ladder.rs:649-712`):
- `startTime = max(0, collected_at / 1000 − LADDER_COLLECT_OVERLAP_S)` beside `start`, `count` and `queue`.
- Paged 100 at a time from `start=0` until a short page, with no depth limit. `startTime` is already a declared query of the endpoint (`src/riot/endpoints.rs:175-183`).
- Ids seen twice are dropped by `crawl_match_ids`' `INSERT OR IGNORE` (`store::add_match_ids`, `store.rs:493-510`) and by `filter_unarchived` at the archive stage (`ladder.rs:735`).
- Paging a newest-first list while a game lands can only *repeat* an id at a page boundary, never skip one: a new id goes in at the front and pushes the rest back.

**(e) First sight.** No cursor: the walk is today's (`start=0` up to `LADDER_BACKFILL_LIMIT`, `ladder.rs:656-690`). `mark_walk_started` (`:606-609`) and the `mark_walk_complete` branch (`:694-706`) stay, for this walk only (owner answer 2). THR-05 relies on this path; its shape doesn't change.

**(f) The stamp.** After the page loop ends normally, in `collect_one`, per player, one write: `UPDATE ladder_entries SET collected_games = wins + losses, collected_at = ?first WHERE (key_scope, platform, queue, puuid) = …`. `?first` is `Clock::now()` taken just before the walk's first request is built, so it is at or before the moment Riot answered. The first sight stamps too: it is the walk that starts the cursor. Nothing is stamped on any of these:
- a 404 (`:675-678`, which returns before the stamp);
- a yield, retry or failure (they return `Err` from the page loop);
- a crawl cancelled mid-batch (the loop `break`s before the player, `:601-603`).

**(g) Remakes** don't move `wins`/`losses`, so a player whose only new game is a remake is skipped. The remake is picked up by their next real game: that walk's `startTime` reaches back to the *old* `collected_at`, from before the remake. A remake is listed by `match.idsByPuuid`, so it is collected then (test 9). ADR-185 says so.

**(h) `players_skipped`.**
- A column on `ladder_crawls`, `Counters.players_skipped`, `COLUMNS` and `crawl_row`.
- A `Counter::PlayersSkipped` arm for `bump`.
- `LadderCrawlSummary.players_skipped`, documented "Players the collect stage left out because their wins and losses hadn't changed since their last collect".
- On the card, `['players skipped', fmt(c.playersSkipped)]` right after `histories queued` (`admin.html:1809`). The label follows the column, as owner answer 3 keeps "histories queued" for `backfillsEnqueued`.
- The `crawl.phase` frames' `stats` serialise `Counters`, so they carry it too (`playersSkipped`). That is additive.

## 5. Gaps (each settled in ADR-185)

- **G1: `backfills_enqueued` and `players_skipped`.** Owner answer 3: `backfills_enqueued` is the candidates, the players this crawl walks. `players_skipped` is the players this crawl stamped on the ladder whose total is unchanged, *after* the `startedAt` exclusion, so the two plus the excluded add up to `entries_seen`. Both come from the one query of §4 (b) and are bumped in `advance()`'s transaction. That keeps `admin.rs:2136`'s stage total exact.
- **G2: `backfill_state`.** Owner answer 2: a cursor walk calls neither `mark_walk_started` nor `mark_walk_complete`. Its `start` counts a filtered list, so it is no depth, and that state serves lookups. A side effect: a cursor walk no longer stamps `startedAt`, so it no longer excludes this player from another ladder's crawl that starts later (G6).
- **G3: yields.** The invariant: *a player's cursor is stamped only by a walk that reached a short page, and its `collected_at` is the moment that walk began.* A yield re-queues `puuids[done..]` with this player first (`ladder.rs:612-622`). The re-run reads the same cursor (still unstamped) and sends the same `startTime`, from `start=0`. Its ids so far are already in the set, and `collected_at` is the re-run's own start. Games that ended between the two runs are inside the re-run's window. Tests 5 and 5b in §8.
- **G4: legs.** Legs are still `ladder:collect:<offset>`, offsets `0, 25, 50, …` over the candidates. Fewer candidates means fewer legs and nothing else. `a_crawls_activity_shows_each_stage…` (`tests/ladder.rs:1372`) runs a first crawl, so every player is a candidate and it holds unchanged. An all-skipped ladder takes `advance()`'s empty branch (`ladder.rs:902-905`) straight to archive. That leg exists today for a ladder walked since the crawl began.
- **G5: totals that move during the crawl.** `wins + losses` is what this crawl's enumerate wrote, minutes before the collect. A game finished in between is in the list but not the total, so the stored total is one low and the next crawl collects that player again. That costs one request, and never a missed game (row (f)).
- **G6: two ladders.** Solo and flex are two `ladder_entries` rows, two cursors and two walks with different `queue`. The stamp's `WHERE` names the whole key. Production crawls solo only (`LADDER_QUEUES` unset, default `RANKED_SOLO_5x5`, `src/config.rs:755`). One pre-existing quirk, kept: `startedAt` is per player, not per ladder, so a first-sight walk on one queue excludes that player from the other queue's crawl if that crawl started earlier (`store.rs:449`). With cursors only first-sight walks stamp it, so the quirk shrinks to a player's first crawl.
- **G7: `LADDER_COLLECT_OVERLAP_S`.** `int(0..=86400)`, default 7,200 (owner answer 1). It needs all of these:
  - `VARS` and `Config.ladder_collect_overlap_s` (`src/config.rs:66-70`, `:343`, `:754-761`), plus the `redacted()` list `/v1/admin/info` shows (`:398`, `:472-476`);
  - `LadderContext.collect_overlap_s`, passed from `src/cli/serve.rs:197`;
  - `.env.example` beside `LADDER_BACKFILL_LIMIT` (`:120`);
  - `src/config/tests.rs`: the parity test `env_example_documents_every_variable` (`:433`), and a bounds case;
  - `docs/deployment/configuration.md:84` and its in-force table (`:189`);
  - a row in design/07.

  `GET /v1/admin/ladder/options` returns `backfillLimit` only to preselect the crawl form (`src/routes/admin.rs:1756`). The overlap isn't a per-crawl choice, so it isn't added there.
- **G8: a stale cursor** (new; Q2). `ladder_entries` rows are never deleted (`grep 'DELETE FROM ladder_entries' src` is empty). A player who leaves the ladder and returns months later has an old `collected_at`, and an uncapped walk would page back through every ranked game since: hundreds of ids, archived and stamped at today's tier (ADR-105). Today's walk would take the newest 100. **Recommended:** a cursor older than `TIER_LATE_STAMP_DAYS` (14, already in `LadderContext.late_stamp_days`) is ignored, and the player is walked as a first sight. That needs no new setting. The 14 days fit because they are already the window in which this codebase trusts a player's current tier for past games.
- **G9: `LADDER_BACKFILL_LIMIT=0`.** It ends the crawl at enumeration (`next_stage`, `ladder.rs:872-879`) and walks nobody, cursor players included. Unchanged, and so the cursor stays where it was.
- **G10: the crawl view.** The collect stage's `done/total` is in batches of 25 over `backfills_enqueued` (`admin.rs:2136`). It stays exact (G1). An all-skipped crawl shows collect `0/0`, then archive. The view's stage state for a zero total is checked in test 1.
- **G11: `match_ids_seen` includes re-listed ids.** A cursor walk's overlap lists games from the two hours before the last collect again (§1). `match_ids_seen` counts ids new to *this crawl's* set, so it includes them, while `matches_queued` (after `filter_unarchived`) doesn't. The field's description "Distinct matches those players have played, after de-duplication" (`src/routes/admin.rs:1784`) stays true. ADR-185 notes that the gap between the two grows by up to two hours of games per walked player.

## 6. V0030

```sql
-- THR-04 (ADR-185): the collect cursor. A ladder player's wins + losses and the
-- time their last match-id walk began; a crawl skips players whose total hasn't
-- moved and asks the rest only for games since then. NULL until a crawl's walk
-- finishes (a first sight). players_skipped counts the players a crawl left out.
ALTER TABLE ladder_entries ADD COLUMN collected_games INTEGER;
ALTER TABLE ladder_entries ADD COLUMN collected_at INTEGER;     -- unix ms
ALTER TABLE ladder_crawls ADD COLUMN players_skipped INTEGER NOT NULL DEFAULT 0;
```

Three `ADD COLUMN`s, nullable or with a constant default, so no row is rewritten (`docs/workflows/adding-a-migration.md` §Cheap and expensive changes). No index: the candidate query already seeks `ladder_entries_last_seen` and filters the crawl's rows. The probe's prototype applied the same DDL in a fresh database at every test start (§11). The first crawl after the upgrade finds every cursor NULL and is a first-sight crawl, today's cost. The saving starts with the second.

## 7. Files

- `src/db/migrations/V0030__collect_cursor.sql`.
- `src/jobs/ladder/store.rs`: `collect_candidates` returns candidates and the skipped count; `read_cursor` and `stamp_cursor`; `Counters`, `COLUMNS`, `crawl_row` and `Counter::PlayersSkipped`.
- `src/jobs/ladder.rs`: `advance()` bumps both counters; `collect_batch` calls `mark_walk_started` only for a first sight; `collect_one` takes the cursor, sends `startTime`, has no depth limit with a cursor, and stamps; `LadderContext.collect_overlap_s`.
- `src/jobs/ladder/tests.rs:339`: the candidates test, now with the partition.
- `src/config.rs`, `src/config/tests.rs`, `.env.example` and `src/cli/serve.rs`.
- `src/routes/admin.rs`: `LadderCrawlSummary.players_skipped`.
- `src/ui/admin.html:1809`, `tests/dom/fake-admin.mjs:205` and `tests/dom/admin-ladder.test.mjs:39`.
- `tests/snapshots/it__openapi__openapi_document.snap` and `docs/api/openapi.json`.
- `src/db/tests.rs:898,902`: 29 → 30, plus a seeded upgrade test.
- `tests/ladder.rs` and `tests/lanes.rs`: §8.
- The docs of §9.

## 8. Tests

Integration tests are modules of `tests/it.rs` (CI-04). The crawl suites are `tests/ladder.rs` (fixture `a_crawl_runs_every_stage_and_fetches_each_match_once`, `:874`, with `ladder_of_thirty`, `:675-708`) and `tests/lanes.rs` (the yield fixture `a_collect_that_yields_resumes_with_the_players_it_has_not_done`, `:189`). The probe's `Ids` responder (§11) becomes a helper in `tests/ladder.rs`: it is a wiremock `Respond` that filters a player's list by `startTime` and pages by `start`/`count`.

**`tests/ladder.rs`**
1. `a_second_crawl_over_an_unchanged_ladder_queues_no_collect_and_goes_to_archive`: no `ladder:collect` row; `backfills_enqueued` 0, `players_skipped` 30; zero `match.idsByPuuid` requests; the crawl view's collect stage reads `0/0`.
2. `a_player_whose_total_rose_by_three_gets_one_request_with_start_time`: exactly one request for that player, carrying `startTime = collected_at/1000 − 7200` (checked against the stamped `collected_at`) and `start=0&count=100&queue=420`; the three new ids are in the set; the player's `players.backfill_state` is byte-identical before and after (owner answer 2).
3. `a_player_with_250_new_games_is_paged_three_times`: requests at `start=0`, `100` and `200` (counts of 100), answered 100, 100 and 50; all 250 ids in the set. This is "no depth limit".
4. `a_total_that_dropped_is_collected`: a season reset.
6. `a_404_player_is_not_stamped`: `collected_at` stays NULL, and the next crawl walks them as a first sight.
7. `a_first_sight_stamps_and_the_overlap_follows_the_setting`: with `LADDER_COLLECT_OVERLAP_S=0` the second crawl's `startTime` equals `collected_at/1000`.
8. `an_old_shape_collect_payload_still_runs`: a `ladder:collect` row written as today's JSON, `{crawlId, platform, queue, puuids, offset}`, runs and stamps (§4 (c)).
9. `a_remake_waits_for_the_next_real_game`: crawl 2 skips the remake-only player; crawl 3, after a real game, collects both ids.
10. `a_cursor_older_than_the_late_stamp_window_walks_as_a_first_sight` (G8, if Q2 is taken).
- `a_crawl_runs_every_stage…`: adds `players_skipped == 0` and every player stamped with `collected_games == 90`.

**`tests/lanes.rs`**
5. `a_collect_that_yields_stamps_only_the_players_it_finished`: extends `:189`. After the yield P0 and P1 are stamped and P2 and P3 aren't. After the resume all four are.
5b. `a_cursor_walk_interrupted_mid_page_is_not_stamped_and_resumes_from_its_cursor`: a 250-game cursor player, a limiter with room for two requests. It yields after page 2 unstamped. The re-run sends the same `startTime` from `start=0`, and the stamp is the re-run's start time.

**`src/db/tests.rs`**: `migrations_apply_once_across_reopens` 29 → 30; `v0030_adds_the_collect_cursor` (seeded at V0029: a ladder row survives with NULL cursor columns; a crawl row reads `players_skipped = 0`).

**`src/jobs/ladder/tests.rs`**: the candidates test (`:339`) gains an unchanged player (left out and counted), a changed one, and a dropped total.

**`src/config/tests.rs`**: default 7,200, bounds 0 and 86,400, and 86,401 rejected.

**`tests/openapi.rs`** (snapshot) and **`tests/dom/admin-ladder.test.mjs`**: `playersSkipped` in the schema and `players skipped 0` on the card (10).

**Mutation checks**, each applied alone and then reverted; each must fail a test (not a compile error):

| # | mutation | must be caught by |
|---|---|---|
| 1 | drop the `unchanged` partition (every player a candidate) | 1 |
| 2 | `<` for `<>` (`unchanged` when `collected_games >= total`) | 4 |
| 3 | stamp on 404 | 6 |
| 4 | stamp before the page loop ends (after the first page) | 5b |
| 5 | a cursor walk sends no `startTime` | 2 |
| 6 | a cursor walk capped at `LADDER_BACKFILL_LIMIT` | 3 |
| 7 | `collected_at` taken after the walk, not before | 2 (the `startTime` arithmetic) |
| 8 | a cursor walk calls `mark_walk_started` | 2 (`backfill_state` unchanged) |
| 9 | `players_skipped` not bumped | 1 |

## 9. Docs in the same PR

`docs/_meta/tools/docs-guard.py` requires a docs change for a code change, and a change to every page whose *Sources* cite a changed file.
- `docs/design/06-jobs-and-realtime.md`: the `ladder:collect` catalogue row (`:148`), "25 players per job, each since its cursor, or 100 deep at first sight", and the crawl view paragraph (`:115`) for `playersSkipped`.
- `docs/design/07-deployment.md`: a row for `LADDER_COLLECT_OVERLAP_S`. `docs/design/04-data-and-cache.md` §Schema for the three columns.
- `docs/deployment/configuration.md`, `.env.example`, `docs/operations/runbook.md` (the overlap as a lever).
- `docs/data/schema.md`, `docs/data/migrations.md` (to V0030), `docs/workflows/adding-a-migration.md` §Numbering (V0031 next; the history count 30) and `docs/_meta/inventory.md` (30 migrations).
- `docs/api/openapi.json` regenerated; `docs/api/endpoints*.md` if they list the crawl summary's fields.
- `docs/reference/riot-api-usage.md`: `startTime` is now sent by the crawl, with the portal text of §3.
- `docs/DECISIONS.md` ADR-185, in ADR-182's layout: Why, What, the `startTime` assumption, the gaps, Measured (§1), Observed (the accept drill), Rejected, Tests.
- `docs/IMPLEMENTATION.md:339`: the row's Files and Tests columns refreshed (`admin.html`, not `dashboard.html`). `PROGRESS.md:263` ticked with the PR number, and `:400`'s `admin.html` size refreshed. §Notes for the next task: THR-05's discovered players get a cursor on their first collect, and what G8 does to a returning player.
- `docs/_meta/changelog.md`, and each touched page's *Last verified* footer.
- Before pushing: `linkcheck.py docs`, `docs-guard.py origin/main HEAD` and `mermaidcheck.sh docs`.

## 10. Accept (owner, dev VM, production key)

**The `startTime` check first** (owner answer 5, since this session had no key). It needs two players with a recent ranked game each, run twice. The commands bypass the cache and the limiter (`src/cli/riot.rs:1-3`):

```bash
# 1. a recent match and its timestamps
just riot match.idsByPuuid asia "$PUUID" -q queue=420 -q count=1
just riot match.byId asia "$MATCH" | jq '.info | {gameCreation, gameStartTimestamp, gameEndTimestamp}'
# 2. startTime strictly between start and end (seconds): listed => end (or creation), absent => start
S=$(( (START_MS + END_MS) / 2000 ))
just riot match.idsByPuuid asia "$PUUID" -q queue=420 -q startTime=$S
# 3. the control: just before gameCreation, which must list it
just riot match.idsByPuuid asia "$PUUID" -q queue=420 -q startTime=$(( CREATION_MS / 1000 - 60 ))
```

Then the row's drill. One ladder on the dev VM (kr, `LADDER_TIER_FLOOR=CHALLENGER` keeps it to 300 players), two crawls an hour apart:

```bash
curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/metrics | grep 'proxy_upstream_requests_total{.*method="match.idsByPuuid"'   # before, between and after
curl -s -H "Authorization: Bearer $ADMIN_KEY" 'localhost:8080/v1/admin/ladder/crawls?platform=kr&limit=2' \
  | jq '.crawls[] | {id, entriesSeen, backfillsEnqueued, playersSkipped, matchIdsSeen, matchesQueued}'
```

The second crawl sends fewer `match.idsByPuuid` requests than it has players, and `playersSkipped + backfillsEnqueued = entriesSeen`. For 20 players it walked, every id the player's `match.idsByPuuid?startTime=<first crawl's start>` lists is in `matches`. Also useful, from production after the release:

```sql
SELECT count(*), sum(collected_games = wins + losses) FROM ladder_entries WHERE platform = 'kr' AND queue = 'RANKED_SOLO_5x5';
```

That query, run just before a crawl, gives the real idle share for ADR-185's *Observed* block.

## 11. The probe (in `/home/dev/thr04`, not in the repo)

- **Two worktrees of c9e1b33.**
  - `base/`: unchanged.
  - `proto/`: the prototype. It has V0030's DDL (§6), `collect_candidates` with the row's clause and a `Candidate {puuid, games, collectedAt}`, `CollectJob.players` under `#[serde(default)]`, `collect_one` with `startTime`, no depth limit for a cursor and the stamp after the loop, and `mark_walk_started` for first sights only. It is about 90 lines and won't be reused as is: Phase B writes the code to §4.
- **`probe/thr04_probe.rs`**: the test module both worktrees ran, with the `Apex` and `Ids` wiremock responders and the scenario of §1. Its `Ids` responder seeds the integration tests' helper (§8).
- **The two runs**: `base-run.log` and `proto-run.log`. `proto` is built in its own target directory (`target-proto/`): with a shared one, cargo reused `base`'s test binary for `proto`, and the first `proto` run measured today's code again. That run was discarded.
- **`portal/api-details-match-v5.json`**: the portal data of §3.

## 12. Questions for the owner

Owner answers 1–5 (brief §8) are settled and not asked again.

**Q1. The cursor is read from `ladder_entries` when the walk starts, not carried in the payload** (§4 (c)). This departs from row (c). It keeps today's payload, so an old queued job needs no compatibility branch and gets cursor walks too. The yield path stays one slice, and THR-05's discovered players need nothing extra. It costs one primary-key read per player. *Recommended: read it from the row.* The alternative is the row's shape: `players: [{puuid, games, collectedAt}]` beside `puuids` under `#[serde(default)]`, with an empty list meaning a first-sight walk.

**Q2. A stale cursor** (G8). A cursor older than `TIER_LATE_STAMP_DAYS` (14) is ignored, and the player is walked as a first sight (newest 100). Without this, a player back on the ladder after months away gets an uncapped walk through every ranked game since, archived at today's tier. *Recommended: yes, with no new setting.* The other choices are a separate `LADDER_COLLECT_MAX_AGE_S`, or no bound at all (the row as written).

**Q3. What `players_skipped` counts** (G1): the players this crawl saw on the ladder whose total hadn't moved, after the `startedAt` exclusion. So `entriesSeen = backfillsEnqueued + playersSkipped + excluded`. The excluded are usually 0, and are players a lookup or the other queue's crawl walked since this crawl began. *Recommended: yes.* Folding the excluded into `players_skipped` would make the two counters add up to `entriesSeen` exactly, but would call a walked player "skipped".

**Q4. A real idle share before the release.** ADR-185's *Measured* block has only the mock (§1). `ladder_entries` keeps only the latest totals, so production can't say today how many players were idle between two crawls. One snapshot before a crawl and one query after it would give the share, on any ladder:

```sql
-- before kr's next crawl (read-only on the live tables; a scratch table in the same file)
CREATE TABLE thr04_snapshot AS SELECT puuid, wins + losses AS games FROM ladder_entries
 WHERE platform = 'kr' AND queue = 'RANKED_SOLO_5x5';
-- after it
SELECT count(*), sum(le.wins + le.losses = s.games) AS idle FROM ladder_entries le JOIN thr04_snapshot s USING (puuid)
 WHERE le.platform = 'kr' AND le.queue = 'RANKED_SOLO_5x5';
DROP TABLE thr04_snapshot;
```

*Recommended: run it once if you are willing to write a scratch table on production. Otherwise the accept drill (§10) and the post-release query stand in.* The idle share decides the size of the saving, not whether the design is right.

**Q5. `main`'s red `ops` job** (Preflight). `deploy/test.sh` failed on different cases in the last two `main` runs, and every Rust and docs job passed. *Recommended:* re-run the failed job on c9e1b33 before this PR's CI is read, and treat a repeat as its own task (an ops flake). This PR touches nothing under `deploy/`.

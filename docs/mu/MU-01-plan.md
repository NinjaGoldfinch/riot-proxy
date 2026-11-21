# MU-01 plan: how much a matchup's win rate can be trusted

Written 2026-11-21 against `origin/main` **65ccdc9**. Every `file:line` below is at 65ccdc9. The spec is the MU-01 row of `docs/IMPLEMENTATION.md` §Post-release — MU; this plan adds nothing to it except the decisions in §2, which the owner has not yet seen.

**Preflight.**
- Branch `feat/MU-01-matchup-confidence`, worktree `.claude/worktrees/mu-01`, cut from 65ccdc9. No migration. No Riot call, header or status code is involved: everything is computed from `champion_matchups` rows the route already reads.
- The helper exists. `win_rate_low(wins, games)` (`src/routes/analytics.rs:660`) and its unit test (`:1992`) came with FLT-04 (ADR-171), which put it beside `rate()` so this task "can add it to the matchups with one call". `SMALL_SAMPLE = 10` and the *fewer than 10 games* wording are in the showcase since FLT-05 (`src/ui/showcase.html:596`, `:170`, `:1546`).
- The newest ADR on `main` is ADR-224; 221 is reserved for SITE-15 and 222 is taken by SITE-16 (#297, open). This task's is **ADR-225**.
- SITE-16 (#297) is open and touches the record files and the OpenAPI snapshot. Whichever merges second merges `origin/main` in and re-cites.

**The change in one paragraph.** `ChampionMatchupEntry` gains `winRateLow` (one call in `matchup()`, so the champion detail's matchups carry it too). The matchups route takes `sort=games|confidence`; `confidence` orders every row that passes the filters by `winRateLow` descending, then games descending, and only then applies `limit`. The showcase's Matchups card dims rows under 10 games and gets a *Most played · Most reliable* toggle that re-sorts the rows it already holds. About 150 lines of code, 200 of tests, and the docs.

---

## 1. Today

- `matchup()` (`src/routes/analytics.rs:745`) builds an entry with `winRate` only. `ChampionMatchupEntry` is at `:149`; both `ChampionMatchupsResponse` (`:187`) and `ChampionDetailResponse` (`:273`) use it.
- The route (`:1128-1231`) reads `analytics::facet(Facet::Matchups, …)` (`src/archive/analytics.rs:1593`), whose SQL orders `sum(games) DESC, role, opponent_id` and applies `LIMIT` in the database.
- `Request::Matchups(i64, Common)` (`:912`); `Request::parse` (`:933`) validates through `common()`; `Request::key` (`:971`) and the route's `etag(...)` (`:1197`) list every parameter. The builds route shows the pattern for a route-only parameter: `BuildsQuery { common, opponent_id }` (`:890`), validated after `common()`, last in the key (`:973`).
- `STATS_SHAPE` (`:444`) is in every champion route's tag and "changes whenever" a body's shape does. `tests/analytics.rs:237-278` rebuilds the tag with the marker's literal and checks a tag from before gets a 200.
- The showcase calls the route with `limit=200` and sorts in the page: `renderMatchups` (`src/ui/showcase.html:1581-1594`), `b.games - a.games || b.winRate - a.winRate`. Lane tabs are `data-role` buttons in the card's `.tools`; the click handler is at `:1619`.

## 2. Decisions this plan takes (for the ADR; each is the owner's to overturn)

1. **`sort=confidence` sorts, then limits.** The order is over every row that passes `role`, `tier`/`minTier`, `side`, `patch`, `remakes` and `minGames`; `limit` cuts the sorted list. Sorting only the 50 most-played rows would make `limit` change which rows count as reliable. Cost: the confidence read is unbounded (a champion has at most a few hundred `(lane, opponent)` rows). Wilson needs `sqrt`, which the bundled SQLite may not have, so the sort is in Rust.
2. **Ties.** `winRateLow` descending (the rounded value the body carries, compared with `f64::total_cmp`), then `games` descending, then the read's own order (`role`, `opponent_id`) by a stable sort.
3. **`sort=games` is the omitted `sort`.** One `Request`, one cache key, one tag part (`games`). `sort` is validated after every `common()` parameter, as `opponentId` is, so no existing 400 moves. Message: `querystring/sort must be equal to one of the allowed values` (`validate::query_one_of`).
4. **Only the matchups route takes `sort`.** The other analytics routes ignore it as they ignore any unknown parameter today (assert this for the detail rather than assume it).
5. **`STATS_SHAPE` becomes `tier-side-range-opponent-matches-low`.** The matchups and detail bodies gain a field, so a validator from before must not get a 304. This moves every champion route's tag, as each earlier change did.
6. **`computedAt` and the tag's stamp stay "the newest of the rows returned"** under either sort; `slice_stamp` already covers the rest.
7. **The toggle is page state** (`ch.sort`, `'games'` by default): not in the hash, kept across lane tabs, champions and slice changes, and never sent to the server (the spec: it "re-sorts the rows it already holds", the 200 most played).
8. **`winRateLow` is not shown as a number**, as on the builds (design/11). Not asked for.

## 3. Route (`src/routes/analytics.rs`)

- `ChampionMatchupEntry.win_rate_low: f64` with `js_number`, after `win_rate`, doc comment as `ChampionBuildEntry`'s (`:346`). `matchup()` fills it with `win_rate_low(r.wins, r.games)`.
- `enum MatchupSort { Games, Confidence }` with `as_str`; `struct MatchupsQuery { common: Common, sort: MatchupSort }`; `Request::Matchups(i64, MatchupsQuery)`.
- `Request::parse`: `common(...)?`, then `validate::query_one_of("sort", q(query, "sort"), &["games", "confidence"])?`.
- `Request::key`: the sort last, as the opponent is for builds. Update the unit test at `:1955`.
- `matchups_doc`: under `Confidence`, read with `limit: i64::MAX`, stable-sort the `FacetRow`s by decision 2, `truncate(c.limit)`; the rest of the function is unchanged. Add the sort to the `etag(...)` parts, last.
- OpenAPI text: the `sort` parameter, the 200's description (no longer always "most played first"), and one sentence in the route description saying what `winRateLow` is and that small samples sit low.
- `STATS_SHAPE` and its comment (decision 5).

## 4. Showcase (`src/ui/showcase.html`)

- CSS: `#cMatchups tr.small { opacity: .55; }` beside the builds rule at `:170`, no new colour.
- `ch.sort: 'games'` in the state object (`:1426`); not reset where `role` and `tab` are (`:1446`).
- `renderMatchups`: a pure comparator pair in the tested helpers block near `roleList` (`:586`), `games` as today and `confidence` as `b.winRateLow - a.winRateLow || b.games - a.games`; a row with `m.games < SMALL_SAMPLE` gets `class="small"` and `title="fewer than 10 games"` (from the constant); the toggle is two `data-msort` buttons after the lane tabs (a spacer between, as the Ladder card's `.tools` has), shown whenever there are rows; the note names the current order and what the toggle does (e.g. *Most reliable ranks by the low end of each win rate's 95 % interval, so a few lucky games don't lead*).
- Click handler: `if (t.dataset.msort) { ch.sort = t.dataset.msort; renderMatchups(); }`.
- The failure and empty states keep today's markup (no toggle).

## 5. Tests

| Where | What |
|---|---|
| `src/routes/analytics.rs` unit | `win_rate_low`: add 2/2 = 0.3424, 100/200 = 0.4314, 0/3 = 0 (±1e-4) to the existing table. `matchup()` fills `winRateLow`. The key: `sort` omitted and `sort=games` are one key, `confidence` another; other routes' keys don't move. |
| `tests/analytics.rs` | `winRateLow` on every matchups row and on the detail's `matchups`, equal to the Wilson bound of that row's `wins`/`games`. A seeded 40/60 row (0.5406) and 3/3 row (0.4385): default order leads with 40/60 by games either way, so also seed a row that separates the orders (e.g. 3/9, 0.1206, more games than 3/3 but below it under `confidence`). `sort=confidence&limit=1` returns the 40/60 row even when another row has more games (decision 1; e.g. 30/100, 0.2189). `sort=games` and no `sort` give byte-equal bodies and equal tags; `confidence` a different tag. `sort=bogus` is 400 `INVALID_PARAM` with the message above (add to the table at `:536`). The detail ignores `sort`. The shape-marker test (`:237-278`): the new literal, and the old one gets a 200. |
| snapshots | `cargo insta` for the OpenAPI document and `it__analytics__analytics_champion_detail.snap`; review each diff, accept nothing unexplained. |
| `tests/dom/fake-api.mjs` | `winRateLow` on `MATCHUPS` (3/5 = 0.2307, 3/9 = 0.1206, 2/2 = 0.3424) and on `DETAIL.matchups`. One row at or over 10 games is needed for "not dimmed": add it without disturbing the tests that count on today's three rows (the vs picker's sums, `:924`; the order test, `:752`), or behind a page option. |
| `tests/dom/showcase.test.mjs` | A row under 10 games has `class="small"` and the title; a row of exactly 10 does not. The toggle: *Most played* is on by default and today's order test still passes unchanged; a click on *Most reliable* re-sorts (2/2 Top, then 3/5, then 3/9 with today's rows) without a new call (`p.calls` unchanged); it survives a lane tab; the note changes. No toggle on the empty and failed cards. |
| `tests/ui.rs` | Unchanged route coverage; run it. |

Write the route tests first and see them fail on `main`'s code.

## 6. Docs and records (same PR)

- `docs/design/11-showcase.md`: the calls table row for matchups (`:53`) and §Champion view: the dimming, the toggle, that the page sorts what it holds and does not send `sort`, and that `winRateLow` isn't shown. Line 116's "MU-01 dims matchup rows with the same constant" becomes present tense.
- `docs/DECISIONS.md` **ADR-225**: §2's decisions, what was not chosen, the tests.
- `PROGRESS.md`: MU-01 `[~]` with the branch name (the PR number is the owner's to add at merge; this box cannot open PRs).
- The docs set, per `docs/_meta/doc-maintenance.md`: regenerate `docs/api/openapi.json` (with the `operationId` patch it describes); `api/endpoints-players-analytics.md` (`sort`, `winRateLow`), `api/pagination-filtering-and-versioning.md` (sorting), `api/examples.md` and `integration/contract.md` where they describe the matchups or detail body; the **example ETag in the three pages** that row names, taken from a running server as SITE-19 did (`docs/_meta/changelog.md`, 2026-11-17); a `changelog.md` line; `Last verified` footers.
- `docs-guard.py origin/main HEAD`: every page whose Sources cite `src/routes/analytics.rs`, `src/ui/showcase.html` or another changed file must change. Move its line citations to the new lines and set the footer to the base commit on `main`. Three files are named `analytics.rs` (archive, jobs, routes): resolve short citations by context. After re-citing, check that no backticked HTTP status code beside a citation was changed.

## 7. Gates

`cargo fmt --all --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo nextest run --profile ci --all-features`; `cd tests/dom && npm ci && npm test` (the headless-Chromium layout tests cannot run on this box: no fonts; CI runs them); `python3 docs/_meta/tools/linkcheck.py docs`; `docs-guard.py`; `openapi-check.py`; `! grep -rE "RGAPI-[0-9a-f-]{20,}" tests/ src/ docs/`. The musl build is CI's.

## 8. Not in this task

- MU-02's `delta` and *vs expected* column. PAIR-02's reuse of `winRateLow`.
- A `winRateLow` column or tooltip in the page; `sort` on the detail, builds or champions routes; sending `sort` from the page.
- The acceptance check ("Viego's table no longer leads its 2-game rows with the 100% ones under *Most reliable*") needs the dev VM's archive: the owner's to observe after merge.

## 9. If it grows

Past about 600 lines of diff outside generated files (`openapi.json`, snapshots), the showcase half (§4, the jsdom tests, design/11) becomes MU-01b on its own branch, as FLT-05 was split from FLT-04. Stop and say so rather than batching.

---
**Sources:** `docs/IMPLEMENTATION.md` §Post-release — MU (the spec); every `file:line` above is as of the commit below, as the plan's first line says
**Last verified:** 2026-11-21 against commit `65ccdc9`

# Ladder crawl — plan

Crawl a platform's ranked ladder end to end: enumerate every league entry for a
queue on a platform, persist the ladder, discover the players behind it, walk
their match histories through the existing archive pipeline, and process the
archived matches into aggregates. This document is the design; the work is
tracked in GitHub issues #85–#91 (see §9). Section references of the form §N.N cite
[the spec](riot-proxy-spec.md).

## 1. What exists, and what is missing

Almost the entire back half of this feature already exists:

- **Match fetching** — `match.idsByPuuid`, `match.byId`, `match.timeline` are
  wrapped, archived immutably in Postgres (`matches`, `match_participants`),
  and walked by the `backfill:player` job with resumable per-player state on
  the `players` row (`history_backfill_*` columns).
- **Throttling** — the Redis sliding-window limiter (§9) learns Riot's real
  limits from headers, and `priority: 'bulk'` work self-throttles at
  `BULK_USAGE_CEILING` and yields to interactive traffic. A crawl that goes
  through `fetcher` at bulk priority is rate-limit-safe by construction.
- **Orchestration** — BullMQ queues with the tick → fan-out pattern
  (`poll:*:tick` → per-player jobs), lifecycle-scoped dedupe ids, and the
  worker process that drains them.

What is genuinely new:

- **league-v4 ladder endpoints** — today only `league.entriesByPuuid` is
  wrapped. There is no apex-league coverage
  (`/lol/league/v4/{challenger,grandmaster,master}leagues/by-queue/{queue}`)
  and no paged tier walk (`/lol/league/v4/entries/{queue}/{tier}/{division}?page=N`).
- **Ladder storage** — rank exists only as a transient Redis snapshot for
  `rank.changed` detection. Nothing persists league entries.
- **A crawl orchestrator** — something that walks every page of every
  (tier, division), survives restarts, and knows when it is done.
- **Aggregation** — the archive is only ever read back per match or per
  player. Nothing computes across it.

## 2. Scale, and why scope knobs are load-bearing

Rough numbers (estimates, order-of-magnitude only):

| Scope (EUW1, solo queue) | Entries | Ladder requests |
|---|---|---|
| Apex tiers (Master+) | ~5–10 k | **3** (one per apex league) |
| Emerald+ | ~300–500 k | ~2–3 k pages |
| Full ladder (Iron → Diamond + apex) | ~3–4 M | ~15–20 k pages (205 entries/page) |

Against the two key classes:

- **Dev key** (20/s, 100/2 min ≈ 50 req/min sustained): a full-ladder
  enumeration is ~5–7 hours — slow but feasible. Match histories are **not**:
  3–4 M players × (1 ids call + ~10–20 new match fetches after
  cross-participant dedup) is months of budget.
- **Production key** (~500/10 s, 30 k/10 min): full ladder in minutes; full
  match walk in days, dominated by `match.byId` even with dedup (each match
  covers 10 participants).

Consequences, baked into the design rather than bolted on:

1. **Two independent tier floors.** `LADDER_TIER_FLOOR` bounds what the crawl
   *enumerates*; `LADDER_BACKFILL_TIER_FLOOR` bounds whose *matches* get
   walked. Enumerating Emerald+ while only backfilling Master+ is a sane
   dev-key configuration.
2. **Disabled by default.** `LADDER_CRAWL_S=0` means no repeatable is
   scheduled; the crawl runs only when triggered via the admin API. A
   repeatable cadence is opt-in for people with the budget for it.
3. **The limiter stays the real throttle.** No crawl-side request pacing —
   pages run at bulk priority with a long wait budget, exactly like the
   existing jobs, and the crawl simply takes as long as the budget allows.
4. **Cross-participant dedup does the heavy lifting** — `filterUnarchived()`
   already exists; at high tiers most of a player's opponents are already in
   the archive by the time their backfill runs.

## 3. Riot API surface (Phase L1)

New `METHOD_IDS` + `SPECS` + `build.*` entries in `src/riot/endpoints.ts`, all
platform-host, `scope = platform`:

| Method id | Path | Notes |
|---|---|---|
| `league.challenger` | `/lol/league/v4/challengerleagues/by-queue/{queue}` | full `LeagueListDTO` in one call |
| `league.grandmaster` | `/lol/league/v4/grandmasterleagues/by-queue/{queue}` | 〃 |
| `league.master` | `/lol/league/v4/masterleagues/by-queue/{queue}` | 〃 |
| `league.entriesByTier` | `/lol/league/v4/entries/{queue}/{tier}/{division}` | `?page=N`, 1-based, ~205 entries/page |

- TTLs: short and honest — ladders churn constantly. Reuse the `league`
  override key (`CACHE_TTL_OVERRIDES=league=120` already exists) or give the
  paged walk its own, since caching page N for a crawler that visits it once
  per crawl is near-worthless; a modest TTL still dedupes concurrent crawls.
- Queues: `RANKED_SOLO_5x5`, `RANKED_FLEX_SR`. Tiers below apex:
  `IRON…EMERALD, DIAMOND` × divisions `I–IV`; apex tiers come from the three
  league endpoints (they reject the paged route's tier params).
- **Verify early:** current `LeagueEntryDTO` carries `puuid` (Riot has been
  migrating league-v4 off `summonerId`). If a key/region still returns only
  `summonerId`, an extra resolution hop (`summoner.byId`) is needed — that
  changes the request math materially, so this is the first thing the phase
  confirms against the real API.
- Public passthrough routes (`/v1/lol/league/...`) are optional but cheap once
  the builders exist, and they make acceptance testing easier. `METHOD_IDS` is
  enum'd into route schemas and `/v1/admin/debug/riot`, so the OpenAPI/README
  drift tests will demand `npm run docs:spec` regardless.

## 4. Storage (Phase L2)

Two tables, one migration, `src/db/ladder.ts` beside `players.ts`. Everything
keyed by `key_scope` like the `players` table (§7.4 — a rotated key must not
inherit stale state).

**`ladder_crawls`** — one row per crawl run; the unit of observability and
resumability.

- `id` (pk), `key_scope`, `platform`, `queue`, `tier_floor`,
  `started_at`, `finished_at`, `status` (`running | completed | failed | cancelled`),
  counters (`pages_fetched`, `entries_seen`, `players_discovered`,
  `backfills_enqueued`).
- Per-(tier, division) page cursors live in Redis while running
  (`ladder:cursor:{KEY_SCOPE}:{crawlId}:{tier}:{division}`), since they churn
  on every page; the row holds the durable summary.

**`league_entries`** — latest-state, not history.

- PK `(key_scope, platform, queue, puuid)`; columns `tier`, `division`,
  `league_points`, `wins`, `losses`, `veteran/inactive/fresh_blood/hot_streak`
  flags, `first_seen_crawl_id`, `last_seen_crawl_id`, `updated_at`.
- "Latest-state + last_seen_crawl_id" answers the useful questions cheaply:
  current ladder = rows where `last_seen_crawl_id` is the newest completed
  crawl; dropped/decayed players = rows where it is not. A rank *timeseries*
  is deliberately out of scope — it is unbounded growth and the existing
  `rank.changed` event already covers tracked players; if wanted later it is
  its own phase with its own retention story.
- Upserts batched (the archive helpers batch at 100; same idiom).

## 5. Crawl orchestration (Phase L3)

New queue `QUEUE_NAMES.ladder`, worker concurrency ~2–4 (the limiter is the
real throttle), three job shapes in `src/jobs/processors.ts`:

- **`ladder:crawl`** `{ platform, queue, tierFloor, crawlId? }` — creates the
  `ladder_crawls` row, enqueues one `ladder:apex` per apex league and one
  `ladder:walk` per (tier, division) at or above the floor. Dedupe: one live
  crawl per `(key_scope, platform, queue)` — a second trigger while one runs
  is rejected (or returns the running crawl's id).
- **`ladder:apex`** `{ crawlId, platform, queue, league }` — one fetch,
  upsert all entries, feed discovery (§6).
- **`ladder:walk`** `{ crawlId, platform, queue, tier, division }` — loops
  pages from the Redis cursor: fetch page N at
  `{ priority: 'bulk', waitBudgetMs: <generous> }` → upsert entries → advance
  cursor → repeat until an **empty** page (short pages can occur mid-walk from
  ladder churn; empty is the only reliable terminator). One job per
  (tier, division) rather than one job per page keeps the queue small
  (28 walk jobs, not 20 000 page jobs) while the cursor makes a crash resume
  from page N, not page 1.
- Completion: the last walk/apex job to finish moves the crawl to its next
  stage (§6) rather than ending it; the last leg of the *last* stage marks the
  crawl `completed`, stamps `finished_at`, publishes the event (§7), and cleans
  up cursors and the crawl's match-id set.

Rules inherited from the codebase, restated because they bite:

- Every fetch goes through `fetcher.fetch(build.…)` at bulk priority — never
  a bare client call. Freezes (§9) then pause the crawl for free.
- `RateLimitBudgetExceeded` on a page is a *retry with backoff*, not a
  failure — BullMQ job backoff, cursor untouched.
- Every ladder job carries an explicit priority — the documented BullMQ trap
  is that an unprioritized job outranks all prioritized ones.
- `jobKey()` for scheduler ids (no colons); lifecycle-scoped `deduplication`
  ids like the poll fan-out.

**Admin surface:**

- `POST /v1/admin/ladder/crawl` `{ platform, queue, tierFloor? }` → `202` with
  the crawl id (or `409`/existing id when one is running).
- `GET /v1/admin/ladder/crawls?platform=&queue=` — recent runs with counters.
- `DELETE /v1/admin/ladder/crawls/:id` — cancel: mark row, drain jobs.
- `GET /v1/admin/ladder/options` — the platforms, queues and tier floors a
  crawl can name, and this deployment's defaults. Exists so the dashboard's
  start form is built from the same enums the trigger route enforces.

**Config** (schema + `.env.example` + `test/helpers/env.ts` `PINNED` move
together; pin the cadence to `0` in tests like the backfill limits):

- `LADDER_CRAWL_S` (default `0` = admin-trigger only)
- `LADDER_QUEUES` (default `RANKED_SOLO_5x5`)
- `LADDER_PLATFORMS` (default empty = `DEFAULT_PLATFORM`)
- `LADDER_TIER_FLOOR` (default `MASTER` — safe on a dev key)
- `LADDER_BACKFILL_TIER_FLOOR` (default `CHALLENGER`)
- `LADDER_BACKFILL_LIMIT` (match-depth per discovered player; default modest)

## 6. Player discovery → match fetching (Phase L4)

A crawl runs in three stages, recorded as `phase` on the crawl row and moved on
by whichever leg of the current stage finishes last:

1. **`enumerate`** — the apex and walk legs of §5. Every entry at or above
   `LADDER_BACKFILL_TIER_FLOOR` gets a `players` row (`upsertDiscoveredPlayers`,
   not tracked) and nothing else. No match request is made.
2. **`collect`** — `ladder:collect` jobs, 25 players each, fanned out from
   `league_entries` where `last_seen_crawl_id` is this crawl. Each walks
   `match.idsByPuuid` (queue-filtered to the ladder's queue id, capped at
   `LADDER_BACKFILL_LIMIT`) and adds every id to one Redis set per crawl.
   Still no match is fetched.
3. **`archive`** — one `ladder:archive` job drains that set in batches of 100:
   `filterUnarchived`, then `archive:match` for what is left, at
   `ARCHIVE_PRIORITY.ladder`.

**Why the stages, rather than handing each discovered player straight to
`backfill:player`.** A match has ten participants. Walking a player's history
the moment they are discovered reaches one game from ten walks spread across
the whole crawl, and a walk can only skip what `filterUnarchived` can already
see — so the nine walks that ran before that match landed each paid for it. The
dedup that §2 counts on only works if the whole ladder's ids are in hand before
the first fetch, which is exactly what the stage boundary provides: one set,
one `filterUnarchived`, one `match.byId` per match.

The cost is latency, and it is deliberate: nothing is archived until the last
page of the ladder and the last id have landed, so a crawl that is stopped
halfway has enumerated a ladder and archived nothing. `matchIdsSeen` and
`matchesQueued` on the crawl row are the pair that shows what the arrangement
bought — 40 000 ids and 4 000 fetches means nine tenths were already stored.

Decisions:

- **Never auto-track.** `tracked = true` means 60 s live polling; applying it
  to thousands of discovered players would drown the limiter. Discovery
  populates `players` and the archive; tracking stays a deliberate act.
- The matches a crawl queues sit in a band *below* every depth a lookup walk
  can reach, so a user asking about a specific player always beats the crawl.
- The id list is queue-filtered (`queue=420`), so the walk fetches the ladder's
  ranked games rather than a player's ARAM back-catalogue.
- Players walked since the crawl started are skipped — the crawl should
  converge, not thrash — and a walk capped at `LADDER_BACKFILL_LIMIT` never
  stamps `history_backfilled_at`, which would lock a player out of the deeper
  walk their first lookup does.
- A failed leg ends the crawl where it stands rather than advancing a stage.
  Spending a match budget on the part of a ladder that was seen, as though it
  were the whole thing, is the same mistake as aggregating over it.

## 7. Processing (Phase L5)

The archive's generated columns (`queue_id`, `game_end_ts`) plus
`match_participants` (`champion_id`, `win`, indexed by `puuid`) were built for
exactly this. First deliverable: **champion aggregates joined with ladder
context** — pick/win rates per (champion, tier, queue, patch), which is the
thing a ladder crawl uniquely enables (participant → `league_entries` gives
every match row a tier).

- `champion_stats` table (or materialized view) keyed
  `(key_scope, queue, tier, patch, champion_id)` with `games`, `wins`, and
  bans if the payload allows; recomputed by a `maintenance`-queue job after a
  crawl completes (event-driven) or on demand via admin route. Incremental
  aggregation on archive-write is a later optimization — recompute-from-
  archive is simple, correct, and the archive is immutable.
- Read side: `GET /v1/lol/analytics/champions?platform=&queue=&tier=&patch=`
  (naming per taste) — public, cached, schema'd like everything else.
- Patch attribution from `gameVersion` (major.minor), mapped against the
  Data Dragon mirror's version list already on disk.

**Events** (payload + `EVENT_EXAMPLES` entry or the docs test fails):

- `ladder.crawl.completed` `{ crawlId, platform, queue, entries, players, durationS }`
  on an admin topic (join `ADMIN_TOPICS` — same scope decision as `metrics`).
- `ladder.entry.changed` per-player promotions/demotions is *optional* and
  only for tracked players — firehosing millions of LP deltas through
  pub/sub is a non-goal.

## 8. Observability & acceptance (Phase L6)

- Prometheus: `proxy_ladder_pages_total{platform,queue}`,
  `proxy_ladder_entries_total`, `proxy_ladder_crawl_duration_seconds`,
  plus the existing `proxy_jobs_total{job=…}` picks up the new job names free.
- Metrics snapshot (`buildMetricsSnapshot`) grows a `ladder` block — active
  crawl, counters, last completed — surfaced on `/dashboard` next to the
  queue depths it will be inflating.
- Acceptance: a `phase7-ladder` suite gated like the others — trigger a
  Master-floor crawl on the configured platform (3 apex requests), assert
  entries landed, a backfill ran, and the completion event arrived. Cheap
  enough for a dev key.
- Docs: `npm run docs:spec`, README job-table row, TODO.md phase entry.

## 9. Phasing and issue map

Strictly ordered; each phase lands green (`typecheck`, `lint`, `test`, docs
drift) on its own.

Tracking issue: [#85](https://github.com/NinjaGoldfinch/riot-proxy/issues/85).

| Phase | Issue | Delivers |
|---|---|---|
| L1 | [#86](https://github.com/NinjaGoldfinch/riot-proxy/issues/86) | league-v4 builders + specs + tests |
| L2 | [#87](https://github.com/NinjaGoldfinch/riot-proxy/issues/87) | `league_entries`, `ladder_crawls`, migration, db helpers |
| L3 | [#88](https://github.com/NinjaGoldfinch/riot-proxy/issues/88) | ladder queue, crawl/apex/walk jobs, admin routes, config |
| L4 | [#89](https://github.com/NinjaGoldfinch/riot-proxy/issues/89) | player upsert + backfill hand-off, reason `'ladder'` |
| L5 | [#90](https://github.com/NinjaGoldfinch/riot-proxy/issues/90) | champion aggregates + analytics route + completion event |
| L6 | [#91](https://github.com/NinjaGoldfinch/riot-proxy/issues/91) | metrics, dashboard block, acceptance suite |

## 10. Non-goals (for now)

- Rank history timeseries (retention story needed first).
- Auto-tracking discovered players.
- Timeline archiving for crawled matches (`ARCHIVE_TIMELINES` stays the knob).
- Cross-platform "global" ladders — a crawl is per `(platform, queue)`.
- Live LP-change events for the whole ladder.

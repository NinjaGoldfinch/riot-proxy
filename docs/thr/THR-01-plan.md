# THR-01 plan: crawl matches fetched in batches (`archive:batch`)

Written 2026-10-22 against `origin/main` **6eda84e** (v2.8.0). Every `file:line` is at 6eda84e. Nothing under `src/` or `tests/` is edited before this plan is approved. The only new file besides this one is `scripts/thr01-baseline.py`, the sampler for §1.1.

**Preflight.**
- `git fetch origin main` gives 6eda84e. The last feature PR is #233 (THR-03).
- The latest migration is `V0029__analytics_counted.sql`. This task needs **no migration** unless Q7's index is taken (§3, G1), and then it is **V0030**.
- The latest ADR is ADR-182, and WR-03 holds ADR-181, so this task takes **ADR-183** (re-checked before merging).
- Branch `feat/thr-01-archive-batch`: this plan first, then the code on the same branch once the plan is approved, in one PR.

**Where this stands.** The THR plan has every task measure first. The number THR-01 says it moves, each region's app-bucket usage during a crawl's archive stage, can only be read on the dev VM with a real key, and it has not been recorded (`PROGRESS.md` §THR: "Baseline … not yet recorded"). §1.1 is what to run. §1.2 is what this session could measure without a key: today's per-match path against a mock Riot with fixed latency. **If §1.1 shows usage already at the bulk ceiling, THR-01 is skipped** (the row's own rule), and the code half of this plan is never built.

---

## 1. Measurements

### 1.1 Owner: the baseline and questions 1 and 2 (dev VM, production key)

One run answers step 0 of the THR plan, THR-01's "measure first" and owner questions 1 and 2:

1. Start one kr crawl to MASTER on the dev VM, as the THR plan's step 0 says. Leave `ARCHIVE_TIMELINES` at its default (`true`, ADR-098) and note it, since `PROGRESS.md` asks which setting the baseline used.
2. When the crawl's card shows the archive stage (or as soon as `downloads.ready` is non-zero on `GET /v1/admin/ladder/crawls/{id}`), run on the VM:

   ```sh
   ADMIN_KEY=… scripts/thr01-baseline.py --base http://localhost:8080 \
       --ceiling 0.95 --interval 5 --duration 1800 > thr01-baseline.txt
   ```

   It samples `GET /v1/admin/metrics` (the limiter's windows for each scope, as the Limits tab draws them) and `/metrics` every 5 s. When it ends it prints, for each match-v5 region, app-bucket usage as a share of the limit against the ceiling (median, p90, and how often it was at 90 % of the ceiling or more), the same for the `match.byId` method windows, requests per `(region, method)`, matches archived per second (`proxy_archived_matches_total`), the `match.byId` round trip per region (`proxy_upstream_latency_seconds`: the mean, and a median read off the buckets) and bulk limiter waits (`proxy_rl_wait_seconds{priority="bulk"}`: the mean, and how many ran past the 1 s yield budget). Pass `--ceiling 0.80` instead if anyone is using the app during the run (THR-06a: `BULK_IDLE_CEILING` applies only after `BULK_IDLE_AFTER_S` without an interactive request).
3. For the rest of step 0 (each stage's wall time, `proxy_aggregate_duration_seconds` per step, the database file size and match count), read the crawl's card and `/v1/admin/metrics` `disk` when the crawl ends.
4. **Q1, round trip.** The script's per-region mean is the figure to record. For a cleaner median, time ten `match.byId` calls per regional host from the dev VM (`just riot` or the `/dev` explorer) and record those medians. Add the same from production (Onidel Sydney) if you can, since that is where crawls will run. The only figure on record is 461 ms for one account call from the dev box at the P1 exit check.
5. **Q2, the key's limits.** Copy `X-App-Rate-Limit` and `X-Method-Rate-Limit` off one production `match.byId` response. The `/dev` explorer shows the headers. Or read the `limit:seconds` windows the Limits tab shows for a region scope and its `match.byId` row. The code doesn't depend on these values, because the limiter syncs both from every response (`src/riot/limiter/headers.rs`); only §1.3's sizing does.

**The decision this feeds.** If a region's app bucket sits at 90 % of the ceiling or more for most of the archive stage, the limit is already the bottleneck. THR-01 is then skipped, and `PROGRESS.md` says so. Otherwise the gap between that usage and the ceiling is what THR-01 buys.

### 1.2 This session: today's per-match path and a batch-shaped prototype, mock Riot

There is no Riot key here, so this measures the mechanism, not production. Both runs used a probe test kept out of the repo, since nothing under `tests/` changes before approval. It was a module of `tests/it.rs`, run with `cargo test --release --test it thr01_probe -- --ignored --nocapture`, and its setup becomes the row's integration test. The setup:
- wiremock serving the 79 KB replay match body (`06-match.byId.body`, its `matchId` rewritten per request) with a fixed delay, and `X-App-Rate-Limit: 100000:10,6000000:600` and a matching method limit, so the limiter never binds.
- 1,000 KR ids, `fetchTimeline: false`, a tempfile SQLite, the real fetcher with its archive.
- Release build on this container, 2026-10-22.

The two paths:
- **Per-match (today):** 1,000 crawl-shaped `archive:match` jobs (priority 20 005) run by a real `Scheduler` with 8 workers. The clock runs from the workers' start until `matches` holds 1,000 rows.
- **Batch-shaped (prototype):** 8 tasks, one per worker, each fetching its 125 ids through the same fetcher with `buffer_unordered(16)`. This is the handler's fetch loop without the job plumbing, so the claim and finish of each of the ten batches a real run would queue are left out (ten writes beside 1,000 match writes).

| mock latency | per-match: wall time | per-match: matches/s | batch-shaped: wall time | batch-shaped: matches/s | speed-up |
|---|---|---|---|---|---|
| 0 ms | 2.02 s | 494 | 1.24 s | 808 | 1.6× |
| 200 ms | 26.20 s | 38.2 | 1.89 s | 530 | 13.9× |
| 250 ms | 32.39 s | 30.9 | 2.30 s | 434 | 14.1× |

What this shows:
- At 200 and 250 ms today's rate is 95–97 % of `8 / latency` (40 and 32/s). The worker count and the round trip set it, as the THR plan's table says.
- The batch shape is bounded by `128 / latency` (640 and 512/s) and by the write path (808/s at 0 ms, one writer). The row's integration target, "under a quarter of the per-match time", passes with room to spare on the same setup.
- Against Riot the limiter, not either of these, caps a batch at about 47 requests/s a region (§1.3). So production's gain is the 1.5× there, not the 14× here.
- With timelines on, each per-match job makes two round trips in a row, which halves today's match rate again.

### 1.3 The arithmetic, with placeholders until §1.1 is in

Placeholders: a 250 ms round trip (Q1), and Riot's standard production key, 500 per 10 s app and 30,000 per 10 min (Q2).
- **The limit per region.** At 500 per 10 s the 10-minute window binds over a long crawl: 30,000 per 600 s is 50 requests/s. The bulk ceiling takes 0.95 of that with nobody on the app (`BULK_IDLE_CEILING`) and 0.80 otherwise: **about 47 or 40 requests/s per region**.
- **Today's rate.** With 8 workers and one round trip per job, `8 / 0.25 s` is **32 jobs/s across all regions together**. That is 32 matches/s with timelines off and 16 with them on (a match round trip, then a timeline round trip). A single-region crawl therefore uses at most 32/47, about 68 %, of its region's idle ceiling with timelines off, and still at most 68 % of the requests with them on (16 matches/s × 2 requests). It never reaches the ceiling unless the round trip is under about 170 ms.
- **What THR-01 needs in flight.** To hold 47 requests/s at 250 ms per request takes about 12 in flight per region. A batch at `ARCHIVE_BATCH_CONCURRENCY` 16 does that alone. With 8 workers on one region, 128 fetches queue in the limiter: §3 G5 covers what that does.
- **The gain** (at these placeholders; §1.1 replaces them). For a single-region crawl, from 32 to about 47 requests/s, a factor of about 1.5. With timelines on, from 16 to about 23 matches/s. Several regions crawled at once gain more, since today they share the same 32/s and each gets its own 47/s after the change. The row's integration test (latency-bound, generous limits) asks for 4×, which only holds where the limit is far above what 8 workers can reach. That is the test's setup, not production's.

---

## 2. Design (the row's (a)–(h), against the code)

**(a) The job.** A new kind `kinds::ARCHIVE_BATCH = "archive:batch"` in `src/jobs/mod.rs`, added to `kinds::ALL` and registered in `handlers()`. Payload, `ArchiveBatch` in `src/jobs/archive.rs`:

```json
{"crawlId": "…", "region": "asia", "matchIds": ["KR_…", …], "fetchTimeline": true, "size": 100}
```

`region` and `size` are additions to the row's payload (Q5, Q7). `ladder:archive` (`src/jobs/ladder.rs:721-781`) keeps its loop: `store::peek_match_ids(…, ARCHIVE_BATCH)`, then `filter_unarchived`, then one write transaction. The transaction now queues the unarchived ids as `archive:batch` rows instead of one `archive:match` each. It splits them by `Region::from_match_id` first, so each batch is one region. A kr crawl's ids are all asia, but a player who moved platform within a region brings other prefixes, and an id whose region can't be derived is dropped with a warning, where `archive:match` would have failed it. Priority stays `order::MATCH` (`BACKFILL + 5`). The dedupe key is `"{crawlId}:{firstMatchId}"`: the hand-off already commits jobs and the set's removal together (`ladder.rs:741-768`), so the key only guards a re-run after a crash between that commit and the job's `done`. `MatchesQueued` and `proxy_ladder_matches_queued_total` still count ids.

**(b) Concurrency.** The handler, `ArchiveContext::archive_batch`, runs `futures_util::stream::iter(ids).map(|id| self.fetch_one(id)).buffer_unordered(n)`, with `n = ARCHIVE_BATCH_CONCURRENCY` (new, default 16, `1..=100`). `futures-util` is already a dependency (`archive.rs:13`), so nothing is added.

**(c) Per-id work and the yield.** `fetch_one` is today's `archive_match` body (`archive.rs:245-289`) minus the job plumbing. It returns one of: archived (with whether it was new), not found, limited (`retry_at`), or failed (`FetchError`). The first *limited* result sets a stop flag. The stream's map checks the flag before starting each fetch, so no new fetch starts and those in flight finish. The handler then returns `JobError::Yield { retry_at: earliest, payload }`, where the payload holds the ids that weren't archived (limited, plus never started) and keeps `size`. `finish` already puts a yield's payload in place of the job's and gives the attempt back (`scheduler.rs:1102-1106`), as `collect_batch` relies on (`ladder.rs:612-623`). A limited timeline keeps the id in the remainder, as today (`archive.rs:277-280`). The re-run finds its match in the archive (`X-Cache: ARCHIVE`, no Riot call) and fetches only the timeline.

**(d) Lane.** `lanes::of(ARCHIVE_BATCH, p)` is `on_region("match.byId", Region::parse(p.region))`, and the kind is added to `LANED`. SCH-01's claim (`scheduler.rs:977-1001`) is unchanged.

**(e) `archive:match` stays.** Lookups, polls, backfills, TL-01's missing halves and the boot catch-up keep it, with its depth priority.

**(f) Dedupe.** A batch has no per-id dedupe against `archive:match`. A lookup's job for an id a batch also holds gets one Riot call either way: the fetcher answers the second from the archive, and two at once share one single-flight leg.

**(g) 404.** The id is logged at `warn` and dropped. The batch goes on.

**(h) Labels.** `src/ui/jobs.html` doesn't exist. The jobs page has been the dashboard's Jobs tab in `src/ui/admin.html` since ADM-17 (`/dev/jobs` redirects to it). `jobLabel` (`admin.html:629-642`) gets `archive:batch` → `` `${n} matches · ${p.region}` ``. `groupLabel` (`admin.html:653-667`) gets `` `${k} batches · ${sum} matches · ${p.region}` ``, and `groupJobs` groups by `payload.platform`, so a batch groups by kind alone, which is right because one crawl's batches share a region. `src/ui/dev-ui.html:842` reads `archive:match` per player (`puuid`), which a crawl's jobs never carry, so it is unchanged. `admin.html` is about 154 KB of its 160 KB budget, and this adds well under 1 KB. A DOM test in `tests/dom/admin-jobs.test.mjs` covers both labels.

**`match.archived`.** Published per id when the fetch's `x_cache != Archive`, before the timeline, through the existing `announce` (`archive.rs:423-448`), with `puuid: None` as crawl jobs have today.

**Timelines (ADR-098).** `fetchTimeline` is per batch (the crawl's `ARCHIVE_TIMELINES` at hand-off) and applies to each id. A limited timeline yields that id (c). Another timeline error is logged, as today; G2 covers the boot catch-up that this would otherwise trip.

---

## 3. Gaps found against 6eda84e (each settled in the ADR)

- **G1 — the crawl's download counts.** `GET /v1/admin/ladder/crawls/{id}` `downloads` (WR-01, ADR-179) counts `archive:match` rows at `order::MATCH` whose dedupe key starts with the platform, by state, through the partial index `jobs_crawl_matches` (`store.rs:735-746`, V0028). Batches would vanish from it, and the ETA with them. **Proposal (Q7):** keep the fields and their unit (matches), and add this crawl's batches to them: `sum(json_array_length(payload, '$.matchIds'))` by state for `kind = 'archive:batch' AND json_extract(payload, '$.crawlId') = ?`, and for `recent`, `sum($.size)` over batches finished in the window. A batch row is 100 ids, so a crawl of 1.2 M ids has 12,000 rows at most. The read is planned on `jobs_state_kind`; if a query-plan test or a timing on a 12,000-row queue shows a scan of every row, a partial index `ON jobs(state, finished_at) WHERE kind = 'archive:batch'` follows as V0030. The `archive:match` part stays, for rows queued before the upgrade and for every other crawl on the platform, as now. `docs/api/openapi.json` doesn't change, but the field descriptions do.
- **G2 — the TL-01 boot catch-up.** `queue_missing_halves` (`archive.rs:169-212`) skips a match whose *done* `archive:match` asked for its timeline, so a timeline Riot won't serve isn't asked for on every restart. A batch-archived match has no such row, so its failed timeline would be asked for at every boot. **Proposal (Q6):** a batch hands any id whose timeline failed for a reason other than the limiter to a per-id `archive:match` with `fetchTimeline: true`, deduped by id, at `order::MATCH`. That job retries with backoff and then finishes `done` (it warns and goes on, as today), and the catch-up skips it from then on. It costs one timeline request more per such match, which is rare (old games).
- **G3 — other per-id errors.** A 5xx after the fetcher's retries, a 429 Riot actually sent (not the limiter's), or a network error. Returning `Retry` for the whole batch would re-run every id (`finish` keeps the payload on a backoff), and an id Riot keeps failing would end the batch `failed` after five attempts, with 99 good ids in a failed row. **Proposal (Q6):** the batch finishes the rest, hands each failed id to its own `archive:match` (deduped by id, at `order::MATCH`), and ends `done`. Each such id then gets exactly today's backoff, final failure and retry button. A real 429 also freezes the scope in the limiter (P2-04), so the rest of the batch yields on its next fetch rather than piling on.
- **G4 — the store error.** If the archive write fails inside the fetcher, it surfaces as a fetch error, and G3 applies.
- **G5 — 128 in flight on one region.** The claim spreads workers across lanes, then lets them double up (§Claiming). With one crawl on one region, all 8 workers can hold that region's batches: 128 fetches waiting on a bucket that admits about 47/s. A fetch that would wait longer than `JOB_YIELD_BUDGET_MS` (1 s) fails with `limited_until`, so batches yield often, a write each, and come back at `retry_at`. That is SCH-01's mechanism doing its job: the limiter, not the count, stops over-running, and `bulk_blocked` keeps the lane from being claimed while it is at its ceiling. The cost is yield churn. The integration test counts yields, and Accept's `proxy_rl_wait_seconds{priority="bulk"}` under the budget is held by construction. If churn shows on the dev VM, the remedy is a smaller `ARCHIVE_BATCH_CONCURRENCY`, not code.
- **G6 — worker hold time.** A batch holds its worker until it ends or yields: about `100 / 16 × 0.25 s ≈ 1.6 s` at full speed, and up to the 1 s budget plus in-flight time when limited. Today a worker frees every round trip (0.25 s, 0.5 s with a timeline). A lookup's first `archive:match` (band 100–9 999) can therefore wait up to about 2 s for a worker when all 8 hold batches, where today it waits at most one round trip. Lookups are interactive at the limiter, and SCH-02 exists to stop lookups waiting behind a crawl. **Proposal:** accept for THR-01 and say so in the ADR. The cheap alternative, if the owner wants it now, is to cap the batch kind's share of workers. `JOB_KIND_LIMITS` can't do that, because it refuses laned kinds (OPS-06).
- **G7 — live activity.** One trace per job, capped at 200 events (design/06 §Live activity). A batch reports `activity::step("k of n archived, m in flight")` after each id. The fetcher's own per-fetch lines fill the cap at about 100 matches with timelines on, and older events are dropped and counted, as designed.
- **G8 — metrics.** `proxy_jobs_total{job="archive:batch"}` counts batches. Matches per second are read from `proxy_archived_matches_total`, which already counts every archived match. `stats::queue_of` maps any `archive:*` kind to v1's `archive` queue (`stats.rs:54-64`), so `/stats` is unchanged. No new metric is needed. One could be `proxy_archive_batch_yields_total{region}`, if the owner wants G5's churn on a graph (Q8).
- **G9 — upgrade.** Crawl-found `archive:match` rows still pending at the upgrade run as before. A crawl in its archive stage at the upgrade hands off the rest of its set as batches. A rollback to 2.8.0 leaves `archive:batch` rows that no handler claims: each fails with "no handler for job kind" (`scheduler.rs:1129`). The ADR's rollback note says to re-run the crawl, or to delete them with `DELETE /v1/admin/jobs?kind=archive:batch`.
- **G10 — THR-03's trigger.** The row for THR-03 said `archive:batch` would queue `aggregate:delta`. THR-03 shipped with the 60 s tick and no archive trigger (ADR-182, Q2 there), so nothing is added here.

## 4. Files

`src/jobs/archive.rs` (payload, `fetch_one`, `archive_batch`, handler), `src/jobs/ladder.rs` (hand-off), `src/jobs/lanes.rs`, `src/jobs/mod.rs`, `src/jobs/ladder/store.rs` (G1), `src/routes/admin.rs` (G1 descriptions), `src/cli/serve.rs` (context field), `src/config.rs` (+ env table, `config/tests.rs`), `.env.example`, `src/ui/admin.html`, `tests/dom/admin-jobs.test.mjs`, `docs/design/06-jobs-and-realtime.md` (§Claiming lanes list and yields, §Job catalogue, §Activity views), `docs/design/07-deployment.md` (config table), `docs/design/13-admin-dashboard.md` (if the Jobs tab description lists labels), `docs/DECISIONS.md` (ADR-183), the docs set (`docs/reference/glossary.md` kind count 22 → 23, `docs/operations/observability.md`, `docs/workflows/adding-a-job.md` where it names `archive:match` as the crawl's path, `docs/deployment/configuration.md` and `docs/architecture/concurrency-and-rate-limiting.md` for the new variable; `docs-guard.py` names the rest), `PROGRESS.md`, `docs/IMPLEMENTATION.md` (answers next to questions 1, 2 and 6).

## 5. Tests

The row's, plus one per gap:

- **Unit, paused tokio time** (`src/jobs/archive/tests.rs`, a scripted fetch double behind the same `fetch_one` seam, or wiremock with `start_paused` where the fetcher allows it):
  - A batch of 100 at concurrency 16 never has more than 16 fetches in flight (a gauge in the double, its maximum asserted), and it reaches 16.
  - A yield after 40 ids re-queues a payload of exactly the other 60, same `size`, and leaves `attempts` unchanged. This goes through `Scheduler::finish`, so the row is what's checked.
  - An id archived between queueing and fetching makes no upstream call (wiremock `expect(0)` on that path).
  - A 404 id is dropped, and the other 99 archive.
  - `match.archived` is published once per new match, and not for an id that was already archived.
  - A limited timeline keeps its id in the remainder, and the re-run fetches only the timeline.
  - G2/G3: a 503 id and a timeline 404 each become one `archive:match` (deduped by id), and the batch ends `done`.
- **`lanes`**: `archive:batch` → (`asia`, `match.byId`) from `region`; the `LANED` tests cover it.
- **Ladder** (`tests/ladder.rs`): the archive stage queues `ceil(n / 100)` batches per region instead of n `archive:match` rows. Mixed prefixes split by region. `MatchesQueued` still counts ids.
- **G1** (`tests/admin_routes.rs` / `tests/ladder.rs`): the crawl's `downloads` counts batch ids by state and `recent` by `size`. A query-plan check that the batch part uses an index, as WR-01's tests do for the `archive:match` part.
- **Config**: the default 16, and 0 and 101 rejected.
- **DOM**: both labels.
- **Integration** (`tests/it.rs`, `common::app_with`, wiremock with 200 ms latency and generous limits): 1,000 matches through batches archive in under a quarter of the time the per-match path takes on the same setup. Both paths are timed in the test, so the bound is relative and holds on a slow CI runner.

## 6. Accept (owner, dev VM, production key)

A single-region crawl's archive stage holds that region's app bucket at 90 % or more of the bulk ceiling, and `proxy_rl_wait_seconds{priority="bulk"}` stays under the yield budget. Measured with the same `scripts/thr01-baseline.py` run as §1.1, so before and after compare directly.

## 7. Open questions for the owner

1. **Round-trip time (IMPLEMENTATION Q1).** Please run §1.1 and record the per-region `match.byId` round trip, plus ten timed calls per host if you want a clean median. *Recommended:* measure, don't assume. The batch design doesn't depend on the value; only §1.3's sizing does.
2. **The key's limits (IMPLEMENTATION Q2).** Please paste `X-App-Rate-Limit` and `X-Method-Rate-Limit` from one production `match.byId` response. *Recommended:* nothing hard-coded. The limiter syncs from the headers, and the figures go into §1.3 only.
3. **`ARCHIVE_BATCH_CONCURRENCY` (IMPLEMENTATION Q6).** *Recommended:* 16, range 1–100. About 12 in flight saturate 47/s at 250 ms; 16 leaves room for slower hosts and timeline fetches. Revisit only if Q1's latency is well above 250 ms.
4. **The baseline decides.** If §1.1 shows the archive stage already at 90 % or more of the ceiling, THR-01 is skipped and `PROGRESS.md` records why. Agreed?
5. **Payload `region`, and batches split by region.** It is an addition to the row's `{crawlId, matchIds, fetchTimeline}`. *Recommended:* yes. The lane then never depends on which id comes first.
6. **Per-id errors and failed timelines go to `archive:match` (G2, G3).** *Recommended:* yes. The alternative is whole-batch retries, with a failed row that hides 99 good ids, plus a timeline re-asked at every boot.
7. **The crawl's `downloads` counts batch ids, with `size` for the pace (G1).** *Recommended:* yes, with V0030's partial index only if the read measures slow.
8. **Lookups behind batches (G6), and a yields counter (G8).** *Recommended:* accept the up to about 2 s wait until SCH-02, and no new metric.

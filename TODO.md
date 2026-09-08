# TODO

GitHub issues stay the source of truth for anything with a discussion attached;
this file is the map — what is done, what is in flight, and what is known to be
missing. Phases refer to §15 of [the spec](docs/riot-proxy-spec.md).

## Done

### Phases

- [x] **Phase 0** — foundations: config, logger, Docker, CI
- [x] **Phase 1** — Riot client, host routing, §5.5 error policy
- [x] **Phase 2** — header-driven token buckets, priorities, 429 handling
- [x] **Phase 3** — cache, negative cache, single-flight
- [x] **Phase 4** — public routes, consumer auth, scopes, quotas
- [x] **Phase 5** — match archive in Postgres, backfill job
- [x] **Phase 6** — worker polling, transition detection, WebSockets
- [x] **Phase 7** — Data Dragon mirror, composite profile, stale-while-revalidate
- [ ] **Phase 8** — production readiness (#9)

### Fixed

- [x] Test suite inherited the developer's `.env` (#15)
- [x] Test suite leaked consumer rows into the dev database (#16)
- [x] Limiter took accountable 429s on bursts — the Phase 2 gate (#17)
- [x] Re-running a backfill was a silent no-op until its job id aged out (#18)
- [x] Over-quota requests returned 500 instead of 429 (#20)
- [x] Acceptance setup hung instead of failing when Redis was unreachable (#22)
- [x] Riot ID bounds rejected accounts Riot itself accepts (#11, #28)
- [x] Node version pinned to what actually gets installed (#12)

### This round

- [x] `X-Cache-Age` tracks the content, not the last fetch (#29)
- [x] A fifth of every rate-limit bucket reserved for user-invoked requests (#30)
- [x] Archive queue ordered by recency, so a fresh lookup is not stuck behind
      a stranger's 2022 season (#31)
- [x] Composites: profile by Riot ID, and a paged match history in one call (#32)
- [x] `?refresh=true`, metered at one per player per 60 s (#33)
- [x] First lookup of a player archives their whole history (#34)
- [x] A minimal browser client at `/dev` (#35)
- [x] First-lookup backfill reads backfill state on the player instead of
      guessing from the shared archive, so a player whose teammate was walked
      first is no longer skipped forever (#44)
- [x] Match polls resume from `last_seen_match_id` instead of a fixed window, so
      a gap opened by downtime is repaired rather than lost; tracking a player
      now walks their history too (#46)
- [x] Poll fan-out de-duplicates on the job's lifecycle instead of on the
      clock, so a tick no longer stacks another job on every tracked player
      whose previous one has not run (#48)
- [x] The cache hit ratio is a counter pair the query windows, not a gauge
      averaging since boot — `CacheHitRatioLow` can fire again (#49)
- [x] The composite match page returns a summary per match — the requesting
      player's own line — instead of ten full match-v5 payloads; the whole
      document stays available per match from the archive
- [x] An OpenAPI document at `/openapi.json` and a browsable, callable API
      reference at `/docs`, generated from the route schemas so the contract
      cannot drift from what the server enforces (#58, stages #59–#66). The
      README's consumer guide is gone, replaced by a link — the endpoint table
      was hand-maintained and one forgotten row from being wrong.

### From the full read of `src/`

- [x] `?version=` on the static routes is a patch number and nothing else, so
      it cannot walk out of `DDRAGON_DIR` (#51); `queue` is gone from the
      mirror's file list, where it named a file Data Dragon does not serve (#52)
- [x] A cache miss no longer reads back what it just wrote: immutable payloads
      skip the unchanged-content check entirely, and the age a write computed is
      handed to the caller instead of fetched again (#53)
- [x] The composite match page asks the archive for its whole page in one
      query, rather than one per match against a pool of ten (#54)
- [x] Interactive waiters are a score-trimmed sorted set, so one leaked by a
      killed process expires on its own instead of blocking every bulk
      acquisition on that scope forever; `reset:cache` claims them by default
      (#55)
- [x] Six dead exports gone, `revoke-cache` enforces the consumer its path
      names, the debug cache route checks its scope instead of casting it, and
      two per-item loops became one query and one pipeline (#56)
- [x] Coverage for the job processors, the Data Dragon mirror and the debug
      routes, and `REQUIRE_SERVICES=1` in CI so a suite that skipped itself
      fails instead of reading as green (#57)

### Dashboard round

- [x] App-limit configs that equal the bootstrap values are persisted again —
      `storeConfig` compared headers against a local cache `windowsFor` had
      just primed with those same bootstrap values, so a development key's
      limits were never written and its scopes never appeared in
      `knownScopes()`. Scopes are also derived from method configs now, so a
      deployment from before the fix still lists what it has talked to.
- [x] Event channels are key-scoped (`evt:<scope>:<topic>`) like every other
      Redis key — the test suite and a dev server sharing one Redis were
      publishing into each other's firehose, which is where the dashboard's
      `EUW1_1` fixture events came from. The limiter suite also cleans its
      `test-region` configs up on the way out instead of stranding them.
- [x] The snapshot's limiter section names its scopes (`Oceania · oc1`), says
      which host family they are, and carries per-method window usage.
- [x] A metrics history: one compact point per `METRICS_HISTORY_INTERVAL_S`
      (default 60 s) into a capped Redis list, recorded whether or not anyone
      is watching, served at `GET /v1/admin/metrics/history`.
- [x] The dashboard grew 24 h charts (archive growth, queue backlog, cache hit
      ratio) and a players panel — who is tracked, who has been backfilled and
      how deep, resumed cursors, last touch.

### Ladder round

- [x] A crawl runs in three stages — enumerate the ladder, collect every
      discovered player's match ids, then fetch the matches — instead of
      handing each player to `backfill:player` as it found them. A match has
      ten participants, so the old order reached one game from ten walks spread
      across the whole run and every walk that ran before it landed paid for it
      again. `matchIdsSeen` / `matchesQueued` on the crawl row are what the
      arrangement bought.
- [x] A crawl can be started and cancelled from `/dashboard` — platform, queue
      and tier floor from `GET /v1/admin/ladder/options`, so the form is built
      from the same enums the trigger route enforces. The crawl card draws
      which stage it is in, and a history panel lists every run with what it
      produced and when each ladder was last crawled.
- [x] `POST /v1/admin/ladder/crawl` names the ladder it answers about. One live
      crawl is enforced per `(key_scope, platform, queue)`, so `already-running`
      was always about the ladder asked for — but a response carrying only an
      id reads, next to a panel showing some other ladder crawling, as a
      refusal about that one.

### Analytics round

Growing L5's minimal `champion_stats` into a real analytics layer (#108,
phases C1–C7). C1–C4 landed; C5–C7 are still open below.

- [x] `matches.patch` and `matches.game_duration` are indexed generated
      columns, so a recompute never opens `data` JSONB to find out which patch
      a game was played on, and the GIN index no longer drifts (#109)
- [x] `match_participants` carries the facts an aggregate consumes — role,
      team, the KDA/cs/gold/damage/vision sums, items, runes, spells — extracted
      once at archive time on both persist paths, with `match_bans` beside it
      and a `facts:reextract` job to sweep the pre-C2 archive without spending
      a request (#110)
- [x] `champion_stats` gained a role dimension, summed facts rather than
      pre-divided averages, and honest denominators: `analytics_slices` is the
      match count a pick or ban rate divides into, so `share` is no longer
      quietly standing in for a pick rate it never measured (#111)
- [x] Lane matchups and the item/rune/spell frequency tables, plus a champion
      detail composite that answers a champion page in one call (#112)
- [x] `GET /v1/players/{puuid}/champions` — a player's champion pool, grouped
      out of `match_participants` at read time on the puuid index. No table and
      no recompute: precomputing a pool would mean a table per player,
      invalidated by every game any of them plays, to save a grouped read of
      their own rows. Cached 300 s under a key-scoped `derivedKey`, and
      deliberately outside `proxy_cache_reads_total`, which is about reads that
      would otherwise have cost Riot quota (#113)
- [x] Review pass over #112: the detail composite defaulted `minGames` to `0`
      where the list route uses `AGGREGATE_MIN_GAMES`, so a champion page could
      publish a one-game 100% win rate the champion list correctly hid; `share`
      changed meaning between the two routes without either publishing the
      denominator it divided by; four recomputes returned every inserted row to
      count them, the widest of them materialising patches × champions × roles ×
      items in the worker's heap for one integer; the matchup key sorted `role`
      ahead of `champion_id`, which is the opposite of how it is read; the
      anti-fan-out CTE grouped the whole archive rather than this ladder's
      matches; mirror lanes counted one match twice; `computed_at` was selected
      and discarded, so the staleness the per-table transactions deliberately
      allow was invisible to callers; and the analytics routes sent
      `Cache-Control: public` on responses that required a bearer key

## Next

### Open

- [ ] Production readiness — compose, dashboards, production key (#9)
- [ ] Ladder crawl — enumerate a server's ranked ladder, walk every discovered
      player's matches, aggregate the archive (#85, phases L1–L6 in
      [the plan](docs/ladder-crawl-plan.md); all six landed, `LADDER_CRAWL_S=0`
      until someone opts in)
- [ ] Obtain a Riot API key and run the live acceptance checks (#10)
- [ ] Re-resolve tracked players after a key rotation (#13)
- [ ] Analytics C6–C7: the multi-table `aggregate:analytics` job with bounded
      recomputes and metrics (#114), and the polish pass — queue names, ETags,
      the `analytics.updated` event (#115)
- [ ] `docs/champion-stats-plan.md` does not exist. #108 and #113–#115 all cite
      it as the design of record, down to section numbers (§7.4, §9.4, §13),
      and the ladder and openapi rounds both have their plan doc committed —
      this one never was, in the working tree or anywhere in the history. Three
      open issues currently point at a spec nobody can read.

### Follow-ups from this round

- [ ] A crawl is marked `completed` when the archive stage has _queued_ its
      matches, not when they have been fetched — so `aggregate:champions` runs
      over an archive that is still filling. It was true before the stages too
      (the backfills were merely queued), and the aggregate is a recompute, so
      the fix is to make the crawl wait on the archive queue draining rather
      than to reorder anything.
- [ ] Nothing is archived until the whole ladder has been enumerated and every
      id collected, which is the price of fetching each match once. On a
      full-ladder dev-key crawl that is hours before the first match lands. A
      per-tier barrier — collect and archive Challenger while Master is still
      enumerating — would keep most of the dedup and shorten the wait, at the
      cost of a boundary per tier rather than one per crawl.
- [ ] Acceptance coverage for the composites: neither `by-riot-id/…/profile`
      nor `players/{puuid}/matches` is exercised against the real API yet, so
      the fan-out is only proven against stubs.
- [ ] The refresh window is held per route part (`profile`, `matches`), so one
      Update spends two windows. It is right for the UI, which calls both, and
      arbitrary for anyone calling one — worth collapsing to a single window
      per player if a second consumer ever appears.
- [ ] No metrics for either addition — lookup-triggered backfills and refresh
      claims are only visible in the logs. Both belong in §13.
- [ ] The dev UI hardcodes a subset of queue ids and skips summoner spells and
      runes. Summoner spells and runes are already in the Data Dragon mirror;
      queue ids are not, and never will be — they live at
      `https://static.developer.riotgames.com/docs/lol/queues.json`, which is
      un-versioned and outside Data Dragon (#52). Serving them needs a decision
      about where non-patch-versioned static data lives.
- [ ] `BULK_USAGE_CEILING` now defaults to 0.80 where §9.3 and Appendix A of
      the spec both say 75%. The spec is reproduced verbatim and was not
      edited; the deviation is deliberate and recorded in the README.

### Follow-ups from the analytics review

- [ ] `src/jobs/processors.ts` is ~1500 lines across five unrelated domains —
      player polling, backfill, the ladder crawl state machine, the analytics
      recomputes, and Data Dragon plus maintenance. #114 turns one aggregate
      step into five inside it. Splitting it (`jobs/ladder/` for the crawl
      machine, `jobs/analytics.ts` for the recomputes, `processors.ts` left as
      polling and dispatch) is worth doing _before_ C6, but not while #120 has
      the file open — the conflict would be total.
- [ ] The analytics routes have no read-side cache; every request runs the
      joins, and the 300 s `max-age` is the only thing between a polling
      dashboard and Postgres. #113 specifies a Redis cache for the new
      player-pool route — doing all four at once is cheaper than doing it
      twice, and the key shape is already standard.
- [ ] Route handlers are thinly covered: only eight test files use
      `app.inject`, and `routes/admin.ts` and `routes/lol.ts` are the two
      largest route files. Three of the #112 review findings were
      handler-composition bugs — a wrong default, a denominator that changed
      meaning, an unechoed filter — that the schema tests cannot see and a
      couple of inject tests would have caught.
- [ ] Mirror lane matchups are no longer stored at all (the review fix): they
      were the one row shape whose `games` counted a match twice, and their win
      rate is 50% by construction. If a mirror's _frequency_ turns out to be
      wanted, it belongs in `champion_stats`, which already counts picks
      honestly — not in a matchup row that has to mean two things at once.

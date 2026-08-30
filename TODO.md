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

## Next

### Open

- [ ] Production readiness — compose, dashboards, production key (#9)
- [ ] Obtain a Riot API key and run the live acceptance checks (#10)
- [ ] Re-resolve tracked players after a key rotation (#13)
- [ ] An OpenAPI document and a browsable, callable API reference at `/docs`
      (#58) — planned in [docs/openapi-docs-plan.md](docs/openapi-docs-plan.md),
      broken into stages #59–#66. Generated from the route schemas rather than
      hand-written, so the contract cannot drift from what the server enforces;
      served with Scalar, which unlike ReDoc's open-source build has a request
      console. **In flight — stages 0 and 1 done:**
  - [x] Stage 0 (#59) — the spike passed. Both plugins register under Fastify 5,
        the document comes out as OpenAPI 3.1, and the TypeBox enums survive.
        Two corrections to the plan: `logLevel: 'silent'` does not suppress the
        §13 request line (that line is our own `onResponse` hook, not
        Fastify's), and `/v1/ws` needs no `hide` because it never reaches the
        document at all.
  - [x] Stage 1 (#60) — the seven routes that had no schema have one, the three
        `/dev` routes are hidden, and nothing in the document is untagged. Real
        schemas for our own payloads, so `GET /v1/admin/consumers` can never
        grow a `keyHash`. `GET /v1/admin/limits/:scope` now 400s on a scope that
        is not a host, where it used to answer 200 with an empty bucket.
  - [ ] Stages 2–7 (#61–#66) — shared components, the document's own content,
        response examples, serving it, the committed snapshot, and retiring the
        README's consumer guide.

### Follow-ups from this round

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
      runes. Everything it needs is already in the Data Dragon mirror.
- [ ] `BULK_USAGE_CEILING` now defaults to 0.80 where §9.3 and Appendix A of
      the spec both say 75%. The spec is reproduced verbatim and was not
      edited; the deviation is deliberate and recorded in the README.

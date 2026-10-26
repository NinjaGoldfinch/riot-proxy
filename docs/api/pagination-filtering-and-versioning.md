# Pagination, filtering and versioning

How list routes page, which filters each route accepts and how they combine, how the analytics routes use `ETag`, and how the API is versioned. Route-by-route reference is in [endpoints.md](endpoints.md).

## Match history

Two routes page a player's match history. Both forward `start` and `count` to Riot's `match.idsByPuuid` (`/lol/match/v5/matches/by-puuid/{puuid}/ids`), which is the only place paging state lives: there is no cursor, the proxy stores nothing between pages.

| Route | `start` | `count` | Other filters | Returns |
|---|---|---|---|---|
| `GET /v1/lol/matches/ids/{region}/{puuid}` (`src/routes/lol.rs:L217-L243`) | 0 to 10000, default 0 | 1 to 100, default 20 | `queue` 0 to 5000; `type` in `ranked`, `normal`, `tourney`, `tutorial`; `startTime`, `endTime` ≥ 0 (epoch seconds, passed through) | Riot's `string[]` of match ids |
| `GET /v1/players/{puuid}/matches` (`src/routes/players.rs:L633-L656`) | 0 to 10000, default 0 | 1 to 20, default 10 | `platform` **required**; `queue`; `type`; `champion` 1 to 10000; `refresh` | `MatchPage` with `matchIds[]`, `matches[]`, `hasMore` |

Out-of-range values are `400 VALIDATION`, for example `querystring/count must be <= 100` (captured). Unknown query keys are dropped, not refused (`tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`, the `junk=x` row).

`hasMore` on the match page means the id list came back full (`count` ids), so another page probably exists; Riot's history for a PUUID is bounded by its own retention and by `start ≤ 10000`.

The `champion` filter is different: Riot's ids endpoint cannot filter by champion, so the page is built from the **archive**. The proxy still asks Riot for the newest 20 ids (`RECENT_IDS`, `src/routes/players.rs:L541`, `L667-L671`) so the archive catches up, archives any it lacks, then pages the archived matches of that champion (`L825-L837`). Because `type` is a Riot-side filter and `champion` an archive-side one, the two cannot combine: `querystring/type must not be set with champion` (`L649-L653`). A champion page carries `archive: {"complete": bool}`; `false` means the player's whole history has not been walked yet, so older games may be missing until the `backfill:player` job finishes.

Queue ids are match-v5's: `420` ranked solo/duo and `440` ranked flex (`src/riot/ladder.rs:L24`); the site's filter also offers `400`, `490`, `480`, `450` and `1700` (`docs/_meta/scratch/siblings.md` §1.5). Descriptions for every id are served by `GET /v1/static/queues`.

## Ladder paging

`GET /v1/lol/league/entries/{platform}/{queue}/{tier}/{division}?page=N` forwards `page` (1 to 100000, default 1, always sent) to `league.entriesByTier` (`src/routes/lol.rs:L180-L187`). Riot returns 205 entries per page (`ENTRIES_PER_PAGE`, `src/riot/ladder.rs:L26`) and an **empty array** past the end, which the route passes through unchanged (`src/routes/lol.rs:L154-L155`); there is no `hasMore`, so stop when a page is empty.

Apex tiers (`MASTER`, `GRANDMASTER`, `CHALLENGER`) are not paged: `GET /v1/lol/league/apex/{platform}/{tier}/{queue}` returns the whole league in one document, and Riot caps it at 10 000 entries (`RIOT_APEX_LIST_CAP`, `src/riot/ladder.rs:L14-L20`, observed on kr, euw1 and na1 on 2026-10-09). The paged route refuses an apex tier and the apex route refuses a paged tier (`params/tier must be equal to one of the allowed values`, captured).

## Admin list limits

| Route | Paging | Limit |
|---|---|---|
| `GET /v1/admin/players/{puuid}/archive/matches` | `start` 0 to 1 000 000, `count` 1 to 100 (default 25); body carries `total` | `src/routes/admin.rs:L775-L780` |
| `GET /v1/admin/jobs` | newest first, `limit` 1 to 500 (default 50); filters `state`, `kind` | `L1335-L1354` |
| `GET /v1/admin/jobs/queue` | `limit` 1 to 100 (default 15) | `L1937-L1940` |
| `GET /v1/admin/jobs/activity` | `limit` 1 to 200 (default 50) | `L1976-L1979` |
| `GET /v1/admin/jobs/{id}/activity` | `after` ≥ 0 (trace offset) | `L2022-L2025` |
| `GET /v1/admin/ladder/crawls` | `limit` 1 to 100 (default 20); filters `platform`, `queue` | `L1832-L1841` |
| `GET /v1/admin/recent` | `limit` 1 to 500 (default 100); `kind` `requests` or `errors`; rings hold 500 and 200 | `L2548-L2558`, `src/http/recent.rs:L109-L111` |
| `GET /v1/admin/timelines/backfill` | `patches` 1 to 1000 | `L2307-L2311` |

## Analytics filters

All five `/v1/lol/analytics/*` routes parse the same query (`src/routes/analytics.rs:L486-L561`, `L903-L945`); each route then accepts a subset.

| Filter | Values | Default | Notes |
|---|---|---|---|
| `platform` | platform value, case-sensitive | all platforms summed; answers `"platform": null` | ADR-065: there is no default platform |
| `queue` | `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` | first of `LADDER_QUEUES` (`RANKED_SOLO_5x5`) | analytics only exist for ladders that were crawled |
| `tier` | `IRON`…`CHALLENGER`, `UNKNOWN` | none | without it, one summed row per champion with `"tier": null` (ADR-123) |
| `minTier` | `IRON`…`CHALLENGER` | none | that tier and above; `tier` and `minTier` together is `400 VALIDATION` `querystring/minTier must NOT be sent with tier` |
| `side` | `blue`, `red` | both | |
| `patch` | `all`, or `major.minor` such as `16.20` | newest aggregated patch | 3 to 8 chars matching `^[0-9]+\.[0-9]+$`; `16` alone is `querystring/patch must NOT have fewer than 3 characters` (captured); Data Dragon's `16.20.1` is not a patch value |
| `role` | `champions`, `builds` without `opponentId`: `TOP`, `JUNGLE`, `MIDDLE`, `BOTTOM`, `UTILITY`, `""`; `matchups`, `builds` with `opponentId`: the five lanes only | all | `""` is the "no position" bucket (`src/routes/analytics.rs:L53-L56`) |
| `minGames` | integer ≥ 0 | 0 | rows with fewer games are dropped before `limit` |
| `limit` | `champions` 1 to 500 (200); detail 1 to 50 (10) per section; `matchups` 1 to 200 (50); `builds` 1 to 10 (3) | | counts champions on the list route, so `limit=200` is 200 champions (ADR-123) |
| `remakes` | `exclude`, `include` | `exclude` | `src/http/validate.rs:L248-L250` |
| `opponentId` | integer ≥ 1 | none | `builds` only: builds against that champion in a lane |
| `championId` | integer ≥ 1 | | path on three routes, query on `patches`; `params/championId must be integer` / `must be >= 1` |

`patches` takes only `platform`, `queue`, `championId`, `side` and `remakes` (`L915-L925`).

## ETags and conditional requests

Analytics responses carry a weak validator `ETag: W/"<base64url sha256 of the row parts and a shape marker>"` and `Cache-Control: private, max-age=300` (`src/routes/analytics.rs:L52`, `L577-L588`, `L607`). Send it back as `If-None-Match` (a list or `*` are accepted) and an unchanged document is a `304 Not Modified` with the same `ETag`, `Cache-Control` and `X-Cache` headers and no body (`L591-L603`). Captured:

```text
HTTP/1.1 304 Not Modified
cache-control: private, max-age=300
etag: W/"pG-2XjBV9zuyNBcF5V3Tf_TERlw-OdnaWXVDeIr2mTM"
x-cache: HIT
```

The validator changes when the `aggregate:analytics` job rewrites the slice (its `computedAt`); on every analytics route but `/patches`, when a rebuild or an `aggregate:delta` run adds a match anywhere in the slice, because one part is the newest `computed_at` of the slice's `analytics_match_totals` rows (THR-03, ADR-182; `src/routes/analytics.rs:L749-L765`), so every tag changed once at that deploy; and when the document's shape changes (ADR-123 added the marker so a per-tier document could not revalidate as a summed one). `private` because the body is per-consumer scoped by key; `max-age=300` matches the in-memory response cache's intent. ninjagoldfinch.lol sends no conditional headers at all (`docs/_meta/scratch/siblings.md` §1.3), so in production the 304 path is unused; ADR-166 records that the ETag saved nothing.

`/dashboard` uses a strong `ETag` and `Cache-Control: no-cache` the same way (`src/routes/ui.rs:L113-L131`). No other route emits a validator; passthrough bodies are described by `X-Cache*` instead (see [endpoints.md](endpoints.md#shared-passthrough-behaviour-v1riot-v1lol)).

## API versioning

- Every API route is under `/v1`. The binary is at version 2.7.0, but the HTTP contract is still v1's: the Rust rewrite kept v1's paths, headers, error codes and metric names so existing consumers, dashboards and alerts kept working (`README.md`, "v2 is a Rust rewrite of v1"). There is no `/v2` prefix and no version header to negotiate.
- The owner retired v1 on 2026-10-04 and since then approves breaking changes case by case; each is recorded as an ADR in `docs/DECISIONS.md` and the OpenAPI document is regenerated. Examples: ADR-065 removed the default platform (`?platform=` became required on the match page and the body `platform` on crawl and recompute); ADR-066 added the region-less account routes that pick a cluster; ADR-123 made `tier` nullable and changed the tierless `champions` document; ADR-143 routes `ME1_` match ids to `europe`.
- `operationId`s are stable handler names (`profile`, `match_page`, `champions`, …). The generator emits `champions` twice (the analytics list and the player champion pool); the committed copy in `docs/api/openapi.json` renames the player one to `player_champions`.
- ninjagoldfinch.lol generates its client types from a pinned copy of the OpenAPI document and bumps that copy when it adopts a change (its handoff notes record spec versions such as 2.0.0-rc.3 and 2.7.0; `docs/_meta/scratch/siblings.md` §5-6).

### Detecting the server version

| Method | Needs | Gives |
|---|---|---|
| `GET /openapi.json` and read `info.version` | `DOCS_UI` on (default); no key | `"2.7.0"` (captured: OpenAPI `3.1.0`, version `2.7.0`) |
| `GET /v1/admin/info` | admin key | `version`, `gitSha` (12 chars), `rustc`, `builtAt`, `startedAt`, `features{…}` and every config value in effect (secrets redacted) |
| `GET /dev/config.json` | `DEV_UI` on (never in production) | `version`, `env`, `authDisabled`, platform and region lists |
| `riot-proxy --version` | shell on the host | `riot-proxy 2.7.0` |

`/healthz` and `/readyz` carry no version. There is no `Server` header.

---
**Sources:** `src/routes/lol.rs:L154-L155,L180-L187,L217-L243`, `src/routes/players.rs:L541,L633-L671,L825-L837`, `src/routes/admin.rs:L775-L780,L1335-L1354,L1836-L1845,L1941-L1983,L2026-L2029,L2311-L2315,L2552-L2562`, `src/routes/analytics.rs:L52-L56,L486-L561,L577-L607,L903-L945`, `src/routes/ui.rs:L113-L131`, `src/http/validate.rs:L211-L250`, `src/http/recent.rs:L109-L111`, `src/riot/ladder.rs:L14-L26`, `tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`, `docs/DECISIONS.md` ADR-065, ADR-066, ADR-123, ADR-143, ADR-166, `docs/api/openapi.json`, `README.md`, `docs/_meta/scratch/siblings.md` §1.3-1.5, §5-6, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-26 against commit `c9e1b33`

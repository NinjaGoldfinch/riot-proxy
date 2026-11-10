# Endpoints: `/v1/riot`, `/v1/lol`, `/v1/static`, `/ddragon`

The passthrough routes (Riot's bytes, cached and rate-limited) and the Data Dragon routes (no Riot key involved). Shared passthrough behaviour, the platform table and the account-cluster rule are in [endpoints.md](endpoints.md#conventions); errors in [errors.md](errors.md).

All `/v1/riot/*`, `/v1/lol/*` and `/v1/static/*` routes need a **read** key and count against the consumer quota. Every documented passthrough route answers 200, 400, 401, 403, 404, 429, 502 or 503 (`src/routes/passthrough.rs:L325-L351`).

Common parameter rules (`src/http/validate.rs`):

| Param | Rule | Error message on failure |
|---|---|---|
| `puuid` | 60 to 128 chars, `^[A-Za-z0-9_-]+$` (`L133-L147`) | `params/puuid must NOT have fewer than 60 characters` (captured) |
| `gameName` | 1 to 16 code points (`L117-L119`) | `params/gameName must …` |
| `tagLine` | 1 to 5 code points (`L125-L127`) | |
| `region` | `americas`, `europe`, `asia`, `sea`, case-sensitive (`L67-L69`) | `400 BAD_REGION` `params/region must be equal to one of the allowed values` |
| `platform` | one of the 17 platform values, case-sensitive (`L71-L78`) | `400 BAD_REGION` `params/platform must be equal to one of the allowed values` |
| `matchId` | 6 to 40 chars, `^[A-Za-z0-9]+_[0-9]+$` (`L149-L166`) | `400 VALIDATION` |

## `/v1/riot/*`: account-v1

Riot endpoint: `account.byRiotId` `/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}`, `account.byPuuid` `/riot/account/v1/accounts/by-puuid/{puuid}`, `account.regionByPuuid` `/riot/account/v1/region/by-game/lol/by-puuid/{puuid}` (`src/riot/endpoints.rs:L126-L147`, `L287-L304`). Soft TTL 86 400 s, hard 345 600 s, negative TTL 300 s, persisted to L2.

| Method | Path | Params | Riot call | Handler |
|---|---|---|---|---|
| GET | `/v1/riot/accounts/by-riot-id/{gameName}/{tagLine}` | gameName, tagLine | `account.byRiotId` on whichever cluster has room (`asia`, `americas`, `europe`) | `src/routes/riot.rs:L172` |
| GET | `/v1/riot/accounts/by-puuid/{puuid}` | puuid | `account.byPuuid`, cluster picked | `src/routes/riot.rs:L201` |
| GET | `/v1/riot/accounts/region/by-puuid/{puuid}` | puuid | `account.regionByPuuid` with game `lol`; body's `region` is a **platform** value such as `euw1` | `src/routes/riot.rs:L230` |
| GET | `/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}` | region, gameName, tagLine | `account.byRiotId` pinned to `region` (`sea` goes to `asia`) | `src/routes/riot.rs:L262` |
| GET | `/v1/riot/accounts/by-puuid/{region}/{puuid}` | region, puuid | `account.byPuuid` pinned | `src/routes/riot.rs:L294` |

The pinned and picked variants share one cache entry (`src/routes/riot.rs:L155-L158`). Response is Riot's `AccountDto`:

```json
{"puuid":"AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEf","gameName":"Hide on bush","tagLine":"KR1"}
```

A Riot ID that does not exist is `404 NOT_FOUND` `Resource not found upstream`, and the 404 is cached for 300 s, so the next call is `404` with `X-Cache: HIT-NEG` and message `Resource not found upstream (negative-cached)` (`src/fetcher.rs:L466-L472`).

> **TODO (unverified):** the 200 body of `/v1/riot/accounts/region/by-puuid/{puuid}` is not typed in the OpenAPI document (it is not in `RIOT_BODIES`, `src/routes/docs.rs:L312-L345`); the proxy forwards Riot's `AccountRegionDTO` bytes unchanged.

## `/v1/lol/*`: League passthrough

| Method | Path | Params | Riot call (host) | Cache | Handler |
|---|---|---|---|---|---|
| GET | `/v1/lol/summoners/by-puuid/{platform}/{puuid}` | platform, puuid | `summoner.byPuuid` `/lol/summoner/v4/summoners/by-puuid/{puuid}` (platform) | soft 3 600 s, L2, neg 30 s | `src/routes/lol.rs:L105` |
| GET | `/v1/lol/league/entries/by-puuid/{platform}/{puuid}` | platform, puuid | `league.entriesByPuuid` `/lol/league/v4/entries/by-puuid/{puuid}` | soft 300 s, neg 30 s, L2 (AVL-03) | `src/routes/lol.rs:L121` |
| GET | `/v1/lol/league/apex/{platform}/{tier}/{queue}` | `tier` in `MASTER`, `GRANDMASTER`, `CHALLENGER`; `queue` in `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` | `league.master` / `grandmaster` / `challenger` `/lol/league/v4/<tier>leagues/by-queue/{queue}` | soft 120 s, L2, no neg | `src/routes/lol.rs:L139` |
| GET | `/v1/lol/league/entries/{platform}/{queue}/{tier}/{division}` | `tier` in `IRON`…`DIAMOND`; `division` in `I`…`IV`; `?page` 1 to 100000, default 1, always sent | `league.entriesByTier` `/lol/league/v4/entries/{queue}/{tier}/{division}?page=` | soft 120 s, L2 | `src/routes/lol.rs:L171` |
| GET | `/v1/lol/matches/ids/{region}/{puuid}` | `?start` 0 to 10000 (default 0); `?count` 1 to 100 (default 20); `?queue` 0 to 5000; `?type` in `ranked`, `normal`, `tourney`, `tutorial`; `?startTime`, `?endTime` ≥ 0 | `match.idsByPuuid` `/lol/match/v5/matches/by-puuid/{puuid}/ids` (region); `start` and `count` always sent, the others when given; unknown query keys are dropped | soft 120 s, neg 30 s | `src/routes/lol.rs:L214` |
| GET | `/v1/lol/matches/{region}/{matchId}` | region, matchId | `match.byId` `/lol/match/v5/matches/{matchId}` | immutable: `MISS` once, then `ARCHIVE` | `src/routes/lol.rs:L283` |
| GET | `/v1/lol/matches/{region}/{matchId}/timeline` | region, matchId | `match.timeline` `…/{matchId}/timeline` | immutable, archived | `src/routes/lol.rs:L300` |
| GET | `/v1/lol/spectator/active/{platform}/{puuid}` | platform, puuid | `spectator.activeGame` `/lol/spectator/v5/active-games/by-summoner/{puuid}` | soft 30 s; a 404 (not in game) is cached 30 s as `HIT-NEG` | `src/routes/lol.rs:L317` |
| GET | `/v1/lol/mastery/by-puuid/{platform}/{puuid}` | `?top` 1 to 200 | with `top`: `mastery.topByPuuid` `…/champion-masteries/by-puuid/{puuid}/top?count=N`; else `mastery.byPuuid` | soft 3 600 s, L2 | `src/routes/lol.rs:L334` |
| GET | `/v1/lol/rotations/{platform}` | platform | `platform.championRotations` `/lol/platform/v3/champion-rotations` | soft 21 600 s | `src/routes/lol.rs:L393` |
| GET | `/v1/lol/status/{platform}` | platform | `status.platformData` `/lol/status/v4/platform-data` | soft 300 s; kept warm by the status poller, so usually `HIT` (SITE-13) | `src/routes/lol.rs:L408` |
| GET | `/v1/lol/status` | none | **none**: a typed summary of every polled platform from `platform_status` (SITE-14, [below](#every-platforms-status-get-v1lolstatus)) | always `HIT` | `src/routes/lol.rs:L443` |

Hard TTL is soft × 4 while `STALE_WHILE_REVALIDATE=true` (default); the full table with line references is in [../reference/riot-api-usage.md](../reference/riot-api-usage.md). Exact upstream URLs per route are asserted in `tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`.

Match routes derive the region from the id's prefix (`EUW1_` is `europe`; `ME1_` is the `me1` platform's, also `europe`, ADR-143 and ADR-216). A declared region that disagrees is refused before any Riot call (`src/routes/lol.rs:L61-L72`):

```text
GET /v1/lol/matches/americas/EUW1_7000000001
400 {"error":{"code":"BAD_REGION","message":"Match EUW1_7000000001 belongs to region 'europe', not 'americas'", …}}
```

Page past the end of a ladder division gives an empty array, not an error (`src/routes/lol.rs:L162-L163`). Riot caps each apex league at 10 000 entries (`src/riot/ladder.rs:L20`).

### Every platform's status: `GET /v1/lol/status`

The one `/v1/lol/*` route that never calls Riot and is not a passthrough (SITE-14, ADR-220). The status poller reads each platform in `STATUS_POLL_PLATFORMS` every `STATUS_POLL_INTERVAL_S` (300 s) into `platform_status` ([../operations/observability.md](../operations/observability.md) has its metric); this route summarises those rows (`src/status/poll/summary.rs:L215-L226`). Same read key and scope as the per-platform route, and one call is one request against the consumer quota, however many platforms it lists. An available entry is at most one poll interval old.

```text
GET /v1/lol/status
HTTP/1.1 200 OK
x-cache: HIT
x-cache-fetched-age: 41

{"platforms":[
 {"platform":"euw1","available":true,"error":null,"incidents":2,"maintenances":1,"worstSeverity":"critical","maintenanceInProgress":true,"fetchedAgeSeconds":41},
 {"platform":"tw2","available":false,"error":"NOT_FOUND: Resource not found upstream","incidents":null,"maintenances":null,"worstSeverity":null,"maintenanceInProgress":null,"fetchedAgeSeconds":null}
]}
```

| Field | Meaning |
|---|---|
| `platforms` | one entry per platform in `STATUS_POLL_PLATFORMS`, in the platform order of [endpoints.md](endpoints.md#regions-and-platforms) (`na1` … `vn2`) |
| `available` | the poller has stored a 200 and no read has failed since; otherwise `false`, the counts are `null` (not read from the older body), and the call is still a 200 |
| `error` | why not available: the last failure's error (`CODE: message`) or `not read yet`; `null` when available |
| `incidents` | entries in Riot's `incidents[]` |
| `maintenances` | entries in `maintenances[]` whose `maintenance_status` is not `complete` |
| `worstSeverity` | the worst incident `incident_severity`, `critical` > `warning` > `info`, or `null`; any other value is logged once and not counted |
| `maintenanceInProgress` | a maintenance has `maintenance_status: "in_progress"` |
| `fetchedAgeSeconds` | seconds since the last 200, available or not; `null` before any |

`X-Cache` is always `HIT`; `X-Cache-Fetched-Age` is the stalest **available** entry's age and is left out when none is available; there is no `X-Cache-Age`. With the poller off (`STATUS_POLL_PLATFORMS=none`) the route answers `503` with code `NOT_FOUND` and no `Retry-After` (`src/routes/lol.rs:L469-L475`). Riot's field names are the portal's `PlatformDataDto` and `StatusDto` (ADR-217); a body without them counts as listing nothing.

Example, a match (shortened; Riot's `MatchDto`, served from the archive after the first fetch):

```text
GET /v1/lol/matches/sea/OC1_700000001
HTTP/1.1 200 OK
content-type: application/json; charset=utf-8
x-cache: ARCHIVE
x-cache-age: 0

{"metadata":{"matchId":"OC1_700000001","participants":["AbC…", …]},"info":{"gameVersion":"16.19.821.7343","queueId":420,"gameDuration":1682,"participants":[…],"teams":[…]}}
```

## `/v1/static/*`: Data Dragon JSON

Served from the mirror on disk; never touches Riot's API or the limiter (`src/routes/statics.rs:L4`). `X-Cache-Age` is always `0` (`src/routes/statics.rs:L161-L167`).

| Method | Path | Params | Response | Handler |
|---|---|---|---|---|
| GET | `/v1/static/versions` | none | `{"current":"16.20.1","versions":["16.20.1","16.19.1",…]}`; `X-Cache: HIT` from the mirror, `MISS` when fetched live from the CDN before the first sync; CDN failure is `500 INTERNAL` | `src/routes/statics.rs:L177` |
| GET | `/v1/static/queues` | none | Riot's `queues.json` (`[{queueId,map,description,notes}]`); not synced yet is `404` `The queue table has not been synced yet. Run the ddragon:sync job.` | `src/routes/statics.rs:L210` |
| GET | `/v1/static/{file}` | `file` in `champion`, `item`, `runesReforged`, `summoner`, `profileicon`, `map` or the aliases `champions`, `items`, `runes`, `summoner-spells`, `profile-icons`, `maps`; `?version` up to 20 chars `^[0-9]+(\.[0-9]+)*$` | the Data Dragon file untouched; unknown name is `400 VALIDATION` `params/file must be equal to one of the allowed values`; not mirrored is `404` `Static data '{name}' has not been synced yet. Run the ddragon:sync job.` | `src/routes/statics.rs:L269` (`src/static/mod.rs:L28-L45`, `L94-L101`) |

Captured:

```text
GET /v1/static/versions
HTTP/1.1 200 OK
content-type: application/json; charset=utf-8
x-cache: HIT
x-cache-age: 0
x-ratelimit-limit: 600
x-ratelimit-remaining: 594
x-ratelimit-reset: 46

{"current":"16.20.1","versions":["16.20.1","16.19.1","16.18.1", …]}
```

The mirror is refreshed by the `ddragon:sync` job every `DDRAGON_SYNC_S` (3 600 s) and on demand through `POST /v1/admin/ddragon/sync`.

## `/ddragon/*`: raw mirror files and images

No key, no quota, outside the documented API except the two image operations (`src/routes/docs.rs:L79-L80`). Every successful response carries `Cache-Control: public, max-age=31536000, immutable` (`src/routes/statics.rs:L26`, `L76-L82`). **Errors have an empty body**, not the envelope (`src/routes/statics.rs:L151-L156`; `tests/snapshots/ddragon__ddragon_missing_headers.snap`).

| Method | Path | Response | Handler |
|---|---|---|---|
| GET | `/ddragon/<file on disk>` e.g. `/ddragon/16.20.1/champion.json` | the file, via tower-http `ServeDir` | `src/routes/statics.rs:L75` |
| GET | `/ddragon/{version}/img/{kind}/{file}` | `200 image/png`; `kind` in `champion`, `profileicon`, `item`, `spell`; `version` must be in Riot's list and `file` listed by that version's data file; fetched from the CDN on first request, then from disk. `404` empty when unknown, `502` empty when the CDN fails | `src/routes/statics.rs:L107` |
| GET | `/ddragon/{version}/img/perk-images/{*icon}` | rune icon; `icon` is the `runesReforged.json` path minus `perk-images/`, slashes raw or `%2F` | `src/routes/statics.rs:L140` |
| any | `/ddragon/<unknown>` | `404` empty | `src/routes/statics.rs:L72` |

ninjagoldfinch.lol does not use these image routes; its browser loads icons straight from `ddragon.leagueoflegends.com` (site ADR-009, `docs/_meta/scratch/siblings.md` §1.7).

> **TODO (unverified):** whether `ServeDir` sends `Last-Modified` and honours `If-Modified-Since` for images written by the fill route on a later request; the behaviour is tower-http's (`docs/_meta/scratch/routes.md` §7).

---
**Sources:** `src/routes/riot.rs:L127-L300`, `src/routes/lol.rs:L37-L475`, `src/status/poll/summary.rs:L1-L245`, `src/routes/statics.rs:L4-L292`, `src/routes/passthrough.rs:L325-L476`, `src/http/validate.rs:L53-L166,L232-L244`, `src/riot/endpoints.rs:L126-L330`, `src/riot/ladder.rs:L7-L26`, `src/riot/routing.rs:L138-L205`, `src/static/mod.rs:L28-L101`, `src/fetcher.rs:L446-L472`, `tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`, `tests/snapshots/ddragon__ddragon_file_headers.snap`, `tests/snapshots/ddragon__ddragon_missing_headers.snap`, `docs/_meta/scratch/routes.md` §1.2-1.3,1.6-1.7, `docs/_meta/scratch/riot-and-cache.md` §2, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-11-10 against commit `8bcfd77`

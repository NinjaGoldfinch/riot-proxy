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
| `platform` | one of the 16 platform values, case-sensitive (`L71-L78`) | `400 BAD_REGION` `params/platform must be equal to one of the allowed values` |
| `matchId` | 6 to 40 chars, `^[A-Za-z0-9]+_[0-9]+$` (`L149-L166`) | `400 VALIDATION` |

## `/v1/riot/*`: account-v1

Riot endpoint: `account.byRiotId` `/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}`, `account.byPuuid` `/riot/account/v1/accounts/by-puuid/{puuid}`, `account.regionByPuuid` `/riot/account/v1/region/by-game/lol/by-puuid/{puuid}` (`src/riot/endpoints.rs:L93-L112`, `L271-L284`). Soft TTL 86 400 s, hard 345 600 s, negative TTL 300 s, persisted to L2.

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

A Riot ID that does not exist is `404 NOT_FOUND` `Resource not found upstream`, and the 404 is cached for 300 s, so the next call is `404` with `X-Cache: HIT-NEG` and message `Resource not found upstream (negative-cached)` (`src/fetcher.rs:L321-L327`).

> **TODO (unverified):** the 200 body of `/v1/riot/accounts/region/by-puuid/{puuid}` is not typed in the OpenAPI document (it is not in `RIOT_BODIES`, `src/routes/docs.rs:L288-L321`); the proxy forwards Riot's `AccountRegionDTO` bytes unchanged.

## `/v1/lol/*`: League passthrough

| Method | Path | Params | Riot call (host) | Cache | Handler |
|---|---|---|---|---|---|
| GET | `/v1/lol/summoners/by-puuid/{platform}/{puuid}` | platform, puuid | `summoner.byPuuid` `/lol/summoner/v4/summoners/by-puuid/{puuid}` (platform) | soft 3 600 s, L2, neg 30 s | `src/routes/lol.rs:L97` |
| GET | `/v1/lol/league/entries/by-puuid/{platform}/{puuid}` | platform, puuid | `league.entriesByPuuid` `/lol/league/v4/entries/by-puuid/{puuid}` | soft 300 s, neg 30 s | `src/routes/lol.rs:L113` |
| GET | `/v1/lol/league/apex/{platform}/{tier}/{queue}` | `tier` in `MASTER`, `GRANDMASTER`, `CHALLENGER`; `queue` in `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` | `league.master` / `grandmaster` / `challenger` `/lol/league/v4/<tier>leagues/by-queue/{queue}` | soft 120 s, L2, no neg | `src/routes/lol.rs:L131` |
| GET | `/v1/lol/league/entries/{platform}/{queue}/{tier}/{division}` | `tier` in `IRON`…`DIAMOND`; `division` in `I`…`IV`; `?page` 1 to 100000, default 1, always sent | `league.entriesByTier` `/lol/league/v4/entries/{queue}/{tier}/{division}?page=` | soft 120 s, L2 | `src/routes/lol.rs:L163` |
| GET | `/v1/lol/matches/ids/{region}/{puuid}` | `?start` 0 to 10000 (default 0); `?count` 1 to 100 (default 20); `?queue` 0 to 5000; `?type` in `ranked`, `normal`, `tourney`, `tutorial`; `?startTime`, `?endTime` ≥ 0 | `match.idsByPuuid` `/lol/match/v5/matches/by-puuid/{puuid}/ids` (region); `start` and `count` always sent, the others when given; unknown query keys are dropped | soft 120 s, neg 30 s | `src/routes/lol.rs:L206` |
| GET | `/v1/lol/matches/{region}/{matchId}` | region, matchId | `match.byId` `/lol/match/v5/matches/{matchId}` | immutable: `MISS` once, then `ARCHIVE` | `src/routes/lol.rs:L275` |
| GET | `/v1/lol/matches/{region}/{matchId}/timeline` | region, matchId | `match.timeline` `…/{matchId}/timeline` | immutable, archived | `src/routes/lol.rs:L292` |
| GET | `/v1/lol/spectator/active/{platform}/{puuid}` | platform, puuid | `spectator.activeGame` `/lol/spectator/v5/active-games/by-summoner/{puuid}` | soft 30 s; a 404 (not in game) is cached 30 s as `HIT-NEG` | `src/routes/lol.rs:L309` |
| GET | `/v1/lol/mastery/by-puuid/{platform}/{puuid}` | `?top` 1 to 200 | with `top`: `mastery.topByPuuid` `…/champion-masteries/by-puuid/{puuid}/top?count=N`; else `mastery.byPuuid` | soft 3 600 s, L2 | `src/routes/lol.rs:L326` |
| GET | `/v1/lol/rotations/{platform}` | platform | `platform.championRotations` `/lol/platform/v3/champion-rotations` | soft 21 600 s | `src/routes/lol.rs:L385` |
| GET | `/v1/lol/status/{platform}` | platform | `status.platformData` `/lol/status/v4/platform-data` | soft 60 s | `src/routes/lol.rs:L400` |

Hard TTL is soft × 4 while `STALE_WHILE_REVALIDATE=true` (default); the full table with line references is in [../reference/riot-api-usage.md](../reference/riot-api-usage.md). Exact upstream URLs per route are asserted in `tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`.

Match routes derive the region from the id's prefix (`EUW1_` is `europe`; `ME1_` is served by `europe`, ADR-143). A declared region that disagrees is refused before any Riot call (`src/routes/lol.rs:L53-L64`):

```text
GET /v1/lol/matches/americas/EUW1_7000000001
400 {"error":{"code":"BAD_REGION","message":"Match EUW1_7000000001 belongs to region 'europe', not 'americas'", …}}
```

Page past the end of a ladder division gives an empty array, not an error (`src/routes/lol.rs:L154-L155`). Riot caps each apex league at 10 000 entries (`src/riot/ladder.rs:L20`).

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
**Sources:** `src/routes/riot.rs:L127-L300`, `src/routes/lol.rs:L30-L405`, `src/routes/statics.rs:L4-L292`, `src/routes/passthrough.rs:L325-L476`, `src/http/validate.rs:L53-L166,L232-L244`, `src/riot/endpoints.rs:L93-L284`, `src/riot/ladder.rs:L7-L26`, `src/riot/routing.rs:L132-L195`, `src/static/mod.rs:L28-L101`, `src/fetcher.rs:L306-L327`, `tests/snapshots/lol_routes__lol_routes_upstream_calls.snap`, `tests/snapshots/ddragon__ddragon_file_headers.snap`, `tests/snapshots/ddragon__ddragon_missing_headers.snap`, `docs/_meta/scratch/routes.md` §1.2-1.3,1.6-1.7, `docs/_meta/scratch/riot-and-cache.md` §2, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-19 against commit `52fa67d`

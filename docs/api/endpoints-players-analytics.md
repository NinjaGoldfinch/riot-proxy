# Endpoints: `/v1/players` and `/v1/lol/analytics`

The composite player routes (several Riot calls assembled into one document) and the archive-derived analytics routes (never call Riot). Both groups need a **read** key and count against the consumer quota. Conventions and the platform table are in [endpoints.md](endpoints.md#conventions); filters and paging rules in detail in [pagination-filtering-and-versioning.md](pagination-filtering-and-versioning.md).

## `/v1/players/*`: composites

Shared behaviour (`src/routes/players.rs:L66-L105`, `L128-L147`):

- `Content-Type: application/json; charset=utf-8`.
- `X-Cache` is `MISS` if any part was a miss or bypass, else `HIT`; `X-Cache-Age` is the oldest part's content age; `X-Cache-Fetched-Age` is the oldest fetched age when any part has one. The body also reports ages per part (`ageSeconds`, `fetchedAgeSeconds`), which is what the site reads (site ADR-019).
- A part that fails becomes `null` and adds a `warnings[]` entry `"<part> unavailable (<CODE>: <message>)"`; only the id-list failure on the match page fails the whole request.
- `?refresh=true` is accepted from **any** consumer (unlike the passthrough routes) but metered: one claim per 60 s per `(part, puuid)` (`src/http/refresh.rs:L18`, `L43-L71`). Losing the race is not an error: the body says `"refreshed":false` and `"refreshAvailableIn":<seconds>`. `refresh` must be exactly `true` or `false` (`src/http/validate.rs:L185-L192`).
- `?platform` is validated case-sensitively; an unknown value is `400 BAD_REGION` `querystring/platform must be equal to one of the allowed values` (`src/http/validate.rs:L194-L198`).

| Method | Path | Query | Riot calls | Handler |
|---|---|---|---|---|
| GET | `/v1/players/{puuid}/profile` | `platform` optional; `topMastery` 1 to 20 (default 5); `refresh` | without `platform`: `account.regionByPuuid` (cached, never bypassed) finds it; then in parallel `account.byPuuid` (cluster picked), `summoner.byPuuid`, `league.entriesByPuuid`, `mastery.topByPuuid?count=<topMastery>` | `src/routes/players.rs:L400` |
| GET | `/v1/players/by-riot-id/{gameName}/{tagLine}/profile` | same | `account.byRiotId` (cached read path) resolves the PUUID, then as above; Riot's spelling of the name wins | `src/routes/players.rs:L435` |
| GET | `/v1/players/{puuid}/matches` | `platform` **required**; `start` 0 to 10000 (default 0); `count` 1 to 20 (default 10); `queue` 0 to 5000; `type` in `ranked`, `normal`, `tourney`, `tutorial`; `champion` 1 to 10000; `refresh` | `match.idsByPuuid` on the platform's region, then `match.byId` for every id not already archived | `src/routes/players.rs:L623` |
| GET | `/v1/players/{puuid}/champions` | `platform` optional filter; `queue` 0 to 5000; `patch` `major.minor`; `limit` 1 to 500 (default 200); `remakes` `exclude` (default) or `include` | none: reads the archive | `src/routes/players.rs:L1005` |

### Profile

Errors specific to this route: all four parts failing is `404 NOT_FOUND` `No profile data available for this PUUID` (`src/routes/players.rs:L358-L360`); a Riot ID whose account has no `puuid` is `404` `Riot ID '{gameName}#{tagLine}' did not resolve to a PUUID` (`L460-L468`); an unparsable `region` from `account.regionByPuuid` is `502 UPSTREAM_ERROR` (`L244-L263`). A successful profile upserts the `players` row (`L363-L379`).

Response `ProfileBody` (`src/routes/players.rs:L190-L218`), shortened from `tests/snapshots/players_routes__players_profile.snap`:

```json
{
  "puuid": "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEf",
  "platform": "kr",
  "region": "asia",
  "account": {"gameName": "Hide on bush", "puuid": "AbC…", "tagLine": "KR1"},
  "summoner": {"profileIconId": 6, "puuid": "AbC…", "revisionDate": 1790247628000, "summonerLevel": 939},
  "league": [{"queueType": "RANKED_SOLO_5x5", "tier": "CHALLENGER", "rank": "I", "leaguePoints": 2178, "wins": 410, "losses": 335, "hotStreak": true, "veteran": false, "freshBlood": false, "inactive": false, "puuid": "AbC…"}],
  "mastery": [{"championId": 7, "championLevel": 59, "championPoints": 623792, "lastPlayTime": 1790234538000, "puuid": "AbC…"}],
  "ageSeconds": {"account": 0, "league": 0, "mastery": 0, "summoner": 0},
  "fetchedAgeSeconds": {"account": 0, "league": 0, "mastery": 0, "summoner": 0},
  "refreshed": false,
  "refreshAvailableIn": 0,
  "warnings": []
}
```

`account`, `summoner`, `league` and `mastery` are Riot's own DTOs, untouched.

### Match page

`platform` missing is `400 VALIDATION` `querystring must have required property 'platform'` (ADR-065). `type` and `champion` together is `400 VALIDATION` `querystring/type must not be set with champion` (`src/routes/players.rs:L649-L653`). With `champion`, the page is built from the archive: the proxy asks Riot only for its newest 20 ids to keep the archive current, then filters archived matches by champion (`L541`, `L667-L671`, `L825-L837`); see [pagination-filtering-and-versioning.md](pagination-filtering-and-versioning.md#match-history).

The first page (`start=0`) of a player not yet walked queues a `backfill:player` job and reports it as `backfill{jobId,status,limit}` (`L547-L599`, `L484-L492`); `limit` is `LOOKUP_BACKFILL_LIMIT`, by default the whole history (`4294967295`).

Response `MatchPage` (`src/routes/players.rs:L496-L526`, `MatchSummary` `src/routes/summary.rs:L14-L43`), shortened from `tests/snapshots/players_routes__players_match_page.snap`:

```json
{
  "puuid": "AbC…", "platform": "kr", "region": "asia",
  "start": 0, "count": 5, "hasMore": true,
  "matchIds": ["KR_8393343196", "KR_8393320187", "KR_8393276163", "KR_8393234454", "KR_8393207026"],
  "matches": [
    {"matchId": "KR_8393343196", "queueId": 420, "gameMode": "CLASSIC", "gameVersion": "16.19.821.7343",
     "gameCreation": 1790245930375, "gameDuration": 1682, "gameEndTimestamp": 1790247623902, "endOfGameResult": "GameComplete",
     "player": {"championId": 134, "championName": "Syndra", "teamPosition": "MIDDLE", "teamId": 200, "win": true,
                "kills": 8, "deaths": 6, "assists": 14, "champLevel": 16, "goldEarned": 13360, "totalMinionsKilled": 206,
                "visionScore": 33, "item0": 3137, "item6": 3363, "summoner1Id": 12, "summoner2Id": 4,
                "perks": {"keystone": 8229, "primaryStyle": 8200, "subStyle": 8000}, "puuid": "AbC…"}}
  ],
  "matchIdsAgeSeconds": 0, "matchIdsFetchedAgeSeconds": 0,
  "backfill": {"jobId": "01M58N9VRRQQGTJX5Y8NRJ9533", "status": "queued", "limit": 4294967295},
  "refreshed": false, "refreshAvailableIn": 0, "warnings": []
}
```

With `champion`, the body also carries `champion` and `archive: {"complete": bool}` (whether the player's whole history has been walked).

### Champion pool

Reads the archive only. Served from a derived L1 entry with a 300 s TTL keyed on the full query (`POOL_TTL`, `src/routes/players.rs:L884`, `L1036-L1079`): a hit is `X-Cache: HIT` with the age; a computed answer is `X-Cache: MISS`, `X-Cache-Age: 0`. Captured against an empty archive:

```text
GET /v1/players/AbC…/champions
HTTP/1.1 200 OK
x-cache: MISS
x-cache-age: 0

{"puuid":"AbC…","platform":null,"queue":null,"patch":null,"archivedGames":0,"champions":[]}
```

With data, `champions[]` rows are `{championId, games, wins, winRate, avgKda, csPerMin, lastPlayedAt}` (`tests/snapshots/players_routes__players_champion_pool.snap`).

## `/v1/lol/analytics/*`: champion analytics

Computed by the `aggregate:analytics` job from archived ranked matches; the routes only read tables (`src/routes/analytics.rs:L1-L3`). Tagged `lol` in the OpenAPI document. Documented responses: 200, 304, 400, 401, 403, 404, 429.

### Common filters (`src/routes/analytics.rs:L486-L561`)

| Query | Values | Default |
|---|---|---|
| `platform` | a platform value; omitted sums every platform and answers `"platform": null` (ADR-065) | all |
| `queue` | `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` | first of `LADDER_QUEUES` (`RANKED_SOLO_5x5`) |
| `tier` | `IRON`…`CHALLENGER` or `UNKNOWN` | none: one summed row per champion with `"tier": null` (ADR-123) |
| `minTier` | `IRON`…`CHALLENGER`; exclusive with `tier` (`400 VALIDATION` `querystring/minTier must NOT be sent with tier`) | none |
| `side` | `blue`, `red` | both |
| `patch` | `all` or `major.minor` (3 to 8 chars, `^[0-9]+\.[0-9]+$`) | newest aggregated patch |
| `role` | `TOP`, `JUNGLE`, `MIDDLE`, `BOTTOM`, `UTILITY`, and `""` where team positions are allowed | all |
| `minGames` | integer ≥ 0 | 0 |
| `remakes` | `exclude`, `include` | `exclude` |

Routes and their own parameters:

| Method | Path | Specific params | Handler |
|---|---|---|---|
| GET | `/v1/lol/analytics/champions` | `role` team positions; `limit` 1 to 500 (default 200) | `src/routes/analytics.rs:L1013` |
| GET | `/v1/lol/analytics/champions/{championId}` | `championId` integer ≥ 1; `limit` 1 to 50 (default 10) per section | `src/routes/analytics.rs:L1215` |
| GET | `/v1/lol/analytics/champions/{championId}/matchups` | `role` lanes only (no `""`); `limit` 1 to 200 (default 50) | `src/routes/analytics.rs:L1111` |
| GET | `/v1/lol/analytics/champions/{championId}/builds` | `limit` 1 to 10 (default 3); `opponentId` integer ≥ 1; `role` lanes when `opponentId` is set, else team positions | `src/routes/analytics.rs:L1486` |
| GET | `/v1/lol/analytics/patches` | only `platform`, `queue`, `championId`, `side`, `remakes` | `src/routes/analytics.rs:L1623` |

### Response headers

Every 200 carries a weak `ETag` and `Cache-Control: private, max-age=300`; `If-None-Match` (a list or `*`) that matches is a `304` with the same headers (`src/routes/analytics.rs:L52`, `L577-L614`). `X-Cache: HIT` means the in-memory response cache (`ANALYTICS_CACHE_ENTRIES`, default 512, cleared on every rebuild and on a Data Dragon version change); `MISS` means computed now. There is **no** `X-Cache-Age` on these routes. Captured:

```text
GET /v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&limit=2
HTTP/1.1 200 OK
content-type: application/json; charset=utf-8
cache-control: private, max-age=300
etag: W/"HwbkHHPVcX8gqbVqhdoe6Fa0DRAJwgyuz7r2LxH1V0s"
x-cache: MISS

{"platform":null,"queue":"RANKED_SOLO_5x5","tier":null,"minTier":null,"side":null,"patch":null,"role":null,"computedAt":null,"totalGames":0,"champions":[]}

GET … with If-None-Match: W/"HwbkHHPVcX8gqbVqhdoe6Fa0DRAJwgyuz7r2LxH1V0s"
HTTP/1.1 304 Not Modified
etag: W/"HwbkHHPVcX8gqbVqhdoe6Fa0DRAJwgyuz7r2LxH1V0s"
x-cache: HIT
```

A read failure is `500 INTERNAL` (`L627`).

### Bodies

`champions` (`ChampionStatsResponse`, `src/routes/analytics.rs:L111-L134`), one row per champion from `tests/snapshots/analytics__analytics_champions.snap`:

```json
{"platform": null, "queue": "RANKED_SOLO_5x5", "tier": null, "minTier": null, "side": null, "patch": "16.19", "role": null,
 "computedAt": "2026-10-18T23:00:00.000Z", "totalGames": 10,
 "champions": [{"championId": 134, "tier": null, "patch": "16.19", "games": 1, "wins": 1, "winRate": 1, "pickRate": 1, "banRate": 0,
                "share": 0.1, "avgKda": 3.6667, "avgDamage": 36696, "avgVision": 33, "csPerMin": 7.3484, "goldPerMin": 476.5755}]}
```

`champions/{championId}` (`ChampionDetailResponse`, `L230-L267`) adds `championName`, `stats[]` (the summed row, or the named tier's), `byTier[]` (one row per tier), `matchups[]` (`{opponentId, role, games, wins, winRate}`), `items[]` (`{itemId, games, wins, winRate}`), `runes[]` (`{keystoneId, subStyleId, …}`), `spells[]` (`{spellA, spellB, …}`) and `sectionsComputedAt{stats,matchups,items,runes,spells}` (`tests/snapshots/analytics__analytics_champion_detail.snap`).

`champions/{championId}/builds` (`ChampionBuildsResponse`, `L352-L390`) returns `builds[]` of set builds: `{core: [6655, 3157], games, wins, winRate, winRateLow, pickRate, starter[], boots[], next[[…],[…],[…]], runes[], spells[], skillOrder[{order: "QEW", …}]}`; `winRateLow` is the Wilson lower bound (`L649-L658`). `role` is always set in the response (`tests/snapshots/analytics__analytics_champion_builds.snap`).

`patches` (`AnalyticsPatchesResponse`, `L408-L421`): `{"platform":null,"queue":"RANKED_SOLO_5x5","championId":null,"side":null,"patches":[…]}`.

What ninjagoldfinch.lol sends: `queue=RANKED_SOLO_5x5`, optional `patch` and `role`, `minGames=0`, `limit=500` on the list, `limit=10` on the detail, `limit=200` on matchups; never `tier`, `side` or `minTier` (`docs/_meta/scratch/siblings.md` §1.5).

---
**Sources:** `src/routes/players.rs:L5-L7,L66-L147,L190-L233,L244-L263,L358-L379,L400-L492,L541-L599,L623-L671,L825-L837,L884,L912-L926,L1005-L1085`, `src/routes/summary.rs:L14-L43`, `src/http/refresh.rs:L18-L71`, `src/http/validate.rs:L185-L250`, `src/routes/analytics.rs:L1-L3,L52-L56,L111-L134,L153-L178,L230-L267,L326-L421,L486-L629,L649-L658,L749-L754,L885-L965,L1013,L1111,L1215,L1486,L1623`, `src/routes/analytics/cache.rs:L1-L19,L89-L137,L181-L264`, `tests/snapshots/players_routes__*.snap`, `tests/snapshots/analytics__*.snap`, `docs/DECISIONS.md` ADR-065, ADR-123, `docs/_meta/scratch/routes.md` §1.4-1.5, `docs/_meta/scratch/siblings.md` §1.5, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-19 against commit `52fa67d`

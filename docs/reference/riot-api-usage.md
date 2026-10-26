# Riot API usage

Which Riot endpoints riot-proxy calls, how it routes them, how it reads Riot's rate-limit headers, and the policy facts the repository records. Links at the end go to Riot's own documentation, which is the authority for anything not evidenced here.

## Endpoints

The registry is `ENDPOINTS` in `src/riot/endpoints.rs:L91-L288`: v1's sixteen ids (`docs/contract/v1-endpoints.txt`) plus two added in v2. Soft TTL is the registry default; hard TTL is soft × 4 while `STALE_WHILE_REVALIDATE=true` (`src/riot/endpoints.rs:L13-L14`, `L476-L497`); `CACHE_TTL_OVERRIDES` can replace a soft TTL by override key. Negative TTL is `NEG_TTL_SECONDS` (30) or `NEG_TTL_ACCOUNT_SECONDS` (300) (`src/config.rs:L732-L733`). L2 means the entry is also written to the SQLite `cache` table and survives restarts.

| id | Riot path | Routing | soft / hard TTL (s) | neg | L2 | Called by |
|---|---|---|---|---|---|---|
| `account.byRiotId` | `/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}` | account cluster | 86 400 / 345 600 | 300 | yes | `/v1/riot/accounts/by-riot-id/*`, profile by Riot ID, admin track by Riot ID |
| `account.byPuuid` | `/riot/account/v1/accounts/by-puuid/{puuid}` | account cluster | 86 400 / 345 600 | 300 | yes | `/v1/riot/accounts/by-puuid/*`, profile |
| `account.regionByPuuid` (v2) | `/riot/account/v1/region/by-game/lol/by-puuid/{puuid}` | account cluster | 86 400 / 345 600 | 300 | yes | `/v1/riot/accounts/region/by-puuid`, profile without `platform` |
| `summoner.byPuuid` | `/lol/summoner/v4/summoners/by-puuid/{puuid}` | platform | 3 600 / 14 400 | 30 | yes | `/v1/lol/summoners/*`, profile |
| `league.entriesByPuuid` | `/lol/league/v4/entries/by-puuid/{puuid}` | platform | 300 / 1 200 | 30 | no | `/v1/lol/league/entries/by-puuid/*`, profile, `poll:rank`, `ranks:lookup` |
| `league.challenger` | `/lol/league/v4/challengerleagues/by-queue/{queue}` | platform | 120 / 480 | none | yes | `/v1/lol/league/apex/*`, `ladder:apex`, ladder probe |
| `league.grandmaster` | `/lol/league/v4/grandmasterleagues/by-queue/{queue}` | platform | 120 / 480 | none | yes | as above |
| `league.master` | `/lol/league/v4/masterleagues/by-queue/{queue}` | platform | 120 / 480 | none | yes | as above |
| `league.entriesByTier` | `/lol/league/v4/entries/{queue}/{tier}/{division}?page=` | platform | 120 / 480 | none | yes | `/v1/lol/league/entries/*`, `ladder:walk`, ladder probe |
| `league.expEntries` (v2) | `/lol/league-exp/v4/entries/{queue}/{tier}/{division}?page=` | platform | 120 / 480 | none | no | ladder probe only |
| `match.idsByPuuid` | `/lol/match/v5/matches/by-puuid/{puuid}/ids?start&count&queue&type&startTime&endTime` | region | 120 / 480 | 30 | no | `/v1/lol/matches/ids/*`, match page, `poll:matches`, `backfill:player`, `ladder:collect` |
| `match.byId` | `/lol/match/v5/matches/{matchId}` | region | immutable, archived | 30 | archive | `/v1/lol/matches/*`, match page, `archive:match` |
| `match.timeline` | `/lol/match/v5/matches/{matchId}/timeline` | region | immutable, archived | 30 | archive | `/v1/lol/matches/*/timeline`, `archive:match` (when `ARCHIVE_TIMELINES`), `timelines:backfill` |
| `spectator.activeGame` | `/lol/spectator/v5/active-games/by-summoner/{puuid}` | platform | 30 / 120 | 30 | no | `/v1/lol/spectator/active/*`, `poll:live` |
| `mastery.byPuuid` | `/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}` | platform | 3 600 / 14 400 | 30 | yes | `/v1/lol/mastery/by-puuid/*` |
| `mastery.topByPuuid` | `…/champion-masteries/by-puuid/{puuid}/top?count=` | platform | 3 600 / 14 400 | 30 | yes | `/v1/lol/mastery/*?top=`, profile |
| `platform.championRotations` | `/lol/platform/v3/champion-rotations` | platform | 21 600 / 86 400 | none | no | `/v1/lol/rotations/*` |
| `status.platformData` | `/lol/status/v4/platform-data` | platform | 60 / 240 | none | no | `/v1/lol/status/*`, default bucket of the admin debug route |

Sources per row: `src/riot/endpoints.rs:L93-L284`; the job column from `docs/_meta/scratch/jobs-and-observability.md` §3. Every call from a job goes through the same fetcher at bulk priority with a 1 s yield budget (`FetchOptions::JOB`).

### `startTime` on `match.idsByPuuid`

A crawl's collect sends `startTime` for a player it has collected before (THR-04, `src/jobs/ladder.rs` `collect_one`). The developer portal's parameter text, read on 2026-10-26 from `developer.riotgames.com/api-details/match-v5`, is: "Epoch timestamp in seconds. The matchlist started storing timestamps on June 16th, 2021. Any matches played before June 16th, 2021 won't be included in the results if the startTime filter is set." It doesn't say which of a match's timestamps (`gameCreation`, `gameStartTimestamp`, `gameEndTimestamp`) the filter compares. ADR-185 assumes the game's start, the reading that needs the larger overlap, and sizes `LADDER_COLLECT_OVERLAP_S` (7 200 s) to cover either.

## Static and CDN URLs (no key, no limiter)

| Use | URL | Evidence |
|---|---|---|
| Version list | `https://ddragon.leagueoflegends.com/api/versions.json` | `src/jobs/ddragon.rs:L22-L23,L108-L121` |
| Patch data | `https://ddragon.leagueoflegends.com/cdn/<version>/data/<locale>/<file>.json` for `champion`, `item`, `runesReforged`, `summoner`, `profileicon`, `map`; locale `DDRAGON_LOCALE` (`en_US`) | `src/jobs/ddragon.rs:L83-L85,L128-L152`, `src/static/mod.rs:L28-L35` |
| Images | `https://ddragon.leagueoflegends.com/cdn/<version>/img/<kind>/<file>` for `champion`, `profileicon`, `item`, `spell`; rune icons `…/cdn/img/perk-images/…` | `src/jobs/ddragon.rs:L87-L97`, `src/static/images.rs:L32-L39` |
| Queue table | `https://static.developer.riotgames.com/docs/lol/queues.json` | `src/jobs/ddragon.rs:L24-L25,L267-L285` |

The Data Dragon client is a separate reqwest client with a 15 s connect and 60 s total timeout (`src/jobs/ddragon.rs:L255-L261`). `DDRAGON_BASE_URL` swaps the CDN for a mock in tests.

## Routing

Hosts are `<platform>.api.riotgames.com` or `<region>.api.riotgames.com` (`src/riot/routing.rs:L123-L125`, `L201-L203`); `RIOT_BASE_URL` replaces the host for a mock and adds `x-riot-host` so one mock can keep per-host buckets (ADR-059, `src/riot/client.rs:L270-L275`).

| platform | match-v5 region | account cluster |
|---|---|---|
| `na1`, `br1`, `la1`, `la2` | `americas` | `americas` |
| `euw1`, `eun1`, `tr1`, `ru` | `europe` | `europe` |
| `kr`, `jp1` | `asia` | `asia` |
| `oc1`, `ph2`, `sg2`, `th2`, `tw2`, `vn2` | `sea` | `asia` |

`src/riot/routing.rs:L108-L121`, `L174-L181`. Match ids route by their prefix (`EUW1_` is `europe`); `ME1_` is not a platform the proxy serves but `europe` answers for it, verified live on 2026-10-13 (ADR-143, `src/riot/routing.rs:L132-L143`).

**Account-cluster rule (ADR-066).** Riot serves every account from each of `americas`, `asia`, `europe`. When the caller names no region the proxy tries `asia`, `americas`, `europe` (`Region::ACCOUNT_PICK`), taking the first with a limiter token free now, waiting for the soonest otherwise, and demoting a cluster that answered a service 429 for the rest of the request (`src/riot/routing.rs:L151-L153`, `src/fetcher.rs:L612-L650`). All clusters share one cache entry (`src/cache/keys.rs:L72-L79`). Pinned `/{region}/` routes never move.

## Request shape

| Header | Value |
|---|---|
| `X-Riot-Token` | the configured key, marked sensitive so it never appears in debug output (`src/riot/client.rs:L26,L234-L236,L268`) |
| `Accept` | `application/json` (`src/riot/client.rs:L269`) |
| `User-Agent` | `RIOT_USER_AGENT`, default `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)` (`src/config.rs:L103,L715`) |

The key never goes in the URL (`src/riot/client.rs:L267`). Client settings: 10 s connect, 25 s total timeout, 32 idle connections per host, embedded Mozilla roots only (`src/riot/client.rs:L20-L24`, `L237-L245`; ADR-007). Path params are percent-encoded like `encodeURIComponent`; query strings like `URLSearchParams` (`src/riot/endpoints.rs:L362-L382`, `src/riot/client.rs:L87-L127`).

## The rate-limit model

Riot enforces limits per API key per routing host (the limiter's *scope*) at two levels, and tells the client what they are in every response. The proxy never hard-codes a limit; it learns from the headers (`src/riot/limiter/headers.rs`, `src/riot/limiter/mod.rs:L370-L401`).

| Header from Riot | What it carries | What the proxy does |
|---|---|---|
| `X-App-Rate-Limit` | `limit:seconds,…` application windows for the key on that host, for example `20:1,100:120` | reconfigures the scope's app windows; sliding logs (ADR-023) |
| `X-App-Rate-Limit-Count` | `count:seconds,…` current usage | pads each window up to Riot's count so a restart or an outside caller cannot overrun |
| `X-Method-Rate-Limit` / `-Count` | the same per method | per-method windows keyed by endpoint id |
| `X-Rate-Limit-Type` | on a 429: `application`, `method` or `service` | with a numeric `Retry-After`, freezes the whole scope for that long (any type, ADR-024); `application`/`method` are logged as accounting errors |
| `Retry-After` | seconds | the freeze length; forwarded as `retryAfter` on `RATE_LIMITED` |

A fresh scope starts with the bootstrap limits `20:1,100:120`, which are the personal (development) key's limits: 20 requests per second and 100 per 2 minutes (`BOOTSTRAP_APP_LIMITS`, `src/riot/limiter/headers.rs:L31-L42`). That is roughly 0.83 requests per second sustained per host, which bounds everything the proxy does with the current key. Inspect a scope with `GET /v1/admin/limits/{scope}`:

```json
{"scope":"euw1","usage":[{"window":"20:1","used":0,"limit":20},{"window":"100:120","used":0,"limit":100}],"frozenMs":0}
```

Interactive requests (HTTP callers) wait up to `CLIENT_WAIT_BUDGET_MS` (2 000 ms) for a token; background jobs yield after `JOB_YIELD_BUDGET_MS` (1 000 ms) and are held below `BULK_USAGE_CEILING` (0.80) of every window while someone is using the app, `BULK_IDLE_CEILING` (0.95) after 30 s idle (`src/fetcher.rs:L42-L44`, `README.md`). A 5xx is retried twice (250 ms, 750 ms); an untyped 429 backs off 500 ms × 2ⁿ up to three times; a typed 429 waits out the freeze (`src/fetcher.rs:L34-L41`, `L544-L607`). Design detail is in [../architecture/concurrency-and-rate-limiting.md](../architecture/concurrency-and-rate-limiting.md).

## Keys and policy, as evidenced in the repository

- **One key.** The proxy holds one `RIOT_API_KEY` and one limiter; two processes on the same key would split, not double, the budget (ADR-001, `docs/design/03-architecture.md:L32-L36`). No multi-key rotation exists in the code.
- **Personal key in production.** The deployment runs on a personal (development) key with the `20:1,100:120` limits; there is no production key (`docs/_meta/scratch/WRITING-BRIEF.md`). Production keys are issued per application on the developer portal and need Riot's approval; their limits arrive in the same headers, so nothing in the proxy changes when one is granted.
- **Development keys expire every 24 hours.** A Riot 401/403 logs `RIOT KEY REJECTED` at `error` level with that reminder, and the consumer sees `502 UPSTREAM_ERROR` `Upstream authentication failed`; the proxy never retries it (`src/riot/client.rs:L143`, `L305`).
- **Apex list cap.** Riot returns at most 10 000 entries for an apex league; observed, not documented (`src/riot/ladder.rs:L14-L20`).
- **Disclaimer.** "riot-proxy isn't endorsed by Riot Games and doesn't reflect the views or opinions of Riot Games" (`README.md`, last line).

> **TODO (unverified):** whether a Riot *production* key exists. The repository calls the key used on the production host "the production key" (`src/riot/ladder.rs:L15-L16`, `docs/DECISIONS.md:L1338`, `L1519`, `docs/IMPLEMENTATION.md:L353`) while other ADR text describes a personal key's 100 per 2 min limit and says the owner "expects a production key" later (`docs/DECISIONS.md:L1098`, `L1440`); the writing brief records a personal key and no production key. The limiter learns whichever it is from the headers, so the proxy's behaviour does not depend on the answer.

## Raw calls for development

`riot-proxy riot get <endpoint> <routing> [params…] [-q key=value]` (built with `--features dev-cli`) GETs one endpoint through the configured key and prints Riot's raw body to stdout, the status to stderr, for example `riot-proxy riot get account/by-riot-id europe Faker KR1` (`src/cli/riot.rs`). `riot-proxy riot record` saves the exchange as a redacted replay fixture.

Through the running server, `GET /v1/admin/debug/riot?scope=<platform|region>&path=<riot path>[&method=<id>][&noCache=true]` makes any call on the named host with the limiter and cache of `method` (default `status.platformData`), pinned to that host (`src/routes/admin.rs:L1075-L1204`). `GET /v1/admin/debug/cache?scope&path&method` shows the L1 entry it would hit.

## Riot documentation

- API reference and per-endpoint methods: <https://developer.riotgames.com/apis>
- Portal documentation, rate limits and policies: <https://developer.riotgames.com/docs/portal>
- League of Legends developer docs, Data Dragon: <https://developer.riotgames.com/docs/lol>
- Data Dragon version list: <https://ddragon.leagueoflegends.com/api/versions.json>

---
**Sources:** `src/riot/endpoints.rs:L13-L14,L91-L288,L362-L382,L476-L497`, `src/riot/routing.rs:L108-L203`, `src/riot/client.rs:L20-L26,L87-L127,L143,L234-L275,L305`, `src/riot/limiter/headers.rs:L31-L42,L80-L154`, `src/riot/limiter/mod.rs:L370-L431`, `src/riot/ladder.rs:L14-L20`, `src/fetcher.rs:L34-L44,L544-L650`, `src/cache/keys.rs:L72-L79`, `src/jobs/ddragon.rs:L22-L25,L83-L97,L108-L152,L255-L285`, `src/static/mod.rs:L28-L35`, `src/static/images.rs:L32-L39`, `src/routes/admin.rs:L1075-L1204`, `src/cli/riot.rs`, `src/config.rs:L103,L715,L732-L733`, `docs/contract/v1-endpoints.txt`, `docs/design/03-architecture.md:L32-L36`, `docs/DECISIONS.md` ADR-001, ADR-007, ADR-023, ADR-024, ADR-059, ADR-066, ADR-143, `docs/_meta/scratch/riot-and-cache.md` §1-§4, `docs/_meta/scratch/jobs-and-observability.md` §3, `docs/_meta/scratch/WRITING-BRIEF.md`, `README.md`
**Last verified:** 2026-10-26 against commit `c9e1b33`

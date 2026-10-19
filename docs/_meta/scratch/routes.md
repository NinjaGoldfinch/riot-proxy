# riot-proxy v2 — HTTP / WebSocket / error inventory

Evidence-only inventory of the axum service at HEAD `7491ff6` (branch `docs/DOC-01-documentation-set`). Every row cites `file:line` in `src/` unless stated otherwise. Cross-checked against `tests/snapshots/openapi__openapi_document.snap` (70 documented paths, 76 operations; see §6).

Conventions used below:

- **Auth column**: `none` | `read` (consumer key with `read` scope) | `admin` (consumer key with `admin` scope **and** admin IP allowlist) | `socket` (any active key, checked inside the handler) | toggles (`DEV_UI`, `DOCS_UI`, `DASHBOARD_UI`).
- **Riot column**: endpoint ids from `src/riot/endpoints.rs` (`ENDPOINTS` table, `endpoints.rs:92`). Id → Riot path: `account.byRiotId` `/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}` (endpoints.rs:94-96); `account.byPuuid` `/riot/account/v1/accounts/by-puuid/{puuid}` (104-106); `summoner.byPuuid` `/lol/summoner/v4/summoners/by-puuid/{puuid}` (114-116); `league.entriesByPuuid` `/lol/league/v4/entries/by-puuid/{puuid}` (124-126); `league.challenger` `/lol/league/v4/challengerleagues/by-queue/{queue}` (136-138); `league.grandmaster` (146-148); `league.master` (156-158); `league.entriesByTier` `/lol/league/v4/entries/{queue}/{tier}/{division}` (166-168); `match.idsByPuuid` `/lol/match/v5/matches/by-puuid/{puuid}/ids` (176-178); `match.byId` `/lol/match/v5/matches/{matchId}` (186-188); `match.timeline` (196-198); `spectator.activeGame` `/lol/spectator/v5/active-games/by-summoner/{puuid}` (206-208); `mastery.byPuuid` `/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}` (216-218); `mastery.topByPuuid` `…/top` (226-228); `platform.championRotations` `/lol/platform/v3/champion-rotations` (238-240); `status.platformData` `/lol/status/v4/platform-data` (248-250); `league.expEntries` `/lol/league-exp/v4/entries/{queue}/{tier}/{division}` (262-264); `account.regionByPuuid` `/riot/account/v1/region/by-game/{game}/by-puuid/{puuid}` (276-278). `V2_ADDED = [league.expEntries, account.regionByPuuid]` (endpoints.rs:288).
- **X-Cache values** (fetcher.rs:50-69): `HIT`, `MISS`, `STALE`, `HIT-NEG`, `ARCHIVE`, `BYPASS`.
- Validation rule messages follow ajv wording; see §3 for the `BAD_REGION`/`VALIDATION` split (validate.rs:21-31).

---

## 0. Router assembly (`src/app.rs`)

`router()` (app.rs:69-119):

1. `routes::docs::api_router(Some(state))` builds the documented API: health + ws merged **without** a guard; the five read routers (`players`, `riot`, `lol`, `statics`, `analytics`) behind `route_layer(require_read)`; the admin router behind `route_layer(require_admin)` (docs.rs:118-141).
2. Merged in: `telemetry::metrics_router` (`/metrics`, app.rs:81), `routes::ui::router` (`/dev*`, `/dashboard`, app.rs:74,82), `routes::statics::files` (`/ddragon/*`, app.rs:75,83), `docs_router` only if `config.docs_ui` (app.rs:84-86), `routes::dev::router` only if `config.dev_ui` (app.rs:77,87-89).
3. `.fallback(not_found)` and `.method_not_allowed_fallback(not_found)` (app.rs:90-93): a known path with the wrong method is a 404, not a 405 (Fastify parity).
4. CORS middleware (`cors::scoped`) is layered **after** the fallbacks, only when `ADMIN_CORS_ORIGINS` is non-empty (app.rs:94-101).
5. Layers, innermost first (app.rs:103-118): `record_request` (metrics + recent ring) → `DefaultBodyLimit::max(1_048_576)` (app.rs:31,105) → `CompressionLayer::new()` (app.rs:106) → `TraceLayer` (span `http{request_id,method,path,consumer}`, app.rs:107-116) → `CatchPanicLayer::custom(panic_response)` (app.rs:117) → `request_id` (outermost, app.rs:118).

`serve()` binds with `into_make_service_with_connect_info::<SocketAddr>()` (app.rs:214), so `ConnectInfo` is available to auth; graceful shutdown grace is 10 s (`SHUTDOWN_GRACE`, app.rs:35).

Metrics label: `proxy_requests_total{route,status,cache}` where `route` is the matched template in Fastify `:param` form or `unmatched` (app.rs:121-131,169-170,174-186); `cache` label derived from the response `X-Cache` header (app.rs:144-152).

---

## 1. Route table

### 1.1 Ops: `/healthz`, `/readyz`, `/metrics`

| Method | Path | Handler | Auth | Params | Riot | Response | X-Cache | Notes |
|---|---|---|---|---|---|---|---|---|
| GET | `/healthz` | `health::healthz` health.rs:43 | none (`security(())` health.rs:40; router merged unguarded docs.rs:137) | — | none | `{"ok":true}` (`Health` health.rs:28-30) via `axum::Json` | — | CORS-covered when enabled (cors.rs:42); not recorded in recent ring (recent.rs:116-122). |
| GET | `/readyz` | `health::readyz` health.rs:82 | none | — | none | `{ok,sqlite,limiter,keyScope,sqliteReaders,sqliteReadersFree}` camelCase (`Ready` health.rs:51-63). 200 when `sqlite && limiter`, else **503 with the same body** (health.rs:87-94). | — | SQLite probe is `BEGIN IMMEDIATE; ROLLBACK;` through `Db::write` with a 2 s timeout (`READY_TIMEOUT` health.rs:19,83-87). `limiter` = checkpoint restored flag (health.rs:88). |
| GET | `/metrics` | `telemetry::render_metrics` telemetry.rs:119 (router telemetry.rs:102-106) | none (`security(())` telemetry.rs:116) | — | none | Prometheus text exposition, `Content-Type: text/plain; version=0.0.4; charset=utf-8` (telemetry.rs:24,120) | — | Documented in OpenAPI via `paths(crate::telemetry::render_metrics)` (docs.rs:78). Not recorded in recent ring. |

### 1.2 `/v1/riot/*` — account-v1 passthrough (`src/routes/riot.rs`)

Shared behaviour for every passthrough route (riot + lol): auth `read` (docs.rs:127-130); malformed path segments → `VALIDATION` with message `params <axum text>` (`bad_path` riot.rs:137-139); `?refresh=true|1` sets `bypass` **only for admin-scoped consumers**, silently ignored otherwise (passthrough.rs:470-476); response built by `respond()` (passthrough.rs:435-466): status 200, `Content-Type: application/json; charset=utf-8` (passthrough.rs:401), body = Riot bytes verbatim, `X-Cache`, `X-Cache-Age` (ms rounded to whole s, passthrough.rs:441-446), `X-Cache-Fetched-Age` when the fetcher reports one (not from `ARCHIVE`, passthrough.rs:447-454). On error the envelope plus `X-Cache` when the fetcher set one (`HIT-NEG`, passthrough.rs:457-464). Documented responses = `PassthroughResponses` 200/400/401/403/404/429/502/503 (passthrough.rs:325-351).

| Method | Path | Handler | Auth | Params | Riot | Response | X-Cache | Notes |
|---|---|---|---|---|---|---|---|---|
| GET | `/v1/riot/accounts/by-riot-id/{gameName}/{tagLine}` | `account_by_riot_id_any` riot.rs:172 | read | `gameName` 1–16 code points (validate.rs:117-119); `tagLine` 1–5 (validate.rs:125-127) | `account.byRiotId` on whichever cluster has room, order asia→americas→europe (`RiotRequest::account` riot.rs:150-153; `Region::ACCOUNT_PICK` routing.rs:153) | Riot `AccountDto` (docs.rs:289 `type_riot_bodies`) | HIT/MISS/STALE/HIT-NEG/BYPASS | One cached copy shared with the region-pinned route (riot.rs:155-158). |
| GET | `/v1/riot/accounts/by-puuid/{puuid}` | `account_by_puuid_any` riot.rs:201 | read | `puuid` 60–128 chars, `^[A-Za-z0-9_-]+$` (validate.rs:133-147) | `account.byPuuid` any cluster | `AccountDto` (docs.rs:290) | as above | |
| GET | `/v1/riot/accounts/region/by-puuid/{puuid}` | `account_region_by_puuid` riot.rs:230 | read | `puuid` | `account.regionByPuuid` with params `["lol", puuid]` (riot.rs:241) | Riot `AccountRegionDTO` verbatim (`region` = platform routing value) — not typed in OpenAPI (not in `RIOT_BODIES` docs.rs:288-321) | as above | |
| GET | `/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}` | `account_by_riot_id` riot.rs:262 | read | `region` ∈ `americas|europe|asia|sea` case-sensitive, else `BAD_REGION` (validate.rs:67-69; `Region::ALL` routing.rs:146); gameName/tagLine as above | `account.byRiotId` pinned to region; `sea` routed to `asia` (riot.rs:141-147) | `AccountDto` (docs.rs:291-295) | as above | |
| GET | `/v1/riot/accounts/by-puuid/{region}/{puuid}` | `account_by_puuid` riot.rs:294 | read | `region`, `puuid` | `account.byPuuid` pinned | `AccountDto` (docs.rs:289-290) | as above | |

### 1.3 `/v1/lol/*` — League passthrough (`src/routes/lol.rs`)

Platform validation: `platform` ∈ `Platform::ALL` (16 values `na1,br1,la1,la2,euw1,eun1,tr1,ru,kr,jp1,oc1,ph2,sg2,th2,tw2,vn2`, routing.rs:46-63), case-sensitive, failure → `BAD_REGION` `params/platform must be equal to one of the allowed values` (validate.rs:71-78,53-65).

| Method | Path | Handler | Auth | Params | Riot | Response | X-Cache | Notes |
|---|---|---|---|---|---|---|---|---|
| GET | `/v1/lol/summoners/by-puuid/{platform}/{puuid}` | `summoner_by_puuid` lol.rs:97 (via `platform_puuid` lol.rs:71-88) | read | platform, puuid | `summoner.byPuuid` | `SummonerDTO` (docs.rs:301-305) | passthrough | |
| GET | `/v1/lol/league/entries/by-puuid/{platform}/{puuid}` | `league_entries_by_puuid` lol.rs:113 | read | platform, puuid | `league.entriesByPuuid` | `LeagueEntryDTO[]` (docs.rs:306-310) | passthrough | |
| GET | `/v1/lol/league/apex/{platform}/{tier}/{queue}` | `league_apex` lol.rs:131 | read | `tier` ∈ `MASTER|GRANDMASTER|CHALLENGER` (`APEX_TIERS` ladder.rs:13; lol.rs:143); `queue` ∈ `RANKED_SOLO_5x5|RANKED_FLEX_SR` (`RANKED_QUEUES` ladder.rs:7; lol.rs:144) | `league.master` / `league.grandmaster` / `league.challenger` by tier (`apex_endpoint` ladder.rs:50-56) | Riot league list (untyped) | passthrough | Riot caps apex lists at 10 000 entries (`RIOT_APEX_LIST_CAP` ladder.rs:20). |
| GET | `/v1/lol/league/entries/{platform}/{queue}/{tier}/{division}` | `league_entries_page` lol.rs:163 | read | queue; `tier` ∈ `PAGED_TIERS` IRON…DIAMOND (ladder.rs:9-11; lol.rs:177); `division` ∈ `I|II|III|IV` (ladder.rs:22; lol.rs:178); `?page` int 1–100000 default 1, always sent (lol.rs:180-187) | `league.entriesByTier` `?page=` | `LeagueEntryDTO[]` (docs.rs:311-315) | passthrough | Page past the end → empty array (lol.rs:154-155 description). |
| GET | `/v1/lol/matches/ids/{region}/{puuid}` | `match_ids` lol.rs:206 | read | region, puuid; `?start` 0–10000 default 0; `?count` 1–100 default 20; `?queue` 0–5000; `?type` ∈ `ranked|normal|tourney|tutorial`; `?startTime` ≥0; `?endTime` ≥0 (lol.rs:217-229) | `match.idsByPuuid` with start,count always and queue/type/startTime/endTime when given (lol.rs:230-243) | `string[]` | passthrough | |
| GET | `/v1/lol/matches/{region}/{matchId}` | `match_by_id` lol.rs:275 (via `match_route` lol.rs:248-264) | read | `matchId` 6–40 chars `^[A-Za-z0-9]+_[0-9]+$` (validate.rs:149-166); region; prefix→region must agree else `BAD_REGION` `Match {id} belongs to region '{derived}', not '{declared}'`; unknown prefix falls back to declared (`match_region` lol.rs:53-64) | `match.byId` | Riot match (untyped) | passthrough; `ARCHIVE` once archived (lol.rs:269-270 description) | |
| GET | `/v1/lol/matches/{region}/{matchId}/timeline` | `match_timeline` lol.rs:292 | read | as above | `match.timeline` | Riot timeline | passthrough / ARCHIVE | |
| GET | `/v1/lol/spectator/active/{platform}/{puuid}` | `spectator_active` lol.rs:309 | read | platform, puuid | `spectator.activeGame` | Riot CurrentGameInfo | passthrough; 404 negatively cached → `HIT-NEG` (lol.rs:303-304) | |
| GET | `/v1/lol/mastery/by-puuid/{platform}/{puuid}` | `mastery_by_puuid` lol.rs:326 | read | platform, puuid; `?top` 1–200 (lol.rs:340) | `?top=N` → `mastery.topByPuuid?count=N`; else `mastery.byPuuid` (lol.rs:340-356) | `ChampionMasteryDto[]` (docs.rs:316-320) | passthrough | |
| GET | `/v1/lol/rotations/{platform}` | `rotations` lol.rs:385 (via `platform_only` lol.rs:361-377) | read | platform | `platform.championRotations` | Riot ChampionInfo | passthrough | |
| GET | `/v1/lol/status/{platform}` | `platform_status` lol.rs:400 | read | platform | `status.platformData` | Riot PlatformDataDto | passthrough | |

### 1.4 `/v1/lol/analytics/*` — archive-derived analytics (`src/routes/analytics.rs`, tag `lol`)

Shared: auth `read` (merged into the read router docs.rs:124). Never calls Riot (analytics.rs:1-3). Query parsing in `Request::parse` (analytics.rs:885-927) → `common()` (analytics.rs:486-520):

- `platform` optional (`query_platform`, bad → `BAD_REGION`, validate.rs:195-198); omitted = every platform summed (analytics.rs:464).
- `queue` ∈ `RANKED_QUEUES`, default first of `LADDER_QUEUES` (analytics.rs:554-561; config default `RANKED_SOLO_5x5` config.rs:742).
- `tier` ∈ `tiers()` + `UNKNOWN` (analytics.rs:493-496); `minTier` ∈ `tiers()`; both set → `VALIDATION` `querystring/minTier must NOT be sent with tier` (analytics.rs:524-539).
- `side` ∈ `blue|red` (analytics.rs:542-544).
- `patch` = `all` or `major.minor` (3–8 chars, `^[0-9]+\.[0-9]+$`, validate.rs:211-229; analytics.rs:499-502); default newest aggregated patch (`patch_or_latest` analytics.rs:749-754).
- `role` ∈ route-specific set: `TEAM_POSITIONS` = `TOP,JUNGLE,MIDDLE,BOTTOM,UTILITY,""` (analytics.rs:54) or `LANE_POSITIONS` (analytics.rs:56).
- `minGames` int ≥ 0 (analytics.rs:504); `limit` per route; `remakes` ∈ `exclude|include` default exclude (validate.rs:248-250).
- `championId` path: integer ≥ 1 else `params/championId must be integer` / `must be >= 1` (analytics.rs:564-572).

Response: `ETag` weak `W/"<base64url(sha256(parts))>"` (analytics.rs:577-588); `If-None-Match` list or `*` → **304** (analytics.rs:591-596,600-603); `Cache-Control: private, max-age=300` (analytics.rs:52,607); `X-Cache: HIT` when served from the in-memory response cache, `MISS` when computed (analytics.rs:600-616,619-629); **no `X-Cache-Age`** (docs.rs:224-232). Read failure → `INTERNAL` (analytics.rs:627). Cache (`analytics/cache.rs`): LRU of `ANALYTICS_CACHE_ENTRIES` entries (default 512, 0–4096, config.rs:754; `0` stores nothing cache.rs:109-110), keyed by `route|keyScope|championId|…` (analytics.rs:929-965), invalidated by `Analytics::clear` on each `aggregate:analytics` rebuild and by a Data Dragon version change (cache.rs:1-19,89-100,133-137), concurrent misses coalesced through `SingleFlight` keyed by generation (cache.rs:207-235); warmed after rebuild with the `WARM` slices (cache.rs:181-187,240-264).

| Method | Path | Handler | Params (route-specific) | Response body |
|---|---|---|---|---|
| GET | `/v1/lol/analytics/champions` | `champions` analytics.rs:1013 | roles `TEAM_POSITIONS`; `limit` 1–500 default 200 (analytics.rs:893) | `ChampionStatsResponse` analytics.rs:111-134 (`{platform,queue,tier,minTier,side,patch,role,computedAt,totalGames,champions[]}`) |
| GET | `/v1/lol/analytics/champions/{championId}` | `champion_detail` analytics.rs:1215 | `limit` 1–50 default 10 per section (analytics.rs:896) | `ChampionDetailResponse` analytics.rs:230-267 (`stats`, `byTier`, `matchups`, `items`, `runes`, `spells`, `sectionsComputedAt`) |
| GET | `/v1/lol/analytics/champions/{championId}/matchups` | `champion_matchups` analytics.rs:1111 | roles `LANE_POSITIONS`; `limit` 1–200 default 50 (analytics.rs:900) | `ChampionMatchupsResponse` analytics.rs:153-178 |
| GET | `/v1/lol/analytics/champions/{championId}/builds` | `champion_builds` analytics.rs:1486 | `limit` 1–10 default 3; `opponentId` int ≥ 1; roles = lanes when `opponentId` present else `TEAM_POSITIONS` (analytics.rs:902-913) | `ChampionBuildsResponse` analytics.rs:352-390 (`builds[]` of `ChampionBuildEntry` analytics.rs:326-347 with `winRateLow` Wilson bound analytics.rs:649-658) |
| GET | `/v1/lol/analytics/patches` | `patches` analytics.rs:1623 | `platform`, `queue`, `championId` ≥ 1, `side`, `remakes` only (analytics.rs:915-925) | `AnalyticsPatchesResponse` analytics.rs:408-421 |

Documented responses per route: 200, 304, `LocalErrors` (400/401/403/404/429) (analytics.rs:1010-1011 etc.).

### 1.5 `/v1/players/*` — composite reads (`src/routes/players.rs`)

Shared: auth `read`. Composite `X-Cache` = `MISS` if any part was `Miss|Bypass`, else `HIT`; `X-Cache-Age` = max part age; `X-Cache-Fetched-Age` = max fetched age when any part had one (`Tally` players.rs:66-105). Content-Type `application/json; charset=utf-8` (players.rs:97). `?refresh=true` is allowed for **any** consumer (players.rs:5-7) but metered per `(part, puuid)`: one claim per 60 s (`COOLDOWN` refresh.rs:18; `RefreshWindows::window` refresh.rs:43-71); losing the race is not an error, the body reports `refreshed:false` and `refreshAvailableIn` seconds (rounded up, refresh.rs:63-70). Metric `proxy_refresh_claims_total{part,outcome=claimed|coalesced}` (refresh.rs:53,61; metrics.rs:34). A failed part becomes `null` plus a `warnings[]` entry `"<part> unavailable (<CODE>: <message>)"` (players.rs:128-147).

| Method | Path | Handler | Params | Riot | Response | Notes |
|---|---|---|---|---|---|---|
| GET | `/v1/players/{puuid}/profile` | `profile` players.rs:400 | `puuid`; `?platform` optional (`query_platform`); `?topMastery` 1–20 default 5; `?refresh` bool `true|false` only (players.rs:227-233; `bool_query` validate.rs:185-192) | Without `platform`: `account.regionByPuuid` (cached, never bypassed) → platform; unparsable `region` → `UPSTREAM_ERROR` 502 (players.rs:244-263). Then concurrently `account.byPuuid` (any cluster), `summoner.byPuuid`, `league.entriesByPuuid`, `mastery.topByPuuid?count=<topMastery>` (players.rs:285-324) | `ProfileBody` players.rs:190-218: `{puuid,platform,region,account,summoner,league,mastery,ageSeconds{…},fetchedAgeSeconds{…},refreshed,refreshAvailableIn,warnings[]}` | All four parts failing → 404 `No profile data available for this PUUID` (players.rs:358-360). Upserts the `players` row (players.rs:363-379). Refresh window part key `"profile"` (players.rs:418). Documented: 200 + `UpstreamErrors`. |
| GET | `/v1/players/by-riot-id/{gameName}/{tagLine}/profile` | `profile_by_riot_id` players.rs:435 | gameName, tagLine, same query | `account.byRiotId` always from cache path (no bypass, players.rs:452-459), reused as the `account` part unless the refresh was won (players.rs:475) | same `ProfileBody` | Account body without `puuid` → 404 `Riot ID '{g}#{t}' did not resolve to a PUUID` (players.rs:460-468). Window keyed on the PUUID so both entry points share it (players.rs:473-474). |
| GET | `/v1/players/{puuid}/matches` | `match_page` players.rs:623 | `?platform` **required** → `VALIDATION` `querystring must have required property 'platform'` (validate.rs:201-208; players.rs:635); `?start` 0–10000 default 0; `?count` 1–20 default 10; `?queue` 0–5000; `?type` ∈ ranked/normal/tourney/tutorial; `?refresh`; `?champion` 1–10000; `type`+`champion` → `VALIDATION` `querystring/type must not be set with champion` (players.rs:633-656) | `match.idsByPuuid` (region = platform's) with start/count/queue/type — with `champion`, always Riot's newest 20 ids (`RECENT_IDS` players.rs:541,667-671); `match.byId` for every id not already archived (players.rs:725-746; champion path players.rs:825-837) | `MatchPage` players.rs:496-526: `{puuid,platform,region,start,count,matchIds[],matches[MatchSummary],hasMore,champion?,archive?{complete},matchIdsAgeSeconds,matchIdsFetchedAgeSeconds,backfill,refreshed,refreshAvailableIn,warnings[]}`; `MatchSummary` summary.rs:14-43 | Id-list failure fails the page (players.rs:685-688). First page (`start=0`) queues `backfill:player` (`maybe_backfill` players.rs:547-599) when `LOOKUP_BACKFILL_LIMIT>0` (default `u32::MAX`, config.rs:739) and no completed walk → `backfill{jobId,status,limit}` (players.rs:484-492). Archive hits counted in `proxy_cache_reads_total{state=hit}` (players.rs:731-734). Window part `"matches"` (players.rs:662). |
| GET | `/v1/players/{puuid}/champions` | `champions` players.rs:1005 | `?platform` optional filter; `?queue` 0–5000; `?patch` major.minor; `?limit` 1–500 default 200; `?remakes` (players.rs:1015-1030) | none (archive read) | `PlayerChampions` players.rs:912-926 | L1 derived cache key `derived_key(scope,"pool","/players/{puuid}/champions?…")`, TTL 300 s (`POOL_TTL` players.rs:884,1036-1059,1072-1079): hit → `X-Cache: HIT` + `X-Cache-Age`; computed → `X-Cache: MISS`, `X-Cache-Age: 0`, no fetched age (players.rs:1081-1085). Documented 200 + `LocalErrors`. |

### 1.6 `/v1/static/*` — Data Dragon JSON (`src/routes/statics.rs`)

Shared: auth `read`; never touches Riot's API or the limiter (statics.rs:4). `local()` sets `X-Cache` and `X-Cache-Age: 0` (statics.rs:161-167).

| Method | Path | Handler | Params | Response | X-Cache | Notes |
|---|---|---|---|---|---|---|
| GET | `/v1/static/versions` | `static_versions` statics.rs:177 | — | `{"current":<string|null>,"versions":[…]}` (statics.rs:195-199) | `HIT` from mirror; `MISS` when fetched live from the CDN before the first sync (statics.rs:180-194) | CDN failure → `INTERNAL` (statics.rs:189-192). |
| GET | `/v1/static/queues` | `static_queues` statics.rs:210 | — | Riot `queues.json` | `HIT` | Not synced → 404 `The queue table has not been synced yet. Run the ddragon:sync job.` (statics.rs:213-214). |
| GET | `/v1/static/{file}` | `static_file` statics.rs:269 | `file` ∈ `champion,item,runesReforged,summoner,profileicon,map` + aliases `champions,items,runes,summoner-spells,profile-icons,maps` (static/mod.rs:28-45; `resolve_file` static/mod.rs:94-101) else `VALIDATION` `params/file must be equal to one of the allowed values` (statics.rs:274-279); `?version` ≤ 20 chars, `^[0-9]+(\.[0-9]+)*$` (validate.rs:232-244) | Data Dragon JSON untouched | `HIT` | Missing → 404 `Static data '{name}' has not been synced yet. Run the ddragon:sync job.` (statics.rs:289-292). |

### 1.7 `/ddragon/*` — raw mirror files and images (`statics::files` statics.rs:67-83)

No key, no quota, outside the documented API except the two image operations (docs.rs:79-80). `nest_service("/ddragon", ServeDir::new(mirror.dir()).fallback(fill))` (statics.rs:75). Every **successful** response gets `Cache-Control: public, max-age=31536000, immutable` (`IMMUTABLE` statics.rs:26,76-82). Error responses have an **empty body**, not the envelope (statics.rs:151-156).

| Method | Path | Handler | Response | Notes |
|---|---|---|---|---|
| GET | `/ddragon/<anything on disk>` | `tower_http::services::ServeDir` (statics.rs:75) | file bytes; `Last-Modified`/`If-Modified-Since` 304 handled by ServeDir (docs.rs:192-197 describes it) | Serves mirrored JSON (e.g. `/ddragon/<version>/<file>.json`) and already-fetched images straight from disk. Undocumented in OpenAPI. |
| GET | `/ddragon/{version}/img/{kind}/{file}` | `ddragon_image` statics.rs:107 (fill route statics.rs:70) | `200 image/png` (statics.rs:150); `404` empty (`ImageError::NotFound` statics.rs:152); `502` empty on CDN failure (statics.rs:153-156) | `kind` ∈ `champion|profileicon|item|spell` (`IMAGE_KINDS` images.rs:32-37; `ImageKind` schema statics.rs:35-40). Version must be in Riot's list; file must be listed by that version's data file (statics.rs:49-53). Reached only when the file is not on disk yet. |
| GET | `/ddragon/{version}/img/perk-images/{*icon}` | `ddragon_rune_image` statics.rs:140 (fill route statics.rs:71) | as above | `icon` is `runesReforged.json`'s path minus `perk-images/`; slashes raw or `%2F` (statics.rs:126-129). |
| * | `/ddragon/<unknown>` | fill fallback statics.rs:72 | `404` empty | |

### 1.8 `/v1/admin/*` — administration (`src/routes/admin.rs`, `admin/ladder_probe.rs`)

Shared: auth `admin` = admin scope **and** IP allowlist (docs.rs:131-134; auth.rs:276-287); consumer quota applies (auth.rs:327-342); CORS-covered when enabled (cors.rs:42). Router list admin.rs:44-82. Helpers: `ok()`/`json()` emit `application/json; charset=utf-8` (admin.rs:84-93); store failures → `INTERNAL` without detail (`internal` admin.rs:103-107). JSON bodies are parsed by `http::body::Body` with ajv-style coercion and messages (body.rs:341-352); non-JSON → `VALIDATION` `body is not valid JSON`, non-object → `body must be object` (body.rs:367-376); missing required → `body must have required property '<n>'` (body.rs:383-391). ULID ids: 26 chars parseable as ULID else `VALIDATION` `params/id must match format "ulid"` (validate.rs:253-259). `keyScope` throughout = `state.fetcher.key_scope()` (admin.rs:99-101).

Consumers:

| Method | Path | Handler | Params / body | Riot | Response | Notes |
|---|---|---|---|---|---|---|
| POST | `/v1/admin/consumers` | `create_consumer` admin.rs:180 | body `name` 1–100 chars **required**; `scopes` array of `read|admin`, ≥1 item, scalar coerced to array, default `["read"]`; `quotaPerMin` int 1–1 000 000 default 600 (admin.rs:181-200; `DEFAULT_QUOTA_PER_MIN` consumers.rs:14) | none | `CreatedConsumer` `{id,name,scopes,quotaPerMin,key,warning}` admin.rs:162-171; `warning` = `Store this key now — it cannot be retrieved again.` (admin.rs:214) | 200 (not 201). `DuplicateName`/`Invalid` → `VALIDATION` with the store's message (admin.rs:217-219; consumers.rs:53,113-133,163-172). |
| GET | `/v1/admin/consumers` | `list_consumers` admin.rs:231 | — | none | `{consumers:[ConsumerSummary]}` admin.rs:141-144; `ConsumerSummary` `{id,name,scopes,quotaPerMin,createdAt,disabledAt}` admin.rs:114-126 | Includes revoked rows (`disabledAt` set). |
| DELETE | `/v1/admin/consumers/{id}` | `revoke_consumer` admin.rs:248 | `id` ULID | none | `{"ok":true,"id":…}` (admin.rs:268) | Soft delete (consumers.rs:256-283); drops the key from the auth cache at once (admin.rs:264) and closes that consumer's sockets with 4401 (admin.rs:266; hub.rs:190-198). Not active → 404 `No active consumer with that id` (admin.rs:270). |
| POST | `/v1/admin/consumers/{id}/revoke-cache` | `revoke_cache` admin.rs:293 | `id` ULID; body `keyHash` **required**, exactly 64 chars (admin.rs:303-307) | none | `{"ok":true,"id":…,"stillActive":bool}` (admin.rs:322-324) | Non-hex or wrong consumer → 404 `No consumer with that id and key hash` (admin.rs:312-318,326). |

Players / archive:

| Method | Path | Handler | Params / body | Riot | Response | Notes |
|---|---|---|---|---|---|---|
| GET | `/v1/admin/tracked-players` | `list_players` admin.rs:393 | `?tracked` — only value accepted is `true` (admin.rs:395-399) | none | `{players:[PlayerRecord]}` admin.rs:378-381; `PlayerRecord` admin.rs:336-357 | Rows for the current `keyScope` only (admin.rs:386-389). |
| POST | `/v1/admin/tracked-players` | `track_player` admin.rs:454 | body `platform` **required** (`platform_at("body")`); `puuid` (validated `puuid_at("body")`) **or** `gameName`+`tagLine`; `tracked` bool default true (admin.rs:455-475). Neither → `VALIDATION` `Provide either puuid, or gameName and tagLine` (admin.rs:484-490) | `account.byRiotId` (any cluster, cached read path) when resolving a Riot ID (`resolve` admin.rs:560-571) | `TrackedPlayer` = `PlayerRecord` flattened + `backfill` (admin.rs:430-435) | Riot's spelling of the Riot ID wins (admin.rs:493-495); no `puuid` in account → 404 `Riot ID '{n}#{t}' did not resolve to a PUUID` (admin.rs:499-502). Queues `backfill:player` (reason `track`) when tracked, not yet walked and `LOOKUP_BACKFILL_LIMIT>0` (admin.rs:524-550). Documented `UpstreamErrors`. |
| DELETE | `/v1/admin/tracked-players/{puuid}` | `untrack_player` admin.rs:580 | `puuid` | none | `{"ok":true,"puuid":…,"tracked":false}` (admin.rs:593) | Untracks, keeps the row (admin.rs:576); unknown → 404 `No such player for the current key scope` (admin.rs:594). |
| GET | `/v1/admin/players/{puuid}/archive` | `player_archive` admin.rs:664 | `puuid` (`puuid_path` admin.rs:649-653) | none | `PlayerArchive` admin.rs:630-642 (`player`, `archive{matches,remakes,wins,timelines,oldestGameEnd,newestGameEnd,byQueue[]}`, `jobs{kind→StateCounts}`, `latestJobs[]`) | Job kinds `archive:match`, `backfill:player` (admin.rs:644-647). |
| GET | `/v1/admin/players/{puuid}/archive/matches` | `player_archive_matches` admin.rs:764 | `?start` 0–1 000 000 default 0; `?count` 1–100 default 25; `?queue` 0–5000 (admin.rs:775-780) | none | `ArchivedPage` `{puuid,total,start,count,matches[ArchivedLine]}` admin.rs:744-751; `ArchivedLine` admin.rs:717-739 | |
| POST | `/v1/admin/players/names/backfill` | `queue_names_backfill` admin.rs:2277 | — | none | **202** `{"ok":true,"unnamed":n}` (admin.rs:2283-2286) | Queues `names:backfill`. |
| POST | `/v1/admin/backfill` | `queue_backfill` admin.rs:1583 | body `puuid`, `platform` **required**; `limit` 1–10 000 default 500; `fetchTimeline` bool default false (admin.rs:1584-1599) | none | **200** `{"ok":true,"jobId":…,"status":"queued"|"already-queued"}` (admin.rs:1613) | Reason `admin` (admin.rs:1610). |

Cache / stats / limits / debug:

| Method | Path | Handler | Params / body | Riot | Response | Notes |
|---|---|---|---|---|---|---|
| POST | `/v1/admin/cache/purge` | `purge_cache` admin.rs:838 | body `pattern` **required** 1–200 chars (Redis-style glob); `dryRun` bool default false (admin.rs:839-845) | none | `{"ok":true,"pattern":…,"deleted":n}` or dry run `{"ok":true,"pattern":…,"matched":n,"dryRun":true}` (admin.rs:864-866,882) | Pattern scoped to the current key unless it already starts with a scope (admin.rs:850); purges L1 and L2 (admin.rs:868-880). Archive never purged (admin.rs:832-833). |
| GET | `/v1/admin/stats` | `stats` admin.rs:969 | — | none | `AdminStats` admin.rs:890-924 (archive/player counts, archive bytes, `archiveBodies{matches,timelines}{local,packed,offloaded}`, pack stats, SQLite/WAL/disk readings, `sampledAt{fast,slow}`) | |
| GET | `/v1/admin/limits/{scope}` | `limits` admin.rs:1038 | `scope` ∈ platforms ∪ regions, else `VALIDATION` `params/scope must be equal to one of the allowed values` (`limiter_scope` validate.rs:81-88) | none | `Limits` `{scope,usage[{window:"limit:seconds",used,limit}],frozenMs}` admin.rs:1010-1027 | |
| GET | `/v1/admin/debug/riot` | `debug_riot` admin.rs:1184 | `?scope` **required** 2–12 chars, any case, platform or region; `?path` **required** 1–400 chars starting with `/`, query string allowed (`scope_and_path(bounded=true)` admin.rs:1137-1169; `debug_request` admin.rs:1075-1123); `?method` ∈ `ENDPOINTS` ids (admin.rs:1125-1135); `?noCache` bool (admin.rs:1188-1189) | Any Riot path on the named host; bucket/TTL/cache key from `method` or default `status.platformData` (admin.rs:1091); never cluster-moved (admin.rs:1113-1114) | `PassthroughResponses` via `respond()` (admin.rs:1193-1204) | Missing scope/path → `VALIDATION` `querystring must have required property '…'`; bad scope → `BAD_REGION` `'{scope}' is neither a platform nor a region` (admin.rs:1084-1086); `method` named but path mismatched → `VALIDATION` `path does not match {id}'s route {template}` (admin.rs:1096-1103). `noCache=true` → `bypass` → `X-Cache: BYPASS`. |
| GET | `/v1/admin/debug/cache` | `debug_cache` admin.rs:1232 | `?scope`, `?path` required (unbounded); `?method` **required** (admin.rs:1233-1237; `method_query(required=true)` admin.rs:1128-1132) | none | `CacheProbe` `{key,present,ageSeconds,stale}` admin.rs:1212-1220 | L1 only (admin.rs:1244-1248). |

Jobs:

| Method | Path | Handler | Params / body | Response | Notes |
|---|---|---|---|---|---|
| GET | `/v1/admin/jobs` | `list_jobs` admin.rs:1334 | `?state` ∈ `pending|running|done|failed` (`STATES` scheduler.rs:506); `?kind` ≤ 40 chars; `?limit` 1–500 default 50 (admin.rs:1335-1354) | `{jobs:[JobSummary]}` admin.rs:1305-1307; `JobSummary` `{id,kind,dedupeKey,priority,state,attempts,runAfter,claimedAt,finishedAt,error,payload}` admin.rs:1262-1284 | Newest first. |
| DELETE | `/v1/admin/jobs` | `purge_jobs` admin.rs:1504 | `?kind` **required** ∈ `kinds::ALL` (jobs/mod.rs:46); `?state` **required** ∈ `pending|failed` (`PURGEABLE_STATES` scheduler.rs:510); `?minPriority` ≥ 0 default 0; `?dryRun` bool (admin.rs:1506-1536) | `JobsPurged` `{kind,state,minPriority,matched,deleted,dryRun}` admin.rs:1479-1488 | Chunked deletes; running rows untouched (admin.rs:1493-1497). |
| GET | `/v1/admin/jobs/stats` | `job_stats` admin.rs:1401 | — | `JobStats` `{kinds{kind→StateCounts},totals}` admin.rs:1369-1393 | |
| GET | `/v1/admin/jobs/queue` | `job_queue` admin.rs:1936 | `?limit` 1–100 default 15 (`ACTIVITY_ROWS` admin.rs:1904,1937-1940) | `JobQueue` `{running[],next[],ready,delayed,nextDelayedAt,heldKinds[],held}` admin.rs:1908-1925 | |
| GET | `/v1/admin/jobs/activity` | `job_activity` admin.rs:1975 | `?limit` 1–200 default 50 (admin.rs:1976-1979) | `JobActivity` `{workers[],finished[]}` admin.rs:1958-1964 | In-memory; empty under `ROLE=api` (admin.rs:1959-1960). |
| GET | `/v1/admin/jobs/{id}/activity` | `job_trace` admin.rs:2009 | `id` ULID; `?after` ≥ 0 default 0 (admin.rs:2022-2025) | `JobTrace` `{job,trace}` admin.rs:1989-1997 | Both null → 404 `No such job` (admin.rs:2031-2033). |
| POST | `/v1/admin/jobs/{id}/retry` | `retry_job` admin.rs:1426 | `id` ULID | `JobSummary` | `job_error` (admin.rs:1309-1323): `NotFound` → 404 `No such job`; `WrongState` → `VALIDATION` `Job {id} is {state}`; `Duplicate` → `VALIDATION` `An identical job is already queued ({other})`. |
| DELETE | `/v1/admin/jobs/{id}` | `cancel_job` admin.rs:1455 | `id` ULID | `JobSummary` (state `failed`, error `cancelled`, admin.rs:1450-1451) | same `job_error` mapping. |
| POST | `/v1/admin/maintenance/run` | `run_maintenance` admin.rs:2662 | — | **202** `{"ok":true,"jobId":…,"status":"queued"|"already-queued"}` (admin.rs:2667-2670) | Dedupe key `maintenance` (admin.rs:2664-2665). |
| POST | `/v1/admin/ddragon/sync` | `queue_ddragon_sync` admin.rs:1634 | body `force` bool default false (admin.rs:1636-1639) | **200** `{"ok":true,"jobId":…}` (admin.rs:1652) | Dedupe `ddragon:sync` or `ddragon:sync:force` (admin.rs:1640-1650). |
| POST | `/v1/admin/analytics/recompute` | `recompute_analytics` admin.rs:2340 | body `platform` **required** (`platform_at`); `queue` ∈ `RANKED_QUEUES` default first of `LADDER_QUEUES` (admin.rs:2342-2353; `body_enum` admin.rs:1674-1679; `default_ladder_queue` admin.rs:1682-1689) | **202** `{"ok":true,"platform":…,"queue":…}` (admin.rs:2359-2362) | Queues `aggregate:analytics` ahead of the queue (admin.rs:2358). |
| POST | `/v1/admin/analytics/reextract` | `reextract_facts` admin.rs:2375 | — | **202** `{"ok":true,"stale":n}` (admin.rs:2381-2384) | Queues `facts:reextract`. |
| GET | `/v1/admin/timelines/backfill` | `timeline_backfill` admin.rs:2306 | `?patches` 1–1000, default `TIMELINE_BACKFILL_PATCHES` (admin.rs:2307-2311; default 0, config.rs:728) | `jobs::timelines::Progress` (admin.rs:2304) | Read-only. |

Ladder:

| Method | Path | Handler | Params / body | Riot | Response | Notes |
|---|---|---|---|---|---|---|
| POST | `/v1/admin/ladder/crawl` | `start_ladder_crawl` admin.rs:1700 | body `platform` **required**; `queue` ∈ `RANKED_QUEUES`; `tierFloor` ∈ `tiers()` (IRON…CHALLENGER, ladder.rs:29-31) (admin.rs:1703-1716) | none at request time | **202** `{"crawlId","status":"started"|"already-running","platform","queue","legs"}` (admin.rs:1723-1733) | `StartError::Invalid` → its own `ApiError` (admin.rs:1734). Not a 409 on duplicate (admin.rs:1727). |
| GET | `/v1/admin/ladder/options` | `ladder_options` admin.rs:1746 | — | none | `{platforms[{id,label}],queues[],tiers[],defaults{platform,queue,tierFloor,backfillLimit}}` (admin.rs:1747-1758) | |
| POST | `/v1/admin/ladder/probe` | `ladder_probe::ladder_probe` ladder_probe.rs:464 | body `platform` **required**; `queue` ∈ `RANKED_QUEUES` default first of `LADDER_QUEUES` (ladder_probe.rs:465-476) | `league.master`/`grandmaster`/`challenger` for the queue; `league.expEntries` `MASTER/I` pages 1..≤100 in batches of 10 (`MAX_EXP_PAGES`, `EXP_BATCH` ladder_probe.rs:42-44,387-421); `league.entriesByTier` `MASTER/I?page=1` (ladder_probe.rs:422-434). All with `bypass:true`, `Priority::Interactive` (`FRESH` ladder_probe.rs:351-355) | **200** `Report` `{cap,churnTolerance,summary,checks[{id,title,verdict,detail}],lists[],exp{…},pagedMaster{…}}` + `platform`, `queue`, `tookMs` (ladder_probe.rs:139-149,485-496) | Verdicts `confirmed|not-seen|changed|error` (ladder_probe.rs:121-128). ~55 Riot calls on a capped shard (ladder_probe.rs:458). Stores nothing. |
| GET | `/v1/admin/ladder/crawls` | `list_ladder_crawls` admin.rs:1831 | `?platform` (`query_platform`); `?queue` ∈ `RANKED_QUEUES`; `?limit` 1–100 default 20 (admin.rs:1832-1841) | none | `{crawls:[LadderCrawlSummary]}` admin.rs:1764-1795 | Scoped to current key (admin.rs:1848). |
| DELETE | `/v1/admin/ladder/crawls/{id}` | `cancel_ladder_crawl` admin.rs:1870 | `id` ULID | none | `{"ok":true,"crawlId":…,"status":"cancelled","droppedJobs":n}` (admin.rs:1887) | 404 `No such ladder crawl for the current key scope` (admin.rs:1889-1891); not running → `VALIDATION` `Crawl {id} is already {status}` (admin.rs:1892-1894). Publishes `crawl.phase` (admin.rs:1886). |
| GET | `/v1/admin/ladder/crawls/{id}` | `crawl_activity` admin.rs:2197 | `id` ULID | none | `CrawlActivity` `{crawl,asOf,stages[CrawlStage],openLegs[],running[],next[],ahead,failed[],downloads{…}}` admin.rs:2092-2110 | 404 `No such ladder crawl for the current key scope` (admin.rs:2223-2225). Pace window 10 min (admin.rs:1902). |

Process / dashboard:

| Method | Path | Handler | Params | Response | Notes |
|---|---|---|---|---|---|
| GET | `/v1/admin/metrics` | `metrics_snapshot` admin.rs:2399 | — | `stats::Snapshot` (admin.rs:2397) — the same document the `metrics` WS topic sends (admin.rs:2394) | Not recorded in the recent ring (recent.rs:121). |
| GET | `/v1/admin/metrics/history` | `metrics_history` admin.rs:2424 | — | `MetricsHistory` `{intervalS,maxPoints,points[]}` admin.rs:2409-2415 | `intervalS` = `METRICS_HISTORY_INTERVAL_S` (default 60, config.rs:722). |
| GET | `/v1/admin/info` | `info` admin.rs:2486 | — | `AdminInfo` `{version,gitSha,rustc,builtAt,startedAt,uptimeSeconds,env,role,keyScope,features{devUi,docsUi,dashboardUi,authDisabled,archiveTimelines,staleWhileRevalidate,trustProxy,postgres},workers,config{…redacted…}}` admin.rs:2438-2477 | `RIOT_API_KEY` never listed; secrets read `[REDACTED]` (admin.rs:2474-2476). |
| GET | `/v1/admin/recent` | `recent` admin.rs:2546 | `?kind` ∈ `requests|errors` default `requests`; `?limit` 1–500 default 100 (admin.rs:2548-2558) | `Recent` `{kind,capacity,requests?[RecentRequest],errors?[RecentError]}` admin.rs:2522-2533 | Not recorded in the ring itself (recent.rs:120). |
| GET | `/v1/admin/backups` | `list_backups` admin.rs:2616 | — | `Backups` `{dir,keep,files[{name,bytes,modifiedAt}],lastRun,diskFreeBytes}` admin.rs:2583-2606 | Metadata only (admin.rs:2624). |

### 1.9 `/v1/ws` — realtime socket (`src/routes/ws.rs`)

| Method | Path | Handler | Auth | Params | Response | Notes |
|---|---|---|---|---|---|---|
| GET (Upgrade) | `/v1/ws` | `ws::socket` ws.rs:532 (router ws.rs:497-499, merged unguarded docs.rs:138) | `socket`: any active key via `Authorization: Bearer` or `?token=` (`Auth::socket` auth.rs:246-260). Admin topics allowed iff admin scope **and** IP allowlist, decided once at handshake (auth.rs:258). `AUTH_DISABLED` → `dev-local` with admin (auth.rs:253-255). | `?token` (ws.rs:525) | 101 with `X-RateLimit-*` applied to the upgrade response (ws.rs:564-572). Bad/missing key: **the upgrade still completes**, then an error frame `{"op":"error","error":{"code":"UNAUTHORIZED","message":"Invalid key"}}` and close `4401 "unauthorized"` (ws.rs:542-555). Over quota: HTTP **429** envelope with `X-RateLimit-*` (ws.rs:556-563). | The handshake is metered like any request (ws.rs:6,556). Documented responses 101/429; security `bearerAuth` or `tokenQuery` (ws.rs:526-530). Protocol in §4. |

### 1.10 Docs: `/docs`, `/openapi.json`, `/openapi.yaml` (`docs::docs_router` docs.rs:374-387)

Mounted only when `DOCS_UI=true` (default true, config.rs:764; app.rs:84-86). No key. Not in the OpenAPI document.

| Method | Path | Handler | Response |
|---|---|---|---|
| GET | `/openapi.json` | closure docs.rs:378-381 | pretty JSON, `Content-Type: application/json` |
| GET | `/openapi.yaml` | closure docs.rs:382-385 | `Content-Type: application/yaml` |
| GET | `/docs` | `utoipa_scalar::Scalar::with_url("/docs", doc)` docs.rs:386 | Scalar HTML reference |

### 1.11 Dev / UI pages (`src/routes/ui.rs`, `src/routes/dev.rs`)

`DEV_UI` defaults true but is forced **off in production** (config.rs:664-666,762); `DASHBOARD_UI` default true (config.rs:765). None of these are in the OpenAPI document; none need a key except `/dev/reset`.

| Method | Path | Handler | Toggle | Auth | Response | Notes |
|---|---|---|---|---|---|---|
| GET | `/dev` | `ui::page` ui.rs:194 (page ui.rs:94-106) | `DEV_UI` | none | `text/html; charset=utf-8`, `Cache-Control: no-store` (ui.rs:31,100) | Rendered once at startup with the page bar (ui.rs:89-91). |
| GET | `/dev/showcase` | ui.rs:195-198 | `DEV_UI` | none | same | |
| GET | `/dev/config.json` | `json_route` ui.rs:199-202 (ui.rs:163-169) | `DEV_UI` | none | `DevConfig` `{authDisabled,version,env,docsUi,dashboardUi,regions[],platforms[{value,label,region}]}` ui.rs:146-161, `no-store` | |
| GET | `/dev/openapi.json` | ui.rs:203-215 | `DEV_UI` | none | the finished OpenAPI JSON, `no-store` | Served even when `DOCS_UI` is off (ui.rs:171-173). |
| GET | `/dev/jobs` | ui.rs:223-230 | `DEV_UI` **and** `DASHBOARD_UI` | none | **302** `Location: /dashboard#jobs` | ADM-17 redirect. |
| GET | `/dashboard` | `cached_page` ui.rs:218-221 (ui.rs:111-142) | `DASHBOARD_UI` | none (page is inert; everything it calls needs an admin key, ui.rs:5-8) | HTML with strong `ETag: "<sha256 hex[..32]>"`, `Cache-Control: no-cache`; `If-None-Match` match → **304** (ui.rs:113-131) | |
| GET | `/dev/reset` | `dev::preview` dev.rs:152 | `DEV_UI` (app.rs:77) | **admin** (`require_admin` route_layer dev.rs:247-250) | `Preview` `{tables[{name,rows}],l1Entries,runningJobs,kept[]}` dev.rs:94-102, `Cache-Control: no-store` (dev.rs:143-145) | |
| POST | `/dev/reset` | `dev::reset` dev.rs:185 | `DEV_UI` | **admin** | `Done` `{ok,tables[],l1Entries,stoppedJobs,runningJobs,tookMs}` dev.rs:106-117, `no-store` | Body must be `{"confirm":"reset"}` (`CONFIRM` dev.rs:36; 1–20 chars) else `VALIDATION` `body/confirm must be equal to 'reset'` (dev.rs:186-198). Halts workers (10 s `HALT_WAIT` dev.rs:33,202), clears L1, deletes `WIPED` tables (dev.rs:41-79), keeps `consumers`, `limiter_state`, `refinery_schema_history` (dev.rs:83), clears the analytics response cache (dev.rs:210). |

### 1.12 Fallback

| Case | Handler | Response |
|---|---|---|
| Unknown path, or known path with the wrong method | `app::not_found` app.rs:189-191 via `.fallback` + `.method_not_allowed_fallback` (app.rs:90-93) | **404** envelope `{"error":{"code":"NOT_FOUND","message":"No route for <METHOD> <uri>","requestId":…}}`. Metrics `route="unmatched"` (app.rs:131). |
| Unknown path under `/ddragon` | fill fallback statics.rs:72 | 404 empty body |
| Panic in a handler | `panic_response` app.rs:117,193-195 | **500** `INTERNAL` envelope |

---

## 2. Consumer key model

**Key format** (`src/consumers.rs`): `rpx_` prefix (`KEY_PREFIX` consumers.rs:12) + base64url-no-pad of 24 random bytes from `getrandom` = **36 characters** (`generate_key` consumers.rs:95-103; asserted consumers.rs:359-366). Only `sha256(key)` is stored in `consumers.key_sha256` (`hash_key` consumers.rs:105-107; insert consumers.rs:154-157); plaintext is returned once in `Created.key` as a `config::Secret` (consumers.rs:79-84,159-162). An imported key (`BOOTSTRAP_ADMIN_KEY`) must be ≥ 8 chars (consumers.rs:125-131). Consumer id is a ULID (consumers.rs:145). Duplicate name → `DuplicateName`; duplicate key hash → `Invalid("that key is already registered")` (consumers.rs:163-172).

**Scopes**: `read`, `admin`, serialised lowercase (consumers.rs:18-45); sorted and de-duplicated on insert (consumers.rs:141-143); stored as JSON text in `scopes` (consumers.rs:152-153). Create validation: non-empty name, ≥1 scope, quota ≥1 (consumers.rs:113-133). `DEFAULT_QUOTA_PER_MIN = 600` (consumers.rs:14).

**Bootstrap**: on first `serve`, when the table is empty, `bootstrap-admin` with `read+admin`, quota 600, generated or imported key; never again once any row exists (consumers.rs:308-333).

**Revocation**: soft delete sets `revoked_at`; the hash row is kept so a key can never be reissued (consumers.rs:228-251,256-283). Lookup for auth is `WHERE key_sha256 = ? AND revoked_at IS NULL` (`find_active_by_hash` consumers.rs:213-226).

**How the key is sent** (`bearer` auth.rs:141-156): `Authorization: Bearer <key>` — scheme matched case-insensitively, token trimmed, empty refused — else the first `token=` pair in the raw query string (non-empty). Both forms are accepted on **every** guarded HTTP route as well as the socket (`Ask::from` auth.rs:302-315 calls `bearer`). OpenAPI declares `bearerAuth` (HTTP bearer) and `tokenQuery` (apiKey in query `token`, "WebSocket handshake only") (docs.rs:86-113).

**Auth cache** (`Auth` auth.rs:172-241): `moka::future::Cache<[u8;32], Option<Arc<Consumer>>>`, max 10 000 entries (auth.rs:206-209), per-entry expiry 60 s for a found consumer (`AUTH_CACHE_TTL` auth.rs:30) and 30 s for an unknown key (`NEGATIVE_AUTH_TTL` auth.rs:32; `AuthTtl` auth.rs:160-170). `invalidate(hash)` drops one entry (auth.rs:263-265), used by consumer revoke and `revoke-cache` (admin.rs:264,321). DB failure during lookup → `INTERNAL` (auth.rs:225-230).

**Guard flow** (`guard` auth.rs:319-343; `require_read`/`require_admin` auth.rs:346-353):
1. `authorise` (auth.rs:267-290): `AUTH_DISABLED` → synthetic consumer; no token → **401** `UNAUTHORIZED` `Missing or invalid API key` (auth.rs:271; error.rs:106-108); unknown/revoked key → 401 same message, logged `rejected unknown key` (auth.rs:272-275); missing scope → **403** `FORBIDDEN` `This key lacks the '<read|admin>' scope` (auth.rs:276-281); admin from a non-allowlisted IP → **403** `Admin access is not permitted from this address` (auth.rs:282-288).
2. Records `consumer` on the tracing span (auth.rs:325).
3. Quota check (auth.rs:327-342): on success the `Arc<Consumer>` goes into request extensions (auth.rs:330) and `X-RateLimit-*` are applied to the response (auth.rs:332); on exhaustion the **429** envelope also carries `X-RateLimit-*` (auth.rs:337-341). 401/403 responses carry **no** `X-RateLimit-*` (returned before metering; mirrored in docs.rs:218).
4. `ServedTo(name)` response extension for the recent ring (auth.rs:333-334; recent.rs:159-162).

**`AUTH_DISABLED=true`** (config.rs:658): every guarded request and socket runs as `Consumer::dev_local()` — id/name `dev-local`, scopes `[read, admin]`, `quota_per_min` 100 000, never persisted (auth.rs:43-52,267-270,253-255); the IP allowlist is not consulted (auth.rs:268-270). Refused by config validation when `ENV=production` (`AUTH_DISABLED cannot be enabled when ENV=production`, config.rs:659-662). Logged as a warning at startup (auth.rs:201-205). Dev-local requests are still quota-metered (at 100 000/min).

**Per-consumer quota** (`src/http/quota.rs`): sliding log of `quota_per_min` admissions in any rolling **60 s** window (`WINDOW_SECONDS` quota.rs:320; reuses the limiter's `Window`, quota.rs:401-404), keyed by consumer **id** (quota.rs:397-399); `limit = quota_per_min.max(1)` and follows a changed quota immediately (quota.rs:396,408-409). Headers (`QuotaState::apply` quota.rs:332-336): `x-ratelimit-limit`, `x-ratelimit-remaining` (= limit − used), `x-ratelimit-reset` (seconds, ceil: until the oldest admission leaves on success; until the next free slot on refusal, quota.rs:412-425). Refusal: `QUOTA_EXCEEDED` 429, message `Quota of {limit}/min exceeded`, `retryAfter = reset_secs.max(1)` → also `Retry-After` header (quota.rs:429-431; error.rs:190-193). Independent of Riot's limits: over quota is never `RATE_LIMITED` (quota.rs:305-306). `Quotas::usage()` feeds the dashboard's `quotas` (quota.rs:339-391).

**Admin IP allowlist** (`ADMIN_IP_ALLOWLIST`): entries are single IPs or CIDRs (`AllowEntry::parse` auth.rs:59-77; bad entries logged and ignored auth.rs:192-200); IPv4-mapped IPv6 normalised to IPv4 (auth.rs:108-114); **empty list allows everyone**, non-empty list refuses an unknown/unparseable source (`ip_allowed` auth.rs:116-126). Applies to the admin guard (auth.rs:282) and the socket's admin-topic right (auth.rs:258).

**`TRUST_PROXY` / `X-Forwarded-For`** (`client_ip` auth.rs:128-139): default `false` (config.rs:758). When true, the **leftmost** `X-Forwarded-For` entry is used if it parses as an IP, else the TCP peer; when false the header is ignored entirely (ADR-068). Peer comes from `ConnectInfo<SocketAddr>` (or `MockConnectInfo` in tests) (auth.rs:302-309; ws.rs:539).

**CORS (ADM-07)** (`src/http/cors.rs`): only when `ADMIN_CORS_ORIGINS` lists ≥1 parseable origin (cors.rs:22-37; config.rs:759). `CorsLayer`: allow-origin list, methods `GET, POST, DELETE`, headers `Authorization, Content-Type`, exposed `x-request-id`, `max-age` 600 s (cors.rs:19,31-36). Scoped to `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz` only (`covers` cors.rs:41-43); every other path gets no CORS headers (cors.rs:6-7). Layered outside the auth guards and the 404 fallbacks so an `OPTIONS` preflight needs no key (app.rs:94-101; cors.rs:3-6).

---

## 3. Error envelope (`src/http/error.rs`)

**JSON shape** (`Envelope`/`Body` error.rs:158-172; serialised camelCase):

```json
{"error":{"code":"<CODE>","message":"<text>","requestId":"<id>","retryAfter":<seconds>}}
```

`requestId` is omitted when no request id is in scope (task-local `request_id::current()`, error.rs:133,187; request_id.rs:298-301); `retryAfter` is omitted when `None` (error.rs:168-171). Response: `(status, Json(envelope))` → `Content-Type: application/json` (error.rs:189); `Retry-After: <seconds>` header whenever `retry_after` is set (error.rs:190-193). 5xx logged at `error`, 4xx at `warn` (error.rs:176-181). Every `into_response` also pushes a `RecentError` onto the errors ring (error.rs:182-188).

**Codes** (`ErrorCode` error.rs:15-25; strings error.rs:41-53; default statuses error.rs:56-67):

| Code | Status | Raised by (cite) |
|---|---|---|
| `UNAUTHORIZED` | 401 | No token / unknown or revoked key: auth.rs:271-275 (`Missing or invalid API key`, error.rs:106-108). WS handshake sends the same code as a frame, not HTTP (ws.rs:545). |
| `FORBIDDEN` | 403 | Missing scope auth.rs:276-281; admin IP refused auth.rs:282-288. WS frame for admin topics socket.rs:165-170. |
| `QUOTA_EXCEEDED` | 429 | Consumer quota exhausted quota.rs:429-431 (`Quota of {n}/min exceeded`, with `retryAfter`). |
| `NOT_FOUND` | 404 | Router fallback app.rs:189-191 (`No route for {method} {uri}`); Riot 404 client.rs:175,303 (`Resource not found upstream`); negative-cache hit fetcher.rs:321-326 (`Resource not found upstream (negative-cached)`, with `X-Cache: HIT-NEG`); composite profile players.rs:358-360, 464-467; static data statics.rs:213, 289-292; admin: admin.rs:270, 312, 499-502, 594, 1312, 1889-1891, 2032, 2223-2225. |
| `UPSTREAM_ERROR` | 502 | Riot 401/403 → `Upstream authentication failed` (client.rs:176-178,304-307); Riot 5xx or unexpected status → `Upstream request failed` (client.rs:183,319-327; error.rs:119-121); non-JSON part body players.rs:136; unroutable active region players.rs:262; unparsable account on track admin.rs:568. |
| `RATE_LIMITED` | 503 | Riot answered 429: `Upstream rate limited`, `retryAfter` = Riot's `Retry-After` or 1 (client.rs:179-182,308-318). Proxy's own limiter had no token within the budget: `Upstream rate limit budget exceeded`, `retryAfter` = ceil secs until next token, min 1 (limiter/mod.rs:75-80; fetcher.rs:108-115; error.rs:114-117). |
| `BAD_REGION` | 400 | Any validation failure whose message contains `platform` or `region` (`invalid` validate.rs:21-31): `params/region`, `params/platform`, `querystring/platform`, `body/platform` enum failures (validate.rs:67-78,195-198); match-id/region mismatch lol.rs:57-60; debug scope admin.rs:1084-1086. |
| `VALIDATION` | 400 | Everything else from `validate.rs` (lengths, patterns, enums, int/bool query parsing, ULID ids, required `platform` query validate.rs:201-208); JSON body parsing body.rs:373-374,386-389 and field coercion/bounds body.rs:394-468; malformed path segments riot.rs:137-139 (`params <axum text>`); `type`+`champion` players.rs:649-653; `minTier`+`tier` analytics.rs:528-534; `championId` analytics.rs:564-572; debug route rules admin.rs:1076-1077,1096-1103,1116-1121,1128-1132,1138-1143,1147-1166; consumer create admin.rs:217-219; track body admin.rs:484-490; job state errors admin.rs:1313-1320; crawl not running admin.rs:1892-1894; dev reset confirm dev.rs:190-193. WS frames `Invalid JSON` / `Unknown op '…'` / `Unknown topic '…'` (protocol.rs:103,119-123; socket.rs:162). |
| `INTERNAL` | 500 | `ApiError::internal()` (`Internal server error`, error.rs:123-125): panics app.rs:193-195; consumer lookup DB failure auth.rs:227-230; store failures admin.rs:103-107, dev.rs:147-150; analytics read failure analytics.rs:627; champion pool read players.rs:1064-1067; CDN version list statics.rs:189-192; JSON serialisation failures admin.rs:87, players.rs:95; endpoint-table lookups that cannot fail in practice riot.rs:143-146. |

**Headers set on responses** (and the code that sets them):

| Header | When | Code |
|---|---|---|
| `X-Request-Id` | every response, including 404 fallback, panics and CORS preflights (outermost layer) | request_id.rs:324-339 |
| `X-RateLimit-Limit` / `-Remaining` / `-Reset` | every response of a guarded route after successful auth (200s, 4xx/5xx from the handler, and the 429); `/v1/ws` upgrade and its 429 | auth.rs:332,339; quota.rs:332-336; ws.rs:560,571 |
| `Retry-After` | any envelope with `retryAfter` (`QUOTA_EXCEEDED`, `RATE_LIMITED`) | error.rs:190-193 |
| `X-Cache` | passthrough 200 (passthrough.rs:440) and passthrough errors carrying a cache state (`HIT-NEG`, passthrough.rs:459-462); composites (players.rs:99); champion pool (players.rs:1056-1057,99); static JSON (statics.rs:164); analytics 200/304 (analytics.rs:611-614) | |
| `X-Cache-Age` | passthrough 200 (rounded seconds, passthrough.rs:441-446); composites (players.rs:100); static JSON always `0` (statics.rs:165); champion pool (players.rs:1058,100). Never on analytics. | |
| `X-Cache-Fetched-Age` | passthrough 200 when the fetcher has a fetch time — never from `ARCHIVE` (passthrough.rs:447-454; fetcher.rs:79-82); composites when any part has one (players.rs:101-103) | |
| `Cache-Control` | analytics `private, max-age=300` (analytics.rs:52,607); `/ddragon` 2xx `public, max-age=31536000, immutable` (statics.rs:26,76-82); `/dev*` pages and `/dev/reset` `no-store` (ui.rs:31,100,167,209; dev.rs:144); `/dashboard` `no-cache` (ui.rs:128) | |
| `ETag` | analytics (weak, analytics.rs:608-610); `/dashboard` (strong, ui.rs:127) | |
| `Content-Type` | proxy JSON routes `application/json; charset=utf-8` (passthrough.rs:401; players.rs:97; admin.rs:86; statics.rs:162; analytics.rs:604); `axum::Json` routes `application/json` (health, errors); `/metrics` `text/plain; version=0.0.4; charset=utf-8` (telemetry.rs:24); images `image/png` (statics.rs:150) | |
| `Content-Encoding` | negotiated by `CompressionLayer` (app.rs:106) | see §5 |

---

## 4. WebSocket protocol (`src/ws/protocol.rs`, `hub.rs`, `socket.rs`, `metrics.rs`)

- **URL**: `GET /v1/ws` with `Upgrade: websocket`. **Auth** at the handshake only: bearer header or `?token=` (auth.rs:246-260; ws.rs:502-503). A bad key still gets its upgrade, then `{"op":"error","error":{"code":"UNAUTHORIZED","message":"Invalid key"}}` and close code **4401** reason `unauthorized` (ws.rs:542-555). Quota: the handshake counts as one request; over quota is an HTTP 429 before upgrade (ws.rs:556-563).
- **First frame** from the server: `{"op":"ready","consumer":"<consumer name>"}` (socket.rs:63-68; protocol.rs:144).
- **Client frames** (`ClientFrame::parse` protocol.rs:98-125): `{"op":"subscribe","topics":[…]}`, `{"op":"unsubscribe","topics":[…]}`, `{"op":"ping"}`. Invalid JSON → `{"op":"error","error":{"code":"VALIDATION","message":"Invalid JSON"}}`; unknown/missing `op` → `…"message":"Unknown op '<String(op)>'"` (`undefined` when absent) (protocol.rs:103,119-123,127-137). Non-string topics and topics longer than 200 chars (`MAX_TOPIC_LEN` protocol.rs:29) are dropped silently (protocol.rs:104-114). Binary frames are read as UTF-8 text (socket.rs:84). Errors never close the socket (ws.rs:510).
- **Topics** (`Topic::parse` protocol.rs:47-53): `player:<puuid>` (puuid must pass `validate::puuid`), `patch`, and admin-only `metrics`, `ladder`, `firehose` (`is_admin` protocol.rs:68-71). Unknown topic → error `VALIDATION` `Unknown topic '<raw>'` (socket.rs:161-163); admin topic without admin right → error `FORBIDDEN` `Topic '<t>' requires the admin scope` (socket.rs:165-170); duplicates ignored (socket.rs:172-174); beyond **200 topics per socket** further topics are ignored (`MAX_TOPICS_PER_SOCKET` protocol.rs:27; socket.rs:175-178). Each subscribe/unsubscribe is answered by `{"op":"subscribed","topics":[…every topic the socket holds…]}` — a refused topic is visible by its absence (socket.rs:202-206).
- **Ping/pong (application)**: `{"op":"ping"}` → `{"op":"pong","at":<unix ms>}` (socket.rs:195-200; protocol.rs:147).
- **Event frames**: `{"op":"event","event":"<name>","topic":"<topic>","at":<unix ms>,"data":{…}}` (`Event::frame` events.rs:159-172; published through `events::publish` events.rs:191-206 → `Hub::publish` hub.rs:103-116). Serialised once per event and shared by all sockets (hub.rs:5-6). Event names (events.rs:127-137): `game.started`, `game.ended`, `rank.changed`, `match.archived` (player topics); `patch.new` (`patch`); `crawl.phase`, `ladder.crawl.completed`, `analytics.updated` (`ladder`); `metrics.snapshot` (`metrics`). `firehose` receives every event (hub.rs:110-114); a socket holding `firehose` skips its other topics so nothing arrives twice (socket.rs:105-109).
- **Backpressure**: one `tokio::broadcast` channel per topic, capacity **256** (`CAPACITY` hub.rs:20), created on first subscriber and dropped with the last (hub.rs:118-138). A socket that lags gets `{"op":"resync","topic":"<t>","dropped":<n>}` and carries on (socket.rs:110-116; protocol.rs:146). Order is kept within a topic, not across topics (socket.rs:9-11).
- **Heartbeat**: server sends a WebSocket **ping every 30 s** (`HEARTBEAT` protocol.rs:23; socket.rs:60,121-130); each ping increments `missed`, a pong resets it (socket.rs:85-88); at a tick with `missed >= 2` (`MAX_MISSED_PONGS` protocol.rs:25) the socket is dropped — i.e. two unanswered pings, closed on the third tick (~90 s of silence). Client pings are answered by axum automatically (socket.rs:89-90).
- **Metrics tick**: `ws::metrics::spawn` runs every `METRICS_INTERVAL_S` (default **5 s**, range 1–300, config.rs:721; spawned cli/serve.rs:350-352) and publishes `metrics.snapshot` (`stats::Stats::snapshot`) **only while at least one socket holds `metrics`** (ws/metrics.rs:223-237). Missed ticks are skipped (ws/metrics.rs:244).
- **Closes**: `4401 "key revoked"` when the consumer is revoked (`Hub::close_consumer` hub.rs:188-198; socket.rs:38-47; admin.rs:266); `1001 "server shutting down"` on graceful shutdown (`Hub::shutdown` hub.rs:200-205; socket.rs:41). Client close/error ends the task; held topics are released (socket.rs:91,134-137).
- **Metrics**: gauge `proxy_ws_connections` (hub.rs:78,180; metrics.rs:31); `events_published_total{name}` (events.rs:198); hub counters `subscriptions`, `event_counts` for the snapshot (hub.rs:157-165).

---

## 5. Request body limit, compression, panics, request id, recent ring

- **Body limit**: `DefaultBodyLimit::max(1_048_576)` (1 MiB, v1's Fastify `bodyLimit`) (app.rs:30-31,105). Handlers read bodies as `Bytes` (e.g. admin.rs:180). Exceeding it is answered by axum's extractor rejection — **not** the error envelope (see §7).
- **Compression**: `tower_http::compression::CompressionLayer::new()` with default settings (app.rs:106); crate built with `compression-full` (Cargo.toml:42), so gzip, deflate, brotli and zstd are negotiable via `Accept-Encoding`.
- **Panics**: `CatchPanicLayer::custom(panic_response)` (app.rs:117) → `ApiError::internal().into_response()` (app.rs:193-195): 500 `INTERNAL` envelope; it runs inside the `request_id` scope so `requestId` is populated.
- **Request id** (`src/http/request_id.rs`): outermost layer (app.rs:118). Honours an inbound `X-Request-Id` if non-empty, ≤ 128 chars and only `[A-Za-z0-9-_.:]` (`MAX_INBOUND_LEN` request_id.rs:287; `from_inbound` request_id.rs:304-311); otherwise generates a **ULID** (request_id.rs:313-315). Stored in request extensions, a `tracing` span `request{request_id}` and the task-local `CURRENT` (request_id.rs:292-301,330-333); echoed as `X-Request-Id` on every response (request_id.rs:336-338). The `http` span adds `method`, `path`, `consumer` (app.rs:109-113).
- **Tracing**: `TraceLayer` with `DefaultOnResponse` at `INFO` (app.rs:107-116); JSON or pretty format by `LOG_FORMAT` (telemetry.rs:39-66).
- **Recent-requests ring (ADM-05)** (`src/http/recent.rs`): two process-global bounded rings — **500 requests** (`REQUESTS_CAP` recent.rs:109) and **200 errors** (`ERRORS_CAP` recent.rs:111); paths clipped to **200 chars** and stored without query string (`PATH_MAX` recent.rs:113,195-198). Requests pushed by `record_request` after the response (app.rs:133-142,153-168) with `{at,method,route,path,status,cache,latencyMs,consumer,requestId}` (`RecentRequest` recent.rs:125-143); `consumer` from the `ServedTo` extension (auth.rs:334). Skipped paths: `/healthz`, `/readyz`, `/metrics`, `/v1/admin/recent`, `/v1/admin/metrics` (`SKIPPED` recent.rs:116-122). Errors pushed by every `ApiError::into_response` with `{at,code,status,message,requestId}` (`RecentError` recent.rs:146-157; error.rs:182-188) — including errors raised outside a request (`requestId: null`). Read by `GET /v1/admin/recent` (admin.rs:2546-2578). Nothing survives a restart (recent.rs:92-100).
- **`X-Cache` → metrics label** mapping: `HIT-NEG→neg`, `HIT→hit`, `MISS→miss`, `STALE→stale`, `ARCHIVE→archive`, `BYPASS→bypass`, else `none` (app.rs:144-152).

---

## 6. OpenAPI cross-check (`tests/snapshots/openapi__openapi_document.snap`)

Snapshot: OpenAPI 3.1.0, version 2.7.0 (snap lines 5325-5327), 70 `paths` entries (`grep -c '^    "/'`), security schemes `bearerAuth` + `tokenQuery` (snap 5304-5316), `x-tagGroups` Player data / Static data / Realtime / Operations / Administration (docs.rs:354-360). Declared response headers under `components.headers` (docs.rs:144-198,249-284); rules for which header each response declares in `headers_for` (docs.rs:207-245).

| Documented path (snap line) | Present in code? |
|---|---|
| `/ddragon/{version}/img/perk-images/{icon}` (5329), `/ddragon/{version}/img/{kind}/{file}` (5412) | yes — statics.rs:140, 107 (paths registered via docs.rs:79-80; axum template uses `{*icon}` statics.rs:71) |
| `/healthz` (5505), `/metrics` (5535), `/readyz` (5565) | yes |
| 36 `/v1/admin/*` paths (5613–11649): analytics/recompute, analytics/reextract, backfill, backups, cache/purge, consumers, consumers/{id}, consumers/{id}/revoke-cache, ddragon/sync, debug/cache, debug/riot, info, jobs, jobs/activity, jobs/queue, jobs/stats, jobs/{id}, jobs/{id}/activity, jobs/{id}/retry, ladder/crawl, ladder/crawls, ladder/crawls/{id}, ladder/options, ladder/probe, limits/{scope}, maintenance/run, metrics, metrics/history, players/names/backfill, players/{puuid}/archive, players/{puuid}/archive/matches, recent, stats, timelines/backfill, tracked-players, tracked-players/{puuid} | yes — all 40 operations in admin.rs:44-82 (four paths carry two methods: consumers GET+POST, jobs GET+DELETE, ladder/crawls/{id} GET+DELETE, tracked-players GET+POST) |
| 5 `/v1/lol/analytics/*` (11799–12871) | yes — analytics.rs:58-65 |
| 11 `/v1/lol/*` (13083–15392) | yes — lol.rs:30-43 |
| 4 `/v1/players/*` (15616–16349) | yes — players.rs:51-57 |
| 5 `/v1/riot/*` (16589–17485) | yes — riot.rs:127-134 |
| 3 `/v1/static/*` (17696–17986) | yes — statics.rs:55-60 |
| `/v1/ws` (18169) | yes — ws.rs:497-499 |

**Served but not documented** (by design): `/openapi.json`, `/openapi.yaml`, `/docs` (docs.rs:374-387); `/dev`, `/dev/showcase`, `/dev/config.json`, `/dev/openapi.json`, `/dev/jobs`, `/dashboard` (ui.rs:174-233); `/dev/reset` GET/POST (dev.rs:5-6,244-252); `/ddragon/<on-disk files>` via `ServeDir` (statics.rs:75); the 404 fallbacks (app.rs:90-93). No documented path is missing from the code and no API route in `api_router` is missing from the document (`api_router` is the single source for both, docs.rs:116-141; `tests/openapi.rs:12,60` asserts `/openapi.json` equals `spec()`).

---

## 7. TODO (unverified)

- `TODO (unverified):` the exact status/body axum emits when a request body exceeds `DefaultBodyLimit` (expected 413 with a plain-text body, outside the envelope) — not exercised in any test I read; `src/http/body.rs` parses only after extraction.
- `TODO (unverified):` whether `ServeDir` under `/ddragon` sends `Last-Modified` / honours `If-Modified-Since` for images written by the fill route on a *later* request (the OpenAPI text says 304 "for a copy already on disk", statics.rs:100; behaviour is tower-http's, not this crate's).
- `TODO (unverified):` the `Content-Type` of the `/healthz`/`/readyz` bodies — `axum::Json` sets `application/json` (no charset) whereas proxy routes send `application/json; charset=utf-8`; not asserted in a test I read.
- `TODO (unverified):` whether the 429 produced inside `/v1/ws` (ws.rs:556-563) is recorded with a `consumer` in the recent ring — the handler does not insert `ServedTo`, so `consumer` is probably `null` there.
- `TODO (unverified):` the precise shape of `stats::Snapshot`, `jobs::timelines::Progress`, `jobs::activity::{WorkerView,FinishedView,TraceView}` and `stats::HistoryPoint` bodies — referenced by schema only (admin.rs:2304,2397,1961-1963,1996,2414); files not read for this inventory.
- `TODO (unverified):` `/v1/riot/accounts/region/by-puuid/{puuid}` 200 body is untyped in the document (not in `RIOT_BODIES` docs.rs:288-321); confirm whether that is intentional before the API reference promises `AccountRegionDTO`.
- `TODO (unverified):` which encodings `CompressionLayer::new()` actually enables by default with `compression-full` (all four are compiled in; tower-http 0.7 defaults enable all built-in encoders, but this is the dependency's behaviour, not checked here).

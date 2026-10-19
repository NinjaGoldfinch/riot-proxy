# Riot client, limiter, fetch funnel and caches — code inventory

Evidence-only inventory of the outbound Riot side of riot-proxy v2 at branch
`docs/DOC-01-documentation-set`, HEAD `7491ff6`. Every claim cites `file:line`
(paths relative to the repository root). Where
`docs/design/04-data-and-cache.md` or `docs/design/05-rate-limiter.md` and the
code differ, a **Design vs code** note says so.

---

## 1. Riot client (`src/riot/client.rs`)

### 1.1 Base URL construction

| Aspect | Value | Evidence |
|---|---|---|
| Production host | `https://{target.host()}{path_and_query}`; `Target::host()` is `{platform}.api.riotgames.com` or `{region}.api.riotgames.com` | `src/riot/client.rs:249-254`, `src/riot/endpoints.rs:308-313`, `src/riot/routing.rs:123-125`, `src/riot/routing.rs:201-203` |
| Fixed-base override | `Base::Fixed(base)` → `format!("{base}{path_and_query}")`; `base` = `base_url.trim_end_matches('/')` | `src/riot/client.rs:198-202`, `src/riot/client.rs:225-231`, `src/riot/client.rs:252` |
| Override source | `RIOT_BASE_URL` (optional string); `serve` prefers the CLI option: `options.riot_base_url.as_ref().or(config.riot_base_url.as_ref())` → `RiotClient::with_base_url`, else `RiotClient::new` | `src/config.rs:88`, `src/config.rs:374`, `src/config.rs:675`, `src/cli/serve.rs:119-122`; also `src/cli/riot.rs:140`, `src/cli/record.rs:58` |
| Mock host header | With a fixed base the request also carries `x-riot-host: <real Riot host>` so one mock can keep per-host buckets (ADR-059) | `src/riot/client.rs:196`, `src/riot/client.rs:270-275` |
| Path templating | `{name}` segments filled in order; each value percent-encoded like JS `encodeURIComponent` (unreserved `A-Z a-z 0-9 - _ . ! ~ * ' ( )`); arity mismatch → `PathError::Arity` | `src/riot/endpoints.rs:362-382`, `src/riot/endpoints.rs:408-420` |
| Query encoding | `URLSearchParams.toString()` form: space → `+`, `* - . _` + alphanumerics kept, else `%XX`; `None`/empty values omitted; undeclared keys → `RequestError::UnknownQuery` | `src/riot/client.rs:87-127` |

### 1.2 Headers sent

| Header | Value | Evidence |
|---|---|---|
| `X-Riot-Token` | the configured key as a `HeaderValue` with `set_sensitive(true)` (redacted from `Debug`) | `src/riot/client.rs:26`, `src/riot/client.rs:234-236`, `src/riot/client.rs:268` |
| `Accept` | `application/json` | `src/riot/client.rs:269` |
| `User-Agent` | `config.riot_user_agent` via `Client::builder().user_agent(...)`; default `riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)` (`RIOT_USER_AGENT`) | `src/riot/client.rs:238`, `src/config.rs:99`, `src/config.rs:697` |
| `x-riot-host` | only with a fixed base | `src/riot/client.rs:270-275` |

The key never goes in the URL (`src/riot/client.rs:267`); reqwest errors carry only the URL (`src/riot/client.rs:347`).

### 1.3 reqwest client settings

| Setting | Value | Evidence |
|---|---|---|
| `pool_max_idle_per_host` | 32 | `src/riot/client.rs:20`, `src/riot/client.rs:239` |
| `pool_idle_timeout` | 60 s | `src/riot/client.rs:21`, `src/riot/client.rs:240` |
| `connect_timeout` | 10 s | `src/riot/client.rs:22`, `src/riot/client.rs:241` |
| `timeout` (whole request) | 25 s (comment: v1's 10 s headers + 15 s body) | `src/riot/client.rs:23-24`, `src/riot/client.rs:242` |
| Separate read timeout | none; only connect + total | `src/riot/client.rs:237-245` |
| TLS roots | `tls_certs_only(webpki_roots())`: embedded `webpki_root_certs::TLS_SERVER_ROOT_CERTS` only (ADR-007) | `src/riot/client.rs:243-244`, `src/riot/client.rs:370-375`, `Cargo.toml:49` |
| HTTP/2 | Cargo feature `http2` on; no `http2_prior_knowledge`/`http1_only` call, so reqwest's default negotiation | `Cargo.toml:32`, `src/riot/client.rs:237-245` |
| gzip | Cargo feature `gzip` on; body is "Riot's bytes, untouched (after transfer decoding)" | `Cargo.toml:32`, `src/riot/client.rs:129` |
| TLS backend | `rustls`, `default-features = false` | `Cargo.toml:32` |

### 1.4 Metrics recorded by the client

- `proxy_upstream_requests_total{region=<scope>, method=<endpoint id>, status=<code or "network">}` — `src/riot/client.rs:359-362`, `src/metrics.rs:26`.
- `proxy_upstream_latency_seconds{region, method}` — `src/riot/client.rs:362-363`, `src/metrics.rs:27`.
- `proxy_rl_429_total{region, type=<X-Rate-Limit-Type or "service">}` on every 429 — `src/riot/client.rs:312-313`, `src/metrics.rs:29`.

`RiotResponse.upstream_ms` is the round trip excluding limiter wait (`src/riot/client.rs:135-136`, `src/riot/client.rs:288-300`).

### 1.5 Upstream status → error policy

The client classifies and never retries (`src/riot/client.rs:1-3`). Retries, negative caching and stale fallback are the fetcher's (`src/fetcher.rs:9-14`). The table merges both layers.

| Upstream outcome | `RiotErrorKind` | `ApiError` seen by the proxy's client | Negative-cached? | Retried by fetcher? | Evidence |
|---|---|---|---|---|---|
| 2xx | — (`Ok(RiotResponse)`) | 200, Riot's bytes | n/a | no | `src/riot/client.rs:291-300` |
| 304 | `UnexpectedStatus` (the `_` arm: not 2xx/404/401/403/429/5xx) | `UPSTREAM_ERROR` (`ApiError::upstream()`) | no | no — returned at once | `src/riot/client.rs:323-327`, `src/riot/client.rs:183`, `src/fetcher.rs:541-543` |
| 400 | `UnexpectedStatus` | `UPSTREAM_ERROR` | no | no | as 304 |
| 401 / 403 | `UpstreamAuth`; `tracing::error!("RIOT KEY REJECTED — check RIOT_API_KEY ...")` | `UPSTREAM_ERROR` "Upstream authentication failed" (HTTP 502; never mentions the key) | no | no ("Never retry") | `src/riot/client.rs:304-307`, `src/riot/client.rs:176-178`, `src/riot/client.rs:474-480`, `src/fetcher.rs:541-543` |
| 404 | `NotFound` | `NOT_FOUND` "Resource not found upstream" | **yes** when the endpoint has a negative TTL (`NegTtl::Default` → `NEG_TTL_SECONDS`, `NegTtl::Account` → `NEG_TTL_ACCOUNT_SECONDS`, `NegTtl::None` → not cached) | no | `src/riot/client.rs:303`, `src/riot/client.rs:175`, `src/fetcher.rs:535-540`, `src/riot/endpoints.rs:28-37`, `src/riot/endpoints.rs:490-494` |
| 405 / 415 / other non-2xx | `UnexpectedStatus`; `tracing::warn!` with a 200-byte body snippet | `UPSTREAM_ERROR` | no | no | `src/riot/client.rs:323-327` |
| 429 with `X-Rate-Limit-Type` **and** numeric `Retry-After` (application / method / service) | `RateLimited{limit_type: Some, retry_after: Some}` | if the loop gives up: `RATE_LIMITED` "Upstream rate limited", `Retry-After` = header or 1 (HTTP 503) | no | `observe` freezes the scope for `Retry-After` s; the fetcher `continue`s and re-acquires (waits out the freeze inside the budget, or fails `RATE_LIMITED` from the limiter). Loop bounded by `MAX_ATTEMPTS = 8` | `src/riot/client.rs:308-318`, `src/riot/client.rs:179-182`, `src/riot/limiter/mod.rs:396-398`, `src/fetcher.rs:544-552`, `src/fetcher.rs:41`, `src/fetcher.rs:609` |
| 429 untyped, or typed without numeric `Retry-After` (the "service" path) | `RateLimited{..}` | `RATE_LIMITED`, `Retry-After` = header or 1 | no | account-picked requests first try every other cluster (`avoid`); then back off `500 ms × 2ⁿ` capped at 8 s (500, 1000, 2000 ms) for up to `SERVICE_429_TRIES = 3`, each ±20 % jitter; then error | `src/fetcher.rs:36-39`, `src/fetcher.rs:553-586`, `src/fetcher.rs:406-413`, `src/fetcher.rs:783-795` |
| 5xx | `UpstreamUnavailable`; `tracing::warn!("upstream 5xx")` | `UPSTREAM_ERROR` (502) — unless a cached copy inside its hard TTL exists, served as `STALE` | no | 2 retries after 250 ms then 750 ms (±20 % jitter), then stale copy or error | `src/riot/client.rs:319-322`, `src/fetcher.rs:34-35`, `src/fetcher.rs:587-607` |
| Network error / timeout (send or body read failed) | `UpstreamUnavailable`, `status: None`, empty headers; metric status `"network"` | as 5xx | no | as 5xx | `src/riot/client.rs:277-287`, `src/riot/client.rs:338-356` |
| Limiter budget exhausted (no Riot call) | `RateLimited` from `Limiter` | `RATE_LIMITED` "Upstream rate limit budget exceeded", `Retry-After` = ceil(seconds to `retry_at`), min 1 — unless a cached copy inside its hard TTL exists (served `STALE`) | n/a | no | `src/riot/limiter/mod.rs:58-80`, `src/fetcher.rs:460-466` |
| `MAX_ATTEMPTS` (8) exhausted | — | `UPSTREAM_ERROR` "Upstream request failed" | no | no | `src/fetcher.rs:40-41`, `src/fetcher.rs:609` |

Every response's headers, errors included, go to `Limiter::observe` before classification (`src/fetcher.rs:486-497`, `src/riot/client.rs:157-168`).

**Design vs code (05 §Observe step 3):** design places the untyped-429 backoff in `client.rs` as `250 ms × 2^attempt ± 25 %`, 3 attempts; the code puts it in the fetcher as `500 ms × 2ⁿ` capped at 8 s, ±20 %, 3 tries. Design 05's "As built" table already records this (`docs/design/05-rate-limiter.md:79`, `docs/design/05-rate-limiter.md:150`, `src/fetcher.rs:36-39`).

---

## 2. Endpoint catalogue (`src/riot/endpoints.rs`)

Registry constants: `STALE_MULTIPLIER = 4` (hard = soft × 4 when `STALE_WHILE_REVALIDATE=true`, else × 1) — `src/riot/endpoints.rs:13-14`, `src/riot/endpoints.rs:476-497`. `method_scope_key` is always the endpoint `id` (`src/riot/endpoints.rs:79`; test `src/riot/endpoints.rs:812-818`). `immutable = soft_ttl.is_none()` (`src/riot/endpoints.rs:83`). Soft TTLs are the registry defaults; `CACHE_TTL_OVERRIDES` (`key=seconds,...`) replaces the soft TTL of every endpoint sharing an `override_key`, hard follows (`src/riot/endpoints.rs:424-444`, `src/riot/endpoints.rs:476-497`). Negative TTL defaults: `NEG_TTL_SECONDS = 30`, `NEG_TTL_ACCOUNT_SECONDS = 300` (`src/config.rs:714-715`).

| id | Riot path template | Routing (`HostKind`) | method-scope key | soft TTL (s) | hard TTL (s, SWR on) | immutable / archived? | L2 (`persist_l2`) | override key | neg TTL | query params | `src/riot/endpoints.rs` lines |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `account.byRiotId` | `/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}` | Account (americas/asia/europe; sea → asia) | `account.byRiotId` | 86 400 | 345 600 | no | yes | `account` | Account (300) | — | 93-102 |
| `account.byPuuid` | `/riot/account/v1/accounts/by-puuid/{puuid}` | Account | `account.byPuuid` | 86 400 | 345 600 | no | yes | `account` | Account (300) | — | 103-112 |
| `summoner.byPuuid` | `/lol/summoner/v4/summoners/by-puuid/{puuid}` | Platform | `summoner.byPuuid` | 3 600 | 14 400 | no | yes | `summoner` | Default (30) | — | 113-122 |
| `league.entriesByPuuid` | `/lol/league/v4/entries/by-puuid/{puuid}` | Platform | `league.entriesByPuuid` | 300 | 1 200 | no | no | `league` | Default (30) | — | 123-132 |
| `league.challenger` | `/lol/league/v4/challengerleagues/by-queue/{queue}` | Platform | `league.challenger` | 120 | 480 | no | yes | `ladder` | None | — | 135-144 |
| `league.grandmaster` | `/lol/league/v4/grandmasterleagues/by-queue/{queue}` | Platform | `league.grandmaster` | 120 | 480 | no | yes | `ladder` | None | — | 145-154 |
| `league.master` | `/lol/league/v4/masterleagues/by-queue/{queue}` | Platform | `league.master` | 120 | 480 | no | yes | `ladder` | None | — | 155-164 |
| `league.entriesByTier` | `/lol/league/v4/entries/{queue}/{tier}/{division}` | Platform | `league.entriesByTier` | 120 | 480 | no | yes | `ladder` | None | `page` | 165-174 |
| `match.idsByPuuid` | `/lol/match/v5/matches/by-puuid/{puuid}/ids` | Regional | `match.idsByPuuid` | 120 | 480 | no | no | `matchIds` | Default (30) | `start`, `count`, `queue`, `type`, `startTime`, `endTime` | 175-184 |
| `match.byId` | `/lol/match/v5/matches/{matchId}` | Regional | `match.byId` | none | none | **immutable → archive (`matches`)** | no (archive instead) | `match` (ineffective) | Default (30) | — | 185-194 |
| `match.timeline` | `/lol/match/v5/matches/{matchId}/timeline` | Regional | `match.timeline` | none | none | **immutable → archive (`timelines`)** | no | `timeline` (ineffective) | Default (30) | — | 195-204 |
| `spectator.activeGame` | `/lol/spectator/v5/active-games/by-summoner/{puuid}` | Platform | `spectator.activeGame` | 30 | 120 | no | no | `spectator` | Default (30) | — | 205-214 |
| `mastery.byPuuid` | `/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}` | Platform | `mastery.byPuuid` | 3 600 | 14 400 | no | yes | `mastery` | Default (30) | — | 215-224 |
| `mastery.topByPuuid` | `/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}/top` | Platform | `mastery.topByPuuid` | 3 600 | 14 400 | no | yes | `mastery` | Default (30) | `count` | 225-234 |
| `platform.championRotations` | `/lol/platform/v3/champion-rotations` | Platform | `platform.championRotations` | 21 600 | 86 400 | no | **no** (design/04's table wins over its ≥ 1 h rule, ADR-016) | `rotations` | None | — | 235-246 |
| `status.platformData` | `/lol/status/v4/platform-data` | Platform | `status.platformData` | 60 | 240 | no | no | `status` | None | — | 247-256 |
| `league.expEntries` (v2, LAD-03) | `/lol/league-exp/v4/entries/{queue}/{tier}/{division}` | Platform | `league.expEntries` | 120 | 480 | no | no (only the apex probe calls it) | `ladder` | None | `page` | 257-270 |
| `account.regionByPuuid` (v2, SITE-09) | `/riot/account/v1/region/by-game/{game}/by-puuid/{puuid}` | Account | `account.regionByPuuid` | 86 400 | 345 600 | no | yes | `account` | Account (300) | — | 271-284 |

Registry order is v1's `METHOD_IDS` then `V2_ADDED = ["league.expEntries", "account.regionByPuuid"]` (`src/riot/endpoints.rs:91`, `:285-288`; parity test against `docs/contract/v1-endpoints.txt` at `:540-550`). TTL table test: `:552-578`; immutables test: `:598-606`; L2 membership test: `:639-657`; negative TTL test: `:660-677`; `ineffective_overrides()` (unknown keys or keys naming only immutable endpoints, logged at warn on boot): `:500-512`; `STALE_WHILE_REVALIDATE=false` → hard = soft: `:734-741`.

Ladder constants used by callers: `RANKED_QUEUES = [RANKED_SOLO_5x5, RANKED_FLEX_SR]`, `PAGED_TIERS` IRON..DIAMOND, `APEX_TIERS = [MASTER, GRANDMASTER, CHALLENGER]`, `DIVISIONS = [I..IV]`, `QUEUE_IDS = [(RANKED_SOLO_5x5,420),(RANKED_FLEX_SR,440)]`, `ENTRIES_PER_PAGE = 205`, `RIOT_APEX_LIST_CAP = 10_000` — `src/riot/ladder.rs:7-26`.

### 2.1 Data Dragon / static endpoints (not the Riot API: no key, no limiter)

| Use | URL | Client settings | Cadence / trigger | Written to disk | Evidence |
|---|---|---|---|---|---|
| Patch list | `{DDRAGON_BASE}/api/versions.json`, `DDRAGON_BASE = https://ddragon.leagueoflegends.com` | separate `reqwest::Client`: `user_agent = RIOT_USER_AGENT`, `connect_timeout = 15 s`, `timeout = 60 s`, same embedded webpki roots; no pool settings | every `ddragon:sync` run | `DDRAGON_DIR/<version>/versions.json`, written **last**; its presence marks the patch complete | `src/jobs/ddragon.rs:22-23`, `:27-29`, `:79-81`, `:108-121`, `:255-256`, `src/static/mod.rs:47-48` |
| Patch data files | `{DDRAGON_BASE}/cdn/<version>/data/<locale>/<file>.json` for `DATA_FILES = [champion, item, runesReforged, summoner, profileicon, map]`; locale = `DDRAGON_LOCALE` (default `en_US`) | as above; body checked to be JSON; a missing file is skipped with a warning | only when the newest version is not complete on disk, or `force` | `DDRAGON_DIR/<version>/<file>.json` via `write_atomic` (write `.tmp`, rename) | `src/jobs/ddragon.rs:83-85`, `:128-152`, `:239-254`, `:204-209`, `src/static/mod.rs:28-35`, `src/config.rs:756` |
| Queue table | `QUEUES_URL = https://static.developer.riotgames.com/docs/lol/queues.json` | as above | every run, before the version check; failure keeps what is on disk | `DDRAGON_DIR/meta/queues.json` | `src/jobs/ddragon.rs:24-25`, `:225-227`, `:267-285`, `src/static/mod.rs:50-54` |
| Images (on first request) | `{DDRAGON_BASE}/cdn/<version>/img/<kind>/<file>` for `IMAGE_KINDS = [(champion,champion),(profileicon,profileicon),(item,item),(spell,summoner)]`; body must start with the PNG magic and be ≤ `MAX_IMAGE = 1 MiB`; only names listed in that patch's data file (`data.*.image.full`) are fetched; file name must match `safe_name` | same client | on a `/ddragon/*` miss; one fetch per path at a time (`once`) | `DDRAGON_DIR/<version>/img/<kind>/<file>` | `src/jobs/ddragon.rs:87-91`, `:30-32`, `:155-173`, `src/static/images.rs:32-37`, `:74-81`, `:146-160`, `:174-198`, `:237-264` |
| Rune icons | `{DDRAGON_BASE}/cdn/img/<icon>` (unversioned upstream); `icon` must start with `perk-images/` and be listed in that patch's `runesReforged.json` | same | on first request | per patch at `DDRAGON_DIR/<version>/img/<icon>` | `src/jobs/ddragon.rs:93-97`, `src/static/images.rs:39`, `:102-115`, `:162-172` |
| Older-patch data file for images (SITE-07) | `{DDRAGON_BASE}/cdn/<version>/data/<locale>/<file>.json` for a version in Riot's list whose file is not on disk | same | once per `(version, file)`; a 404 is remembered in `Missing` and never asked again | `DDRAGON_DIR/<version>/<file>.json` (no `versions.json`, so never "mirrored") | `src/static/images.rs:46`, `:200-235`, `src/static/mod.rs:13-15` |

Sync cadence: the `ddragon:sync` tick runs every `DDRAGON_SYNC_S` (default 3600 s, min 60) and the first tick of every loop fires immediately at boot (`src/jobs/ticks.rs:1-4`, `src/jobs/ticks.rs:115`, `src/config.rs:726`). The job is a deduped singleton (`src/jobs/ticks.rs:76-80`); a failed sync returns `JobError::Retry` and a patch change publishes `patch.new` (`src/jobs/ddragon.rs:299-314`). `DDRAGON_BASE_URL` / the `--ddragon-urls` CLI option swap the CDN for a mock via `CdnUrls::mock` (`src/config.rs:89`, `src/cli/serve.rs:157-168`, `src/jobs/ddragon.rs:69-77`). Data Dragon "is not rate limited and never goes through the limiter" (`src/jobs/ddragon.rs:7`, `src/static/mod.rs:5-6`).

---

## 3. Routing (`src/riot/routing.rs`)

### 3.1 Platform → region table (as implemented)

`Platform::region()` `src/riot/routing.rs:108-116`; `account_region()` `:118-121` and `:174-181`; hosts `:123-125`, `:201-203`; labels `:86-106`, `:164-172`. Unit table: `:265-282`.

| platform | match-v5 region (`region()`) | account-v1 cluster (`account_region()`) | label |
|---|---|---|---|
| na1 | americas | americas | North America |
| br1 | americas | americas | Brazil |
| la1 | americas | americas | LATAM North |
| la2 | americas | americas | LATAM South |
| euw1 | europe | europe | EU West |
| eun1 | europe | europe | EU Nordic & East |
| tr1 | europe | europe | Türkiye |
| ru | europe | europe | Russia |
| kr | asia | asia | Korea |
| jp1 | asia | asia | Japan |
| oc1 | sea | asia | Oceania |
| ph2 | sea | asia | Philippines |
| sg2 | sea | asia | Singapore |
| th2 | sea | asia | Thailand |
| tw2 | sea | asia | Taiwan |
| vn2 | sea | asia | Vietnam |

Host string: `{value}.api.riotgames.com` for both families.

### 3.2 Region rules

- `Region::ALL = [americas, europe, asia, sea]` — `src/riot/routing.rs:146`.
- `Region::ACCOUNT = [americas, asia, europe]` (clusters that serve account-v1) — `:148-149`.
- `Region::account_region()`: `sea → asia`, others unchanged — `:174-181`.
- `Region::ACCOUNT_PICK = [asia, americas, europe]`, the order tried when the caller names no region (RC-02 / ADR-066) — `:151-153`.
- Cluster picking: `RiotRequest::account()` sets `target = Region(ACCOUNT_PICK[0])` and `pick_cluster = true` when `endpoint.host == HostKind::Account`; `targets()` yields all three in `ACCOUNT_PICK` order — `src/riot/client.rs:64-83`. `acquire_target` takes the first target with a token available *now* (zero-budget acquire), clusters that answered a service 429 this request (`avoid`) last; with none free it waits on the one with the soonest `retry_at`, inside the budget — `src/fetcher.rs:612-650`, `src/fetcher.rs:553-565`. Account cache keys use the literal host `account` so every cluster and the picker share one entry — `src/cache/keys.rs:72-79`.
- `Endpoint::target_for_platform`: Platform → `Target::Platform(p)`; Regional → `Target::Region(p.region())`; Account → `Target::Region(p.account_region())` — `src/riot/endpoints.rs:388-396`.
- `Endpoint::target_for_region`: Platform → `None`; Regional → `Target::Region(r)`; Account → `Target::Region(r.account_region())` — `src/riot/endpoints.rs:398-406`.
- `Target::scope()` (the limiter's app scope string) is the platform or region value — `src/riot/endpoints.rs:316-321`.
- Match-id routing: `Platform::from_match_id("EUW1_…")` parses the prefix before `_` case-insensitively; `Region::from_match_id` maps it through `region()`, falling back to `MATCH_ONLY_PREFIXES = [("ME1", Europe)]` (ME-01, ADR-143; checked live 2026-10-13) — `src/riot/routing.rs:132-143`, `:183-195`.

### 3.3 Validation of user-supplied region/platform strings

| Parser | Case | Error | Evidence |
|---|---|---|---|
| `Platform::parse` / `FromStr` (`to_ascii_lowercase`) | case-insensitive | `BAD_REGION` (400) "Unknown platform '{value}'. Expected one of: na1, br1, …" | `src/riot/routing.rs:127-130`, `:214-228` |
| `Region::parse` / `FromStr` | case-insensitive | `BAD_REGION` "Unknown region '{value}'. Expected one of: americas, europe, asia, sea" | `:205-207`, `:230-244` |
| `validate::region` / `validate::platform` / `platform_at` (route path params) | **case-sensitive** (ajv enum; `EUW1` refused) | `BAD_REGION` "params/region must be equal to one of the allowed values" | `src/http/validate.rs:53-78` |
| `validate::query_platform` (`?platform=`) | case-sensitive | `BAD_REGION` "querystring/platform must be equal to one of the allowed values" | `src/http/validate.rs:194-198` |
| `validate::required_query_platform` | — | `VALIDATION` "querystring must have required property 'platform'" when absent | `src/http/validate.rs:200-208` |
| `validate::limiter_scope` (`params/scope`) | case-sensitive; any platform **or** region string | `VALIDATION` "params/scope must be equal to one of the allowed values" (message lacks "platform"/"region", so not `BAD_REGION`) | `src/http/validate.rs:80-88`, `:21-31` |

`invalid()` picks `BAD_REGION` when the lower-cased message contains "platform" or "region", else `VALIDATION` (`src/http/validate.rs:21-31`). Other validators on Riot-request inputs: `gameName` 1–16 code points, `tagLine` 1–5, `puuid` 60–128 chars matching `^[A-Za-z0-9_-]+$`, `matchId` 6–40 chars matching `^[A-Za-z0-9]+_[0-9]+$` (`src/http/validate.rs:11-19`, `:113-166`). The `/v1/riot/accounts/...` routes with a `{region}` param accept `sea` and send it to `asia` (`src/routes/riot.rs:35-41`); the region-less variants use `RiotRequest::account` (`src/routes/riot.rs:43-52`).

---

## 4. Rate limiter (`src/riot/limiter/`)

### 4.1 Bucket model

- `Limiter { bulk_ceiling, idle_ceiling, idle_after, scopes: Mutex<HashMap<String, ScopeEntry>>, changed: Notify, bulk_waiting, interactive_waiting }`; the mutex is held only for check-and-take, never across an await — `src/riot/limiter/mod.rs:97-110`, `:246-251`.
- A **scope** is the routing value on the host; `ScopeEntry { app: ScopeState, app_known, methods: BTreeMap<String, ScopeState>, frozen_until, interactive_waiters, last_interactive }` — `src/riot/limiter/bucket.rs:165-181`.
- `Window { limit, seconds, admitted: VecDeque<Instant> }` is a **sliding log** (ADR-023): `try_take` admits only if `used(now) < limit` after pruning entries older than `seconds`; `rollback` pops the newest; `next_free` = when the `(used − limit)`-th oldest ages out; `until_at_most(max_used)`; `sync(count)` pads with `now`-stamped entries up to `count`, never lowers, never moves own stamps — `src/riot/limiter/bucket.rs:15-121`.
- `ScopeState::reconfigure(specs)`: windows matching by `seconds` keep admissions (limit updated), new lengths start empty, dropped lengths forgotten; identical specs no-op — `src/riot/limiter/bucket.rs:140-162`.
- **Bootstrap (conservative default before headers):** a new scope starts with `BOOTSTRAP_APP_LIMITS = [20:1, 100:120]` (dev-key limits), no method windows, `app_known = false` — `src/riot/limiter/headers.rs:31-42`, `src/riot/limiter/mod.rs:177-184`, `:307-310`. A method with no learned limits has no method windows, so only app windows gate it — `mod.rs:323-327`.

### 4.2 Header parsing (`src/riot/limiter/headers.rs`)

| Header | Parsed as | Rules | Evidence |
|---|---|---|---|
| `X-App-Rate-Limit` | `Vec<LimitWindow{limit,seconds}>` from `limit:seconds,…` | parts that are not two non-negative ints skipped; `limit > 0 && seconds > 0`; sorted by `seconds`; `None` when nothing valid; warn when parts dropped | `headers.rs:1-6`, `:10-15`, `:80-116` |
| `X-App-Rate-Limit-Count` | `Vec<CountWindow{count,seconds}>` | count may be 0; `seconds > 0` | `headers.rs:120-129` |
| `X-Method-Rate-Limit` / `-Count` | same | same | `headers.rs:144-154` |
| `X-Rate-Limit-Type` | `Application` / `Method` / `Service` (trimmed, lowercased); unknown → `None` + warn | | `headers.rs:45-66` |
| `Retry-After` | `u64` seconds; non-numeric (HTTP-date) → `None` | | `headers.rs:152`, test `:273-277` |

### 4.3 Observe (`Limiter::observe`, `src/riot/limiter/mod.rs:370-401`)

Called after **every** upstream response, errors included (`src/fetcher.rs:486-497`):

1. `app_limits` → `entry.app.reconfigure(limits)`, `app_known = true` (even if equal to bootstrap) — `mod.rs:377-381`.
2. `method_limits` → `methods[method].reconfigure(limits)` (creating the method state) — `mod.rs:382-388`.
3. `app_counts` → `sync_counts` pads each app window of matching `seconds` up to Riot's count — `mod.rs:389-391`, `:186-193`.
4. `method_counts` **and** the method already has state → sync; counts for an unknown method are ignored — `mod.rs:392-394`.
5. `limit_type` **and** numeric `retry_after` → `freeze(scope, retry_after s, kind)`; any typed 429 freezes, `service` included (ADR-024); an untyped 429 changes nothing — `mod.rs:396-398`.
6. `changed.notify_waiters()` — `mod.rs:399-400`.

### 4.4 Freeze (`Limiter::freeze`, `src/riot/limiter/mod.rs:407-431`)

- `frozen_until = max(existing, now + retry_after)` on the **whole scope** (every method); a longer freeze is kept — `mod.rs:408-415`.
- `Application`/`Method` → `tracing::error!("accountable 429: limiter accounting is out of sync; scope frozen")`; `Service` → `tracing::warn!` — `mod.rs:416-430`.
- Never counts a 429 metric (the client does) — `mod.rs:403-406`, `src/riot/client.rs:312-313`.
- `frozen_for(scope)` — `mod.rs:433-438`.
- The fetcher `continue`s on a typed 429 with `Retry-After`: the next `acquire` waits out the freeze if it fits the budget, else fails `RateLimited`; a picked account lookup moves cluster — `src/fetcher.rs:544-552`, `mod.rs:315-320`, `src/fetcher.rs:612-650`.

### 4.5 Acquire semantics (`Limiter::acquire`, `mod.rs:262-300`; `try_take_all`, `mod.rs:306-360`)

- `acquire(scope, method, priority, budget) -> Result<Permit{waited}, RateLimited{retry_at}>` — `mod.rs:262-268`, `:50-64`.
- `Permit` has no `Drop`: a token once taken is spent whether or not the request succeeds — `mod.rs:50-56`.
- Loop: enable `Notify`; `try_take_all(now)`; `Taken` → record `proxy_rl_wait_seconds{region, priority}` and return; `WaitUntil(until)`: if `until > deadline` (`started + budget`) fail at once with `RateLimited{retry_at: until}` (ADR-025); else register a `WaiterGuard` once and `select!` on `sleep_until(until)` / notify, then retry — `mod.rs:269-299`.
- `try_take_all` (one critical section):
  1. create the entry with bootstrap if absent — `mod.rs:307-310`;
  2. interactive → `last_interactive = now`, taken or not (THR-06a) — `:311-314`;
  3. `frozen_until > now` → `WaitUntil(frozen_until)`; an expired freeze is cleared — `:315-320`;
  4. windows = app ∪ method (if known) — `:323-327`;
  5. **bulk only:** `ceiling = ceiling_for(last_interactive, now)`; bulk needs `used < ceiling × limit` strictly in every window (`max_under_ceiling` = largest integer strictly below `limit × ceiling`; 8 of 10 at 0.80 blocks). Not clear → `WaitUntil(min(idle_at, ceiling_clear))`. Any `interactive_waiters > 0` → `WaitUntil(max(next_free, now + 1 ms))`, woken early when the waiter leaves — `:117-125`, `:329-344`;
  6. take from every window; if one refuses, roll back the taken ones and `WaitUntil(max next_free)` — `:346-359`.
- Waiter accounting: `WaiterGuard` increments `entry.interactive_waiters` and the atomics; `Drop` decrements and (interactive) `notify_waiters()`; a cancelled future never leaves a phantom waiter — `mod.rs:127-175`.
- Gauges `limiter_interactive_waiters`, `limiter_bulk_waiters` published on every change — `mod.rs:237-244`, `src/metrics.rs:48-49`.

**Budgets as wired** (`src/fetcher.rs:346-350`, `:40-44`, `src/cli/serve.rs:136-138`):

| Caller | priority | budget | Evidence |
|---|---|---|---|
| route (`FetchOptions::default()`) | Interactive | `CLIENT_WAIT_BUDGET_MS` (default 2000 ms; range 0..u64::MAX) | `src/fetcher.rs:142-150`, `src/config.rs:716` |
| job (`FetchOptions::JOB`) | Bulk | `JOB_YIELD_BUDGET_MS` (default 1000 ms; range 0..900 000) — then fails with `limited_until` set so the scheduler `Yield`s the worker until `retry_at` (SCH-01) | `src/fetcher.rs:124-140`, `src/config.rs:711`, `src/fetcher.rs:86-115`, `src/jobs/scheduler.rs:123-142` |
| background SWR refresh, or non-job bulk | Bulk | `BULK_BUDGET = 15 min` | `src/fetcher.rs:42-44`, `:376-390` |

**Ceilings** (`Limiter::new(bulk_ceiling).with_idle_ceiling(ceiling, after)`, `mod.rs:203-235`; wired `src/cli/serve.rs:71-74`):

| Config | Default | Range / check | Meaning | Evidence |
|---|---|---|---|---|
| `BULK_USAGE_CEILING` | 0.8 | 0.0–1.0 | bulk's ceiling while a scope is busy | `src/config.rs:717` |
| `BULK_IDLE_CEILING` | 0.95 (`DEFAULT_BULK_IDLE_CEILING`) | 0.0–1.0; must be `< 1` and `>= BULK_USAGE_CEILING` | bulk's ceiling with no interactive acquire in the last `BULK_IDLE_AFTER_S` | `src/config.rs:104`, `:718`, `:777-781` |
| `BULK_IDLE_AFTER_S` | 30 | 0–86 400 | busy window after the last interactive acquire | `src/config.rs:719` |

`ceiling_for`: `last_interactive + idle_after > now` → `(bulk_ceiling, Some(idle_at))`, else `(idle_ceiling, None)`; `idle_at` doubles as a wake time — `mod.rs:229-235`, `:335-338`; tests `src/riot/limiter/tests.rs:919-1005`.

`bulk_blocked()` (SCH-01, ADR-089): per scope `(scope, until)` when frozen, any app window at/over bulk's ceiling, or interactive waiters queued (re-check after `INTERACTIVE_RECHECK = 100 ms`); for unblocked scopes `(scope, method, until)` for methods at the ceiling; the job claim skips that work — `mod.rs:520-594`.

### 4.6 Request/bucket state machine (for a Mermaid stateDiagram)

**Per-scope bucket** (data: `ScopeEntry`, `src/riot/limiter/bucket.rs:165-181`):

- `Bootstrap` — app windows `[20:1, 100:120]`, `app_known=false`, no methods. Entered on the first `acquire`/`observe`/`freeze`/`configure_*` for the scope (`mod.rs:307-310`, `:374-376`, `:410-413`) or by a restored checkpoint (`persist.rs:153-169`).
  - → `Known`: `observe` with `X-App-Rate-Limit` (`mod.rs:377-381`), `configure_app` (`:442-449`), or a restored `app:` row with `known=true` (`persist.rs:157-158`). A scope with any method state also counts as known (`mod.rs:500-509`; method state from `observe` with `X-Method-Rate-Limit` `:382-388` or `configure_method` `:452-462`).
- `Known` — limits learned; windows re-shaped on differing headers (`bucket.rs:143-162`); counts padded upward on `-Count` (`bucket.rs:100-105`).
- `Frozen` (orthogonal flag `frozen_until`, on Bootstrap or Known) — entered by `freeze()` (typed 429 + numeric Retry-After; or a restored future `frozen_until`, `persist.rs:159-162`); extended only upward; exits when `now >= frozen_until` at the next `try_take_all` (`mod.rs:315-320`); never cleared early.
- `Busy` / `Idle` (orthogonal bulk-ceiling sub-state): `Busy` for `idle_after` after any interactive `try_take_all` call; `Idle` otherwise (`mod.rs:311-314`, `:229-235`).
- No removal: scopes are never evicted from the map (no such path in `mod.rs`).

**Per-acquire** (`mod.rs:262-300`):

1. `Trying` → `try_take_all`.
2. `Taken` (terminal, `Ok(Permit)`): every window in app ∪ method admitted.
3. `Waiting(until)`: `WaitUntil(until)` with `until <= deadline`; `WaiterGuard` registered on the first wait (interactive: increments the scope's `interactive_waiters`, so bulk stands aside). Wakes on `sleep_until(until)` **or** `changed` (new limits, freeze lifting, an interactive waiter leaving) → `Trying`.
4. `Rejected` (terminal, `Err(RateLimited{retry_at: until})`): `until > deadline`, checked before registering and on every later iteration.
5. `Cancelled` (future dropped) → `WaiterGuard::drop` withdraws the waiter (`mod.rs:158-175`).

`WaitUntil` reasons in evaluation order (`mod.rs:315-359`): frozen → bulk over ceiling (`min(idle_at, ceiling_clear)`) → bulk held by interactive waiters (`max(next_free, now+1 ms)`) → a window full (`max next_free` after rollback).

### 4.7 Checkpoint / restore (`src/riot/limiter/persist.rs`)

| Aspect | Value | Evidence |
|---|---|---|
| Table | `limiter_state(scope TEXT PK, windows TEXT, frozen_until INTEGER, updated_at INTEGER)` | `persist.rs:178-194`, `:196-213`, `docs/design/04-data-and-cache.md:341-346` |
| Row keys | `app:{scope}`, `method:{scope}:{method}` | `persist.rs:3-4`, `:116-118`, `:129-131` |
| `windows` JSON | `{known?: bool, windows: [{limit, seconds, stamps: [[unix_ms, n], …]}]}`; `known` only on app rows | `persist.rs:37-51`, `:72-78` |
| Stamp quantisation | bucket = `max(100 ms, seconds ms)`, rounded **up**, so a restored stamp never expires earlier than the original; ≈ ≤ 1 000 buckets per window | `persist.rs:5-7`, `:53-70` |
| Cadence | `CHECKPOINT_EVERY = 10 s` (tokio interval, `MissedTickBehavior::Delay`) plus `checkpoint_now` on shutdown; save = `DELETE FROM limiter_state` then `INSERT` every row, one transaction; errors logged, not returned | `persist.rs:24-25`, `:231-242`, `:177-194`, `:215-222`, `src/cli/serve.rs:82`, `:388-390` |
| Restore on boot | `restore_from` loads all rows; `STALE_AFTER = 120 s`: a row older than that restores every window **full**, stamped at boot (closed for one window length); otherwise stamps are re-expanded and pruned; expired `frozen_until` dropped; unreadable/unknown rows skipped with a warning | `persist.rs:21-23`, `:80-105`, `:141-175`, `:224-229` |
| `/readyz` "limiter restored" | `limiter_restored: AtomicBool` is set `true` after `restore_from` returns, **success or failure** (a failure logs "limiter checkpoint could not be read; starting fresh" and boot continues); `/readyz` returns `ok = sqlite && limiter`, 503 otherwise, same JSON body (`ok`, `sqlite`, `limiter`, `key_scope`, `sqlite_readers`, `sqlite_readers_free`) | `src/cli/serve.rs:75-81`, `:368`, `src/app.rs:49`, `src/routes/health.rs:68-115` |

**Design vs code (05 §Persistence, 04 schema):** design describes `[{limit, seconds, count, reset_at}]`; code stores sliding-log stamps (design 05 "As built" rows `docs/design/05-rate-limiter.md:154-155` record this). Design 04's `limiter_state.windows` comment still reads `JSON [{limit, seconds, count, reset_at}]` (`docs/design/04-data-and-cache.md:343`), stale against `persist.rs:37-51`.

### 4.8 Metrics emitted by the limiter / fetcher

| Metric | Type | Labels | Where |
|---|---|---|---|
| `proxy_rl_wait_seconds` | histogram | `region`, `priority` | `src/riot/limiter/mod.rs:283-284`, `src/metrics.rs:28` |
| `limiter_interactive_waiters` | gauge | — | `mod.rs:240-241`, `src/metrics.rs:48` |
| `limiter_bulk_waiters` | gauge | — | `mod.rs:242`, `src/metrics.rs:49` |
| `proxy_rl_429_total` | counter | `region`, `type` | `src/riot/client.rs:312-313` |
| `proxy_cache_reads_total` | counter | `state` ∈ `hit`, `neg`, `stale`, `miss` | `src/fetcher.rs:401-403`, `:310`, `:322`, `:329`, `:333`, `:344` |
| `cache_l2_dropped_total` | counter | — | `src/cache/l2.rs:109`, `src/metrics.rs:59` |

### 4.9 Other design-vs-code notes for 05

- Design 05 §Acquire: bulk's budget "effectively unbounded"; code: `JOB_YIELD_BUDGET_MS` (1 s) for jobs, 15 min for other bulk (`docs/design/05-rate-limiter.md:67`, `src/fetcher.rs:42-44`, `:346-350`). Design 05 §Priorities (`:87`) does describe the SCH-01 yield.
- Design 05 §Observe: only `application|method` freeze; code freezes on any typed 429 with numeric Retry-After (`docs/design/05-rate-limiter.md:78`, `src/riot/limiter/mod.rs:396-398`; "As built" `:149`).
- Design 05 "two Notify-based queues"; code has one `Notify` (`changed`) plus per-scope `interactive_waiters` and two atomics (`docs/design/05-rate-limiter.md:83`, `src/riot/limiter/mod.rs:104-109`).

---

## 5. Fetcher funnel (`src/fetcher.rs`)

### 5.1 Order of checks in `fetch_inner` (`src/fetcher.rs:301-367`)

1. `key = cache_key(scope, req)` — `:302`.
2. **Unless `bypass`** (`:305`):
   1. **Archive**: `endpoint.immutable` and `archive.get(req)` returns a body → `X-Cache: ARCHIVE`, `cache_age = 0`, `fetched_age = None`; metric `hit` — `:306-317`.
   2. **L1 lookup** (`ResponseCache::get` → `L1::get`):
      - `Fresh` **negative** → `Err` `NOT_FOUND` "Resource not found upstream (negative-cached)" with `x_cache = HIT-NEG`; metric `neg` — `:321-327`.
      - `Fresh` positive → `X-Cache: HIT`; metric `hit` — `:328-331`.
      - `Stale` → serve `X-Cache: STALE`; if `swr` (`STALE_WHILE_REVALIDATE`, default true) spawn `refresh_in_background` (bulk, 15 min budget, via single-flight); metric `stale` — `:332-338`, `:376-390`, `src/config.rs:720`.
      - `Miss` → fall through — `:339`.
3. **Miss or bypass**: metric `miss`; budget by caller (job → `job_budget`; interactive → `interactive_budget`; bulk → `BULK_BUDGET`); **single-flight** on `key` so concurrent misses share one `upstream` task — `:343-359`.
4. `X-Cache`: `stale=true` (upstream failed, cached copy served) → `STALE`; `bypass` → `BYPASS`; else `MISS` — `:361-365`.

Inside `upstream` (`:430-610`), per attempt (≤ 8, `:444`):
5. **Limiter**: `acquire_target` (one target → plain `acquire`; account picker → first free cluster, avoided last); on `RateLimited`, serve a cached copy inside its hard TTL as `STALE` if any, else `Err` with `limited_until` — `:449-466`, `:415-428`.
6. **Client** `send` — `:486-495`.
7. **Observe** headers, success or error — `:497`.
8. **Store** on 2xx: immutable → `archive.put`; else `archive.observe` (league entries → `player_ranks`); then `cache.put(key, endpoint, body, ttls)` (L1; L2 enqueue if `persist_l2`); `content_at` from the entry (kept when bytes unchanged), else the fetch time — `:511-533`, `src/archive/mod.rs:146-178`, `:180-200`, `src/cache/mod.rs:39-49`.
9. Errors: §1.5 — `:535-607`.

**Design vs code (fetcher doc):** the funnel is "archive → L1 (HIT / HIT-NEG / STALE + bulk refresh) → single-flight → limiter → client → observe → cache write → archive write" (`src/fetcher.rs:4-7`); there is no separate negative-cache stage — a cached 404 is a `Fresh` L1 entry with `status = 404` (`src/cache/l1.rs:39-42`, `src/cache/keys.rs:13-14`, `docs/design/04-data-and-cache.md:415`).

### 5.2 `X-Cache` values

| Value | When | Evidence |
|---|---|---|
| `HIT` | fresh positive L1 entry | `src/fetcher.rs:328-331` |
| `HIT-NEG` | fresh negative (404) L1 entry; sent on the 404 error response | `:321-327`, `src/routes/passthrough.rs:148-155` |
| `STALE` | entry between soft and hard TTL (background refresh if SWR), **or** upstream/limiter failure answered from a copy inside its hard TTL | `:332-338`, `:362`, `:460-466`, `:600-606` |
| `MISS` | fetched from Riot | `:364` |
| `ARCHIVE` | immutable body from the archive | `:306-317` |
| `BYPASS` | `?refresh=true` (or `=1`) by a consumer with the `admin` scope: cache reads skipped, writes still happen | `src/routes/passthrough.rs:159-167`, `src/fetcher.rs:363`, `:126-127` |

Response headers: `X-Cache`, `X-Cache-Age` (content age, `(ms+500)/1000`), `X-Cache-Fetched-Age` (omitted for `ARCHIVE`) — `src/routes/passthrough.rs:126-157`. For non-admin consumers `?refresh` is ignored (`:159-166`).

### 5.3 Stale-while-revalidate semantics

- `CacheEntry{status, body, content_at, fetched_at, soft_expires, hard_expires}`; `L1::get`: `now < soft_expires` → `Fresh`; `now < hard_expires` → `Stale`; else invalidate and `Miss` — `src/cache/l1.rs:21-37`, `:121-134`.
- Hard TTL = soft × 4 (SWR on) or = soft (off) — `src/riot/endpoints.rs:476-497`.
- Background refresh: `upstream` with `Priority::Bulk`, `BULK_BUDGET` (15 min), through the same single-flight map; failures logged at debug — `src/fetcher.rs:376-390`.
- A byte-identical refresh keeps `content_at` (so `X-Cache-Age` is content age) but restarts `fetched_at`, `soft_expires`, `hard_expires` — `src/cache/l1.rs:161-176`; test `:282-313`.
- Negative entries: `soft = hard = ttl`, never `Stale` — `src/cache/l1.rs:143-146`.
- `cached_copy` (fallback) accepts `Fresh` or `Stale` non-negative entries — `src/fetcher.rs:415-428`.

### 5.4 Negative cache TTLs

- `NEG_TTL_SECONDS` default 30 (min 1); `NEG_TTL_ACCOUNT_SECONDS` default 300 (min 1) — `src/config.rs:714-715`.
- Per-endpoint `NegTtl` (§2): account ×3 → account TTL; summoner, league.entriesByPuuid, match.idsByPuuid, match.byId, match.timeline, spectator, mastery ×2 → default; ladder ×4, rotations, status, league.expEntries → none — `src/riot/endpoints.rs:93-284`; test `:660-677`.
- Negative entries are written to L2 too when the endpoint has `persist_l2` (`put_negative` → `persist`) — `src/cache/mod.rs:52-64`.

**Design vs code (04 §Cache tiers):** the design table says the Negative tier does "not survive restart" and lists only "spectator 404 (30 s), account 404 (300 s)" (`docs/design/04-data-and-cache.md:379`); the code negative-caches every `NegTtl::Default`/`Account` endpoint and persists negatives for L2 endpoints (account, summoner, mastery), which `warm` reloads (`src/cache/mod.rs:52-64`, `src/cache/l2.rs:186-216`).

### 5.5 Cache key scheme (`src/cache/keys.rs`)

- `KeyScope` = first 8 hex chars of `sha256(RIOT_API_KEY)` — `keys.rs:31-37`; design `docs/design/04-data-and-cache.md:417-421`.
- `cache_key` = `{key_scope}:{endpoint.id}:{host}[:{param}]…[:{query hash}]`, `host` = `req.target.host()` **or the literal `account`** for `HostKind::Account`; params are the percent-encoded path values (a `:` cannot collide); query = `canonical_query` (empty values dropped, pairs sorted, `encodeURIComponent` both sides, `k=v&…`) hashed to 16 hex chars of sha256 — `keys.rs:72-90`, `:52-64`, `:67-69`; design `docs/design/04-data-and-cache.md:409-415`.
- Examples (tests): `{scope}:summoner.byPuuid:euw1.api.riotgames.com:PUUID-1`; `{scope}:match.idsByPuuid:europe.api.riotgames.com:P:{16 hex}`; `…:account.byPuuid:account:P` — `keys.rs:269-284`, `:322-338`.
- `derived_key(scope, part, target)` = `{scope}:derived:{part}:{short_hash(target)}` — `keys.rs:96-98`.
- `scoped_purge_pattern` prefixes admin purge globs with the scope unless already scoped (`^[0-9a-f]{8}:`); `glob_match` implements Redis `MATCH` globs — `keys.rs:103-166`.

### 5.6 L1 (`src/cache/l1.rs`)

| Aspect | Value | Evidence |
|---|---|---|
| Store | `moka::future::Cache<String, Arc<CacheEntry>>` | `l1.rs:88-92`, `Cargo.toml:30` |
| Capacity | `max_capacity = CACHE_L1_MAX_MB × 1024 × 1024` bytes (default 128 MB; range 1–1 048 576) | `l1.rs:106-119`, `src/config.rs:713` |
| Weigher | `key.len() + body.len() + ENTRY_OVERHEAD (128)` | `l1.rs:18-19`, `:102-104` |
| Eviction | moka weight-bounded, default policy (no `eviction_policy` call); an entry larger than the budget is not kept | `l1.rs:109-113`; test `:358-366` |
| Expiry | `Expiry` impl `HardTtl`: `hard_expires − now` at create/update (memory reclaim only; freshness is decided from the entry's own instants on tokio time) | `l1.rs:4-7`, `:64-86` |
| Immutable endpoints | `put` returns `None` when `ttls.soft`/`hard` is `None` | `l1.rs:136-141` |
| Admin purge | `invalidate_where(pred)`, `keys_where(pred)` (dry run) | `l1.rs:183-204` |

### 5.7 L2 (`src/cache/l2.rs`)

| Aspect | Value | Evidence |
|---|---|---|
| Table | `cache(key TEXT PK, body BLOB, status INTEGER, content_at, fetched_at, soft_expires, hard_expires INTEGER)`, index `cache_hard(hard_expires)` | `l2.rs:159-183`, `docs/design/04-data-and-cache.md:330-339` |
| Membership | `Endpoint::persist_l2` (account ×3, summoner, ladder ×4, mastery ×2) | `src/cache/mod.rs:58-64`, `src/riot/endpoints.rs:639-657` |
| Write-behind | bounded `mpsc` of `QUEUE = 10_000` rows; the writer flushes when `FLUSH_BATCH = 500` rows are pending or `FLUSH_EVERY = 2 s` after a batch's first row, whichever first; one `INSERT OR REPLACE` transaction per batch; a full queue drops the row (`cache_l2_dropped_total`, warn); the request path never waits | `l2.rs:1-9`, `:24-28`, `:102-112`, `:126-149`, `:159-183` |
| Boot warm | `warm`: `SELECT … FROM cache WHERE hard_expires > now` → `L1::insert_entry` per row (`COALESCE(fetched_at, content_at)` for pre-V0009 rows) | `l2.rs:186-216`, `src/cli/serve.rs:85-89` |
| Sweep | `DELETE FROM cache WHERE hard_expires <= now` at boot after warm, and in the daily `maintenance` job | `l2.rs:219-223`, `src/cli/serve.rs:90-92`, `src/jobs/maintenance.rs:251` |
| Shutdown | sender dropped, writer awaited (flushes the rest) | `l2.rs:115-123`, `src/cli/serve.rs:390` |
| Purge | `delete_where(pred)` / `keys_where(pred)` (full key scan) | `l2.rs:227-261` |

Design 04 describes the same 2 s / 500 batching (`docs/design/04-data-and-cache.md:386`) and boot warm + sweep (`:390`).

### 5.8 Single-flight (`src/singleflight.rs`)

- `DashMap<key, Shared<BoxFuture<Result<T,E>>>>`; the leader `tokio::spawn`s the work so it completes (and its limiter token is not wasted) even if every waiter is cancelled; the slot is removed from inside the shared future on completion, success or failure, so errors are never shared with later callers; `Flight{value, did_work}` — `singleflight.rs:1-12`, `:73-105`.
- Panicked/aborted work → `WorkFailed` → `ApiError::internal()` — `singleflight.rs:22-25`, `src/fetcher.rs:117-122`.

### 5.9 How archived matches/timelines are served

- `SqliteArchive::get` maps `match.byId` → `matches::get`, `match.timeline` → `matches::get_timeline`, by the first path param; read failures log and return `None`, so the request falls through to Riot — `src/archive/mod.rs:118-144`.
- Bodies are zstd blobs (`ZSTD_LEVEL = 3`) decoded with `zstd::decode_all`, returned byte-identical; an empty `body_zstd` means offloaded: the `body_locations` join gives `(pack_id, offset, len, sha256)` and the body is fetched by byte range from its pack and digest-checked; a body with no location or a failed digest is left out (warn) so the caller goes to Riot — `src/archive/matches.rs:1-17`, `:33`, `:156-158`, `:176-239`, `:317-320`.
- `SqliteArchive::put` on a fresh fetch: `put_stored` (match; also derives patch/queue/game_end/facts) or `put_timeline_stored`; re-archiving sets `offloaded_at = NULL`; bytes counted towards the next pack (OBJ-03); the missing half is queued (`archive:match` for the timeline if `ARCHIVE_TIMELINES`, or the match) — `src/archive/mod.rs:146-178`, `:93-110`, `src/archive/matches.rs:351-384`, `:422-428`.
- Archive hits send `X-Cache: ARCHIVE`, `X-Cache-Age: 0`, no `X-Cache-Fetched-Age` — `src/fetcher.rs:310-316`, `src/routes/passthrough.rs:133-145`; design `docs/design/04-data-and-cache.md:388`.

---

## 6. TODO (unverified)

- `TODO (unverified):` reqwest's effective HTTP/2 behaviour against Riot (ALPN vs HTTP/1.1 fallback): the builder sets no `http2_*`/`http1_only` option (`src/riot/client.rs:237-245`); only the `http2` Cargo feature is on (`Cargo.toml:32`).
- `TODO (unverified):` whether reqwest 0.13's `gzip` feature sends `Accept-Encoding: gzip` by default — inferred from the feature flag only (`Cargo.toml:32`).
- `TODO (unverified):` moka's exact eviction policy for the weighted L1 (TinyLFU vs LRU): moka 0.12 defaults, no `eviction_policy` call (`src/cache/l1.rs:109-113`).
- `TODO (unverified):` the migrated SQLite DDL of `cache` and `limiter_state`: taken from `docs/design/04-data-and-cache.md:330-346` and the SQL in `src/cache/l2.rs:159-183` / `src/riot/limiter/persist.rs:178-212`; `src/db/migrations/*` were not read.
- `TODO (unverified):` the ranged-GET implementation for offloaded bodies (`src/archive/objects.rs`, `src/archive/pack.rs`): only the call site `src/archive/matches.rs:217-239` was read.
- `TODO (unverified):` the `BULK` constant used by `src/jobs/archive.rs:255`, `:275`, `:358` (definition not read); jobs otherwise use `FetchOptions::JOB` (`src/jobs/timelines.rs:346`, `src/jobs/ladder.rs:404`, `:670`, `src/jobs/poll.rs:95`) and the admin debug route `FetchOptions::default()` (`src/routes/admin.rs:567`).
- `TODO (unverified):` `docs/design/03-architecture.md` §Request lifecycle was not compared; only 04 and 05 were, as asked.

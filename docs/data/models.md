# Domain models and DTOs

The in-code types riot-proxy moves data through: the Riot request model, cache entries, consumers and players, the derived archive rows, the analytics response documents, the pack records, the limiter checkpoint, job payloads and event frames, each mapped to Riot's payload fields or to the computation that produces it. Table columns are in [schema.md](schema.md); caching behaviour is in [caching.md](caching.md).

## Riot request model

`RiotRequest` (`src/riot/client.rs:31-42`) is one upstream call, built by a route or a job and consumed by the fetcher.

| Field | Type | Source | Notes |
|---|---|---|---|
| `endpoint` | `&'static Endpoint` | registry `src/riot/endpoints.rs:92-288` | one of 18 ids, e.g. `match.byId` |
| `target` | `Target` | caller's routing value | `Platform(oc1)` or `Region(sea)` (`endpoints.rs:302-322`); `host()` gives `oc1.api.riotgames.com`, `scope()` the limiter's app scope |
| `path` | `String` | `Endpoint::path_template` filled in order | percent-encoded |
| `params` | `Vec<String>` | path values as they appear in `path` | joined into the cache key |
| `query` | `Vec<(&str, String)>` | only names in `Endpoint::query` | sorted and hashed into the cache key |
| `pick_cluster` | `bool` | account-v1 only | the fetcher picks the first free cluster (ADR-066) |

`Endpoint` (`endpoints.rs:40-57`): `id`, `host: HostKind` (`Platform`, `Regional`, `Account`; `endpoints.rs:18-26`), `path_template`, `query`, `method_scope_key` (always the id), `override_key` (`CACHE_TTL_OVERRIDES` group), `soft_ttl: Option<Duration>` (`None` = immutable, archived forever), `persist_l2`, `immutable`, `negative: NegTtl` (`None`, `Default`, `Account`). `Ttls` (`endpoints.rs:448-455`) is the registry resolved under the running config: `soft`, `hard` (soft × 4 with stale-while-revalidate, `endpoints.rs:482-489`), `negative`.

## Cache entry

`CacheEntry` (`src/cache/l1.rs:22-37`) is what L1 holds and what L2 persists.

| Field | Type | Source | Notes |
|---|---|---|---|
| `status` | `u16` | upstream | 200, or 404 for a negative entry (`is_negative`, `l1.rs:40-42`) |
| `body` | `Bytes` | Riot's bytes untouched | empty for a negative entry |
| `content_at` | `Instant` | first time these bytes were seen | kept when a refresh returns identical bytes (`l1.rs:162-166`), so `X-Cache-Age` is content age |
| `fetched_at` | `Instant` | last time Riot answered | `X-Cache-Fetched-Age` |
| `soft_expires` | `Instant` | `now + soft` | fresh until |
| `hard_expires` | `Instant` | `now + max(hard, soft)` | servable as `STALE` until |

L2 stores the same six values as unix ms in `Row` (`src/cache/l2.rs:31-63`), converted through the process `Clock`. A lookup is `Fresh` before `soft_expires`, `Stale` before `hard_expires`, else `Miss` (`l1.rs:121-134`).

## Consumer

`Consumer` (`src/consumers.rs:70-77`): `id` (ULID), `name`, `scopes: Vec<Scope>` (`Read`, `Admin`, serialised lowercase, `consumers.rs:18-23`), `quota_per_min: u32` (default 600), `created_at`, `revoked_at: Option<i64>`. The key itself exists only in `Created.key` at creation (`consumers.rs:79-84`): `rpx_` plus 24 random bytes base64url, 36 characters (`consumers.rs:95-103`); the row keeps `sha256(key)`. Example key: `rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM`.

## Player

`Player` (`src/players.rs:22-36`) mirrors the `players` row: `puuid`, `platform`, `game_name`, `tag_line`, `tracked: bool`, `last_seen_match_id`, `backfill_state` (JSON text), `updated_at`, `in_game_id`, `last_rank` (JSON `{queue: {tier, rank, lp}}`). `backfill_state` parses to `BackfillState` (`src/jobs/archive.rs:83-100`):

```json
{"startedAt": 1760000000000, "doneAt": 1760003600000, "depth": 500, "cursor": null, "limit": 500}
```

`doneAt` set means the player's history is accounted for and no further lookup backfill is queued.

## Composite response bodies

`ProfileBody` (`src/routes/players.rs:188-218`): `puuid`, `platform`, `region`, then `account`, `summoner`, `league`, `mastery` as Riot's payloads verbatim or `null`, `ageSeconds` and `fetchedAgeSeconds` per part, `refreshed`, `refreshAvailableIn`, `warnings[]`. Without `?platform`, `platform` comes from `account.regionByPuuid`'s `region` parsed as a platform (`players.rs:244-263`).

`MatchPage` (`players.rs:494-526`): `puuid`, `platform`, `region`, `start`, `count`, `matchIds[]` as Riot returned them, `matches[]` of `MatchSummary`, `hasMore`, optional `champion` and `archive.complete`, `matchIdsAgeSeconds`, `matchIdsFetchedAgeSeconds`, `backfill`, `refreshed`, `refreshAvailableIn`, `warnings[]`.

`MatchSummary` (`src/routes/players/summary.rs:14-122`) is the requesting player's line in one match, Riot's names and values verbatim, absent means absent:

| Field | Source (Riot) | Notes |
|---|---|---|
| `matchId` | `metadata.matchId`, else the id asked for | never missing |
| `queueId`, `gameMode`, `gameVersion`, `gameCreation`, `gameEndTimestamp`, `gameDuration`, `endOfGameResult` | `info.*` | `gameEndTimestamp` and `gameCreation` unix ms; `gameDuration` seconds |
| `ddragonVersion` | computed: newest Data Dragon version sharing `gameVersion`'s `major.minor` | absent without a mirrored version list (`summary.rs:25-29`) |
| `player.*` | the participant whose `puuid` matches | `win`, `gameEndedInEarlySurrender`, `championId`, `championName`, `champLevel`, `teamId`, `teamPosition`, `kills`, `deaths`, `assists`, `totalMinionsKilled`, `neutralMinionsKilled`, `goldEarned`, `visionScore`, `totalDamageDealtToChampions`, `item0`–`item6`, `roleBoundItem`, `summoner1Id`, `summoner2Id`, `placement`, `playerSubteamId` |
| `player.perks` | `perks.styles[]` | reduced to `keystone`, `primaryStyle`, `subStyle` (`summary.rs:201-215`) |

`PlayerChampions` (`players.rs:912-926`) is the champion pool grouped from `match_facts` at read time (`src/archive/pool.rs:39-84`). Per `PlayerChampionEntry` (`players.rs:889-906`): `games`, `wins`, `winRate` = wins/games, `avgKda` = (kills + assists) / max(deaths, 1) over the games that carry a K/D/A, `csPerMin` = summed `cs` over the minutes of the games that carry both CS and a `game_duration` (`players.rs:971-984`, `pool.rs:49-53`), `lastPlayedAt` ISO 8601 of `max(game_end_ms)`.

## Match row and facts

`MatchMeta` (`src/archive/matches.rs:53-62`) is what `put_stored` indexes from a body: `patch` = `major.minor` of `info.gameVersion`, `queue_id` = `info.queueId`, `game_end_ms` = `info.gameEndTimestamp`, `game_duration_s` = `info.gameDuration` (seconds since Riot's 11.20). A body missing any of the first three is served but not archived (`matches.rs:78-103`). `region` is the request's routing scope (`src/archive/mod.rs:150`).

`Fact` (`src/archive/facts.rs:23-48`), one per `info.participants[]`, written at `FACTS_VERSION = 4` (`facts.rs:14-19`: 2 added `cs`, 3 added gold, damage, vision, bans and `remake`, 4 made `runes` positional).

| Field | Riot field | Notes |
|---|---|---|
| `puuid`, `team_id`, `champion_id`, `win` | `puuid`, `teamId`, `championId`, `win` | a participant missing any of these is left out (`facts.rs:178-185`) |
| `position` | `teamPosition` | `None` for `""` |
| `kills`, `deaths`, `assists` | same names | nullable |
| `cs` | `totalMinionsKilled + neutralMinionsKilled` | `None` only when both are absent |
| `gold`, `damage`, `vision` | `goldEarned`, `totalDamageDealtToChampions`, `visionScore` | |
| `items` | `item0`–`item5` | slot order, 0 for empty; the trinket `item6` is left out |
| `runes` | `perks.styles[]`, `perks.statPerks` | `[primaryStyle, keystone, subStyle, [perks…], [offense, flex, defense] or null]` (`facts.rs:53-80`); SQL reads the keystone as `json_extract(runes, '$[1]')` |
| `summoners` | `summoner1Id`, `summoner2Id` | `None` unless both present |

`Extras` (`facts.rs:216-228`): `remake` = any participant's `gameEndedInEarlySurrender`; `bans` from `info.teams[].bans[]` with `-1` and incomplete entries dropped. The `tier` stamp is not part of `Fact`: `tiers::stamp` sets it in the archive transaction from `ladder_entries` and `player_ranks`, else `UNKNOWN` (`src/archive/tiers.rs:37-57`), and `facts::write` never touches it.

## Build facts

`BuildFact` (`src/archive/builds.rs:51-65`) is derived from the match-v5 timeline plus the mirrored `item.json`, written at `BUILDS_VERSION = 3` (`builds.rs:24`).

| Field | Computation | Notes |
|---|---|---|
| `starter` | `ITEM_PURCHASED` before 60 000 ms, undone purchases removed, trinkets left out, sorted | `BUILD_STARTER_MS` (`builds.rs:29`) |
| `boots` | first purchase the catalogue calls boots | base Boots (1001) do not count (`builds.rs:34`) |
| `items` | finished items (total cost ≥ 1 500) in purchase order, at most 6 | `builds.rs:32,36` |
| `skills` | first 15 `SKILL_LEVEL_UP`s as `Q`, `W`, `E`, `R` | `builds.rs:38-40` |
| `skill_order` | Q, W, E ranked by points at the end, ties to the first there | Udyr (77) ranks R too, `"RWEQ"` (ADR-132, `builds.rs:279-284`); Viego (234) drops level-ups in the same millisecond as his own `ITEM_DESTROYED`, the possessed champion's ranks (ADR-131, `builds.rs:263-277`) |

ADR-131 moved `BUILDS_VERSION` 1 → 2 and ADR-132 2 → 3; `builds:extract` rewrites rows below the current version.

## Analytics response documents

All five are built from the derived tables only, never from `matches` or `match_facts` (`src/routes/analytics.rs:1-3`). Every `platform`, `tier`, `minTier`, `side`, `patch` and `role` echo is `null` when the request left it open and the rows were summed.

| Struct | Route | Fed by | Notes |
|---|---|---|---|
| `ChampionStatsResponse` (`analytics.rs:111-134`) | `/champions` | `champion_stats_totals` without `tier`; `champion_stats` with `tier` or `minTier`; denominators from `analytics_slices`, `analytics_match_totals`, `analytics_floor_totals` and the three ban tables | `totalGames` = games over the rows returned; `champions[]` of `ChampionStatEntry` |
| `ChampionDetailResponse` (`:230-267`) | `/champions/{id}` | the above plus `champion_matchups`, `champion_items`, `champion_runes`, `champion_spells` | `stats` is one summed row, `byTier` one per tier; `sectionsComputedAt` per section, `computedAt` the oldest |
| `ChampionMatchupsResponse` (`:153-178`) | `/champions/{id}/matchups` | `champion_matchups` | `matchups[]` of `{role, opponentId, opponentName?, games, wins, winRate}` |
| `ChampionBuildsResponse` (`:352-390`) | `/champions/{id}/builds` | `champion_builds` + `champion_build_parts`; with `opponentId`, `champion_matchup_builds` + `_parts` | `builds[]` of `ChampionBuildEntry` (`:326-347`): `core`, `games`, `wins`, `winRate`, `winRateLow`, `pickRate`, `next[3]`, `starter`, `boots`, `skillOrder`, `runes`, `spells` |
| `AnalyticsPatchesResponse` (`:408-421`) | `/patches` | `champion_stats_totals` | `patches[]` newest first, each `{patch, games, computedAt}` |

`ChampionStatEntry` (`analytics.rs:72-106`) computations (`enrich`, `analytics.rs:690-720`): `winRate` = wins/games; `share` = games over the rows returned; `pickRate` = `matches_picked` over the slice's matches, capped at 1; `banRate` = bans over the same matches; `avgKda` = (kills + assists)/max(deaths, 1) and `avgDamage`, `avgVision` over `stated_games`; `csPerMin`, `goldPerMin` over `duration_s / 60`. All rates are rounded to 4 places. `winRateLow` is the lower bound of the 95 % Wilson interval with z = 1.96, 0 for no games (`analytics.rs:645-658`).

## Pack and location records

| Type | Fields | Where |
|---|---|---|
| `pack::Entry` (`src/archive/pack.rs:79-86`) | `kind`, `match_id`, `offset`, `len`, `sha256` (32 bytes of the zstd body) | one body's place in a pack |
| index `Line` (`pack.rs:89-97`) | `{"kind","matchId","offset","len","sha256"}` camelCase, sha256 hex | one JSON line per body in the pack's index |
| `Pack` (`pack.rs:100-122`) | `id` (ULID), `kind`, `bytes`, `entries`, `sha256` of the whole object; `key()` and `metadata()` = `sha256`, `bodies` | the built object |
| `PackRow` (`src/archive/recovery.rs:63-75`) | `packId`, `kind`, `bytes`, `bodies`, `liveBytes`, `sha256` hex, `createdAt` | a `body_packs` row and a line of `index/packs-<date>.jsonl.zst` |
| `LocationRow` (`recovery.rs:77-88`) | `matchId`, `kind`, `packId`, `offset`, `len`, `sha256` hex | a `body_locations` row and a line of `index/locations-<date>.jsonl.zst` |

The pack binary layout is in [../integration/object-storage.md](../integration/object-storage.md#pack-format).

## Limiter checkpoint

`limiter_state.windows` is JSON `StoredScope` (`src/riot/limiter/persist.rs:37-51`): `known` (app rows only, omitted when false) and `windows[]` of `{limit, seconds, stamps: [[unix_ms, count], …]}`, stamps ascending and rounded up to `max(100 ms, seconds)` buckets (`persist.rs:53-70`).

```json
{"known": true, "windows": [{"limit": 100, "seconds": 120, "stamps": [[1760000000120, 3], [1760000000240, 1]]}]}
```

## Job payloads

`jobs.payload` per kind; every struct is `camelCase` (`src/jobs/mod.rs:20-71` lists the kinds).

| Kind | Payload | Source |
|---|---|---|
| `poll:live`, `poll:rank`, `poll:matches` | `{"puuid", "platform"}` | `src/jobs/poll.rs:55-56` |
| `archive:match` | `{"matchId", "puuid"?, "fetchTimeline"?}` | `ArchiveMatch`, `src/jobs/archive.rs:49-58` |
| `backfill:player` | `{"puuid", "platform", "limit": 500, "fetchTimeline"?, "queueId"?, "reason"?}`; `reason` ∈ lookup, track, catchup, admin | `BackfillPlayer`, `archive.rs:60-81` |
| `ddragon:sync` | `{"force": false}` | `SyncPayload`, `src/jobs/ddragon.rs:287-292` |
| `ladder:crawl` | `{"platform", "queue", "tierFloor"?}` | `CrawlRequest`, `src/jobs/ladder.rs:130-138` |
| `ladder:apex`, `ladder:walk` | `{"crawlId", "platform", "queue", "tier", "division"?}` | `LegJob`, `ladder.rs:140-150` |
| `ladder:collect` | `{"crawlId", "platform", "queue", "puuids": [...], "offset"}` | `CollectJob`, `ladder.rs:102-112` |
| `ladder:archive` | `{"crawlId", "platform", "queue"}` | `ArchiveLeg`, `ladder.rs:120-128` |
| `ranks:lookup` | `{"platform", "queue", "planned": false}` | `RankLookup`, `src/jobs/ladder/ranks.rs:33-43` |
| `aggregate:analytics` | `{"platform", "queue"}` | `src/jobs/analytics.rs:54-55` |
| `timelines:backfill` | `{"region"}` | `src/jobs/timelines.rs:195` |
| `aggregate:delta` | `{}`, one at a time for every ladder (THR-03) | `src/jobs/analytics.rs:93-96` |
| `names:backfill`, `facts:reextract`, `builds:extract`, `tiers:backfill`, `maintenance`, `archive:offload`, `archive:index-snapshot` | `{}` | e.g. `src/jobs/offload.rs:42-49` |

## Event frames

Events are serialised once into v1's frame `{"op":"event","event":"<name>","topic":"<topic>","at":<unix ms>,"data":{…}}` (`src/events.rs:158-172`). The `Event` enum (`events.rs:30-111`) carries, per name: `game.started` `{puuid, platform, gameId, queueId?, championId?}`; `game.ended` the same with `platform` optional; `rank.changed` `{puuid, queue, before, after}` where each side is `{tier?, rank?, lp?}` or `null`; `match.archived` `{puuid?, matchId, patch?, participants[]}`; `patch.new` `{version}`; `crawl.phase` `{crawlId, platform, queue, phase, stats}`; `ladder.crawl.completed` `{crawlId, platform, queue, entries, players, durationS}`; `analytics.updated` `{platform, queue, durationS, tables}`; `metrics.snapshot` the dashboard snapshot. `HistoryPoint` (`src/stats.rs:357-372`), the `metrics_history` row, is `{t, totals, queues, analytics, cache, disk?, requests?, upstream?}`.

---
**Sources:** `src/riot/client.rs:L31-L42`, `src/riot/endpoints.rs:L18-L57`, `src/riot/endpoints.rs:L302-L322`, `src/riot/endpoints.rs:L448-L496`, `src/cache/l1.rs:L22-L62`, `src/cache/l1.rs:L121-L177`, `src/cache/l2.rs:L31-L63`, `src/consumers.rs:L18-L23`, `src/consumers.rs:L70-L107`, `src/players.rs:L22-L36`, `src/jobs/archive.rs:L49-L100`, `src/routes/players.rs:L188-L218`, `src/routes/players.rs:L244-L263`, `src/routes/players.rs:L494-L526`, `src/routes/players.rs:L889-L926`, `src/routes/players.rs:L953-L989`, `src/routes/players/summary.rs:L14-L215`, `src/archive/pool.rs:L39-L84`, `src/archive/matches.rs:L53-L103`, `src/archive/mod.rs:L146-L198`, `src/archive/facts.rs:L14-L80`, `src/archive/facts.rs:L173-L289`, `src/archive/tiers.rs:L37-L57`, `src/archive/builds.rs:L23-L65`, `src/archive/builds.rs:L263-L290`, `src/routes/analytics.rs:L72-L134`, `src/routes/analytics.rs:L153-L421`, `src/routes/analytics.rs:L645-L658`, `src/routes/analytics.rs:L690-L720`, `src/archive/pack.rs:L79-L122`, `src/archive/recovery.rs:L63-L88`, `src/riot/limiter/persist.rs:L37-L70`, `src/jobs/mod.rs:L20-L71`, `src/jobs/poll.rs:L55-L56`, `src/jobs/analytics.rs:L54-L55`, `src/jobs/analytics.rs:L93-L96`, `src/jobs/timelines.rs:L195`, `src/jobs/ddragon.rs:L287-L292`, `src/jobs/ladder.rs:L102-L150`, `src/jobs/ladder/ranks.rs:L33-L43`, `src/jobs/offload.rs:L42-L49`, `src/events.rs:L30-L172`, `src/stats.rs:L357-L372`, `docs/DECISIONS.md` (ADR-131, ADR-132), `docs/_meta/scratch/routes.md`
**Last verified:** 2026-10-22 against commit `6eda84e`

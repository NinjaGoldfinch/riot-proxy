# Endpoints: `/v1/admin/*`

All 40 administration operations. Every one needs a key with the **admin** scope from an address on `ADMIN_IP_ALLOWLIST` (an empty list allows everyone) and counts against the consumer quota (`src/routes/docs.rs:L131-L134`, `src/http/auth.rs:L276-L288`, `L327-L342`). In production the public host `api.ninjagoldfinch.lol` blocks `/v1/admin*` at the Cloudflare WAF; use `admin.ninjagoldfinch.lol` or the loopback. Conventions are in [endpoints.md](endpoints.md#conventions).

Shared rules (`src/routes/admin.rs:L84-L107`, `src/http/body.rs:L341-L391`, `src/http/validate.rs:L253-L259`):

- Responses are `application/json; charset=utf-8`; store failures are `500 INTERNAL` without detail.
- JSON bodies use ajv-style coercion and messages: not JSON is `400 VALIDATION` `body is not valid JSON`; not an object is `body must be object`; a missing field is `body must have required property '<name>'`. The `Content-Type` request header is not required (captured with `curl -d`).
- `{id}` path params are ULIDs (26 chars) else `400 VALIDATION` `params/id must match format "ulid"`.
- `keyScope` in bodies is the first 8 hex chars of `sha256(RIOT_API_KEY)`; player and crawl listings are scoped to it (`src/routes/admin.rs:L99-L101`, `L386-L389`, `L1848`).
- None of these routes call Riot except `POST /v1/admin/tracked-players` (Riot ID resolution), `GET /v1/admin/debug/riot` and `POST /v1/admin/ladder/probe`.

## Consumers

| Method | Path | Params / body | Response | Handler |
|---|---|---|---|---|
| POST | `/v1/admin/consumers` | body `name` 1 to 100 chars **required**; `scopes` array of `read`/`admin` (≥ 1, a scalar is coerced to an array, default `["read"]`); `quotaPerMin` 1 to 1 000 000 (default 600) | **200** `{id,name,scopes,quotaPerMin,key,warning}`; `key` is the plaintext, shown once; duplicate name is `400 VALIDATION` | `src/routes/admin.rs:L180` |
| GET | `/v1/admin/consumers` | none | `{consumers:[{id,name,scopes,quotaPerMin,createdAt,disabledAt}]}`, revoked rows included with `disabledAt` set | `src/routes/admin.rs:L231` |
| DELETE | `/v1/admin/consumers/{id}` | `id` ULID | `{"ok":true,"id":…}`; soft delete, the auth cache entry is dropped at once and that consumer's sockets close with `4401`; not active is `404` `No active consumer with that id` | `src/routes/admin.rs:L248` |
| POST | `/v1/admin/consumers/{id}/revoke-cache` | body `keyHash` **required**, exactly 64 hex chars | `{"ok":true,"id":…,"stillActive":bool}`; wrong pair is `404` `No consumer with that id and key hash` | `src/routes/admin.rs:L293` |

```text
POST /v1/admin/consumers  {"name":"my-website","scopes":["read"],"quotaPerMin":600}
200 {"id":"01M58N8FH4FCJVS50C8BZZPEBZ","name":"my-website","scopes":["read"],"quotaPerMin":600,"key":"rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM","warning":"Store this key now …"}
```

The `warning` text (`src/routes/admin.rs:L214`) says the key cannot be retrieved again; it is abridged here.

## Players and archive

| Method | Path | Params / body | Response | Handler |
|---|---|---|---|---|
| GET | `/v1/admin/tracked-players` | `?tracked=true` (the only accepted value) | `{players:[PlayerRecord]}` for the current key scope | `src/routes/admin.rs:L393` |
| POST | `/v1/admin/tracked-players` | body `platform` **required**; `puuid` **or** `gameName`+`tagLine`; `tracked` bool (default true). Neither id is `400 VALIDATION` `Provide either puuid, or gameName and tagLine` | `PlayerRecord` plus `backfill{jobId,status,limit}`; a Riot ID is resolved through `account.byRiotId` (cluster picked), Riot's spelling wins; no `puuid` is `404` `Riot ID '{n}#{t}' did not resolve to a PUUID`. Queues `backfill:player` when tracked and not yet walked | `src/routes/admin.rs:L454` |
| DELETE | `/v1/admin/tracked-players/{puuid}` | puuid | `{"ok":true,"puuid":…,"tracked":false}`; row kept; unknown is `404` `No such player for the current key scope` | `src/routes/admin.rs:L580` |
| GET | `/v1/admin/players/{puuid}/archive` | puuid | `{player, archive{matches,remakes,wins,timelines,oldestGameEnd,newestGameEnd,byQueue[]}, jobs{kind→{pending,running,done,failed}}, latestJobs[]}` for `archive:match` and `backfill:player` | `src/routes/admin.rs:L664` |
| GET | `/v1/admin/players/{puuid}/archive/matches` | `?start` 0 to 1 000 000 (default 0); `?count` 1 to 100 (default 25); `?queue` 0 to 5000 | `{puuid,total,start,count,matches[ArchivedLine]}` | `src/routes/admin.rs:L764` |
| POST | `/v1/admin/players/names/backfill` | none | **202** `{"ok":true,"unnamed":n}`; queues `names:backfill` | `src/routes/admin.rs:L2277` |
| POST | `/v1/admin/backfill` | body `puuid`, `platform` **required**; `limit` 1 to 10 000 (default 500); `fetchTimeline` bool (default false) | **200** `{"ok":true,"jobId":…,"status":"queued"\|"already-queued"}` | `src/routes/admin.rs:L1583` |

Captured `PlayerRecord` (tracking by PUUID on a scratch server):

```json
{"puuid":"AbC…","keyScope":"95b708b6","platform":"euw1","gameName":null,"tagLine":null,"tracked":true,"lastSeenMatchId":null,
 "historyBackfillStartedAt":"2026-10-18T23:24:43.162Z","historyBackfilledAt":null,"historyBackfillDepth":0,"updatedAt":"2026-10-18T23:24:43.168Z",
 "backfill":{"jobId":"01M58N9VRRQQGTJX5Y8NRJ9533","status":"already-queued","limit":4294967295}}
```

## Cache, stats, limits, debug

| Method | Path | Params / body | Response | Handler |
|---|---|---|---|---|
| POST | `/v1/admin/cache/purge` | body `pattern` **required** 1 to 200 chars, Redis-style glob; `dryRun` bool (default false) | `{"ok":true,"pattern":…,"deleted":n}` or `{"ok":true,"pattern":…,"matched":n,"dryRun":true}`; the pattern is prefixed with the key scope unless it already starts with one; purges L1 and L2; the archive is never purged | `src/routes/admin.rs:L838` |
| GET | `/v1/admin/stats` | none | `AdminStats`: archive and player counts, archive bytes, `archiveBodies{matches,timelines}{local,packed,offloaded}`, pack stats, SQLite/WAL/disk readings, `sampledAt{fast,slow}` | `src/routes/admin.rs:L969` |
| GET | `/v1/admin/limits/{scope}` | `scope` a platform or region value (case-sensitive) else `400 VALIDATION` `params/scope must be equal to one of the allowed values` | `{scope,usage[{window:"limit:seconds",used,limit}],frozenMs}` | `src/routes/admin.rs:L1038` |
| GET | `/v1/admin/debug/riot` | `?scope` **required** (platform or region, any case); `?path` **required** 1 to 400 chars starting with `/`, query allowed; `?method` an endpoint id; `?noCache` bool | Riot's bytes through the passthrough responder (`X-Cache`, `BYPASS` with `noCache=true`); bad scope is `400 BAD_REGION` `'{scope}' is neither a platform nor a region`; a `method` whose template does not match the path is `400 VALIDATION` `path does not match {id}'s route {template}`. Never cluster-moved | `src/routes/admin.rs:L1184` |
| GET | `/v1/admin/debug/cache` | `?scope`, `?path`, `?method` all **required** | `{key,present,ageSeconds,stale}`, L1 only | `src/routes/admin.rs:L1232` |

Captured on a fresh scope (bootstrap limits before Riot's headers have been seen):

```text
GET /v1/admin/limits/euw1
{"scope":"euw1","usage":[{"window":"20:1","used":0,"limit":20},{"window":"100:120","used":0,"limit":100}],"frozenMs":0}

GET /v1/admin/debug/cache?scope=euw1&path=/lol/status/v4/platform-data&method=status.platformData
{"key":"95b708b6:status.platformData:euw1.api.riotgames.com","present":false,"ageSeconds":null,"stale":null}
```

## Jobs

| Method | Path | Params | Response | Handler |
|---|---|---|---|---|
| GET | `/v1/admin/jobs` | `?state` in `pending`, `running`, `done`, `failed`; `?kind` ≤ 40 chars; `?limit` 1 to 500 (default 50) | `{jobs:[{id,kind,dedupeKey,priority,state,attempts,runAfter,claimedAt,finishedAt,error,payload}]}`, newest first | `src/routes/admin.rs:L1334` |
| DELETE | `/v1/admin/jobs` | `?kind` **required** (a known kind); `?state` **required** in `pending`, `failed`; `?minPriority` ≥ 0 (default 0); `?dryRun` bool | `{kind,state,minPriority,matched,deleted,dryRun}`; running rows untouched | `src/routes/admin.rs:L1504` |
| GET | `/v1/admin/jobs/stats` | none | `{kinds{kind→{pending,running,done,failed}},totals{…}}` | `src/routes/admin.rs:L1401` |
| GET | `/v1/admin/jobs/queue` | `?limit` 1 to 100 (default 15) | `{running[],next[],ready,delayed,nextDelayedAt,heldKinds[],held}` | `src/routes/admin.rs:L1936` |
| GET | `/v1/admin/jobs/activity` | `?limit` 1 to 200 (default 50) | `{workers[],finished[]}`, in-memory, lost on restart | `src/routes/admin.rs:L1975` |
| GET | `/v1/admin/jobs/{id}/activity` | `id` ULID; `?after` ≥ 0 | `{job,trace}`; both null is `404` `No such job` | `src/routes/admin.rs:L2009` |
| POST | `/v1/admin/jobs/{id}/retry` | `id` ULID | the job summary; `404` `No such job`; wrong state is `400 VALIDATION` `Job {id} is {state}`; `An identical job is already queued ({other})` | `src/routes/admin.rs:L1426` |
| DELETE | `/v1/admin/jobs/{id}` | `id` ULID | the job summary with state `failed`, error `cancelled`; same error mapping | `src/routes/admin.rs:L1455` |

Captured job summary:

```json
{"id":"01M58N8RQM0MYWC6NYWB7ZY7N7","kind":"ddragon:sync","dedupeKey":"ddragon:sync","priority":30000,"state":"done","attempts":1,
 "runAfter":"2026-10-18T23:24:07.284Z","claimedAt":"2026-10-18T23:24:07.286Z","finishedAt":"2026-10-18T23:24:08.575Z","error":null,"payload":{}}
```

> **TODO (unverified):** the exact field names of `workers[]`, `finished[]` and `trace[]` (`src/jobs/activity.rs` views) and of `stats::Snapshot`; the inventory recorded them by schema reference only (`docs/_meta/scratch/routes.md` §7).

## Maintenance, Data Dragon, analytics, timelines

| Method | Path | Params / body | Response | Handler |
|---|---|---|---|---|
| POST | `/v1/admin/maintenance/run` | none | **202** `{"ok":true,"jobId":…,"status":"queued"\|"already-queued"}`; dedupe key `maintenance` | `src/routes/admin.rs:L2662` |
| POST | `/v1/admin/ddragon/sync` | body `force` bool (default false) | **200** `{"ok":true,"jobId":…}`; dedupe `ddragon:sync` or `ddragon:sync:force` | `src/routes/admin.rs:L1634` |
| POST | `/v1/admin/analytics/recompute` | body `platform` **required**; `queue` in `RANKED_SOLO_5x5`, `RANKED_FLEX_SR` (default first of `LADDER_QUEUES`) | **202** `{"ok":true,"platform":…,"queue":…}`; queues `aggregate:analytics` at priority 0, ahead of everything | `src/routes/admin.rs:L2340` |
| POST | `/v1/admin/analytics/reextract` | none | **202** `{"ok":true,"stale":n}`; queues `facts:reextract` | `src/routes/admin.rs:L2375` |
| GET | `/v1/admin/timelines/backfill` | `?patches` 1 to 1000 (default `TIMELINE_BACKFILL_PATCHES`, 0) | the timeline backfill progress document; read-only | `src/routes/admin.rs:L2306` |

> **TODO (unverified):** the field names of `jobs::timelines::Progress` returned by `GET /v1/admin/timelines/backfill` (schema reference only in the inventory).

## Ladder

| Method | Path | Params / body | Riot | Response | Handler |
|---|---|---|---|---|---|
| POST | `/v1/admin/ladder/crawl` | body `platform` **required**; `queue`; `tierFloor` in `IRON`…`CHALLENGER` (default `LADDER_TIER_FLOOR`, `MASTER`) | none at request time (the legs call league-v4 later) | **202** `{"crawlId","status":"started"\|"already-running","platform","queue","legs"}`; a duplicate is not a 409 | `src/routes/admin.rs:L1700` |
| GET | `/v1/admin/ladder/options` | none | none | `{platforms[{id,label}],queues[],tiers[],defaults{platform,queue,tierFloor,backfillLimit}}` | `src/routes/admin.rs:L1746` |
| POST | `/v1/admin/ladder/probe` | body `platform` **required**; `queue` | about 55 live calls: the three apex leagues, `league.expEntries` `MASTER/I` pages 1 to 100, `league.entriesByTier` `MASTER/I?page=1`, all bypassing the cache at interactive priority | **200** report `{cap,churnTolerance,summary,checks[{id,title,verdict,detail}],lists[],exp{…},pagedMaster{…},platform,queue,tookMs}`; verdicts `confirmed`, `not-seen`, `changed`, `error`; stores nothing | `src/routes/admin/ladder_probe.rs:L464` |
| GET | `/v1/admin/ladder/crawls` | `?platform`; `?queue`; `?limit` 1 to 100 (default 20) | none | `{crawls:[LadderCrawlSummary]}` for the current key scope | `src/routes/admin.rs:L1831` |
| GET | `/v1/admin/ladder/crawls/{id}` | `id` ULID | none | `{crawl,asOf,stages[],openLegs[],running[],next[],ahead,failed[],downloads{…}}`; `404` `No such ladder crawl for the current key scope` | `src/routes/admin.rs:L2197` |
| DELETE | `/v1/admin/ladder/crawls/{id}` | `id` ULID | none | `{"ok":true,"crawlId":…,"status":"cancelled","droppedJobs":n}`; not running is `400 VALIDATION` `Crawl {id} is already {status}`; publishes `crawl.phase` | `src/routes/admin.rs:L1870` |

Captured:

```text
POST /v1/admin/ladder/crawl  {"platform":"euw1","queue":"RANKED_SOLO_5x5","tierFloor":"CHALLENGER"}
HTTP/1.1 202 Accepted
{"crawlId":"01M58N9VS82BHDZ404EBRES74C","legs":1,"platform":"euw1","queue":"RANKED_SOLO_5x5","status":"started"}

GET /v1/admin/ladder/crawls?limit=1
{"crawls":[{"id":"01M58N9VS82BHDZ404EBRES74C","platform":"euw1","queue":"RANKED_SOLO_5x5","tierFloor":"CHALLENGER","status":"running","phase":"enumerate",
            "pendingLegs":1,"pagesFetched":0,"entriesSeen":0,"playersDiscovered":0,"backfillsEnqueued":0,"matchIdsSeen":0,"matchesQueued":0,"apexCapped":[],
            "startedAt":"2026-10-18T23:24:43.176Z","finishedAt":null}]}
```

A probe on a Riot key that is rejected answers `502 UPSTREAM_ERROR`, as any Riot-backed route does.

## Process and dashboard

| Method | Path | Params | Response | Handler |
|---|---|---|---|---|
| GET | `/v1/admin/metrics` | none | the `stats::Snapshot` document the `metrics` WebSocket topic sends, including `sqliteWriter{busySince,busyMs,lastProgress,lastFinished,queued}` (WR-02); not recorded in the recent ring | `src/routes/admin.rs:L2399` |
| GET | `/v1/admin/metrics/history` | none | `{intervalS,maxPoints,points[]}`; `intervalS` is `METRICS_HISTORY_INTERVAL_S` (60) | `src/routes/admin.rs:L2424` |
| GET | `/v1/admin/info` | none | `{version,gitSha,rustc,builtAt,startedAt,uptimeSeconds,env,role,keyScope,features{devUi,docsUi,dashboardUi,authDisabled,archiveTimelines,staleWhileRevalidate,trustProxy,postgres},workers,config{…}}`; `RIOT_API_KEY` is never listed and other secrets read `[REDACTED]` | `src/routes/admin.rs:L2486` |
| GET | `/v1/admin/recent` | `?kind` in `requests` (default), `errors`; `?limit` 1 to 500 (default 100) | `{kind,capacity,requests?[{at,method,route,path,status,cache,latencyMs,consumer,requestId}],errors?[{at,code,status,message,requestId}]}`; rings of 500 requests and 200 errors, lost on restart | `src/routes/admin.rs:L2546` |
| GET | `/v1/admin/backups` | none | `{dir,keep,files[{name,bytes,modifiedAt}],lastRun,diskFreeBytes}`, metadata only | `src/routes/admin.rs:L2616` |

Captured `/v1/admin/info`, shortened:

```json
{"version":"2.7.0","gitSha":"7491ff6c1c5c","rustc":"rustc 1.98.1 (48a229cea 2026-09-01)","builtAt":1792365233404,"startedAt":1792365847264,
 "uptimeSeconds":35,"env":"development","role":"all","keyScope":"95b708b6",
 "features":{"devUi":true,"docsUi":true,"dashboardUi":true,"authDisabled":false,"archiveTimelines":true,"staleWhileRevalidate":true,"trustProxy":false,"postgres":true},
 "workers":1,
 "config":{"ADMIN_IP_ALLOWLIST":"","ARCHIVE_OBJECT_SECRET_KEY":null,"BULK_USAGE_CEILING":"0.8","CACHE_L1_MAX_MB":"128","JOB_CONCURRENCY":"1",
           "JOB_KIND_LIMITS":"aggregate:analytics=1,archive:offload=1","LADDER_TIER_FLOOR":"MASTER","NEG_TTL_SECONDS":"30",
           "RIOT_USER_AGENT":"riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)","STALE_WHILE_REVALIDATE":"true", "…":"…"}}
```

---
**Sources:** `src/routes/admin.rs:L44-L107,L114-L171,L180-L326,L336-L435,L454-L502,L524-L571,L580-L594,L630-L653,L664,L717-L780,L832-L882,L890-L924,L969,L1010-L1038,L1075-L1204,L1212-L1248,L1262-L1354,L1369-L1536,L1583-L1613,L1634-L1652,L1674-L1689,L1700-L1758,L1764-L1795,L1831-L1848,L1870-L1894,L1904-L1925,L1936-L1997,L2009-L2033,L2092-L2110,L2197-L2225,L2277-L2311,L2340-L2362,L2375-L2384,L2394-L2415,L2424,L2438-L2486,L2522-L2558,L2583-L2624,L2662-L2670`, `src/routes/admin/ladder_probe.rs:L42-L44,L121-L149,L351-L355,L387-L434,L458-L496`, `src/http/body.rs:L341-L391`, `src/http/validate.rs:L81-L88,L253-L259`, `src/http/auth.rs:L276-L288,L327-L342`, `src/http/recent.rs:L109-L122`, `docs/_meta/scratch/routes.md` §1.8, live capture against `target/debug/riot-proxy` 2.7.0
**Last verified:** 2026-10-20 against commit `b7b363c`

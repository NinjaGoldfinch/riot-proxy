# Examples

Copy-pasteable `curl` walkthroughs against a local `riot-proxy serve`. Set the two variables once; every snippet uses them. Ids, keys and names below are fakes.

```bash
BASE=http://127.0.0.1:8080
KEY=rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM      # a read key; admin snippets use ADMIN_KEY
PUUID=AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEf
```

Outputs marked "captured" were taken from a scratch server running this commit with a fake Riot key, so Riot-backed calls show the error the proxy gives when Riot rejects its key. Against a real key they return Riot's bodies as described in [endpoints.md](endpoints.md).

## 1. Mint a key

With the CLI, against the same `DATA_DIR` the server uses (`src/cli/mod.rs`):

```bash
riot-proxy key create --name my-website                       # scope read, quota 600/min
riot-proxy key create --name ops --scopes read,admin --quota 1200
riot-proxy key list
```

Captured:

```text
  API KEY   rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM

  This key is shown once and cannot be recovered. Store it now.

ID                          NAME                      SCOPES       QUOTA  STATUS
01M58N8FH4FCJVS50C8BZZPEBZ  my-website                read           600  active
```

With the admin API (an existing admin key is needed; on a fresh database the bootstrap admin key is printed once at the first `serve`, `README.md` "First run"):

```bash
curl -s -H "Authorization: Bearer $ADMIN_KEY" -H 'Content-Type: application/json' \
  -d '{"name":"my-website","scopes":["read"],"quotaPerMin":600}' $BASE/v1/admin/consumers
```

```json
{"id":"01M58N8FH4FCJVS50C8BZZPEBZ","name":"my-website","scopes":["read"],"quotaPerMin":600,"key":"rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM","warning":"…"}
```

## 2. Health

```bash
curl -i $BASE/healthz
curl -i $BASE/readyz
```

Captured:

```text
HTTP/1.1 200 OK
content-type: application/json
x-request-id: 01M58N9D55K65Z2YZ2FFZAKBZ3

{"ok":true}
```

```text
{"ok":true,"sqlite":true,"limiter":true,"keyScope":"95b708b6","sqliteReaders":4,"sqliteReadersFree":4}
```

`/readyz` answers 503 with the same body when `sqlite` or `limiter` is false.

## 3. Look up an account by Riot ID

No region: the proxy picks the account-v1 cluster with room (`asia`, `americas`, `europe`). Spaces in the game name are percent-encoded.

```bash
curl -i -H "Authorization: Bearer $KEY" "$BASE/v1/riot/accounts/by-riot-id/Hide%20on%20bush/KR1"
```

Expected on a working key:

```text
HTTP/1.1 200 OK
content-type: application/json; charset=utf-8
x-cache: MISS
x-cache-age: 0
x-cache-fetched-age: 0
x-ratelimit-limit: 600
x-ratelimit-remaining: 599
x-ratelimit-reset: 60
x-request-id: 01M58N9D7MAGZXE2A432CA0EQ7

{"puuid":"AbC…","gameName":"Hide on bush","tagLine":"KR1"}
```

Captured with the fake Riot key (what a rejected key looks like to a consumer):

```text
HTTP/1.1 502 Bad Gateway
{"error":{"code":"UPSTREAM_ERROR","message":"Upstream authentication failed","requestId":"01M58N9D7MAGZXE2A432CA0EQ7"}}
```

Pinned to a cluster (the pinned route never moves, even if that cluster is frozen):

```bash
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/riot/accounts/by-riot-id/asia/Hide%20on%20bush/KR1"
```

Repeat either call and `X-Cache` becomes `HIT` with `X-Cache-Age` counting up; the account TTL is a day.

## 4. Summoner by PUUID

```bash
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/lol/summoners/by-puuid/kr/$PUUID"
```

```json
{"puuid":"AbC…","profileIconId":6,"revisionDate":1790247628000,"summonerLevel":939}
```

Platform values are lower-case and case-sensitive: `KR` gives `400 BAD_REGION`.

## 5. Player profile and match page

```bash
# the proxy finds the platform through account-v1's region lookup
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/by-riot-id/Hide%20on%20bush/KR1/profile"

# or name it; topMastery defaults to 5
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/$PUUID/profile?platform=kr&topMastery=3"

# match page: platform is required; count 1 to 20
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/$PUUID/matches?platform=kr&count=5"

# ranked solo only, second page
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/$PUUID/matches?platform=kr&queue=420&start=10&count=10"

# only games on champion 134 (built from the archive; cannot be combined with type=)
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/$PUUID/matches?platform=kr&champion=134"

# force a refresh (any consumer; one claim per 60 s per part and PUUID)
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/players/$PUUID/profile?platform=kr&refresh=true"
```

The profile body has `puuid`, `platform`, `region`, `account`, `summoner`, `league[]`, `mastery[]`, `ageSeconds{…}`, `fetchedAgeSeconds{…}`, `refreshed`, `refreshAvailableIn`, `warnings[]`; the match page has `matchIds[]`, `matches[]`, `hasMore`, `backfill{…}` on the first page, and `archive{complete}` with `champion` (shapes in [endpoints-players-analytics.md](endpoints-players-analytics.md)). Captured without `platform`:

```text
400 {"error":{"code":"VALIDATION","message":"querystring must have required property 'platform'","requestId":"01M58N9D6PGC58Y7A9BZ3VXMY3"}}
```

## 6. A match and its timeline

The region comes from the id prefix (`KR_` is `asia`, `OC1_` is `sea`, `EUW1_` is `europe`). Declare the right one or get `400 BAD_REGION`.

```bash
curl -si -H "Authorization: Bearer $KEY" "$BASE/v1/lol/matches/asia/KR_8393343196" | grep -i '^x-cache\|^HTTP'
curl -si -H "Authorization: Bearer $KEY" "$BASE/v1/lol/matches/asia/KR_8393343196" | grep -i '^x-cache\|^HTTP'
curl -s  -H "Authorization: Bearer $KEY" "$BASE/v1/lol/matches/asia/KR_8393343196/timeline" | head -c 200
```

Expected: the first call is `x-cache: MISS` (fetched and archived), the second `x-cache: ARCHIVE` with `x-cache-age: 0` and no `x-cache-fetched-age`; the match never expires. The timeline follows the same path.

## 7. Static data and versions

```bash
curl -s -H "Authorization: Bearer $KEY" $BASE/v1/static/versions
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/static/champion?version=16.20.1" | head -c 200
curl -s -H "Authorization: Bearer $KEY" $BASE/v1/static/queues | head -c 200
curl -sI $BASE/ddragon/16.20.1/img/champion/Aatrox.png          # no key; Cache-Control immutable
```

Captured:

```text
{"current":"16.20.1","versions":["16.20.1","16.19.1","16.18.1", …]}
{"type":"champion","format":"standAloneComplex","version":"16.20.1","data":{"Aatrox":{"version":"16.20.1","id":"Aatrox","key":"266","name":"Aatrox", …
[{"queueId":0,"map":"Custom games","description":null,"notes":null},{"queueId":2,"map":"Summoner's Rift","description":"5v5 Blind Pick games", …
```

## 8. Champion analytics with filters and `If-None-Match`

```bash
# what the site asks for: solo queue, every tier summed, newest patch
curl -si -H "Authorization: Bearer $KEY" \
  "$BASE/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&minGames=0&limit=500"

# one lane, one patch, Emerald and above, red side only
curl -s -H "Authorization: Bearer $KEY" \
  "$BASE/v1/lol/analytics/champions?role=MIDDLE&patch=16.20&minTier=EMERALD&side=red&limit=20"

# a champion's detail, matchups and set builds against one opponent
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/lol/analytics/champions/134?limit=10"
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/lol/analytics/champions/134/matchups?role=MIDDLE&limit=200"
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/lol/analytics/champions/134/builds?opponentId=157&role=MIDDLE"
curl -s -H "Authorization: Bearer $KEY" "$BASE/v1/lol/analytics/patches?queue=RANKED_SOLO_5x5&championId=134"

# revalidate with the ETag from the first call
ETAG=$(curl -si -H "Authorization: Bearer $KEY" "$BASE/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&limit=2" \
       | grep -i '^etag' | cut -d' ' -f2 | tr -d '\r')
curl -si -H "Authorization: Bearer $KEY" -H "If-None-Match: $ETAG" "$BASE/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&limit=2"
```

Captured, first call then the revalidation:

```text
HTTP/1.1 200 OK
cache-control: private, max-age=300
etag: W/"HwbkHHPVcX8gqbVqhdoe6Fa0DRAJwgyuz7r2LxH1V0s"
x-cache: MISS

{"platform":null,"queue":"RANKED_SOLO_5x5","tier":null,"minTier":null,"side":null,"patch":null,"role":null,"computedAt":null,"totalGames":0,"champions":[]}

HTTP/1.1 304 Not Modified
etag: W/"HwbkHHPVcX8gqbVqhdoe6Fa0DRAJwgyuz7r2LxH1V0s"
x-cache: HIT
```

`tier` and `minTier` together: `400 VALIDATION` `querystring/minTier must NOT be sent with tier`.

## 9. WebSocket subscription

With `websocat`:

```bash
websocat "ws://127.0.0.1:8080/v1/ws?token=$KEY"
{"op":"subscribe","topics":["player:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEf","patch"]}
{"op":"ping"}
```

With Node 22 (built-in `WebSocket`), `node ws.mjs $KEY`:

```javascript
const key = process.argv[2];
const puuid = 'AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz0123456789AbCdEf';
const ws = new WebSocket(`ws://127.0.0.1:8080/v1/ws?token=${key}`);
ws.onopen = () => {
  ws.send(JSON.stringify({ op: 'subscribe', topics: [`player:${puuid}`, 'patch'] }));
  ws.send(JSON.stringify({ op: 'ping' }));
};
ws.onmessage = (m) => console.log('<<', m.data);
ws.onclose = (e) => console.log('closed', e.code, e.reason);
```

Captured (the same script, with two deliberate extras: an admin topic and a typo):

```text
<< {"op":"ready","consumer":"my-website"}
<< {"op":"error","error":{"code":"FORBIDDEN","message":"Topic 'metrics' requires the admin scope"}}
<< {"op":"error","error":{"code":"VALIDATION","message":"Unknown topic 'nope'"}}
<< {"op":"subscribed","topics":["player:AbC…","patch"]}
<< {"op":"pong","at":1792365906183}
```

Events then arrive as `{"op":"event","event":"game.started","topic":"player:AbC…","at":…,"data":{…}}` while a `poll:live` job sees the player in a game, and `{"op":"event","event":"patch.new","topic":"patch","data":{"version":"16.21.1"}}` when `ddragon:sync` mirrors a new patch.

## 10. Admin

```bash
ADMIN_KEY=rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEADMN
A="Authorization: Bearer $ADMIN_KEY"

curl -s -H "$A" $BASE/v1/admin/consumers                                            # list consumers
curl -s -H "$A" -d '{"puuid":"'$PUUID'","platform":"kr","limit":50}' $BASE/v1/admin/backfill      # queue a backfill
curl -s -H "$A" -d '{"platform":"kr","gameName":"Hide on bush","tagLine":"KR1"}' $BASE/v1/admin/tracked-players   # track a player
curl -si -H "$A" -d '{"platform":"kr","queue":"RANKED_SOLO_5x5","tierFloor":"MASTER"}' $BASE/v1/admin/ladder/crawl  # start a crawl
curl -s -H "$A" "$BASE/v1/admin/jobs?state=pending&limit=5"                          # check jobs
curl -s -H "$A" $BASE/v1/admin/jobs/stats
curl -s -H "$A" -d '{"pattern":"*:summoner.byPuuid:*","dryRun":true}' $BASE/v1/admin/cache/purge  # purge dry run
curl -s -H "$A" $BASE/v1/admin/limits/euw1                                          # Riot-side windows for euw1
curl -s -H "$A" $BASE/v1/admin/info | head -c 400
```

Captured:

```text
{"jobId":"01M58N9VRRQQGTJX5Y8NRJ9533","ok":true,"status":"queued"}
HTTP/1.1 202 Accepted
{"crawlId":"01M58N9VS82BHDZ404EBRES74C","legs":1,"platform":"euw1","queue":"RANKED_SOLO_5x5","status":"started"}
{"kinds":{"ddragon:sync":{"pending":0,"running":0,"done":1,"failed":0},"maintenance":{…},"names:backfill":{…}},"totals":{"pending":0,"running":0,"done":3,"failed":0}}
{"dryRun":true,"matched":0,"ok":true,"pattern":"*:summoner.byPuuid:*"}
{"scope":"euw1","usage":[{"window":"20:1","used":0,"limit":20},{"window":"100:120","used":0,"limit":100}],"frozenMs":0}
{"version":"2.7.0","gitSha":"7491ff6c1c5c","rustc":"rustc 1.98.1 (48a229cea 2026-09-01)","builtAt":1792365233404,"startedAt":1792365847264,"uptimeSeconds":35,"env":"development","role":"all","keyScope":"95b708b6","features":{"devUi":true,"docsUi":true,"dashboardUi":true,"authDisabled":false,"archiveTimelines":true,"staleWhileRevalidate":true,"trustProxy":false,"postgres":true},"workers":1,"config":{…}}
```

## 11. Error walkthroughs

```bash
curl -si $BASE/v1/riot/accounts/by-puuid/$PUUID                                   # missing key
curl -si -H "Authorization: Bearer $KEY" $BASE/v1/admin/info                      # wrong scope
curl -si -H "Authorization: Bearer $KEY" "$BASE/v1/lol/summoners/by-puuid/EUW1/$PUUID"   # bad region (case)
for i in 1 2 3 4; do curl -si -H "Authorization: Bearer $KEY" $BASE/v1/static/versions | grep -i '^HTTP\|^x-ratelimit\|^retry-after'; done   # quota (set a 3/min key to see it quickly)
```

Captured:

```text
HTTP/1.1 401 Unauthorized
{"error":{"code":"UNAUTHORIZED","message":"Missing or invalid API key","requestId":"01M58N9D5KGNAK11BF9Z0B2SP1"}}

HTTP/1.1 403 Forbidden
{"error":{"code":"FORBIDDEN","message":"This key lacks the 'admin' scope","requestId":"01M58N9D5TA14QEYBQTCY9EM63"}}

HTTP/1.1 400 Bad Request
x-ratelimit-limit: 600
x-ratelimit-remaining: 599
x-ratelimit-reset: 60
{"error":{"code":"BAD_REGION","message":"params/platform must be equal to one of the allowed values","requestId":"01M58N9D6F8GENK3NHCFMDQ4TK"}}

HTTP/1.1 429 Too Many Requests
retry-after: 60
x-ratelimit-limit: 3
x-ratelimit-remaining: 0
x-ratelimit-reset: 60
{"error":{"code":"QUOTA_EXCEEDED","message":"Quota of 3/min exceeded","requestId":"01M58NAJ68D1A5HK99Q2PF0GCC","retryAfter":60}}
```

Note that 401 and 403 carry no `X-RateLimit-*` headers (refused before metering) while the 400 does. Every response carries `X-Request-Id`; send your own with `-H 'X-Request-Id: my-trace-123'` and it is echoed back (captured).

---
**Sources:** live capture against `target/debug/riot-proxy` 2.7.0 (`RIOT_API_KEY=RGAPI-docs-not-a-key`, port 18092, keys minted with `riot-proxy key create`), `README.md` "First run", `src/cli/mod.rs`, `src/routes/riot.rs:L127-L300`, `src/routes/lol.rs:L30-L405`, `src/routes/players.rs:L400-L671`, `src/routes/analytics.rs:L486-L614`, `src/routes/admin.rs:L180-L219,L454-L502,L838-L882,L1038,L1583-L1613,L1700-L1733,L2486`, `src/routes/ws.rs:L525-L572`, `src/ws/socket.rs:L63-L206`, `tests/snapshots/ws_hub__ws_session.snap`, `docs/_meta/scratch/routes.md`
**Last verified:** 2026-10-19 against commit `52fa67d`

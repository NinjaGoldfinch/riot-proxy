# riot-proxy

A self-hosted middleman API between Riot Games' public API and everything you
build on top of it. Downstream projects — websites, Discord bots, CLIs,
analytics — talk to this service, never to Riot.

Implements [`riot-proxy-spec_1.md`](docs/riot-proxy-spec.md) (Draft v1.0).

**Not endorsed by Riot Games.** riot-proxy isn't endorsed by Riot Games and
doesn't reflect the views or opinions of Riot Games or anyone officially
involved in producing or managing Riot Games properties.

---

## Why

|                              |                                                                                                                               |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| **Hide the Riot key**        | It exists in exactly one place: this service's environment. No client, browser or downstream project ever sees it.            |
| **Cache aggressively**       | Riot rate limits are the scarcest resource. Every avoidable upstream call is avoided.                                         |
| **Centralise rate limiting** | One shared limiter that understands Riot's application/method/service limits, instead of N projects re-implementing it badly. |
| **Stable internal contract** | When Riot deprecates an endpoint (as with summoner-by-name), you fix it once, here.                                           |

---

## Quick start

```bash
cp .env.example .env      # then set RIOT_API_KEY
docker compose up -d      # redis + postgres
npm install
npm run migrate
npm run dev               # api on :8080
npm run dev:worker        # polling, backfill, ddragon sync
```

Mint a key for your first consumer:

```bash
npm run key:create -- --name my-website
```

The plaintext key is printed once and never stored — only its sha256 is.

```bash
curl -H "Authorization: Bearer rpx_..." \
  http://localhost:8080/v1/riot/accounts/by-riot-id/europe/Faker/KR1
```

### Testing without a key

To poke at the API locally without minting a key first, set `AUTH_DISABLED=true`
in `.env` and restart. Every request then runs as a synthetic `dev-local`
consumer with `read` + `admin` scope, a 100k/min quota and no admin IP
allowlist — including the `/v1/ws` handshake.

```bash
curl http://localhost:8080/v1/riot/accounts/by-riot-id/europe/Faker/KR1
```

This is a development convenience and the service **refuses to start** with it
set while `NODE_ENV=production`. The test suite pins it off, so the auth tests
keep asserting real rejections.

---

## Consumer guide

### Base URL and auth

```
https://api.yourdomain.dev/v1
Authorization: Bearer rpx_<32 chars>
```

Every request needs a key except `/healthz` and `/metrics`. Keys carry scopes
(`read`, `admin`) and a per-minute quota, both set when the key is created.
For local testing the key check can be turned off entirely — see
[Testing without a key](#testing-without-a-key).

### Response headers

| Header        | Meaning                                                                           |
| ------------- | --------------------------------------------------------------------------------- |
| `X-Cache`     | `HIT` · `MISS` · `HIT-NEG` (cached 404) · `STALE` (served stale while refreshing) |
| `X-Cache-Age` | Age of the served payload, in seconds                                             |
| `Retry-After` | Present on `RATE_LIMITED` and `QUOTA_EXCEEDED`                                    |

Successful responses are Riot's payloads, passed through unmodified.

### Errors

One envelope, always:

```json
{ "error": { "code": "RATE_LIMITED", "message": "…", "retryAfter": 3 } }
```

| Code             | Status | Meaning                                                         |
| ---------------- | ------ | --------------------------------------------------------------- |
| `UNAUTHORIZED`   | 401    | Missing, unknown or disabled key                                |
| `FORBIDDEN`      | 403    | Key lacks the required scope, or admin IP not allowlisted       |
| `QUOTA_EXCEEDED` | 429    | Your consumer quota, not Riot's — retry after `retryAfter`      |
| `NOT_FOUND`      | 404    | Riot returned 404 (also served from the negative cache)         |
| `BAD_REGION`     | 400    | Unknown platform/region, or a match ID that belongs elsewhere   |
| `VALIDATION`     | 400    | Failed a schema constraint (`count > 100`, bad Riot ID length…) |
| `RATE_LIMITED`   | 503    | The upstream limiter wait exceeded your request budget          |
| `UPSTREAM_ERROR` | 502    | Riot failed, or rejected our key — never your problem to fix    |

### Endpoints

| Method & path                                                    | Upstream            | Notes                                                                               |
| ---------------------------------------------------------------- | ------------------- | ----------------------------------------------------------------------------------- |
| `GET /v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}` | account-v1          | Canonical entry point. `sea` is accepted and routed to `asia` (see below)           |
| `GET /v1/riot/accounts/by-puuid/{region}/{puuid}`                | account-v1          |                                                                                     |
| `GET /v1/lol/summoners/by-puuid/{platform}/{puuid}`              | summoner-v4         |                                                                                     |
| `GET /v1/lol/league/entries/by-puuid/{platform}/{puuid}`         | league-v4           |                                                                                     |
| `GET /v1/lol/matches/ids/{region}/{puuid}`                       | match-v5            | `?start&count&queue&type&startTime&endTime`, `count ≤ 100`                          |
| `GET /v1/lol/matches/{region}/{matchId}`                         | match-v5            | Served from the Postgres archive when present                                       |
| `GET /v1/lol/matches/{region}/{matchId}/timeline`                | match-v5            | Archived likewise                                                                   |
| `GET /v1/lol/spectator/active/{platform}/{puuid}`                | spectator-v5        | 404 = not in game, negative-cached 30 s                                             |
| `GET /v1/lol/mastery/by-puuid/{platform}/{puuid}`                | champion-mastery-v4 | `?top=N` for the top-N variant                                                      |
| `GET /v1/lol/rotations/{platform}`                               | champion-rotations  |                                                                                     |
| `GET /v1/lol/status/{platform}`                                  | lol-status-v4       |                                                                                     |
| `GET /v1/players/{puuid}/profile`                                | **composite**       | `?platform&topMastery` — account + summoner + league + mastery in one call          |
| `GET /v1/static/versions`                                        | local mirror        | No upstream call                                                                    |
| `GET /v1/static/{file}`                                          | local mirror        | `champions`, `items`, `runes`, `summoner-spells`, `profile-icons`, `maps`, `queues` |
| `WS /v1/ws`                                                      | realtime            | See below                                                                           |
| `GET /healthz` · `/readyz` · `/metrics`                          | —                   | Public                                                                              |

**Platforms:** `na1` `br1` `la1` `la2` `euw1` `eun1` `tr1` `ru` `kr` `jp1`
`oc1` `ph2` `sg2` `th2` `tw2` `vn2`.
**Regions:** `americas` `europe` `asia` `sea`.

Riot serves account-v1 on `americas`, `asia` and `europe` only — there is no
account-v1 on the `sea` host, and asking for one there is a 404. The proxy
routes any `sea` account lookup to `asia` for you, so a SEA platform such as
`oc1` resolves by Riot ID without the caller having to know this. account-v1 is
a global service, so the answer is identical whichever host serves it. match-v5
is unaffected: `sea` is a real match host and SEA matches stay there.

Match IDs carry their own platform prefix (`EUW1_7381937461`); pass a region
that disagrees with it and you get `BAD_REGION` rather than a confusing 404.

### The composite profile endpoint

Fans out to four Riot calls concurrently, each individually cached, and returns
one document. A part that fails is `null` and explained in `warnings[]` — a
mastery timeout never fails the whole response.

```json
{
  "puuid": "…", "platform": "euw1", "region": "europe",
  "account": { … }, "summoner": { … }, "league": [ … ], "mastery": [ … ],
  "warnings": []
}
```

### WebSocket

```
wss://api.yourdomain.dev/v1/ws?token=rpx_…
```

```jsonc
// client → server
{ "op": "subscribe",   "topics": ["player:<puuid>", "patch"] }
{ "op": "unsubscribe", "topics": ["player:<puuid>"] }
{ "op": "ping" }
```

```jsonc
// server → client
{ "op": "ready", "consumer": "my-website" }
{ "op": "subscribed", "topics": ["player:…"] }
{ "event": "game.started", "topic": "player:…", "at": 1730000000000, "data": { … } }
```

| Event            | Payload                                            |
| ---------------- | -------------------------------------------------- |
| `game.started`   | `{ puuid, platform, gameId, championId, queueId }` |
| `game.ended`     | `{ puuid, gameId }`                                |
| `rank.changed`   | `{ puuid, queue, before, after }`                  |
| `match.archived` | `{ puuid, matchId }`                               |
| `patch.new`      | `{ version }`                                      |

Events only reach a player topic if that player is **tracked** (see admin
below) — Riot has no webhooks, so everything realtime is poll-derived.
The server pings every 30 s and drops a socket after two missed pongs.

### Admin

Requires an `admin`-scoped key, and a source IP in `ADMIN_IP_ALLOWLIST` when
that is set.

| Endpoint                                    | Purpose                                                                |
| ------------------------------------------- | ---------------------------------------------------------------------- |
| `POST/GET/DELETE /v1/admin/consumers[/:id]` | Issue, list and revoke consumer keys                                   |
| `GET/POST /v1/admin/tracked-players`        | List, or start tracking (by PUUID or Riot ID)                          |
| `DELETE /v1/admin/tracked-players/:puuid`   | Stop tracking                                                          |
| `POST /v1/admin/cache/purge`                | `{ "pattern": "summoner.byPuuid:*" }`, scoped to the current key scope |
| `POST /v1/admin/backfill`                   | Walk a player's match history into the archive                         |
| `POST /v1/admin/ddragon/sync`               | Force a Data Dragon re-sync                                            |
| `GET /v1/admin/limits/:scope`               | Current bucket usage and freeze state                                  |
| `GET /v1/admin/stats`                       | Archive size, tracked player count, key scope                          |
| `GET /v1/admin/debug/riot`                  | Raw passthrough for manual testing                                     |

---

## How it works

### Request lifecycle

```
client → auth (Redis-cached) → quota
       → Postgres archive        (immutable data only — matches, timelines)
       → negative cache          (cached 404s)
       → Redis cache             (fresh → HIT, stale → STALE + background refresh)
       → single-flight           (concurrent identical misses share one call)
       → rate limiter            (token buckets per region and method)
       → Riot                    → cache write → archive write
```

Implemented in [`src/fetcher.ts`](src/fetcher.ts); every public route funnels
through it, so the caching, coalescing and limiting guarantees hold uniformly.

### Rate limiting

Bucket sizes are **never hardcoded**. Every upstream response's
`X-App-Rate-Limit` / `X-Method-Rate-Limit` headers reconfigure the buckets, and
the `-Count` headers sync our counters so usage by _other_ tooling on the same
key is absorbed rather than ignored.

Acquisition is a single Redis Lua script ([`src/riot/limiter-scripts.ts`](src/riot/limiter-scripts.ts)):
it checks the frozen-scope marker, enforces the bulk usage ceiling, then takes
one token from every applicable window — rolling back on the first refusal so a
partial acquisition can never leak tokens. That atomicity is what stops N api
replicas over-committing a bucket.

Two priorities: `interactive` (client requests) and `bulk` (worker backfill).
Bulk stands aside while interactive requests are queued, and refuses to push any
bucket past `BULK_USAGE_CEILING` (default 75 %), which keeps user latency flat
during large backfills.

On a 429: with `Retry-After` and a type header, the whole scope freezes until
the deadline — and an `application`/`method` 429 is logged as an error, because
it means our accounting was wrong. Without a type header, an underlying service
limited us, so we back off with jitter and leave the buckets alone.

### The `key_scope` rule

Encrypted IDs (PUUID, summonerId) are unique **per Riot API key**.
`KEY_SCOPE = sha256(RIOT_API_KEY).slice(0, 8)` is derived at boot and included
in every Redis cache key and every Postgres row holding an encrypted ID. Rotate
the key and the old IDs are stranded automatically instead of silently poisoning
lookups. Match IDs are not encrypted, so the archive survives key rotation.

### Caching

| Endpoint                   | TTL                        |
| -------------------------- | -------------------------- |
| Match / timeline           | forever (Postgres archive) |
| Match ID list              | 120 s                      |
| Account (either direction) | 24 h                       |
| Summoner                   | 3600 s                     |
| League entries             | 300 s                      |
| Spectator                  | 30 s (+ 30 s negative)     |
| Champion mastery           | 3600 s                     |
| Champion rotations         | 6 h                        |
| Platform status            | 60 s                       |

Override with `CACHE_TTL_OVERRIDES=league=120,spectator=20`.

Payloads carry a soft TTL (freshness) and a hard TTL (soft × 4). Between the
two the value is served with `X-Cache: STALE` while a background refresh runs at
bulk priority. Stale values are also served when Riot 5xxs, rather than failing
the request.

Spectator 404s are the single biggest quota waster, so they are negative-cached
under a distinct `neg:` prefix — a cached "not in game" is distinguishable from
"we don't know".

### Background jobs

| Job               | Schedule                           | Action                                           |
| ----------------- | ---------------------------------- | ------------------------------------------------ |
| `poll:live`       | every `TRACK_POLL_LIVE_S` (60 s)   | spectator-v5 → `game.started` / `game.ended`     |
| `poll:rank`       | every `TRACK_POLL_RANK_S` (600 s)  | league-v4 → `rank.changed`                       |
| `poll:matches`    | every `TRACK_POLL_MATCH_S` (300 s) | new match IDs → `archive:match`                  |
| `archive:match`   | on demand                          | fetch, upsert, `match.archived`                  |
| `backfill:player` | admin                              | page 100 IDs at a time, bulk priority            |
| `ddragon:sync`    | hourly                             | on a new patch, mirror data and emit `patch.new` |
| `maintenance`     | daily                              | clear orphaned single-flight locks               |

Each poll type is one repeatable tick that fans out to one job per tracked
player, so adding or removing a tracked player needs no scheduler changes. All
jobs are idempotent and run at bulk priority.

---

## Configuration

| Env var                                      | Default                                           | Notes                                                   |
| -------------------------------------------- | ------------------------------------------------- | ------------------------------------------------------- |
| `RIOT_API_KEY`                               | —                                                 | **Required.** The secret this project exists to protect |
| `RIOT_USER_AGENT`                            | `riot-proxy/1.0 …`                                | Sent upstream so Riot can identify you                  |
| `PORT` / `HOST`                              | `8080` / `0.0.0.0`                                |                                                         |
| `REDIS_URL`                                  | `redis://localhost:6379`                          |                                                         |
| `DATABASE_URL`                               | `postgres://proxy:proxy@localhost:5432/riotproxy` |                                                         |
| `DEFAULT_PLATFORM`                           | `euw1`                                            | Used by the composite endpoint                          |
| `CACHE_TTL_OVERRIDES`                        | —                                                 | CSV, e.g. `league=120,spectator=20`                     |
| `NEG_TTL_SECONDS`                            | `30`                                              | Spectator 404s                                          |
| `NEG_TTL_ACCOUNT_SECONDS`                    | `300`                                             | Typo'd Riot IDs                                         |
| `SF_LOCK_MS`                                 | `5000`                                            | Cross-instance single-flight lock                       |
| `CLIENT_WAIT_BUDGET_MS`                      | `2000`                                            | Max limiter wait for a client request                   |
| `BULK_USAGE_CEILING`                         | `0.75`                                            | Bulk work stops here                                    |
| `STALE_WHILE_REVALIDATE`                     | `true`                                            |                                                         |
| `TRACK_POLL_LIVE_S` / `_RANK_S` / `_MATCH_S` | `60` / `600` / `300`                              |                                                         |
| `DDRAGON_SYNC_S`                             | `3600`                                            | Version check cadence                                   |
| `DDRAGON_DIR` / `DDRAGON_LOCALE`             | `./data/ddragon` / `en_US`                        |                                                         |
| `ARCHIVE_TIMELINES`                          | `false`                                           | Timelines are large; opt in                             |
| `ADMIN_IP_ALLOWLIST`                         | —                                                 | CSV of IPs/CIDRs; empty means key scope is enough       |
| `BOOTSTRAP_ADMIN_KEY`                        | —                                                 | Seeds one admin key on `npm run migrate`                |
| `AUTH_DISABLED`                              | `false`                                           | Dev only: skip key checks; refused in production        |
| `LOG_LEVEL`                                  | `info`                                            |                                                         |

`KEY_SCOPE` is derived, not configured.

---

## Deployment

```bash
cp .env.example .env      # set RIOT_API_KEY, CADDY_DOMAIN, POSTGRES_PASSWORD
docker compose -f docker-compose.prod.yml up -d
```

Five services per the spec: `api`, `worker`, `redis` (AOF persistence on),
`postgres` (volume + nightly `pg_dump`, 14-day retention), `caddy` (TLS and
static Data Dragon). Migrations run as a one-shot `migrate` service.

Redis persistence matters more than it looks: losing limiter state on a restart
means a burst of requests against buckets Riot still considers full.

Observability: import [`ops/grafana/riot-proxy-dashboard.json`](ops/grafana/riot-proxy-dashboard.json)
and load [`ops/prometheus-alerts.yml`](ops/prometheus-alerts.yml). The alerts
that matter are any 401/403 from Riot, any `application`-type 429, and hit ratio
below 70 %.

### Rotating to a production key

1. Apply for a personal/production key on the Riot developer portal.
2. Change `RIOT_API_KEY` and restart — no code change (`KEY_SCOPE` is derived).
3. Old encrypted IDs are stranded automatically. Re-resolve tracked players by
   Riot ID via `POST /v1/admin/tracked-players`; the match archive is unaffected.

Development keys expire every 24 hours. A `RiotKeyRejected` alert on a dev key
usually just means yesterday's key.

---

## Development

```bash
npm run dev           # api, watch mode
npm run dev:worker    # worker, watch mode
npm test              # unit + integration, no Riot key needed
npm run lint          # eslint
npm run typecheck     # tsc --noEmit
npm run format        # prettier
```

Tests that need Redis or Postgres skip themselves when those are unreachable, so
`npm test` works with nothing running — but `docker compose up -d` first gets
you the limiter, HTTP-surface and WebSocket suites too.

### Acceptance checks

`npm test` stubs every upstream call. The checks that can only be answered by
the real Riot API live in `acceptance/` and are run separately:

```bash
ACCEPTANCE_RIOT_ID='Name#TAG' ACCEPTANCE_PLATFORM=oc1 npm run test:acceptance
```

They reuse an api and worker already listening on `PORT`, and start their own
pair if nothing is. Without a real `RIOT_API_KEY` and an `ACCEPTANCE_RIOT_ID`
the whole suite skips itself with a printed reason, so it is safe to run
anywhere.

| Phase | File                         | What it proves                                                                                                                                                      |
| ----- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1     | `phase1-lookups.test.ts`     | Riot ID resolves, account-v1 never dispatches to a host that lacks it, match by id, archived re-read costs no upstream call, `BAD_REGION` on a contradictory region |
| 2     | `phase2-rate-limits.test.ts` | A burst on one method takes zero `application`/`method` 429s, the scope is never frozen, and p95 limiter wait fits the buckets the limiter learned                  |
| 5     | `phase5-archive.test.ts`     | A backfill archives what it fetches, `proxy_upstream_requests_total` stays flat on re-request, interactive p95 holds up while bulk work runs                        |
| 6     | `phase6-events.test.ts`      | A SEA player tracks by Riot ID, the poll tick fans out jobs the queue accepts, and an event reaches a subscribed socket on its topic only                           |

| Variable                     | Default                  |                                              |
| ---------------------------- | ------------------------ | -------------------------------------------- |
| `ACCEPTANCE_RIOT_ID`         | —                        | Required, as `Name#TAG`                      |
| `ACCEPTANCE_PLATFORM`        | `oc1`                    | Platform routing value                       |
| `ACCEPTANCE_BASE_URL`        | `http://127.0.0.1:$PORT` | Target proxy                                 |
| `ACCEPTANCE_API_KEY`         | —                        | Consumer key; omit when `AUTH_DISABLED=true` |
| `ACCEPTANCE_PHASE2_REQUESTS` | `60`                     | Burst size — the spec asks for 500           |
| `ACCEPTANCE_BACKFILL_LIMIT`  | `40`                     | Matches to walk back                         |
| `ACCEPTANCE_LIVE_GAME`       | off                      | Opt in to the live game check                |
| `ACCEPTANCE_KEEP_TRACKED`    | off                      | Leave the player tracked afterwards          |

Two things the suite cannot decide for you:

- **Phase 2 measures nothing under the default `CLIENT_WAIT_BUDGET_MS=2000`.**
  Callers get shed instead of queued, so raise it (the CI job uses 300 s) to
  reproduce the spec's "500 requests, all served".
- **Phase 5 is inconclusive against a warm archive.** If every match is already
  stored the backfill has no work to pace, and the run says so rather than
  reporting a green tick — use a fresh database, or a limit past the archived
  depth.

Phase 6's real gate needs a human in a real game, so it is split: the tracking,
fan-out and delivery path is asserted automatically, and the game itself is
opt-in.

```bash
ACCEPTANCE_LIVE_GAME=1 ACCEPTANCE_RIOT_ID='Name#TAG' npm run test:acceptance
```

That waits up to 30 minutes for `game.started`, then for the `match.archived`
that follows it. CI runs everything else nightly (`.github/workflows/acceptance.yml`),
and skips itself when no `RIOT_API_KEY` secret is configured.

### Layout

```
src/
├─ index.ts        api entry            ├─ cache/
├─ worker.ts       worker entry         │  ├─ keys.ts        canonical cache key
├─ config.ts       env + KEY_SCOPE      │  ├─ store.ts       get/set/neg, soft+hard TTL
├─ app.ts          Fastify assembly     │  └─ singleflight.ts
├─ fetcher.ts      the read path        ├─ routes/           one file per endpoint group
├─ errors.ts       envelope + RiotError ├─ db/               drizzle schema + migrations
├─ logger.ts       pino + key redaction ├─ jobs/             BullMQ queues + processors
├─ metrics.ts      prom-client          ├─ ws/               realtime hub
├─ events/         topics + pub/sub     ├─ static/           Data Dragon mirror
└─ riot/                                └─ auth/             consumer auth + scopes
   ├─ routing.ts   platform ↔ region
   ├─ endpoints.ts method ids, URLs, TTLs
   ├─ client.ts    undici pools + §5.5 error policy
   └─ limiter.ts   header-driven token buckets

test/              unit + integration, every upstream call stubbed
acceptance/        live checks against the real Riot API (opt-in)
```

---

## Compliance

- Register the app on the [Riot developer portal](https://developer.riotgames.com/)
  for a personal or production key before any public use.
- Don't resell Riot data; respect `Retry-After`; keep an identifiable
  `User-Agent`.
- Consuming products must carry the "not endorsed by Riot Games" disclaimer.

## Licence

Private. Not for redistribution.

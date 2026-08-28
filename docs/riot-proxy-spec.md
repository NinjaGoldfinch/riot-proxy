# Riot API Middleman Service — Technical Specification & Implementation Guide

| | |
|---|---|
| **Project codename** | `riot-proxy` |
| **Status** | Draft v1.0 |
| **Last updated** | 2026-08-29 |
| **Scope** | League of Legends (primary), extensible to TFT/Valorant |
| **Audience** | Solo developer / small team |

---

## Table of contents

1. [Overview & goals](#1-overview--goals)
2. [Requirements](#2-requirements)
3. [System architecture](#3-system-architecture)
4. [Tech stack & dependencies](#4-tech-stack--dependencies)
5. [Riot API reference](#5-riot-api-reference)
6. [Proxy API design (your public surface)](#6-proxy-api-design)
7. [Data layer](#7-data-layer)
8. [Caching strategy](#8-caching-strategy)
9. [Rate limiting design](#9-rate-limiting-design)
10. [Background worker & jobs](#10-background-worker--jobs)
11. [WebSocket / realtime layer](#11-websocket--realtime-layer)
12. [Security](#12-security)
13. [Observability](#13-observability)
14. [Configuration reference](#14-configuration-reference)
15. [Implementation guide (step-by-step)](#15-implementation-guide)
16. [Appendix A — important values quick reference](#appendix-a)
17. [Appendix B — external documentation links](#appendix-b)

---

## 1. Overview & goals

A self-hosted middleman API that sits between Riot Games' public API and all of your future
projects (websites, Discord bots, tools, analytics). Downstream projects never talk to Riot
directly — they talk to this service.

### Primary goals

1. **Hide the Riot API key.** The key exists in exactly one place: this service's environment.
   No client, browser, or downstream project ever sees it.
2. **Cache aggressively.** Riot rate limits are the scarcest resource in the system. Every
   upstream call that can be avoided must be avoided.
3. **Centralize rate limiting.** One shared limiter that understands Riot's
   application/method/service limits, instead of every project re-implementing it badly.
4. **Provide a stable internal contract.** When Riot deprecates an endpoint (as happened with
   summoner by-name), you fix it once here instead of in N projects.

### Non-goals (v1)

- Multi-tenant SaaS for other developers (design allows it later, don't build it now)
- Full mirror of every Riot endpoint — only wrap what you use
- Tournament API / RSO OAuth flows
- Serving Data Dragon *through* the proxy (assets are synced and served statically instead)

---

## 2. Requirements

### 2.1 Functional requirements

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-1 | Expose REST endpoints mirroring the Riot endpoints listed in §5.3, namespaced under `/v1/lol/...` and `/v1/riot/...` | Must |
| FR-2 | Resolve players by Riot ID (`gameName` + `tagLine`) via account-v1; never depend on deprecated summoner-name lookup | Must |
| FR-3 | Cache all upstream responses in Redis with per-endpoint TTLs (§8.2) | Must |
| FR-4 | Negative-cache upstream 404s (§8.3) | Must |
| FR-5 | Coalesce concurrent identical upstream requests into a single upstream call (§8.4) | Must |
| FR-6 | Enforce Riot application + method rate limits per region, shared across all service instances | Must |
| FR-7 | Authenticate downstream consumers with per-project API keys | Must |
| FR-8 | Archive completed match JSON permanently in Postgres; serve archived matches without touching Riot | Must |
| FR-9 | Track a configurable list of players: poll rank, live-game status, and new matches on a schedule | Should |
| FR-10 | Push realtime events (game started, game ended, rank changed, new match archived) to WebSocket subscribers | Should |
| FR-11 | Sync Data Dragon static data (champions, items, runes, versions) on new patch and expose it locally | Should |
| FR-12 | Expose service health (`/healthz`) and Prometheus metrics (`/metrics`) | Must |
| FR-13 | Per-consumer request quotas independent of Riot's limits | Should |
| FR-14 | Admin endpoints: cache purge by key pattern, tracked-player CRUD, consumer key CRUD | Should |

### 2.2 Non-functional requirements

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-1 | Cache-hit latency (p95) | < 15 ms in-region |
| NFR-2 | Cache-miss latency overhead vs direct Riot call (p95) | < 25 ms added |
| NFR-3 | Zero Riot 429s attributable to the proxy exceeding known buckets | 0 under steady load |
| NFR-4 | Cache hit rate after warm-up (mixed workload) | > 85 % |
| NFR-5 | Availability | Best effort; single-node acceptable for v1 |
| NFR-6 | Riot key rotation | Env change + restart, < 1 min, no code change |
| NFR-7 | Horizontal scalability | Stateless API processes; all shared state in Redis/Postgres |
| NFR-8 | Riot ToS compliance | No data resale, respect Retry-After, identifiable User-Agent |

### 2.3 Constraints & assumptions

- **Development key** limits (assume until a production key is approved): `20 req / 1 s` and
  `100 req / 2 min` per region, expiring every 24 h — dev keys must be re-generated daily.
- **Personal production key** (after app approval) is typically far higher; the limiter must
  read actual limits from response headers rather than hardcoding them (§9.1).
- Encrypted IDs (PUUID, summonerId) are **unique per API key**. Changing keys invalidates every
  stored encrypted ID (§7.4). Cache/DB rows must be namespaced by a `key_scope` value.
- Riot provides **no webhooks**; all realtime behavior is poll-based.

---

## 3. System architecture

### 3.1 Components

```
 ┌──────────────────────────────┐
 │  Your projects               │  web apps, Discord bots, CLIs
 │  (hold a proxy API key only) │
 └──────────────┬───────────────┘
                │ HTTPS + WS
 ┌──────────────▼───────────────┐
 │  Gateway (Fastify)           │  auth, validation, routing, metrics
 ├──────────────────────────────┤
 │  Cache layer                 │  Redis GET → single-flight → upstream
 ├──────────────────────────────┤
 │  Rate limiter                │  token buckets per (region, method)
 └──────┬───────────────┬───────┘
        │               │
 ┌──────▼──────┐ ┌──────▼──────────┐        ┌─────────────────┐
 │  Riot API   │ │  Postgres       │◄───────┤  Worker (BullMQ)│
 │  (upstream) │ │  match archive, │        │  polling,       │
 └─────────────┘ │  players, keys  │        │  backfill, ddragon
                 └─────────────────┘        └───────┬─────────┘
                                                    │ Redis pub/sub
                                            ┌───────▼─────────┐
                                            │  WS gateway     │→ clients
                                            └─────────────────┘
```

### 3.2 Request lifecycle (read path)

1. Client calls `GET /v1/lol/summoners/by-riot-id/euw/Faker/KR1` with `Authorization: Bearer <proxy key>`.
2. Gateway authenticates the key (Redis-cached lookup of hashed key), checks the consumer quota.
3. Route handler builds the canonical cache key (§8.1) and checks Redis.
   - **Hit** → return immediately with `X-Cache: HIT`.
   - **Negative hit** (cached 404) → return 404 with `X-Cache: HIT-NEG`.
4. **Miss** → acquire single-flight slot for this cache key. If another request already holds
   it, await its result instead of calling upstream.
5. Slot holder acquires tokens from the rate limiter for `(region, method)` — blocks/queues if
   the bucket is empty, honoring `Retry-After` state.
6. Upstream call via undici pooled client → parse → write to Redis with the endpoint TTL →
   (for immutable data) enqueue Postgres archive job → return with `X-Cache: MISS`.

### 3.3 Process layout

| Process | Responsibility | Scaling |
|---------|----------------|---------|
| `api` | HTTP + WS surface, cache reads, upstream fetches | 1..N stateless replicas |
| `worker` | BullMQ consumers: polling, backfill, ddragon sync, archive writes | 1 (v1) |
| `redis` | cache, limiter state, queues, pub/sub | 1 |
| `postgres` | durable storage | 1 |
| `caddy`/`nginx` | TLS termination, static Data Dragon assets | 1 |

Deployment target for v1: a single VPS running Docker Compose with the five services above.

---

## 4. Tech stack & dependencies

### 4.1 Stack decisions

| Concern | Choice | Rationale |
|---------|--------|-----------|
| Language | TypeScript (Node 26, Current — LTS Oct 2026; use Node 24 LTS if you prefer max stability) | Pure-I/O workload; shared types with downstream TS projects |
| HTTP framework | Fastify 5 (latest stable, 5.12.x; v6 is alpha-only — do not use yet) | Schema validation, hooks, pino built in, fast JSON serialization |
| HTTP client | undici | Connection pooling/keep-alive across ~20 Riot hosts |
| Cache / coordination | Redis 8 (8.10 stable) | Cache + limiter + queues + pub/sub in one dependency |
| Database | PostgreSQL 18 (18.6; PG 19 still beta) | JSONB match archive + relational consumer/player tables |
| Jobs | BullMQ | Redis-backed, repeatable jobs, backoff, rate-limited queues |
| Realtime | @fastify/websocket + Redis pub/sub | No webhooks upstream; fan-out is ours |
| Alternative stack | Go + chi + go-redis + pgx | Pick if single-binary deploys matter more than TS type sharing |

> Versions verified current as of 2026-08-29. Node 26 entered the Current line in May 2026 and becomes LTS in October 2026; Fastify v6 exists only as alpha pre-releases; Redis 8.10 and PostgreSQL 18.6 are the latest stable lines. Re-check before starting the build.

### 4.2 npm dependencies

| Package | Purpose |
|---------|---------|
| `fastify` | HTTP server |
| `@fastify/websocket` | WS endpoint |
| `@fastify/rate-limit` | per-consumer quotas (not Riot limits) |
| `undici` | upstream HTTP with pooling |
| `ioredis` | Redis client |
| `@fightmegg/riot-rate-limiter` | Riot header-aware limiter with ioredis datastore (or custom Lua, §9) |
| `@sinclair/typebox` | route schemas + static types |
| `drizzle-orm`, `postgres` | Postgres access + migrations (`drizzle-kit`) |
| `bullmq` | job queues |
| `pino`, `pino-pretty` (dev) | logging |
| `prom-client` | Prometheus metrics |
| `dotenv` | local env loading |
| dev: `typescript`, `tsx`, `vitest`, `msw`, `eslint`, `prettier` | tooling & tests |

---

## 5. Riot API reference

### 5.1 Host routing

Two host families. Getting this wrong is the most common integration bug.

**Platform hosts** — `https://{platform}.api.riotgames.com`

| Platform | Region | Platform | Region |
|----------|--------|----------|--------|
| `na1` | North America | `euw1` | EU West |
| `eun1` | EU Nordic & East | `kr` | Korea |
| `br1` | Brazil | `jp1` | Japan |
| `la1` / `la2` | LATAM N / S | `oc1` | Oceania |
| `tr1` | Türkiye | `ru` | Russia |
| `ph2` `sg2` `th2` `tw2` `vn2` | SEA platforms | | |

**Regional hosts** — `https://{region}.api.riotgames.com`

| Region | Serves platforms |
|--------|------------------|
| `americas` | na1, br1, la1, la2 |
| `europe` | euw1, eun1, tr1, ru |
| `asia` | kr, jp1 |
| `sea` | oc1, ph2, sg2, th2, tw2, vn2 |

**Routing rule:** account-v1 and match-v5 use **regional** hosts; summoner-v4, league-v4,
spectator-v5, champion-mastery-v4, lol-status-v4 use **platform** hosts. Maintain a
`platform → region` map in one module (`src/riot/routing.ts`) and derive everything from it.

### 5.2 Authentication & headers

- Header: `X-Riot-Token: <RIOT_API_KEY>` on every upstream request.
- Set a real `User-Agent` identifying your app (helps if Riot support ever contacts you).
- Never place the key in query strings (it ends up in logs).

### 5.3 Endpoints wrapped in v1

| Riot endpoint | Host | Purpose |
|---------------|------|---------|
| `GET /riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}` | regional | Riot ID → PUUID (canonical entry point) |
| `GET /riot/account/v1/accounts/by-puuid/{puuid}` | regional | PUUID → current Riot ID |
| `GET /lol/summoner/v4/summoners/by-puuid/{puuid}` | platform | summoner level, icon, summonerId |
| `GET /lol/league/v4/entries/by-puuid/{puuid}` | platform | ranked entries (solo/flex) |
| `GET /lol/match/v5/matches/by-puuid/{puuid}/ids?start&count&queue&type` | regional | match ID list (max `count=100`) |
| `GET /lol/match/v5/matches/{matchId}` | regional | full match payload (immutable) |
| `GET /lol/match/v5/matches/{matchId}/timeline` | regional | frame-by-frame timeline (immutable, large) |
| `GET /lol/spectator/v5/active-games/by-summoner/{puuid}` | platform | live game (404 when not in game) |
| `GET /lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}` | platform | mastery list (`/top?count=N` variant too) |
| `GET /lol/platform/v3/champion-rotations` | platform | free rotation |
| `GET /lol/status/v4/platform-data` | platform | platform incidents |

Deprecated — do not build on: `/lol/summoner/v4/summoners/by-name/{name}` and the TFT
equivalent. Summoner names are replaced by Riot IDs.

### 5.4 Rate limiting semantics (upstream)

Three limit classes, all communicated via response headers:

| Header | Meaning |
|--------|---------|
| `X-App-Rate-Limit` | e.g. `20:1,100:120` → 20/1 s and 100/120 s app-wide per region |
| `X-App-Rate-Limit-Count` | current usage of each window |
| `X-Method-Rate-Limit` / `-Count` | same, but per endpoint method per region |
| `Retry-After` | seconds to wait after a 429 (when enforced by Riot's edge) |
| `X-Rate-Limit-Type` | `application` \| `method` \| `service` on a 429 |

Rules the limiter must implement:

1. Parse limits from headers on every response; never hardcode bucket sizes.
2. Buckets are per **(region, limit-window)** for app limits and per
   **(region, method, window)** for method limits. A request must acquire from *all* applicable
   buckets before dispatch.
3. On 429 with `Retry-After`: freeze the affected scope until the deadline.
4. On 429 **without** `X-Rate-Limit-Type` / `Retry-After`: an underlying service limited you —
   back off with jittered exponential retry (it is not your bucket accounting that failed).
5. `service`-type 429s are shared across all API consumers; treat like (4).

### 5.5 Upstream error handling policy

| Riot status | Proxy behavior |
|-------------|----------------|
| 200 | cache with endpoint TTL, return |
| 404 | negative-cache (§8.3), return 404 |
| 401 / 403 | do **not** retry; alert (key expired/revoked/blacklisted); return 502 to client |
| 429 | never propagate immediately; queue + retry per §5.4; if deadline > client budget (2 s), return 503 + `Retry-After` |
| 500 / 502 / 503 / 504 | retry up to 2× with jitter (250 ms, 750 ms); then 502 to client; serve stale cache if available (§8.5) |

### 5.6 Data Dragon / static data (not rate limited)

- Versions: `https://ddragon.leagueoflegends.com/api/versions.json`
- Champions: `https://ddragon.leagueoflegends.com/cdn/{ver}/data/en_US/champion.json`
- Items / runes / summoner spells: same CDN pattern; images under `/cdn/{ver}/img/...`
- CommunityDragon (`https://raw.communitydragon.org/`) for assets ddragon lacks.
- No API key, no meaningful rate limit. Sync to local storage/S3 on new patch (worker job)
  and serve statically — never through the limiter.

---

## 6. Proxy API design

### 6.1 Conventions

- Base URL: `https://api.yourdomain.dev`
- Versioned prefix: `/v1`
- Auth: `Authorization: Bearer rpx_<32 random chars>` (proxy-issued key, §12.1)
- Responses are Riot payloads passed through unmodified, plus proxy headers:
  - `X-Cache: HIT | MISS | HIT-NEG | STALE`
  - `X-Cache-Age: <seconds>`
- Errors use a single envelope: `{ "error": { "code": "RATE_LIMITED", "message": "...", "retryAfter": 3 } }`
- Error codes: `UNAUTHORIZED`, `QUOTA_EXCEEDED`, `NOT_FOUND`, `UPSTREAM_ERROR`, `RATE_LIMITED`, `BAD_REGION`, `VALIDATION`

### 6.2 Public endpoints (v1)

| Method & path | Maps to | Notes |
|---------------|---------|-------|
| `GET /v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}` | account-v1 by-riot-id | `region` = americas/europe/asia/sea |
| `GET /v1/riot/accounts/by-puuid/{region}/{puuid}` | account-v1 by-puuid | |
| `GET /v1/lol/summoners/by-puuid/{platform}/{puuid}` | summoner-v4 | |
| `GET /v1/lol/league/entries/by-puuid/{platform}/{puuid}` | league-v4 | |
| `GET /v1/lol/matches/ids/{region}/{puuid}?start&count&queue` | match-v5 ids | validate `count ≤ 100` |
| `GET /v1/lol/matches/{region}/{matchId}` | match-v5 match | served from Postgres archive when present |
| `GET /v1/lol/matches/{region}/{matchId}/timeline` | match-v5 timeline | archived likewise |
| `GET /v1/lol/spectator/active/{platform}/{puuid}` | spectator-v5 | 404 negative-cached |
| `GET /v1/lol/mastery/by-puuid/{platform}/{puuid}?top=N` | champion-mastery-v4 | |
| `GET /v1/lol/rotations/{platform}` | champion-rotations | |
| `GET /v1/lol/status/{platform}` | lol-status-v4 | |
| `GET /v1/static/versions` · `GET /v1/static/champions` … | local ddragon mirror | no upstream call |
| `GET /v1/players/{puuid}/profile` | **composite** | account + summoner + league + top mastery in one call (§6.3) |
| `WS /v1/ws` | realtime | §11 |
| `GET /healthz` · `GET /metrics` | — | liveness; Prometheus |

Admin (separate key scope): `POST/DELETE /v1/admin/tracked-players`, `POST /v1/admin/cache/purge`,
`POST/DELETE /v1/admin/consumers`.

### 6.3 Composite endpoints

The proxy's biggest ergonomic win: endpoints that fan out to several Riot calls server-side
(each individually cached) and return one merged document. Rules:

- Fan-out happens concurrently; per-part cache still applies.
- Partial failure returns the parts that succeeded plus a `warnings[]` array — never fail the
  whole composite because mastery timed out.

---

## 7. Data layer

### 7.1 PostgreSQL schema (DDL sketch)

```sql
CREATE TABLE consumers (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name          text NOT NULL,
  key_hash      text NOT NULL UNIQUE,        -- sha256 of the bearer token
  scopes        text[] NOT NULL DEFAULT '{read}',
  quota_per_min integer NOT NULL DEFAULT 600,
  created_at    timestamptz NOT NULL DEFAULT now(),
  disabled_at   timestamptz
);

CREATE TABLE players (
  puuid       text PRIMARY KEY,              -- namespaced: '<key_scope>:<puuid>' or add column
  key_scope   text NOT NULL,                 -- see §7.4
  platform    text NOT NULL,
  game_name   text,
  tag_line    text,
  tracked     boolean NOT NULL DEFAULT false,
  last_seen_match_id text,
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE matches (
  match_id    text PRIMARY KEY,              -- e.g. 'EUW1_7381937461'
  region      text NOT NULL,
  queue_id    integer GENERATED ALWAYS AS ((data->'info'->>'queueId')::int) STORED,
  game_end_ts bigint  GENERATED ALWAYS AS ((data->'info'->>'gameEndTimestamp')::bigint) STORED,
  data        jsonb NOT NULL,
  timeline    jsonb,                          -- nullable; fetched on demand
  fetched_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX matches_participants_gin ON matches
  USING gin ((data->'metadata'->'participants'));

CREATE TABLE match_participants (              -- optional denormalization for fast queries
  match_id  text REFERENCES matches(match_id),
  puuid     text NOT NULL,
  champion_id integer,
  win       boolean,
  PRIMARY KEY (match_id, puuid)
);
```

### 7.2 Redis key conventions

| Pattern | Content | TTL |
|---------|---------|-----|
| `c:{scope}:{endpoint}:{args-hash}` | cached JSON response | per §8.2 |
| `neg:{scope}:{endpoint}:{args-hash}` | `"404"` marker | 30–60 s |
| `sf:{cache-key}` | single-flight lock (SET NX PX) | 5 s |
| `rl:app:{region}:{window}` | app bucket state | window-scoped |
| `rl:m:{region}:{method}:{window}` | method bucket state | window-scoped |
| `q:{consumer}:{minute}` | consumer quota counter | 90 s |
| `auth:{key_hash}` | cached consumer row | 300 s |
| pub/sub `evt:{topic}` | realtime events | — |

### 7.3 What lives where

| Data | Store | Why |
|------|-------|-----|
| Hot responses | Redis | ephemeral, TTL-driven |
| Completed matches & timelines | Postgres | immutable forever; Riot should be hit once per match, ever |
| Consumers, tracked players | Postgres | relational, durable |
| Limiter state, locks, queues | Redis | shared across instances, fast |

### 7.4 The `key_scope` rule (critical)

Encrypted PUUIDs/summonerIds differ per Riot API key. Define
`KEY_SCOPE = sha256(RIOT_API_KEY).slice(0,8)` at boot and include it in every Redis cache key
and every Postgres row that stores an encrypted ID. When you move dev → production key, old
IDs become garbage automatically instead of silently poisoning lookups. Match IDs
(`EUW1_123...`) are **not** encrypted and need no scoping — the archive survives key changes.

---

## 8. Caching strategy

### 8.1 Canonical cache key

`c:{key_scope}:{riot-method-id}:{host}:{sha1(sorted path+query)}` — sort query params before
hashing so `?start=0&count=20` and `?count=20&start=0` collide correctly.

### 8.2 TTL table (tune later; start here)

| Endpoint | TTL | Notes |
|----------|-----|-------|
| match by id / timeline | ∞ (archive) | immutable once game ends |
| match ids by puuid | 120 s | new games appear |
| account by-riot-id | 24 h | riot IDs change rarely |
| account by-puuid | 24 h | |
| summoner by-puuid | 3600 s | level/icon churn is slow |
| league entries | 300 s | LP changes per game |
| spectator active-game | 30 s | + negative cache 30 s |
| champion mastery | 3600 s | |
| champion rotations | 6 h | changes weekly |
| lol-status | 60 s | |
| ddragon versions | 3600 s (worker poll) | new patch trigger |

### 8.3 Negative caching

Spectator 404s ("not in game") are the #1 quota waster. Cache the 404 for 30 s under a
distinct `neg:` key so a hit is distinguishable from "unknown". Also negative-cache
account-v1 404s (typo'd Riot IDs) for 300 s.

### 8.4 Single-flight (request coalescing)

Per instance: a `Map<cacheKey, Promise<Response>>` — concurrent identical misses await one
promise. Across instances: `SET sf:{key} 1 NX PX 5000`; losers poll the cache key (25 ms
interval, 2 s budget) before falling through to their own upstream call. Prevents the
"50 dashboard users load the same live match" stampede.

### 8.5 Stale-while-revalidate (optional, phase 7)

Store payloads with `soft_ttl` (freshness) and `hard_ttl = soft_ttl × 4`. Between soft and
hard expiry: serve the stale value with `X-Cache: STALE` and refresh in the background. Also
serve stale on upstream 5xx.

---

## 9. Rate limiting design

### 9.1 Header-driven token buckets

Do not hardcode `20/1s`. On every upstream response, parse `X-App-Rate-Limit` and
`X-Method-Rate-Limit` and (re)configure buckets; sync counts from the `-Count` headers to
absorb drift (e.g. calls made by other tooling with the same key).

### 9.2 Acquisition order

```
acquire(region, method):
  for each app window (region):        take 1 token   -- else queue
  for each method window (region, m):  take 1 token   -- else queue
  dispatch
```

Implement atomically in a single Redis Lua script (EVALSHA) so N instances can't over-commit
a bucket. `@fightmegg/riot-rate-limiter` with the ioredis datastore is an acceptable v1
substitute — swap for custom Lua only if it becomes a bottleneck.

### 9.3 Priorities & fairness

Two logical queues per region: `interactive` (client-facing requests) and `bulk` (worker
backfill). Bulk only consumes tokens when the interactive queue is empty and bucket usage is
< 75 %. This keeps user latency flat during large backfills.

### 9.4 429 handling

- `Retry-After` present → set `rl:frozen:{scope}` until deadline; all acquisitions wait.
- No `X-Rate-Limit-Type` → underlying-service limit: retry with jittered backoff
  (base 500 ms, ×2, max 8 s, max 3 tries); do not touch your bucket accounting.
- Log every 429 with scope + headers; a nonzero `application` 429 rate means the Lua
  accounting is wrong — treat as a bug, not weather.

---

## 10. Background worker & jobs

| Job (BullMQ) | Schedule | Action |
|--------------|----------|--------|
| `poll:live` | every 60 s per tracked player | spectator-v5; on transition publish `game.started` / `game.ended` |
| `poll:rank` | every 10 min per tracked player | league-v4; on change publish `rank.changed` |
| `poll:matches` | every 5 min per tracked player | match ids (count=5); new IDs → enqueue `archive:match` |
| `archive:match` | on demand | fetch match (+timeline if configured), upsert Postgres, publish `match.archived` |
| `backfill:player` | manual/admin | page match ids 100 at a time → `archive:match` (bulk priority) |
| `ddragon:sync` | hourly version check | on new version download data + assets, bust `/v1/static/*` cache, publish `patch.new` |
| `maintenance` | daily | prune negative keys, vacuum counters, rotate logs |

Worker rules: all jobs idempotent (upserts, `ON CONFLICT DO NOTHING`); every job passes
through the same limiter as the API, always at `bulk` priority.

---

## 11. WebSocket / realtime layer

- Endpoint: `WS /v1/ws?token=<proxy key>` (or first-message auth).
- Client → server: `{ "op": "subscribe", "topics": ["player:<puuid>", "patch"] }`
- Server → client events:

| Event | Payload |
|-------|---------|
| `game.started` | `{ puuid, platform, gameId, championId, queueId }` |
| `game.ended` | `{ puuid, gameId }` |
| `rank.changed` | `{ puuid, queue, before: {tier,rank,lp}, after: {...} }` |
| `match.archived` | `{ puuid, matchId }` |
| `patch.new` | `{ version }` |

- Transport: worker publishes to Redis channel `evt:{topic}`; every API instance relays to its
  local sockets. Heartbeat ping every 30 s; drop after 2 missed pongs.

---

## 12. Security

### 12.1 Consumer auth

- Keys look like `rpx_<32 chars from crypto.randomBytes>`; store only `sha256(key)`.
- Scopes: `read`, `admin`. Admin routes additionally IP-allowlisted.
- Per-consumer quota via `@fastify/rate-limit` backed by Redis (`quota_per_min` from DB).

### 12.2 Riot key hygiene

- `RIOT_API_KEY` from env only; never logged (add a pino redact rule), never in URLs,
  never in error messages returned to clients.
- Upstream errors are sanitized: clients see `UPSTREAM_ERROR`, not Riot's raw body.

### 12.3 Input validation

- TypeBox schemas on every route: platform/region against the closed enum from §5.1;
  `count` clamped to 1–100; Riot ID lengths (gameName 3–16, tagLine 3–5) enforced before any
  upstream call.

### 12.4 ToS compliance notes

- Register the app on the developer portal for a personal/production key before any public use.
- Do not expose raw proxied Riot data publicly without the required "not endorsed by Riot
  Games" disclaimer in the consuming product.

---

## 13. Observability

Prometheus metrics (minimum set):

| Metric | Type | Labels |
|--------|------|--------|
| `proxy_requests_total` | counter | route, status, cache (`hit/miss/neg/stale`) |
| `proxy_upstream_requests_total` | counter | region, method, status |
| `proxy_upstream_latency_seconds` | histogram | region, method |
| `proxy_rl_wait_seconds` | histogram | region, priority |
| `proxy_rl_429_total` | counter | region, type |
| `proxy_cache_hit_ratio` | gauge | — |
| `proxy_ws_connections` | gauge | — |

Log lines (pino, JSON): one per request with `consumer`, `route`, `cacheState`, `upstreamMs`,
`totalMs`. Alert on: any 401/403 from Riot, `application`-type 429s > 0, hit ratio < 70 %.

---

## 14. Configuration reference

| Env var | Example | Notes |
|---------|---------|-------|
| `RIOT_API_KEY` | `RGAPI-...` | the secret this whole project exists to protect |
| `PORT` | `8080` | api |
| `REDIS_URL` | `redis://redis:6379` | |
| `DATABASE_URL` | `postgres://proxy:...@postgres:5432/riotproxy` | |
| `KEY_SCOPE` | derived | `sha256(RIOT_API_KEY).slice(0,8)`, computed at boot |
| `DEFAULT_PLATFORM` | `euw1` | convenience default |
| `CACHE_TTL_OVERRIDES` | `league=120,spectator=20` | optional CSV of §8.2 overrides |
| `NEG_TTL_SECONDS` | `30` | |
| `SF_LOCK_MS` | `5000` | single-flight lock |
| `CLIENT_WAIT_BUDGET_MS` | `2000` | max time a client request may wait on the limiter |
| `BULK_USAGE_CEILING` | `0.75` | §9.3 |
| `TRACK_POLL_LIVE_S` / `_RANK_S` / `_MATCH_S` | `60` / `600` / `300` | worker cadence |
| `DDRAGON_DIR` | `/data/ddragon` | static asset root |
| `LOG_LEVEL` | `info` | |

---

## 15. Implementation guide

Each phase is shippable on its own and ends with acceptance criteria. Suggested repo layout:

```
riot-proxy/
├─ docker-compose.yml
├─ .env.example
├─ src/
│  ├─ index.ts            # api entry
│  ├─ worker.ts           # worker entry
│  ├─ config.ts           # env parsing (typebox), KEY_SCOPE derivation
│  ├─ riot/
│  │  ├─ routing.ts       # platform/region maps
│  │  ├─ client.ts        # undici pool + auth header + error policy (§5.5)
│  │  ├─ limiter.ts       # §9
│  │  └─ endpoints.ts     # method ids, URL builders, TTLs (§8.2 as code)
│  ├─ cache/
│  │  ├─ keys.ts          # §8.1
│  │  ├─ store.ts         # get/set/neg
│  │  └─ singleflight.ts  # §8.4
│  ├─ routes/             # one file per §6.2 group
│  ├─ db/                 # drizzle schema + migrations (§7.1)
│  ├─ jobs/               # BullMQ processors (§10)
│  └─ ws/                 # §11
└─ test/
```

### Phase 0 — Foundations (½ day)

1. `git init`, Node 26 (or 24 LTS), TypeScript strict, eslint/prettier, `tsx` dev runner.
2. `docker-compose.yml` with `redis:8-alpine`, `postgres:18-alpine`, volumes, healthchecks.
3. `config.ts`: parse env with TypeBox; fail fast on missing `RIOT_API_KEY`; derive `KEY_SCOPE`.
4. Fastify skeleton with `/healthz` and pino redaction for the key.

**Done when:** `docker compose up` + `npm run dev` serves `/healthz` = `{"ok":true}` and the
key never appears in logs even at trace level.

### Phase 1 — Riot client, routing, error policy (1 day)

1. `riot/routing.ts`: the §5.1 tables as `const` objects + `platformToRegion()`.
2. `riot/client.ts`: one undici `Pool` per host (lazy map); `X-Riot-Token` + `User-Agent`
   injected; implement the §5.5 status policy; typed `RiotError`.
3. `riot/endpoints.ts`: enum of method IDs → URL builder + TTL from §8.2.
4. Wire a temporary raw route `GET /debug/riot/*` (admin scope) for manual testing.

**Done when:** you can fetch your own account by Riot ID and a match by ID through the debug
route with a dev key; 404s and 401s surface as typed errors.

### Phase 2 — Rate limiter (1–2 days; the hard one)

1. Integrate `@fightmegg/riot-rate-limiter` with the ioredis datastore, wrapped behind your
   own `acquire(region, method)` interface so it's swappable.
2. Add the frozen-scope handling for `Retry-After` and the no-type-header backoff path (§9.4).
3. Add `proxy_rl_*` metrics.
4. Load test against the dev key: fire 500 requests at one method; expect zero upstream 429s
   of type `application`/`method` and orderly queuing.

**Done when:** the 500-request test completes with 0 accountable 429s and p95 wait time is
explainable by bucket math.

### Phase 3 — Cache + single-flight (1 day)

1. `cache/keys.ts` (§8.1), `cache/store.ts` (get/set + `neg:` handling), `singleflight.ts`.
2. Compose the read path: cache → single-flight → limiter → upstream → cache write.
3. Add `X-Cache` / `X-Cache-Age` headers and cache metrics.
4. Test: 100 concurrent identical requests ⇒ exactly 1 upstream call (assert with a counter).

**Done when:** the coalescing test passes and repeat requests show `HIT` under 15 ms locally.

### Phase 4 — Public routes + consumer auth (1–2 days)

1. Drizzle schema + migration for `consumers`; a small CLI (`npm run key:create -- --name web`)
   that prints a key once and stores the hash.
2. Auth preHandler: Bearer → sha256 → Redis-cached consumer lookup → scope check → quota
   check (`@fastify/rate-limit`, Redis store, `quota_per_min`).
3. Implement all §6.2 read routes with TypeBox schemas (platform/region enums, clamps).
4. Error envelope + error-code mapping from §6.1.

**Done when:** an unauthenticated request 401s; a valid key fetches summoner + league +
matches; a wrong platform 400s with `BAD_REGION`; quota exhaustion returns `QUOTA_EXCEEDED`.

### Phase 5 — Match archive (1 day)

1. `matches` table + generated columns (§7.1); archive-on-fetch: every match/timeline fetched
   through the proxy is upserted.
2. Route change: `GET /v1/lol/matches/...` checks Postgres before Redis/upstream.
3. `backfill:player` job (bulk priority) + admin trigger.

**Done when:** re-requesting an archived match performs zero Redis-miss upstream calls
(verify via `proxy_upstream_requests_total`), and a 200-match backfill completes without
starving interactive traffic.

### Phase 6 — Worker polling + WebSockets (2 days)

1. BullMQ repeatable jobs `poll:live`, `poll:rank`, `poll:matches` reading tracked players.
2. Transition detection (previous state in Redis) → publish events on `evt:*`.
3. `/v1/ws`: auth, subscribe protocol, Redis relay, heartbeat.
4. Admin CRUD for tracked players.

**Done when:** tracking your own account and starting a game emits `game.started` to a
subscribed WS client within one poll interval; ending it emits `game.ended` and, minutes
later, `match.archived`.

### Phase 7 — Static data + polish (1 day)

1. `ddragon:sync` job + `/v1/static/*` routes served from disk; `patch.new` event.
2. Composite `GET /v1/players/{puuid}/profile` with concurrent fan-out + `warnings[]`.
3. Stale-while-revalidate (§8.5) for league + summoner endpoints.

**Done when:** a new patch (or forced re-sync) refreshes champion data without any client
change; profile endpoint returns in ~1 upstream RTT when cold, ~10 ms warm.

### Phase 8 — Production readiness (1 day)

1. Compose file for the VPS: api, worker, redis (persistence on), postgres (volume +
   `pg_dump` cron), caddy (TLS + static ddragon).
2. Grafana dashboard from §13 metrics; alerts for 401/403, `application` 429s, hit ratio.
3. Apply for a **personal API key** on the Riot developer portal (product description +
   proxy URL); after approval, rotate the env key. `KEY_SCOPE` invalidates old encrypted
   IDs automatically — plan a re-resolve pass for tracked players.
4. Write the consumer README: base URL, auth, endpoint list, error codes, WS protocol.

**Done when:** the service survives a VPS reboot unattended, dashboards populate, and the
production key is live.

---

## Appendix A — important values quick reference

| Value | Default | Where defined |
|-------|---------|---------------|
| Dev key app limit | 20/1 s · 100/120 s per region | Riot (assume until headers say otherwise) |
| Match id list max `count` | 100 | Riot |
| Client limiter wait budget | 2000 ms | `CLIENT_WAIT_BUDGET_MS` |
| Single-flight lock | 5000 ms | `SF_LOCK_MS` |
| Negative-cache TTL | 30 s (spectator), 300 s (account) | `NEG_TTL_SECONDS` |
| Bulk bucket ceiling | 75 % | `BULK_USAGE_CEILING` |
| Live poll cadence | 60 s / tracked player | `TRACK_POLL_LIVE_S` |
| Rank poll cadence | 600 s | `TRACK_POLL_RANK_S` |
| Match poll cadence | 300 s | `TRACK_POLL_MATCH_S` |
| WS heartbeat | 30 s, drop after 2 missed | code |
| 5xx retry | 2 tries, 250/750 ms jitter | code |
| Consumer default quota | 600 req/min | `consumers.quota_per_min` |

## Appendix B — external documentation links

| Resource | URL |
|----------|-----|
| API reference (all endpoints) | https://developer.riotgames.com/apis |
| Rate limiting docs | https://developer.riotgames.com/docs/portal#web-apis_rate-limiting |
| LoL docs (routing, ddragon) | https://developer.riotgames.com/docs/lol |
| Riot ID migration notes | https://www.riotgames.com/en/DevRel/summoner-names-to-riot-id |
| Data Dragon versions | https://ddragon.leagueoflegends.com/api/versions.json |
| CommunityDragon | https://raw.communitydragon.org/ |
| Community docs (HexDocs) | https://hextechdocs.dev/ |
| Rate limiter lib | https://github.com/fightmegg/riot-rate-limiter |
| Third-party dev community | https://developer.riotgames.com/community |

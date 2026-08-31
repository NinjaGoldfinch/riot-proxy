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

Node 26 is required, not merely recommended — `npm install` refuses to run on
anything else. `.nvmrc` pins it, so `nvm use` (or your version manager's
equivalent) is enough; the Dockerfile and CI are on the same major.

```bash
nvm use                   # Node 26, per .nvmrc
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

### The browser client

`http://localhost:8080/dev` is a single self-contained page for looking at what
the API actually returns — summoner, ranked, top mastery and a paged match
history, with the proxy headers and the raw JSON alongside each section. A
profile is a URL you can paste:

```
http://localhost:8080/dev/NinjaGoldfinch-OCENZ?platform=oc1
```

It is served from this origin, so its `fetch` calls are same-origin and the
service needs no CORS layer. The page itself is public — a browser has nowhere
to put a key before it renders the field — and every `/v1` call it makes is
authenticated like any other; with `AUTH_DISABLED=true` it hides the field
entirely. Icons come straight from the Data Dragon CDN, which is not rate
limited (§5.6) and was never mirrored.

`DEV_UI` follows `NODE_ENV` unless you set it: on everywhere but production,
where a page that advertises whether auth is off has no business being. No build
step and no dependencies — edit `public/dev-ui.html` and reload.

It is not the API reference and does not try to be: this page answers "show me
this account", `/docs` answers "what does this route return". `DOCS_UI` defaults
on in production for exactly the reason `DEV_UI` defaults off.

---

## Using the API

The contract lives at **`/docs`** — a browsable reference with a working request
console, generated from the same schemas the server validates against. It cannot
describe an endpoint this build does not serve, or omit one it does, which is
what the table that used to live here could not promise. `/openapi.json` and
`/openapi.yaml` serve the document itself, for client generators and contract
tests.

```
https://api.yourdomain.dev/v1
Authorization: Bearer rpx_<32 chars>
```

Every request needs a key except `/healthz`, `/readyz` and `/metrics`. Keys
carry scopes (`read`, `admin`) and a per-minute quota, both set when the key is
issued. For local testing the check can be turned off entirely — see
[Testing without a key](#testing-without-a-key).

Start with the composites under `/v1/players`. They fan out to several Riot
calls concurrently and return one document, which is what a profile page
actually needs; the passthrough routes under `/v1/riot` and `/v1/lol` are the
escape hatch, not the front door. Realtime is a WebSocket at `/v1/ws`, and the
protocol is documented under the `ws` tag on the same page — OpenAPI cannot
express a socket, so it is written out as prose there rather than faked as an
operation.

Three things surprise people, and all three are in the reference for the same
reason they are here:

- **`X-Cache: MISS` with a large `X-Cache-Age` is not a contradiction.** The age
  describes the _content_, not the fetch. A re-read that comes back
  byte-identical keeps the timestamp the payload was first seen with, so a MISS
  with an age of 71 000 means the proxy asked Riot and the player has not done
  anything since. That is what makes the header safe to drive a "last updated"
  label off.
- **`sea` account lookups are routed to `asia`.** Riot serves account-v1 on
  `americas`, `asia` and `europe` only; there is no `sea` host, and asking for
  one is a 404. The proxy redirects it for you, so a SEA platform like `oc1`
  resolves by Riot ID without the caller knowing any of this. account-v1 is
  global, so the answer is identical either way. match-v5 is unaffected — `sea`
  is a real match host and SEA matches stay there.
- **`QUOTA_EXCEEDED` and `RATE_LIMITED` are opposite problems.** The first is
  your own per-minute allowance and is fixed by slowing down. The second is the
  proxy declining to hold your request any longer while it waits on Riot's
  budget: nothing you did, and every consumer sees it at once.

`/docs` is endpoint-shaped — what a route takes and returns. The browser client
at [`/dev`](#the-browser-client) is player-shaped — show me this account. They
answer different questions and both link to the other.

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
bucket past `BULK_USAGE_CEILING` (default 80 %), which keeps user latency flat
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

| Job               | Schedule                           | Action                                              |
| ----------------- | ---------------------------------- | --------------------------------------------------- |
| `poll:live`       | every `TRACK_POLL_LIVE_S` (60 s)   | spectator-v5 → `game.started` / `game.ended`        |
| `poll:rank`       | every `TRACK_POLL_RANK_S` (600 s)  | league-v4 → `rank.changed`                          |
| `poll:matches`    | every `TRACK_POLL_MATCH_S` (300 s) | new match IDs since the last tick → `archive:match` |
| `archive:match`   | on demand                          | fetch, upsert, `match.archived`                     |
| `backfill:player` | admin, or a player never walked    | page 100 IDs at a time, bulk priority               |
| `ddragon:sync`    | hourly                             | on a new patch, mirror data and emit `patch.new`    |
| `maintenance`     | daily                              | clear orphaned single-flight locks                  |

Each poll type is one repeatable tick that fans out to one job per tracked
player, so adding or removing a tracked player needs no scheduler changes. All
jobs are idempotent and run at bulk priority.

The first time anyone looks up a player, their whole history is queued behind
them. Matches are immutable, so a match stored now is one nobody ever spends
quota on again (§7.3) — the archive is the highest-leverage thing this service
does, and it used to fill only for tracked players and by hand. "First time"
means no completed walk is recorded against them — a fact kept on the player,
not guessed from the archive, since matches are shared and a teammate's walk
says nothing about this player. Tracking someone queues the same walk.
`LOOKUP_BACKFILL_LIMIT` sets how far back to go; `10000` is match-v5's own
ceiling, i.e. all of it.

`archive:match` is ordered by how far back the match sits in its player's
history, in blocks of ten, and the ordering is global rather than per player —
so anyone's most recent ten games are archived before anyone's hundredth, and a
player who has just been looked up sees their history fill in from the top. A
game that has only just finished takes the top rank outright.

`poll:matches` resumes from `last_seen_match_id` rather than reading a fixed
window off the top. In steady state the cursor is on the first page, so a tick
is the single call it has always been; when the ticks themselves stop — a
redeploy, a stalled queue — it pages back in proportion to how long they were
away, and matches past the first page are ranked by depth so a long tail cannot
crowd out live games. `TRACK_CATCHUP_LIMIT` bounds that; a gap deeper than it is
handed to a backfill rather than chased inline.

One BullMQ detail worth knowing before adding another producer: the worker pops
the plain `wait` list first and only falls back to the prioritized set once it
is empty, so a job queued with _no_ priority outranks every prioritized job.
Every archive job therefore carries an explicit priority.

---

## Configuration

| Env var                                      | Default                                           | Notes                                                       |
| -------------------------------------------- | ------------------------------------------------- | ----------------------------------------------------------- |
| `RIOT_API_KEY`                               | —                                                 | **Required.** The secret this project exists to protect     |
| `RIOT_USER_AGENT`                            | `riot-proxy/1.0 …`                                | Sent upstream so Riot can identify you                      |
| `PORT` / `HOST`                              | `8080` / `0.0.0.0`                                |                                                             |
| `NODE_ENV`                                   | `development`                                     | `production` refuses `AUTH_DISABLED` and turns `DEV_UI` off |
| `REDIS_URL`                                  | `redis://localhost:6379`                          |                                                             |
| `DATABASE_URL`                               | `postgres://proxy:proxy@localhost:5432/riotproxy` |                                                             |
| `DEFAULT_PLATFORM`                           | `euw1`                                            | Used by the composite endpoint                              |
| `CACHE_TTL_OVERRIDES`                        | —                                                 | CSV, e.g. `league=120,spectator=20`                         |
| `NEG_TTL_SECONDS`                            | `30`                                              | Spectator 404s                                              |
| `NEG_TTL_ACCOUNT_SECONDS`                    | `300`                                             | Typo'd Riot IDs                                             |
| `SF_LOCK_MS`                                 | `5000`                                            | Cross-instance single-flight lock                           |
| `CLIENT_WAIT_BUDGET_MS`                      | `2000`                                            | Max limiter wait for a client request                       |
| `BULK_USAGE_CEILING`                         | `0.80`                                            | Bulk work stops here, keeping 20% for callers               |
| `STALE_WHILE_REVALIDATE`                     | `true`                                            |                                                             |
| `METRICS_INTERVAL_S`                         | `5`                                               | `metrics` WS topic cadence; idle while nobody subscribes    |
| `TRACK_POLL_LIVE_S` / `_RANK_S` / `_MATCH_S` | `60` / `600` / `300`                              |                                                             |
| `DDRAGON_SYNC_S`                             | `3600`                                            | Version check cadence                                       |
| `DDRAGON_DIR` / `DDRAGON_LOCALE`             | `./data/ddragon` / `en_US`                        |                                                             |
| `ARCHIVE_TIMELINES`                          | `false`                                           | Timelines are large; opt in                                 |
| `LOOKUP_BACKFILL_LIMIT`                      | `10000`                                           | History walked on a first lookup; `0` disables              |
| `TRACK_CATCHUP_LIMIT`                        | `500`                                             | How far a match poll pages back to resume; `0` disables     |
| `ADMIN_IP_ALLOWLIST`                         | —                                                 | CSV of IPs/CIDRs; empty means key scope is enough           |
| `BOOTSTRAP_ADMIN_KEY`                        | —                                                 | Seeds one admin key on `npm run migrate`                    |
| `AUTH_DISABLED`                              | `false`                                           | Dev only: skip key checks; refused in production            |
| `DEV_UI`                                     | follows `NODE_ENV`                                | Serve the browser client at `/dev`; off in production       |
| `DOCS_UI`                                    | `true`                                            | Serve `/docs`, `/openapi.json` and `/openapi.yaml`          |
| `DASHBOARD_UI`                               | `true`                                            | Serve the operational dashboard at `/dashboard`             |
| `LOG_LEVEL`                                  | `info`                                            |                                                             |

`KEY_SCOPE` is derived, not configured.

`BULK_USAGE_CEILING` reads backwards at a glance: `0.80` means bulk work stops
once a bucket is 80% full, so a fifth of every bucket is kept back for requests
a person is waiting on. Bulk also stands aside entirely while an interactive
request is queued (§9.3) — the ceiling is the quieter half of that guarantee,
covering the case where nobody happens to be waiting at this instant but will
be a moment later. The spec opens at 75%; the extra 5% is a deliberate
deviation, taken because bulk has more to do than it did when that number was
chosen.

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
npm run docs:spec     # regenerate openapi.json from the route schemas
```

`openapi.json` is generated, committed, and checked by CI: change a route's
schema without regenerating it and the build fails. It is written against the
schema defaults rather than your `.env`, so the committed file is the contract
as shipped — a deployment that tunes anything gets a document that agrees with
it from its own `/openapi.json`, which is generated live.

Tests that need Redis or Postgres skip themselves when those are unreachable, so
`npm test` works with nothing running — but `docker compose up -d` first gets
you the limiter, HTTP-surface and WebSocket suites too. `REQUIRE_SERVICES=1`
turns that skip into a failure naming the suite that went quiet; CI sets it, so
a green build there means every assertion actually ran.

The README is tested too (`test/docs-readme.test.ts`): its settings table, cache
TTLs, commands, layout diagram and every path it names are asserted against the
code they describe, so the half of the documentation that is not generated
cannot drift silently either.

### Starting over

Three levels of teardown, smallest first. All of them refuse to run while
`NODE_ENV=production` unless you pass `--force` (`--yes` for `reset:all`).

```bash
npm run reset:cache   # cached responses, negative markers, single-flight locks
npm run reset:db      # truncate every table; schema and migrations stay put
npm run reset:all     # volumes, dist/, data/, node_modules → rebuild from scratch
```

`reset:cache` deletes only keys under this deployment's `KEY_SCOPE` (§7.4), so a
Redis shared with another proxy — or holding the previous Riot key's namespace —
is left alone. It leaves the limiter's learned buckets, the job queues, consumer
quotas and the auth cache in place, since none of those are cache.

```bash
npm run reset:cache -- --limiter   # also drop the learned rate-limit buckets
npm run reset:cache -- --all       # FLUSHDB: everything, every key scope
npm run reset:db -- --keep-consumers   # keep minted API keys
```

Dropping the limiter buckets means the next few requests re-learn the windows
from Riot's response headers, which is safe but briefly conservative. `--all`
also drops queued jobs, so anything mid-backfill is lost — re-enqueue it.

`reset:all` is the full remake: `docker compose down -v`, remove `dist/`,
`data/` and `node_modules`, bring the stack back up on its healthchecks, then
`npm ci`, `npm run migrate` and `npm run build`. It never touches `.env` — that
holds your Riot key — and copies `.env.example` over only if no `.env` exists.
It asks first; `--yes` skips the prompt and `--keep-deps` skips the reinstall.

Everything `reset:cache` and `reset:db` delete is re-derivable from Riot, at the
cost of the quota to re-fetch it. The match archive is the expensive one: it is
the only store here holding data Riot will not serve again cheaply, so prefer
`--keep-consumers` and a targeted cache purge over `reset:all` on a warm box.

These are the only tools here that destroy data, so they are tested as
processes rather than as functions — `test/reset-{cache,db,script}.test.ts`
assert the exit codes and the refusals a developer would actually hit. The
suite never points them at what you are working in: the cache tests run against
Redis logical database **15**, the database tests create a throwaway database
and drop it afterwards, and `reset.sh` is exercised only along the paths that
refuse, with a fake `docker` ahead of the real one on `PATH` to prove it never
reaches the destructive half. `shellcheck scripts/*.sh` runs in CI.

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
| `ACCEPTANCE_LOG_LEVEL`       | `warn`                   | Log level for the api/worker it starts       |

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
└─ riot/                                ├─ auth/             consumer auth + scopes
   ├─ routing.ts   platform ↔ region    ├─ docs/             OpenAPI document + /docs
   ├─ endpoints.ts ids, URLs, TTLs      └─ cli/             key minting, resets, spec
   ├─ client.ts    undici pools + §5.5 error policy
   └─ limiter.ts   header-driven token buckets

test/              unit + integration, every upstream call stubbed
acceptance/        live checks against the real Riot API (opt-in)
```

---

## Roadmap

[TODO.md](TODO.md) is the map: what is done, what is in flight, and what is
known to be missing. GitHub issues stay the source of truth for anything with a
discussion attached.

---

## Compliance

- Register the app on the [Riot developer portal](https://developer.riotgames.com/)
  for a personal or production key before any public use.
- Don't resell Riot data; respect `Retry-After`; keep an identifiable
  `User-Agent`.
- Consuming products must carry the "not endorsed by Riot Games" disclaimer.

## Licence

Private. Not for redistribution.

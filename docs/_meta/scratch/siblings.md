# Sibling services: the integration boundary around riot-proxy

Evidence-only survey of the three sibling clones on this machine, written 2026-10-19 for the DOC-01 documentation set. Every claim cites `repo:file:line`. Where a handoff note and code disagree, code wins and the disagreement is called out. No `.env` file was read; only variable names are listed.

Clones surveyed (and the commit each was read at):

| Repo | Path | HEAD read | Note |
|---|---|---|---|
| ninjagoldfinch-lol (the real site, private) | `/home/dev/projects/ninjagoldfinch-lol` | working tree at `9e5e1d1`; local `origin/main` ref at `b8d0b28` (4 commits ahead: `d452f64`, `a796a38`, `5a458bc`, `b8d0b28`) | The GitHub remote is further ahead still (`b8d0b28..8d226db` per `git fetch --dry-run`); not fetched, so those commits are unseen. Citations marked `@origin/main` come from `git show origin/main:<file>`. |
| ninjagoldfinch.lol (GitHub Pages placeholder) | `/home/dev/projects/ninjagoldfinch.lol` | `8da0cf4` (the only commit) | Confirmed placeholder; see §3. |
| ninja-monitor | `/home/dev/projects/ninja-monitor` | `10a6b48` (2026-10-11), v0.2.1, in sync with `origin/main` | Cargo workspace: agent at the root, `ninja-ingest` server in `ingest/`. |

Handoff notes read: `/home/dev/projects/handoff-ninjagoldfinch-lol-2026-10-10.md`, `…-2026-10-15-speed.md`, `…-2026-10-19-filters.md`.

---

## 1. ninjagoldfinch-lol (the site)

### 1.1 Identity, framework, toolchain

| Fact | Evidence |
|---|---|
| Package name `ninjagoldfinch-lol`, private, version 0.0.1 | `ninjagoldfinch-lol:package.json:2-4` |
| pnpm pinned `pnpm@12.10.1`; engines `node >=22.18` | `package.json:6`, `package.json:8`. README says "Node 22.17 or later" (`README.md:12`) — minor drift; `package.json` is enforced (`.npmrc:1` `engine-strict=true`). |
| SvelteKit 3 (`@sveltejs/kit ^3.0.0`), Svelte 5 runes only, TypeScript strict | `package.json:26`, `vite.config.ts:11-14`, `tsconfig.json:4` |
| Adapter: `@sveltejs/adapter-node ^6.0.0`, plain `node build` | `package.json:25`, `vite.config.ts:3,16`; prod runs `node build` (`README.md:90-110`, `docs/DECISIONS.md:500-513` ADR-038) |
| API client: `openapi-fetch ^0.17.0` (runtime), types from `openapi-typescript ^7.13.0` over the committed spec | `package.json:34,51`; `src/lib/server/proxy.ts:1-3`; `justfile` `gen` recipe; `CLAUDE.md:22,48` |
| Only runtime dependency is `openapi-fetch` | `package.json:50-52` |
| Committed riot-proxy OpenAPI copy: `openapi/riot-proxy.json`. Working tree = `2.0.0-rc.3`; `@origin/main` = `2.6.0` (SPD-01, PR #62) | `info.version` read from both; `@origin/main:README.md:66`; `docs/PROGRESS.md` (`@origin/main`) SPD-01 row |
| `just check` is the only gate; **no CI**, no `.github/` directory, and CLAUDE.md forbids adding one | `CLAUDE.md:11-12`, `README.md:39`, file listing (no `.github/`), `.githooks/pre-push:3` (`exec just check`) |
| Pre-commit hook refuses commits on `main` and staged `rpx_`/`RGAPI-` key-shaped strings | `.githooks/pre-commit:4-23` |

### 1.2 How the site configures riot-proxy

Variable names only (values never read):

| Name | Where defined/validated | Semantics |
|---|---|---|
| `PROXY_URL` | `src/env.ts:13-16`, `src/lib/server/env.ts:10-15` | Base URL of riot-proxy. **Required, no default in code** (throws `PROXY_URL is required`). The worktree README still documents a default of the LAN dev instance `http://192.168.68.46:8080` (`README.md:45`); `@origin/main:README.md:45` says "Required, with no default: there is no shared dev instance". |
| `PROXY_KEY` | `src/env.ts:27-30`, `src/lib/server/env.ts:47-55` | riot-proxy consumer key (read scope per `README.md:46`). Optional unless `NODE_ENV=production`, then required. Empty string counts as unset. Sent as `Authorization: Bearer <key>` only when set (`src/lib/server/proxy.ts:22,31`). Lives only under `src/lib/server/` (`CLAUDE.md:13-16`). |
| `CDRAGON_URL` | `src/env.ts:23-26`, `src/lib/server/env.ts:17-25` | CommunityDragon base, default `https://raw.communitydragon.org`; a mirror or the mock can stand in. |
| `ORIGIN` | `vite.config.ts:17-20`, `Dockerfile:9-11` | Build-time only (adapter-node 6 does not read it at runtime; ADR-014 `docs/DECISIONS.md:171-184`). Production builds with `ORIGIN=https://ninjagoldfinch.lol` (`README.md:109`). |
| `PORT`, `HOST`, `NODE_ENV` | `README.md:98-99` | adapter-node's own; prod `HOST=127.0.0.1`, `PORT=3000`. |
| `PUBLIC_SITE_NAME` | `src/env.ts:17-22` | Public, static, default `ninjagoldfinch.lol`. |

`.env.example` names exactly: `PROXY_URL`, `PROXY_KEY`, `CDRAGON_URL`, `ORIGIN`, `PORT`, `PUBLIC_SITE_NAME` (both worktree and `@origin/main`). `just setup` copies it to `.env` (`justfile` `setup` recipe; `README.md:19-26`).

### 1.3 The one proxy client

`src/lib/server/proxy.ts` is the single riot-proxy client (`CLAUDE.md:18-20`, `proxy.ts:37-38`).

| Behaviour | Worktree (`9e5e1d1`) | `@origin/main` (`b8d0b28`) |
|---|---|---|
| Base URL / auth | `createClient<paths>({ baseUrl, headers: key ? { Authorization: \`Bearer ${key}\` } : {} })` — `proxy.ts:28-32` | same, `proxy.ts:96-100` |
| Failure logging | `logFailures` middleware logs `proxy <status> <method> <path> request-id=<X-Request-Id>` on non-2xx, and `proxy unreachable …` on a thrown fetch — `proxy.ts:9-20` | same, `proxy.ts:10-21` |
| Timeouts | **none** | `proxyBudget`: 5 000 ms for `/v1/lol/analytics/*` and `/v1/static/*`, 30 000 ms for everything else (`proxy.ts:55-64`); `withTimeout` wraps fetch with `AbortSignal.any([request.signal, AbortSignal.timeout(budget)])` and reads the body inside the deadline (`proxy.ts:73-87`); a spent budget throws SvelteKit `error(503, 'Temporarily unavailable')` (`proxy.ts:83-84`). ADR-040 (`@origin/main:docs/DECISIONS.md` "ADR-040"). |
| Retries | **none in the site** (grep for retry logic finds only `retryAfter` parsing). ADR-040's context relies on riot-proxy's own Riot-side retries ("retries a 5xx twice"). | same |
| `X-Cache` | not read | read only to log slow analytics calls: `logSlowAnalytics` warns `proxy slow <method> <path> x-cache=<HIT|MISS|none> <ms>ms` when a `/v1/lol/analytics/*` call takes > `SLOW_ANALYTICS_MS = 250` (`proxy.ts:23-53`). Never forwarded to the browser (`proxy.ts:31`). |
| `X-RateLimit-*` | **not read anywhere** (grep over `src/` and `tests/` for `x-ratelimit`: no hits) | same |
| `X-Cache-Age` | not read (kept in fixtures only, `tests/record-fixture.ts:41`); the site uses body fields `fetchedAgeSeconds`/`matchIdsFetchedAgeSeconds` instead (ADR-019, `src/lib/server/profile.ts:132-136`) | same |
| `Retry-After` | read as a fallback for `retryAfter` on 429 (`src/lib/server/errors.ts:47-55`) | same (`errors.ts:66-74`) |
| Conditional requests / ETag | **none**: no `If-None-Match`, no `cache:` option; Node's fetch has no HTTP cache. (See §5 for riot-proxy's ADR-166 wording that assumes otherwise.) | same |

### 1.4 Error mapping (`src/lib/server/errors.ts`)

`classifyProxyError(status, body, headers)` (`errors.ts:16-33`) collapses every proxy failure to three visitor states; raw proxy messages never pass through:

| Proxy response | Site result | Evidence |
|---|---|---|
| HTTP 404, or body `error.code` `NOT_FOUND` or `VALIDATION` (any status, so a 400 `VALIDATION` is a 404) | 404 "Not found" | `errors.ts:19-22` |
| HTTP 429, or `error.code` `QUOTA_EXCEEDED` (site's consumer quota) or `RATE_LIMITED` (Riot budget, which riot-proxy may send as 503 — see `tests/fixtures/synthetic-matches-rate-limited.json` status 503) | 429 "Too many requests" with `retryAfter` from `error.retryAfter` or `Retry-After` | `errors.ts:24-30,47-55` |
| Anything else: 401, 403, 500, 502 `UPSTREAM_ERROR`, 503, unreachable, timeout | 503 "Temporarily unavailable" | `errors.ts:32`; `src/hooks.server.ts:15-20` (unknown throws → generic message); `@origin/main:proxy.ts:83-84` |
| Optional call that threw (unreachable/timeout) | `@origin/main` only: `optional()` turns the throw into a synthetic 503 `Response` so the page's "not ok" branch degrades that part alone (`@origin/main:errors.ts:41-58`) | |

Error pages: `src/lib/domain/error-view.ts:8,15` renders the 404 and 429 states; the cache hook forces `no-store` on every ≥400 except 404 (`public, s-maxage=60`) (`src/lib/server/cache.ts:29-34`, `src/hooks.server.ts:5-10`).

### 1.5 Every riot-proxy endpoint the site calls

All calls are server-side, from `+page.server.ts` loads via the modules below (`CLAUDE.md:57-58`). All are `GET` with `Authorization: Bearer <PROXY_KEY>` when set, no other custom headers. Paths are the OpenAPI operation templates as passed to `client.GET(...)`.

| # | Call | Where | Query / path params | Essential? | On failure |
|---|---|---|---|---|---|
| 1 | `GET /v1/players/by-riot-id/{gameName}/{tagLine}/profile` | `src/lib/server/profile.ts:78-80` | path from the URL's Riot ID; `?refresh=true` only on the Refresh form action (`profile.ts:77`, `src/routes/summoner/[riotId]/+page.server.ts:33-37`). **No `platform`** — riot-proxy finds it (ADR-037, handoff 2026-10-10 §1). | yes | `toHttpError` → 404/429/503 (`profile.ts:81`); unknown `platform` in body → 503 (`profile.ts:83-87`) |
| 2 | `GET /v1/players/{puuid}/matches` | `profile.ts:90-102` | `platform=<from profile.platform>`, `start=(page-1)*10` when page>1, `count=10`, `queue=<id>` if filtered, `champion=<id>` if filtered, `refresh=true` on refresh. Constants: `MATCHES_PER_PAGE = 10` (`src/lib/domain/match-filters.ts:7`), `FILTER_QUEUES = [420,440,400,490,480,450,1700]` (`:5`). | no | `matches: null`, `matchesError: {status, retryAfter}` via `classifyProxyError` (`profile.ts:118-122`); page then sent `no-store` (`src/routes/summoner/[riotId]/+page.server.ts:25-26`). `@origin/main` wraps it in `optional()` (`@origin/main:profile.ts:90-104`). |
| 3 | `GET /v1/static/versions` | `src/lib/server/patch.ts:52` (worktree); `@origin/main:patch.ts:76` | none | yes (every data page) | 503 (`patch.ts:53-59`). `@origin/main` holds the answer 5 min (`PATCH_TTL_MS`, `patch.ts:48,59-68`; ADR-041). |
| 4 | `GET /v1/static/{file}?version=<patch>` for `champion`, `item`, `summoner`, `runesReforged` | `src/lib/server/static-data.ts:42-49` | `version` = current patch from #3 | varies | all five (#4+#5) must succeed or `toHttpError` throws (`static-data.ts:52-54`); callers catch and fall back to `NO_NAMES` (`profile.ts:106-109`, `match.ts:57-60`, `champions.ts:46-49`) except the champion page's URL resolution, which 503s (`champion-page.ts:80-83`). Held in memory per patch (ADR-013, `static-data.ts:24-39`). |
| 5 | `GET /v1/static/queues` | `static-data.ts:50` | none | as #4 | as #4 |
| 6 | `GET /v1/lol/matches/{region}/{matchId}` | `src/lib/server/match.ts:38` | `region` = regional route (`americas|europe|asia|sea`) derived from the match-id prefix (`src/lib/domain/regions.ts:15-31,47-50`) | yes | 404/429/503 (`match.ts:42`); unparsable body → 503 (`match.ts:44-48`); custom game → 404 (`match.ts:49`) |
| 7 | `GET /v1/lol/matches/{region}/{matchId}/timeline` | `match.ts:39` | as #6 | no | page renders without gold/build/skill data; sent `no-store` (`match.ts:51-54`, `src/routes/match/[matchId]/+page.server.ts:13-16`). `@origin/main` wraps in `optional()` (`@origin/main:match.ts:39`). |
| 8 | `GET /v1/lol/analytics/champions` | `src/lib/server/champions.ts:38` | `queue=RANKED_SOLO_5x5`, `patch=<major.minor>` only when filtered (default: omitted = all), `role=<TOP|JUNGLE|MIDDLE|BOTTOM|UTILITY>` when filtered, `minGames=0`, `limit=500` (`src/lib/domain/champion-stats.ts:19,30,33-39,93-107`). Never sends `tier`, `side`, `minTier` (handoff 2026-10-19 §1). | yes | `toHttpError` (`champions.ts:42`) |
| 9 | `GET /v1/lol/analytics/patches` | `champions.ts:39` | `queue=RANKED_SOLO_5x5` | no | picker offers defaults only; logged (`champions.ts:43-44,53`). `@origin/main`: `optional()` (`@origin/main:champions.ts:39-41`). |
| 10 | `GET /v1/lol/analytics/champions/{championId}` | `src/lib/server/champion-page.ts:106-108` | as #8 with `limit=10` | yes | `toHttpError` (`champion-page.ts:119`) |
| 11 | `GET /v1/lol/analytics/champions/{championId}/builds` | `champion-page.ts:109-111` | as #8 without `limit` | no | `setBuilds: null`, falls back to the detail's lists (`champion-page.ts:120-130`); `@origin/main` `optional()` (`:109-113`) |
| 12 | `GET /v1/lol/analytics/champions/{championId}/matchups` | `champion-page.ts:112-114` | as #8 with `limit=200` | no | `matchups: null` (`:140-143`); `@origin/main` `optional()` |
| 13 | `GET /v1/lol/analytics/patches` | `champion-page.ts:115-117` | `queue=RANKED_SOLO_5x5&championId=<id>` | no | `patches: []` (`:145`); `@origin/main` `optional()` |
| 14 | (dev tool) `GET <PROXY_URL><any path>` | `tests/record-fixture.ts:44-48` | whatever is passed to `just fixture` | manual | writes `tests/fixtures/<name>.json`, keeping only `content-type`, `x-cache`, `x-cache-age`, `x-request-id`, `retry-after` (`:41`) and redacting puuids/summonerIds (`tests/redact.ts`) |

Not called by any code (fixtures only, left from before ADR-037): `/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}` (`tests/fixtures/account-hide-on-bush.json`, `account-not-found.json`). No code path references `/v1/riot/*`, `/v1/lol/league/*`, `/v1/lol/summoners/*`, `/v1/lol/mastery/*`, `/v1/lol/spectator/*`, `/v1/lol/status/*`, `/v1/lol/rotations/*`, `/v1/players/{puuid}/profile`, `/v1/players/{puuid}/champions`, `/v1/admin/*`, `/v1/ws`, `/healthz`, `/readyz`, `/metrics`, `/ddragon/*`, `/openapi.json` at runtime (grep `'/v1/` over `src/`: exactly the 13 operations above).

Per-page call shape:

- `/summoner/[riotId]` (`src/routes/summoner/[riotId]/+page.server.ts:14-28`): #1, then in parallel #2 + #3, then #4/#5 (memoised). Two sequential round trips minimum (`profile.ts:55-57`). Refresh is a `POST ?/refresh` form action returning the fresh profile (ADR-012; `+page.server.ts:30-38`).
- `/match/[matchId]` (`src/routes/match/[matchId]/+page.server.ts:7-17`): #6 + #7 + #3 in parallel, then #4/#5 + CommunityDragon perks in parallel (`match.ts:37-41,56-65`).
- `/champions` (`src/routes/champions/+page.server.ts:7-19`): #8 + #9 + #3 in parallel, then #4/#5 (`champions.ts:37-49`).
- `/champions/[key]` (`src/routes/champions/[key]/+page.server.ts:8-25`): #3 → #4/#5 (to resolve the slug) → #10 + #11 + #12 + #13 in parallel (`champion-page.ts:75-87,102-118`).
- `/search` (`src/routes/search/+page.server.ts:36-46`): #3 → #4/#5 only for champion-name queries.
- `/healthz` (`src/routes/healthz/+server.ts:5-6`): touches nothing; `{"ok":true}`, `no-store`.
- `/about`: prerendered (`src/routes/about/+page.ts:2`).
- `/summoner/[region]/[riotId]` and `/match/[region]/[matchId]`: 308 redirects to the region-less URLs, no proxy call (`src/routes/summoner/[region]/[riotId]/+server.ts:9-13`, `src/routes/match/[region]/[matchId]/+server.ts:8-11`).

### 1.6 Caching done by the site

| Layer | What | Evidence |
|---|---|---|
| In-memory, per patch | Reduced Data Dragon lookups (champion/item/summoner/runes/queues) for one patch; failed fetch not kept | `src/lib/server/static-data.ts:24-39` (ADR-013 `docs/DECISIONS.md:159-170`) |
| In-memory, per patch | CommunityDragon stat shards | `src/lib/server/cdragon.ts:9-28` (ADR-026) |
| In-memory, 5 min | `/v1/static/versions` answer | `@origin/main:src/lib/server/patch.ts:45-73` (ADR-041) |
| Edge (Cloudflare) `Cache-Control` | profile `public, s-maxage=60, stale-while-revalidate=300`; match `public, s-maxage=86400, stale-while-revalidate=86400`; analytics `public, s-maxage=600, stale-while-revalidate=600`; 404 `public, s-maxage=60`; everything else/errors/POST `no-store` | `src/lib/server/cache.ts:5-16,29-34`; `src/hooks.server.ts:5-10` |
| **No** cache of Riot player data or analytics bodies | by rule | ADR-013/ADR-041 wording; handoff 2026-10-15 §6 |

The Cloudflare cache rule (HTML eligible, respect origin `Cache-Control`) is a planned owner action (`docs/IMPLEMENTATION.md` "Cloudflare, set once", P5-02) and SPD-06 (`@origin/main:docs/PROGRESS.md` SPD-06 unticked). `TODO (unverified): whether Cloudflare actually caches HTML and __data.json for ninjagoldfinch.lol today.`

### 1.7 Things the site does not do

- **No server-side crawler, poller, cron or background job.** Only request-driven loads plus the in-memory memos above (grep over `src/` finds no timers/intervals beyond memoisation).
- **No WebSocket / EventSource / XHR** anywhere in `src/` (grep for `WebSocket|EventSource|XMLHttpRequest|fetch(`: zero hits outside tests). `/v1/ws` is unused.
- **No object storage** (grep `R2|S3|bucket` over the repo: no hits). Object storage (Onidel bucket, R2 `ngf-backups`) is riot-proxy's own concern (`riot-proxy:PROGRESS.md:325-326`).
- **The browser never calls riot-proxy.** Enforced by rule (`CLAUDE.md:13-20,57`) and by structure: the key is only in `src/lib/server/`, and `src/lib/recent-storage.ts` (the only browser-side state) is `localStorage` for recent searches. Match rows expand via SvelteKit `preloadData(matchPath(...))`, i.e. the site's own `__data.json` (ADR-030, `docs/DECISIONS.md:372`).
- **Images do not come from riot-proxy.** `src/lib/assets.ts:7-8` hardcodes `https://ddragon.leagueoflegends.com/cdn` and `https://raw.communitydragon.org`; champion/item/profile-icon/spell/rune icons (`assets.ts:13-33`), stat-shard icons (`:39-41`) and lane icons (`:68-71`) are built as absolute URLs rendered into HTML, so the **browser** fetches them directly from Riot's CDN and CommunityDragon. riot-proxy's `/ddragon/{version}/img/...` routes (declared for SITE-07, `riot-proxy:docs/IMPLEMENTATION.md:381`) are **not used** (ADR-009 `docs/DECISIONS.md:103-112`). See §5 for the disagreement.
- The only non-riot-proxy **server-side** external call is CommunityDragon `perks.json` (`src/lib/server/cdragon.ts:18`, URL from `src/lib/assets.ts:47-49`): `GET {CDRAGON_URL}/{major.minor}/plugins/rcp-be-lol-game-data/global/default/v1/perks.json`, no auth, failure → no stat shards (`match.ts:61-64`).

### 1.8 Build, deploy, hosts, ports

**Production** (ADR-038, `docs/DECISIONS.md:500-513`; `README.md:90-113`):

| Fact | Evidence |
|---|---|
| Same server as riot-proxy: `ngf-prod-syd1` (Onidel ONI-4, Sydney, Ubuntu 26.04, 6 vCPU / 12 GB) | `riot-proxy:docs/DECISIONS.md:1510` (ADR-142); `riot-proxy:docs/design/12-object-storage-and-hosting.md:63` |
| Not Docker: `node build` under systemd as unit `ninjagoldfinch-lol.service`, user `ninjagoldfinch-lol` | `README.md:97`, `docs/DECISIONS.md:509`; unit file is **not in the repo** (`docs/DECISIONS.md:513`) |
| Checkout `/opt/ninjagoldfinch-lol`, pulled with a read-only deploy key | `README.md:96`; `riot-proxy:docs/design/12-…:615-622` |
| Settings file `/etc/ninjagoldfinch-lol/env` with `NODE_ENV`, `HOST`, `PORT`, `PROXY_URL`, `PROXY_KEY` (names only) | `README.md:98` |
| Listens `127.0.0.1:3000`; reached only via Cloudflare Tunnel `https://ninjagoldfinch.lol → http://127.0.0.1:3000` | `README.md:99`, `docs/DECISIONS.md:510`; `riot-proxy:docs/design/12-…:532-534` |
| riot-proxy on the same box at `127.0.0.1:8080` (tunnel routes `api.ninjagoldfinch.lol` and `admin.ninjagoldfinch.lol` there) | handoff 2026-10-15 §7 line 204; `riot-proxy:docs/design/07-deployment.md:221`; `riot-proxy:docs/design/12-…:889` |
| Deploy = 4 commands: `git pull`, `pnpm install --frozen-lockfile`, `env ORIGIN=https://ninjagoldfinch.lol pnpm build`, `systemctl restart ninjagoldfinch-lol` | `README.md:105-110` |
| Liveness `curl -s http://127.0.0.1:3000/healthz`; logs `journalctl -u ninjagoldfinch-lol` | `README.md:100-101` |
| `cloudflared` is a system service on the box; tunnel is outbound-only, no inbound ports | `docs/DECISIONS.md:504`; `riot-proxy:docs/design/12-…:532-537` |

`TODO (unverified): the exact PROXY_URL value in /etc/ninjagoldfinch-lol/env (expected http://127.0.0.1:8080 from the handoff and design/12, but the env file was not read).`
`TODO (unverified): the contents of ninjagoldfinch-lol.service (not committed anywhere; copied by hand per riot-proxy:docs/design/12-…:627).`
`TODO (unverified): which riot-proxy consumer key name/scope the site uses in production (deploy/proxmox/README.md:55 suggests `riot-proxy key create --name ninjagoldfinch-web` for the VM; production's key name is not recorded).`

**LAN preview VM** (ADR-010, kept but not production):

| Fact | Evidence |
|---|---|
| Docker image `ninjagoldfinch-web:local`, `node:22-alpine`, `pnpm install --frozen-lockfile`, `ORIGIN` build arg, `EXPOSE 3000`, `HEALTHCHECK` on `/healthz`, `CMD node build` | `Dockerfile:1-20` |
| `deploy/compose.yaml`: single `web` service, `env_file: .env`, `ports: ['3000:3000']`; "riot-proxy runs elsewhere; `PROXY_URL` in `deploy/.env` points at it" | `deploy/compose.yaml:1-14` |
| Proxmox VM `ninjagoldfinch-web` (Debian 13, 2 cores/2 GB/16 GB, user `web`), built by `deploy/proxmox/create-vm.sh`; `ninjagoldfinch-update` clones/fetches/rebuilds | `deploy/proxmox/README.md:3-57`, `create-vm.sh:22-35`, `ninjagoldfinch-update:12-48` |
| VM 104 at `192.168.68.47` served a profile from the dev riot-proxy on 2026-10-09 | `docs/PROGRESS.md:50-52` |

**Local dev / tests:**

| Fact | Evidence |
|---|---|
| Dev proxy `http://192.168.68.46:8080` referenced in: `README.md:45,56` (default and `/openapi.json` URL), `deploy/proxmox/README.md:3`, `tests/unit/env.test.ts:28`, `docs/PROGRESS.md:87`, ADR-013 "dev proxy" (`docs/DECISIONS.md:165`). Handoff 2026-10-15 lines 7, 56, 69 and `@origin/main` README say it is **gone**; `riot-proxy:docs/IMPLEMENTATION.md:434` identifies it as dev VM 102. | as listed |
| Local dev points at whatever `PROXY_URL` is in `.env` (created from `.env.example` by `just setup`); a local riot-proxy with `AUTH_DISABLED` needs no key | `README.md:19-26,49`; `@origin/main:README.md:47` |
| Unit + e2e tests never reach a real proxy: Playwright starts `node tests/mock-proxy/server.ts 8787` and `node build` with `PROXY_URL=http://127.0.0.1:8787`, `PROXY_KEY=e2e-placeholder`, `CDRAGON_URL` = the mock, `ORIGIN=http://localhost:4173`, `PORT=4173` | `playwright.config.ts:3-30`; `tests/mock-proxy/server.ts:7-12`; `tests/mock-proxy/mock.ts:32-47` (exact method+path match; miss → 500 `INTERNAL`) |
| Fixtures: recorded (redacted) + hand-written `synthetic-*.json` with `_note` (ADR-017) | `tests/fixtures/*`, `README.md:68-82` |

### 1.9 Site-side state of the series described by the handoffs

| Series | Handoff | Code state seen |
|---|---|---|
| SITE-09/10 (platform from Riot ID; champion search) | 2026-10-10 | Implemented: P4-04 (`docs/PROGRESS.md:48`, ADR-037 `docs/DECISIONS.md:483-498`), `profile.ts:59-62`, `search/+page.server.ts:30-46`. Handoff step 3 ("send platform to matches, champions, mastery, live game") applies only to matches: the site calls none of the others. |
| SPD-01..06 (speed) | 2026-10-15 | `@origin/main`: SPD-01 #62 (spec 2.6.0 + `X-Cache` slow log), SPD-05 #63 (budgets, `optional()`), SPD-02 #64 (patch memo), FIX-01 #65; SPD-03/04 dropped on measurement (ADR-039); SPD-06 unticked (`@origin/main:docs/PROGRESS.md` SPD rows, ADR-039..041). The handoff's claim that the committed spec is rc.3 (line 36) was true at writing and is now stale on `origin/main`. |
| VS-01..04 (tier ranges, builds vs opponent, `winRateLow`) | 2026-10-19 | **Not in the local `origin/main` ref** (`b8d0b28`): no `minTier`, `opponentId` or `vs` in `src/`. `TODO (unverified): whether the 4 unseen remote commits (b8d0b28..8d226db) are VS-01..04; fetch the site repo to check.` |

---

## 2. ninja-monitor

### 2.1 What it is

| Fact | Evidence |
|---|---|
| Rust 2024 edition, `rust-version 1.88`, crate `ninja-monitor` v0.2.1; workspace with `ingest/` (`ninja-ingest` v0.2.1) | `ninja-monitor:Cargo.toml:1-9`, `ingest/Cargo.toml:3` |
| Single ~2 MiB static binary, current-thread tokio, axum API on loopback, `ureq` (rustls) for outbound HTTP; no SQLite in the agent | `Cargo.toml:17-27`, `README.md:12-19` |
| Samples **host** CPU/load/memory/swap/disk (per configured mount) and **per systemd service** (cgroup v2 under `/sys/fs/cgroup/system.slice/<unit>`): state running/down, `cpu_percent`, `mem_anon_bytes`, `mem_file_bytes`, `mem_current_bytes`, `mem_peak_bytes`, `oom_kills`, `pids`, `restarts_seen`, `disk_bytes` under configured `paths` | `README.md:33-81`, `src/sampler/{host,cgroup,disk,walk}.rs` |
| Sampling every `15s`, disk walk every `10m`, `24h` history in memory; nothing written to disk | `deploy/config.example.toml:12-15`, `README.md:17,262-269` |
| Config: one TOML at `/etc/ninja-monitor/config.toml` (generated by `install.sh` / `ninja-monitor --init` from running units; unknown keys rejected); secrets only from env via `/etc/ninja-monitor/env` (0600, root) loaded by systemd `EnvironmentFile` | `README.md:252-301`, `deploy/install.sh:99-131`, `deploy/ninja-monitor.service:15-16` |
| Env var **names**: `NINJA_MONITOR_DISCORD_WEBHOOK`, `NINJA_MONITOR_HEARTBEAT_URL`, `NINJA_MONITOR_PUSH_TOKEN`, `NINJA_MONITOR_LOG` | `deploy/env.example:8-15`, `README.md:293-298`, `src/config.rs:399` |
| Runs as hardened systemd unit `ninja-monitor.service`, user `ninja-monitor`, `MemoryMax=64M`, read-only everywhere | `deploy/ninja-monitor.service:1-49`, `README.md:465-476` |
| Alert rules: `ok → pending (for) → firing`, one message on fire/resolve, optional `repeat`; metrics `cpu_percent`, `load1`, `mem_available_percent|bytes`, `swap_used_percent`, `disk_used_percent` (host) and `cpu_percent`, `mem_anon_bytes`, `mem_current_bytes`, `pids`, `disk_bytes`, `down`, `oom_kill` (service) | `README.md:303-353`, `src/alerts/mod.rs` |
| **Alert channel: Discord webhook only** (plus an optional outbound heartbeat GET to an external checker such as healthchecks.io). No email, no SMTP, no Telegram/ntfy (grep over the repo: none). Delivery queue of 100, 3 retries with backoff, honours Discord `retry_after` | `README.md:343-353`, `src/alerts/notify.rs`, `src/heartbeat.rs:10-28` |
| Local read API `127.0.0.1:9090`: `GET /healthz`, `/stats`, `/history?scope=…`, `/alerts`; reached over an SSH local forward | `src/api.rs:45-48`, `deploy/config.example.toml:7-10`, `README.md:238-250` |
| CI: GitHub Actions `ci.yml` (fmt, clippy, test on ubuntu-24.04 + macos-15, shellcheck) and `release.yml` (tag `v*` → static musl x86_64/aarch64 tarballs for agent and ingest, provenance attestation, `SHA256SUMS`) | `.github/workflows/ci.yml:1-58`, `.github/workflows/release.yml:1-141` |

### 2.2 riot-proxy-specific configuration

ninja-monitor knows riot-proxy only as **a systemd unit and a data directory**; it has no HTTP integration with it.

| Where | What |
|---|---|
| `deploy/config.example.toml:29-32` | `[[service]] name = "riot-proxy"`, `unit = "riot-proxy.service"`, `paths = ["/var/lib/riot-proxy"]` (marked PLACEHOLDER) |
| `deploy/config.example.toml:34-42` | `[[service]] name = "site"`, `unit = "CHANGE-ME.service"` (placeholder for the site's unit) and `cloudflared.service` |
| `deploy/config.example.toml:64-99` | example rules: `riot-proxy memory high` (`mem_anon_bytes > 2 000 000 000` for 5m), `host memory low` (<10 % for 5m), `disk filling` (>85 % for 10m, repeat 6h), `riot-proxy down` (for 1m), `riot-proxy OOM kill` |
| `deploy/ninja-monitor.service:11-14` | comment: add `SupplementaryGroups=riot-proxy` in a drop-in to measure `/var/lib/riot-proxy`; README alternative `setfacl -R -m u:ninja-monitor:rX /var/lib/riot-proxy` (`README.md:478-496`) |
| `dev/config.toml:25-36`, `dev/simulate.sh:10-11`, `tests/fixtures/cgroup/system.slice/riot-proxy.service/*`, `tests/api.rs:56-127` | dev/test fixtures named after `riot-proxy.service`, `cloudflared.service`, `site.service` |
| `README.md:320-347,423-456` | documentation examples use `service:riot-proxy` |

**It does not scrape riot-proxy's `/healthz`, `/readyz` or `/metrics`.** Grep over the ninja-monitor repo for `readyz`, `/metrics`, `prometheus`: no hits. The only `/healthz` the agent ever GETs is **ninja-ingest's**, once, during `install.sh --check` (`src/check.rs:96-98,104-111`, `src/config.rs:244-249` `url_origin`). The only outbound HTTP calls the agent makes at runtime are: Discord webhook POST, optional heartbeat GET, optional `POST <push.url>` to ninja-ingest.

### 2.3 ninja-ingest (central store, same repo)

| Fact | Evidence |
|---|---|
| Listens `127.0.0.1:9091` (or `0.0.0.0:9091` with `--expose`), SQLite at `/var/lib/ninja-ingest/ingest.db`, 30-day raw retention, 180-day events; unit `ninja-ingest.service`, user `ninja-ingest`, `MemoryMax=128M` | `deploy/ingest.config.example.toml:6-23`, `deploy/ninja-ingest.service:7-19` |
| Routes: `GET /healthz`, `/hosts`, `/hosts/{host}/stats|history|alerts`, `/stats`, `POST /v1/ingest` | `ingest/src/api.rs:67-75` |
| Auth on `/v1/ingest`: `Authorization: Bearer <token>`, one token per `[[agent]]` from env var `token_env` (default `NINJA_INGEST_TOKEN_<NAME>`), constant-time compare | `ingest/src/ingest.rs:52-67`, `deploy/ingest.env.example:7-11` |
| Body `{agent, host, samples[], events[]}`, max 200 of each, samples >5 min in the future refused; 400 = rejected, 413 = too large, 401 = unauthorized | `ingest/src/ingest.rs:16-50`, `src/push.rs:305-335` |
| Alerts `SILENT`/`REPORTING` to its own Discord webhook (`NINJA_INGEST_DISCORD_WEBHOOK`) when a host stops reporting for `silence.after` (default 2m) | `deploy/ingest.config.example.toml:26-38`, `ingest/README.md:34-46` |
| Meant to run behind `cloudflared` on a Proxmox LXC (`deploy/proxmox-setup.sh`), read routes behind Cloudflare Access, `/v1/ingest` bypass; `cors_origins` example lists `ninjagoldfinch.com`/`.nz` (not `.lol`) | `ingest/README.md:128-230`, `deploy/ingest.config.example.toml:11-15` |

### 2.4 Hosts referenced

ninja-monitor's own repo references **no** real hostnames (grep `ngf-` over the repo: none; `[host] name` defaults to `"vps"`, `deploy/config.example.toml:22`). The host names come from riot-proxy's docs:

| Host | Role | Evidence |
|---|---|---|
| `ngf-prod-syd1` | Onidel ONI-4 in Sydney running riot-proxy (`127.0.0.1:8080`), the site (`127.0.0.1:3000`), `cloudflared`, and `ninja-monitor` watching `riot-proxy`, `ninjagoldfinch-lol` and `cloudflared` with per-service memory limits 2 GiB / 1 GiB / 512 MiB, "down" after 30 s repeating every 30 min, host memory < 15 %, disk > 70 % and > 85 %, host CPU > 90 % for 20 min; its API reached over SSH local forwards | `riot-proxy:docs/design/12-object-storage-and-hosting.md:63,832-837`; `riot-proxy:docs/DECISIONS.md:1510` |
| `ngf-mon-syd1` | the wiped old OVH VPS, kept as the monitoring host; `ninja-monitor` watching the host; Prometheus + Grafana planned there, Prometheus to scrape riot-proxy `/metrics` over WireGuard (not through the tunnel) — **not yet done** | `riot-proxy:docs/design/12-…:69,478,843-847`; `riot-proxy:docs/design/07-deployment.md:221`; `riot-proxy:docs/design/13-admin-dashboard.md:9,60`; `riot-proxy:docs/DECISIONS.md:1512` |

Each box posts to **its own Discord webhook** (`riot-proxy:docs/design/12-…:832-833`).

`TODO (unverified): whether ninja-ingest is deployed anywhere (no deployment of it is recorded in riot-proxy's docs; ninja-monitor's README describes it as optional). The monitoring boxes' actual config.toml files were not read.`
`TODO (unverified): whether the production ninja-monitor on ngf-prod-syd1 has SupplementaryGroups=riot-proxy or an ACL for /var/lib/riot-proxy disk measurement.`
`TODO (unverified): the WireGuard link ngf-mon-syd1 → ngf-prod-syd1 and the Prometheus scrape of /metrics are "planned" in riot-proxy docs; nothing in ninja-monitor implements or configures them.`

---

## 3. ninjagoldfinch.lol (GitHub Pages placeholder)

Confirmed a placeholder. Entire tracked content: `CNAME` (`ninjagoldfinch.lol`, `:1`), `.nojekyll` (empty), `index.html` (a 5-line page printing the domain, `:1-5`), `README.md` (`# ninjagoldfinch.lol`, `:1`), `riot.txt` (a UUID, `:1` — Riot developer-portal app verification file). One commit `8da0cf4 "Add placeholder index.html so the root URL doesn't 404"`; remote `https://github.com/NinjaGoldfinch/ninjagoldfinch.lol`; no `.github/`, no code, no proxy references.

Since the apex is served through the Cloudflare Tunnel to the SvelteKit server (`ninjagoldfinch-lol:docs/DECISIONS.md:510`), this Pages repo's `CNAME` can no longer be what resolves `ninjagoldfinch.lol`. `TODO (unverified): whether GitHub Pages is still enabled for this repo and whether riot.txt is still needed/served for Riot's app verification (it is not in the SvelteKit site's static/, which holds only robots.txt).`

---

## 4. Cross-service call table (the canonical contract)

Direction uses `site` = ninjagoldfinch-lol server (Node, `127.0.0.1:3000`), `browser` = a visitor, `rp` = riot-proxy (`127.0.0.1:8080`), `mon` = ninja-monitor agent, `ingest` = ninja-ingest.

| Direction | Protocol | Endpoint | Auth | Payload | Frequency | Failure behaviour | Who retries | Evidence |
|---|---|---|---|---|---|---|---|---|
| site → rp | HTTP/1.1 GET, JSON | `/v1/players/by-riot-id/{gameName}/{tagLine}/profile[?refresh=true]` | `Authorization: Bearer <PROXY_KEY>` (if set) | none; response: profile with `puuid`, `platform`, `region`, `account`, `summoner`, `league`, `fetchedAgeSeconds`, `refreshAvailableIn`, `warnings` | every profile view / refresh POST | 404/429/503 page (`errors.ts:16-33`); unknown platform → 503; budget 30 s → 503 (`@origin/main`) | nobody on the site; rp retries Riot 5xx internally (ADR-040 context) | `profile.ts:77-87`, `profile.ts:112-137` |
| site → rp | GET | `/v1/players/{puuid}/matches?platform&start&count=10&queue&champion[&refresh=true]` | Bearer | response: `matches[]`, `hasMore`, `warnings`, `matchIdsFetchedAgeSeconds`, `refreshAvailableIn`, `archive.complete` | every profile view | optional: `matches:null` + `matchesError`, page `no-store` | nobody | `profile.ts:89-104,112-147` |
| site → rp | GET | `/v1/static/versions` | Bearer | response: `current`, `versions[]` | every data page (worktree); once per 5 min (`@origin/main`) | 503 (essential); budget 5 s | nobody (memo not kept on failure) | `patch.ts:49-66`; `@origin/main:patch.ts:45-85` |
| site → rp | GET ×4 | `/v1/static/{champion\|item\|summoner\|runesReforged}?version=<patch>` | Bearer | Data Dragon JSON (item.json ~700 kB) | once per patch per process (memo) | fallback `NO_NAMES` or 503 (champion page) | next page retries (failed memo dropped) | `static-data.ts:24-64` |
| site → rp | GET | `/v1/static/queues` | Bearer | queues JSON | with the four above | as above | as above | `static-data.ts:50` |
| site → rp | GET | `/v1/lol/matches/{region}/{matchId}` | Bearer | Riot match-v5 `MatchDto` (typed by hand, `src/lib/api/match.ts:1-4`) | every match view | 404/429/503; custom game → 404 | nobody | `match.ts:37-49` |
| site → rp | GET | `/v1/lol/matches/{region}/{matchId}/timeline` | Bearer | Riot `TimelineDto` (reduced server-side) | every match view | optional: no gold/build data, page `no-store` | nobody | `match.ts:39,51-54` |
| site → rp | GET | `/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5[&patch][&role]&minGames=0&limit=500` | Bearer | `champions[]` rows (summed over tiers, `tier: null`) | every `/champions` view (edge-cached 10 min) | 404/429/503; budget 5 s | nobody | `champions.ts:37-42`, `champion-stats.ts:93-107` |
| site → rp | GET | `/v1/lol/analytics/patches?queue=RANKED_SOLO_5x5` | Bearer | `patches[]` | every `/champions` view | optional: default picker | nobody | `champions.ts:39,43-44` |
| site → rp | GET | `/v1/lol/analytics/champions/{championId}?…&limit=10` | Bearer | `stats`, `byTier`, `patch`, per-item/rune/spell lists | every champion page view | essential: 404/429/503 | nobody | `champion-page.ts:106-108,119` |
| site → rp | GET | `/v1/lol/analytics/champions/{championId}/builds?…` | Bearer | set builds (`role`, `totalGames`, `builds[]`) | every champion page view | optional: `setBuilds:null` | nobody | `champion-page.ts:109-111,129` |
| site → rp | GET | `/v1/lol/analytics/champions/{championId}/matchups?…&limit=200` | Bearer | `matchups[]` (`opponentId`, `opponentName`, `role`, `games`, `wins`, `winRate`) | every champion page view | optional: `matchups:null` | nobody | `champion-page.ts:112-114,140-143` |
| site → rp | GET | `/v1/lol/analytics/patches?queue=RANKED_SOLO_5x5&championId=<id>` | Bearer | `patches[]` | every champion page view | optional: `patches:[]` | nobody | `champion-page.ts:115-117,145` |
| site (dev tool) → rp | GET | any path, from `PROXY_URL` | Bearer if `PROXY_KEY` set | writes a redacted fixture | manual (`just fixture`) | prints status | human | `tests/record-fixture.ts:37-82` |
| site → CommunityDragon | HTTPS GET | `{CDRAGON_URL}/{major.minor}/plugins/rcp-be-lol-game-data/global/default/v1/perks.json` | none | perks JSON, reduced to stat shards 5000–5999 | once per patch per process | optional: no stat shards | next page (failed memo dropped) | `cdragon.ts:9-28`, `src/lib/api/cdragon.ts:16-30` |
| browser → site | HTTPS via Cloudflare Tunnel | pages, `__data.json`, `POST /summoner/[riotId]?/refresh` (form, same-origin enforced by build-time `ORIGIN`), `/healthz` | none | HTML / SvelteKit data | per visit | SvelteKit error pages; Cloudflare edge cache per `cache.ts` | browser | `cache.ts:5-16`, `hooks.server.ts:5-10`, `summoner/[riotId]/+page.server.ts:30-38`, `README.md:99,113` |
| browser → Riot Data Dragon CDN | HTTPS GET | `https://ddragon.leagueoflegends.com/cdn/{patch}/img/{champion\|item\|profileicon\|spell}/…png`, `…/cdn/img/perk-images/…` | none | PNG | per rendered icon | broken image | browser | `assets.ts:7,13-33` |
| browser → CommunityDragon | HTTPS GET | `https://raw.communitydragon.org/{major.minor}/plugins/rcp-be-lol-game-data/global/default/v1/<shard icon>`, `…/rcp-fe-lol-clash/…/icon-position-<lane>.png` | none | PNG | per rendered icon | broken image | browser | `assets.ts:8,39-49,68-71` |
| browser → rp | — | **none** | — | — | — | — | — | `CLAUDE.md:13-20,57`; grep (no client fetch) |
| rp → site | — | **none** (no callbacks, no WS consumer) | — | — | — | — | — | grep `/v1/ws`, `WebSocket`: none |
| mon → kernel (local) | procfs/cgroupfs reads | `/proc/*`, `/sys/fs/cgroup/system.slice/{riot-proxy,ninjagoldfinch-lol,cloudflared}.service/*`, `du`-style walk of `/var/lib/riot-proxy` | file permissions (needs group/ACL for the data dir) | metrics | every 15 s; disk every 10 m | value `null`, service `down`; warning once per unreadable path | n/a | `README.md:50-81,471-496`, `deploy/config.example.toml:29-42` |
| mon → rp (HTTP) | — | **none** (`/healthz`, `/readyz`, `/metrics` are not probed) | — | — | — | — | — | grep (no `readyz`/`metrics`/`prometheus`) |
| mon → Discord | HTTPS POST | webhook URL from `NINJA_MONITOR_DISCORD_WEBHOOK` | URL is the secret | `**FIRING**/**RESOLVED**/STARTED` text, no pings | on alert transitions, `repeat`, and start | queued (100), oldest dropped | mon: 3× exponential backoff, honours `retry_after` | `README.md:277-278,343-353`, `src/alerts/notify.rs` |
| mon → external checker (optional) | HTTPS GET | URL from `NINJA_MONITOR_HEARTBEAT_URL` | URL is the secret | none | every 60 s | logged and ignored | none | `src/heartbeat.rs:10-28`, `deploy/config.example.toml:50-55` |
| mon → ingest (optional) | HTTPS POST JSON | `<push.url>` e.g. `https://ingest.example.net/v1/ingest` (plain `http://` only to loopback) | `Authorization: Bearer <NINJA_MONITOR_PUSH_TOKEN>` | `{agent, host, samples[≤200], events[≤200]}` | each sample (≥ `flush_interval` = sampling interval) | 400 drop batch, 413 halve, 401 keep buffering, 429/5xx/timeout back off; buffer 1 h / 10 000 samples; Discord `ingest unreachable` after 10 min | mon: 15 s → 30 s → 60 s cap | `src/push.rs:27,266-335`, `README.md:355-389`, `src/config.rs:230-241` |
| mon (`install.sh --check`) → ingest | HTTP GET | `<push.url origin>/healthz` | none | `{status, version, uptime_secs}` | once at install/check | WARN line | human | `src/check.rs:96-123` |
| ingest → Discord | HTTPS POST | webhook from `NINJA_INGEST_DISCORD_WEBHOOK` | URL is the secret | `SILENT`/`REPORTING`/`STARTED` | host silent > 2 m | queued | 3× backoff | `ingest/README.md:34-46`, `deploy/ingest.config.example.toml:26-38` |
| owner → mon / ingest API | HTTP over SSH forward | `127.0.0.1:9090/{healthz,stats,history,alerts}`, `127.0.0.1:9091/{healthz,hosts,...}` | SSH | JSON | ad hoc | — | — | `README.md:238-250,391-463`; `ingest/src/api.rs:67-75` |

Headers on the site ↔ rp hop, summarised: **sent** `Authorization: Bearer` only (`proxy.ts:31`), no `If-None-Match`, no `X-Request-Id`; **read** `X-Request-Id` (failure log), `Retry-After` (429), `X-Cache` (slow-analytics log, `@origin/main`); **ignored** `X-RateLimit-*`, `X-Cache-Age`, `X-Cache-Fetched-Age`, `ETag`.

---

## 5. Where notes/docs and code disagree

1. **Images from the mirror.** `riot-proxy:docs/IMPLEMENTATION.md:381` (SITE-07) records "the site will serve its images from the mirror, and its generated client calls only declared routes". The site's code builds image URLs straight to `ddragon.leagueoflegends.com` and `raw.communitydragon.org` (`ninjagoldfinch-lol:src/lib/assets.ts:7-8`) and ADR-009 (`docs/DECISIONS.md:103-112`) keeps it that way; no code references `/ddragon/`. Code wins: the browser fetches images from Riot/CommunityDragon, not from riot-proxy.
2. **"The site revalidates every call (ADR-099)".** `riot-proxy:docs/DECISIONS.md:1734` (ADR-166) says this about ninjagoldfinch.lol. ADR-099 (`:1102-1106`) is about riot-proxy's own showcase/explorer pages passing `cache: 'no-cache'`. The site sends no conditional headers (no `If-None-Match` anywhere; `openapi-fetch` over Node fetch has no HTTP cache), so every site call is an unconditional full read, not a revalidation. The practical conclusion of ADR-166 (ETag saved nothing) still holds, for a different reason.
3. **`PROXY_URL` default.** Worktree `README.md:45` says it defaults to `http://192.168.68.46:8080`; code has never had a default (`src/lib/server/env.ts:10-15`), and `@origin/main:README.md:45` corrects this. The dev instance is gone (handoff 2026-10-15 line 7).
4. **Node floor.** `README.md:12` "Node 22.17 or later" vs `package.json:8` `>=22.18` (enforced). ADR-038 says 22.18 (`docs/DECISIONS.md:513`).
5. **Deployment plan vs reality.** `docs/IMPLEMENTATION.md` architecture/compose sections (Proxmox VM, `web + cloudflared (+ riot-proxy)` compose, `TUNNEL_TOKEN`, `ADDRESS_HEADER`) describe a setup production does not use; ADR-038 (`docs/DECISIONS.md:500-513`) is the real one (`node build` + systemd on `ngf-prod-syd1`).
6. **Handoff 2026-10-15 line 36** ("committed OpenAPI copy reports 2.0.0-rc.3") is stale against `@origin/main` (2.6.0); it was accurate for the working tree the note was written against.
7. **ninja-ingest `cors_origins` example** lists `ninjagoldfinch.com` and `.nz` (`deploy/ingest.config.example.toml:15`), not the `.lol` site; it is an example, not a deployment.

---

## 6. Open TODOs collected

- `TODO (unverified): PROXY_URL value in /etc/ninjagoldfinch-lol/env on ngf-prod-syd1 (expected http://127.0.0.1:8080).`
- `TODO (unverified): contents of ninjagoldfinch-lol.service (uncommitted; hand-copied per riot-proxy:docs/design/12-…:627).`
- `TODO (unverified): the production consumer key's name/scope for the site.`
- `TODO (unverified): whether Cloudflare's cache rule covers HTML and __data.json (site SPD-06 / P5-02, unticked).`
- `TODO (unverified): whether the 4 unseen remote commits on ninjagoldfinch-lol (b8d0b28..8d226db) are VS-01..04 (spec 2.7.0, minTier, opponentId).`
- `TODO (unverified): whether GitHub Pages is still enabled for ninjagoldfinch.lol and whether riot.txt still needs serving.`
- `TODO (unverified): whether ninja-ingest is deployed; the real /etc/ninja-monitor/config.toml on ngf-prod-syd1 and ngf-mon-syd1 (riot-proxy design/12 §Monitoring describes thresholds that differ from deploy/config.example.toml, e.g. down after 30 s vs 1 m, disk 70 %/85 % vs 85 %, memory 15 % vs 10 %).`
- `TODO (unverified): WireGuard + Prometheus scrape of /metrics from ngf-mon-syd1 — planned only (riot-proxy design/07:221, design/12:846).`
- `TODO (unverified): the Cloudflare Tunnel connector's name and the exact tunnel ingress rules (riot-proxy design/12:894 says the repository does not record its name).`

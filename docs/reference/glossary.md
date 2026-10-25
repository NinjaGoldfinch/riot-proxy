# Glossary

Riot and League of Legends terms as this service uses them, then the project's own vocabulary. Each entry says where the term is defined in the repository.

## Riot and domain terms

| Term | Meaning | Where |
|---|---|---|
| **PUUID** | Riot's global player id, a 78-character URL-safe string that is the same on every platform. The proxy accepts 60 to 128 chars of `[A-Za-z0-9_-]`. Placeholder in these docs: `AbC…`. | `src/http/validate.rs:L133-L147` |
| **Riot ID** | `gameName#tagLine`, the player-facing name (1 to 16 code points, 1 to 5 code points). Resolved to a PUUID by account-v1; Riot's spelling wins. In URLs the two parts are separate segments. | `src/http/validate.rs:L117-L127`, `src/routes/admin.rs:L493-L495` |
| **account-v1** | Riot's account API (`/riot/account/v1/...`), served by three clusters (`americas`, `asia`, `europe`), each holding every account. | `src/riot/endpoints.rs:L93-L112`, `src/riot/routing.rs:L148-L149` |
| **Summoner** | The per-platform League profile (`profileIconId`, `summonerLevel`, `revisionDate`), from summoner-v4 by PUUID. | `src/riot/endpoints.rs:L113-L122` |
| **Platform** | A game shard and its API host: `na1`, `br1`, `la1`, `la2`, `euw1`, `eun1`, `tr1`, `ru`, `kr`, `jp1`, `oc1`, `ph2`, `sg2`, `th2`, `tw2`, `vn2` (`<platform>.api.riotgames.com`). Lower-case and case-sensitive in this API. | `src/riot/routing.rs:L46-L63` |
| **Region** (routing value) | The regional host for match-v5: `americas`, `europe`, `asia`, `sea`. Each platform maps to one; the full table is in [../api/endpoints.md](../api/endpoints.md#regions-and-platforms). | `src/riot/routing.rs:L108-L116` |
| **Account cluster** | The account-v1 host for a platform or region: `sea` has none and goes to `asia`. The proxy picks a cluster when the caller names none (ADR-066). | `src/riot/routing.rs:L118-L121,L151-L153,L174-L181` |
| **Match id** | `<PLATFORM>_<number>`, for example `OC1_700000001`, `KR_8393343196`. The prefix names the hosting platform and therefore the region; `ME1_` is served by `europe`. | `src/riot/routing.rs:L132-L143,L183-L195` |
| **Match** | The match-v5 `MatchDto` for one game: `metadata` and `info` with participants and teams. Immutable once the game ended; archived forever. | `src/riot/endpoints.rs:L185-L194` |
| **Timeline** | The match-v5 timeline for a match: per-minute frames and events (items bought, skills, kills). One extra, large request per match; archived under `timelines`. | `src/riot/endpoints.rs:L195-L204`, `README.md` (`ARCHIVE_TIMELINES`) |
| **Queue** | A game mode, identified by match-v5's numeric `queueId` or league-v4's name. Evidenced ids: `420` `RANKED_SOLO_5x5` (ranked solo/duo), `440` `RANKED_FLEX_SR` (ranked flex); the site's history filter also offers `400`, `450`, `480`, `490` and `1700`. Descriptions come from `GET /v1/static/queues`. | `src/riot/ladder.rs:L7,L24`, `docs/_meta/scratch/siblings.md` §1.5 |
| **Patch** | A game version as `major.minor`, for example `16.20`; the proxy stores it per match and analytics are sliced by it. `all` in a filter means every patch. | `src/http/validate.rs:L211-L229`, `docs/_meta/scratch/persistence.md` (matches.patch) |
| **Data Dragon version** | The CDN's three-part version, for example `16.20.1`, used for `/v1/static/*?version=` and image paths. Not a patch value. | `src/http/validate.rs:L232-L244` |
| **Data Dragon** | Riot's static-data CDN (`ddragon.leagueoflegends.com`): champion, item, rune, spell, icon and map JSON plus images. Mirrored by the `ddragon:sync` job; no key, no rate limit. | `src/jobs/ddragon.rs:L22-L23`, `src/static/mod.rs:L28-L35` |
| **CommunityDragon** | A community mirror (`raw.communitydragon.org`) of client assets. Used by ninjagoldfinch.lol for stat-shard and lane icons; riot-proxy does not mirror it. | `docs/api/openapi.json` `info.description`, `docs/_meta/scratch/siblings.md` §1.7 |
| **Tier** | Ranked tier: paged tiers `IRON`, `BRONZE`, `SILVER`, `GOLD`, `PLATINUM`, `EMERALD`, `DIAMOND`; apex tiers `MASTER`, `GRANDMASTER`, `CHALLENGER`. `UNKNOWN` is the analytics bucket for an unranked or unstamped player. | `src/riot/ladder.rs:L9-L13`, `src/routes/analytics.rs:L493-L496` |
| **Apex tiers** | The top three, served whole by their own league endpoint rather than by page; Riot caps each list at 10 000 entries. Everyone in them reports division `I`. | `src/riot/ladder.rs:L13-L22,L50-L56` |
| **Division** | `I`, `II`, `III`, `IV` within a paged tier; `I` is highest. | `src/riot/ladder.rs:L22` |
| **LP** | League points, `leaguePoints` in a league entry; the `rank.changed` event reports `{tier, rank, lp}`. | `docs/_meta/scratch/jobs-and-observability.md` §5 |
| **Tier floor** | The lowest tier a ladder crawl walks; everything at or above it is enumerated. Default `MASTER` (3 legs); `IRON` means 31 legs. | `src/riot/ladder.rs:L40-L42`, `README.md` (`LADDER_TIER_FLOOR`) |
| **Remake** | A game ended early by vote; marked on the match row and excluded from analytics unless `remakes=include`. | `docs/_meta/scratch/persistence.md` (matches.remake), `src/http/validate.rs:L248-L250` |
| **Lane / role / team position** | `TOP`, `JUNGLE`, `MIDDLE`, `BOTTOM`, `UTILITY` (Riot's `teamPosition`). Team positions add `""` for queues without positions; a matchup is always in a lane. | `src/routes/analytics.rs:L53-L56` |
| **Matchup** | Champion A against champion B in the same lane, with games, wins and win rate. | `src/routes/analytics.rs:L153-L178` |
| **Set build** | A recurring item build: core items, then starter, boots, next-item slots, runes, spells and skill order, with a Wilson lower bound on the win rate. | `src/routes/analytics.rs:L326-L390,L649-L658` |
| **Key scope** (Riot) | A Riot key's purpose: development (personal) keys versus production keys. Not to be confused with the proxy's `keyScope`. | `src/riot/client.rs:L305` (the expiry reminder) |

## Project terms

| Term | Meaning | Where |
|---|---|---|
| **Consumer** | A client of the proxy: a row in `consumers` with a name, scopes, quota and the SHA-256 of its key. | `src/consumers.rs:L18-L45,L113-L162` |
| **Consumer key** | `rpx_` plus 32 URL-safe characters (36 total), shown once at creation, never stored in clear. Sent as `Authorization: Bearer` or `?token=`. | `src/consumers.rs:L12,L95-L107` |
| **Scope** (consumer) | `read` or `admin`. `read` opens every `/v1/*` data route; `admin` opens `/v1/admin/*`, `?refresh` on passthrough and the admin WebSocket topics, subject to the IP allowlist. | `src/consumers.rs:L18-L45`, `src/http/auth.rs:L276-L288` |
| **Quota** | A consumer's requests per rolling 60 s (`quotaPerMin`, default 600), reported in `X-RateLimit-*`; exhaustion is `429 QUOTA_EXCEEDED`. | `src/http/quota.rs:L320-L431` |
| **`keyScope`** (proxy) | The first 8 hex characters of `sha256(RIOT_API_KEY)`. Prefixes every cache key and scopes tracked players and crawls, so changing the Riot key starts a fresh namespace. | `src/cache/keys.rs:L31-L37`, `src/routes/admin.rs:L99-L101` |
| **Interactive vs bulk** | The two priorities at the Riot limiter. Interactive is a request someone is waiting on (an HTTP call); bulk is background work, held under a usage ceiling (`BULK_USAGE_CEILING` 0.80, `BULK_IDLE_CEILING` 0.95 after 30 s idle) so it never starves interactive callers. | `src/riot/limiter/mod.rs:L306-L360`, `README.md` |
| **Budget** (wait) | How long a caller may wait for a limiter token before `RATE_LIMITED`: interactive `CLIENT_WAIT_BUDGET_MS` (2 000 ms), jobs `JOB_YIELD_BUDGET_MS` (1 000 ms), other bulk 15 min. | `src/fetcher.rs:L42-L44,L346-L350` |
| **Scope** (limiter) | A platform or region value on the Riot host, each with its own app and method windows. `GET /v1/admin/limits/{scope}` shows them. | `src/riot/limiter/bucket.rs:L165-L181` |
| **Freeze** | After a typed Riot 429 a whole scope is held for `Retry-After` seconds; `frozenMs` in the limits document. | `src/riot/limiter/mod.rs:L407-L431` |
| **Job** | A row in `jobs` with a kind, payload, priority and dedupe key, claimed by a worker. 22 kinds, for example `archive:match`, `backfill:player`, `ladder:crawl`, `aggregate:analytics`, `ddragon:sync`, `maintenance`. | `src/jobs/mod.rs:L20-L71` |
| **Lane** (job) | A scheduler lane: a concurrency slot class for jobs, separate from the game's lanes. | `src/jobs/lanes.rs` |
| **Yield** | A job giving its turn back when the limiter has no token inside its budget; it is re-queued and resumes from its cursor rather than failing. | `docs/_meta/scratch/jobs-and-observability.md` §2.6 |
| **Tick** | A periodic enqueue of a singleton job (`ddragon:sync` hourly, `maintenance` daily, polls per tracked player, scheduled crawls). | `src/jobs/ticks.rs` |
| **Crawl** | A ladder crawl: enumerate the ladder from the tier floor up, collect each player's recent ranked match ids, archive those matches, then recompute analytics. One running crawl per platform and queue. | `docs/_meta/scratch/jobs-and-observability.md` §4 |
| **Crawl leg** | One unit of a crawl's enumerate stage: an apex league (`ladder:apex`) or one (tier, division) walk (`ladder:walk`). Legs are tracked in `crawl_legs` and the crawl ends when the last leg does. | `src/jobs/ladder.rs:L221-L321` |
| **Backfill** | Walking a player's match history backwards (`backfill:player`) and archiving what is missing, up to `LOOKUP_BACKFILL_LIMIT`. Triggered by a first lookup, tracking, an admin call or a poll that fell behind. | `src/jobs/archive.rs:L309-L419` |
| **Tracked player** | A player the proxy polls for live games, rank changes and new matches (`poll:live`, `poll:rank`, `poll:matches`), publishing events on `player:<puuid>`. | `src/routes/admin.rs:L454-L571` |
| **Facts** | The per-participant rows extracted from an archived match body (champion, role, tier, result, K/D/A, …) that analytics aggregate; versioned, re-extracted by `facts:reextract`. | `docs/_meta/scratch/jobs-and-observability.md` §3 |
| **Aggregate** | The `aggregate:analytics` rebuild of champion stats, matchups and builds per platform, queue, patch and tier, over the newest `AGGREGATE_PATCH_LIMIT` (4) patches. Since THR-03 the repair path; new matches are added by **Delta**. | `src/jobs/analytics.rs:L354-L520` |
| **Delta** | The `aggregate:delta` job (THR-03, ADR-182): counts the matches no rebuild or delta has counted yet (`analytics_counted.bits` below 63) into the same tables, a short write transaction per batch, for every configured ladder a full rebuild has stamped. Every `AGGREGATE_DELTA_S` (60 s) and after each crawl. | `src/jobs/analytics.rs:L772-L951` |
| **Pack** | A 64 MiB object (`ARCHIVE_PACK_TARGET_BYTES`) holding many zstd match or timeline bodies with an index, written to object storage by `archive:offload`. | `docs/design/12-object-storage-and-hosting.md:L83-L94,L151-L156` |
| **Offload** | Freeing a packed body's local bytes once it is older than `ARCHIVE_BODY_LOCAL_DAYS` (14); reads then come from the pack by byte range. | `docs/_meta/scratch/persistence.md` (offload) |
| **Local window** | Those 14 days during which a body stays on disk as well as in its pack. | `src/config.rs:L749-L751` |
| **Index snapshot** | A daily `index/packs-<date>.jsonl.zst` and `index/locations-<date>.jsonl.zst` in the bucket so the body map can be rebuilt without a database backup. | `docs/design/07-deployment.md:L183` |
| **L1 / L2** | L1 is the in-memory response cache (moka, `CACHE_L1_MAX_MB` 128); L2 is the SQLite `cache` table that persists selected endpoints across restarts and warms L1 at boot. | `src/cache/l1.rs:L88-L119`, `src/cache/l2.rs:L186-L216` |
| **SWR** | Stale-while-revalidate: between an entry's soft and hard TTL (hard = soft × 4) it is served as `STALE` while a bulk refresh runs in the background. | `src/riot/endpoints.rs:L476-L497`, `src/fetcher.rs:L332-L338,L376-L390` |
| **Negative cache** | A cached Riot 404 (30 s, or 300 s for accounts), served as `404` with `X-Cache: HIT-NEG`. | `src/fetcher.rs:L321-L327`, `src/config.rs:L727-L728` |
| **`X-Cache` states** | `HIT`, `MISS`, `STALE`, `HIT-NEG`, `ARCHIVE`, `BYPASS`; see [../api/endpoints.md](../api/endpoints.md#shared-passthrough-behaviour-v1riot-v1lol). | `src/fetcher.rs:L50-L69` |
| **ADR** | An architecture decision record in `docs/DECISIONS.md`, numbered `ADR-nnn`, one per decision; indexed at [../architecture/decisions/README.md](../architecture/decisions/README.md). | `docs/DECISIONS.md` |
| **Task id** | The plan's task names: phase tasks `P2-03`, series tasks `OBJ-04`, `FLT-05`, `ADM-17`, `LAD-03`, `SITE-09`, `RC-02`. Used in commit footers (`Task: P2-03`) and ADR text. | `docs/IMPLEMENTATION.md`, `CLAUDE.md` |
| **`ngf-prod-syd1`** | The production VPS (Onidel Sydney) running riot-proxy, ninjagoldfinch.lol, cloudflared and ninja-monitor. | `docs/_meta/scratch/WRITING-BRIEF.md` |
| **`ngf-mon-syd1`** | The older OVH box running ninja-monitor, planned Prometheus/Grafana host. | `docs/_meta/scratch/WRITING-BRIEF.md` |
| **`admin-main`** | The owner's admin consumer key name, used by the dashboard. | `docs/_meta/scratch/WRITING-BRIEF.md` |
| **Edge image** | `ghcr.io/ninjagoldfinch/riot-proxy:edge`, rebuilt by `edge.yml` on every push to `main`; the dev VM tracked it. | `README.md` "Dev VM on Proxmox", [../workflows/ci.md](../workflows/ci.md) |
| **Dev VM** | Proxmox VM 102 at `192.168.68.46:8080` that ran `:edge` with `AUTH_DISABLED`; dormant now. | `docs/_meta/scratch/WRITING-BRIEF.md`, `deploy/proxmox/README.md` |

---
**Sources:** `src/http/validate.rs:L117-L250`, `src/riot/routing.rs:L46-L195`, `src/riot/ladder.rs:L7-L56`, `src/riot/endpoints.rs:L93-L204,L476-L497`, `src/riot/client.rs:L305`, `src/riot/limiter/mod.rs:L306-L431`, `src/riot/limiter/bucket.rs:L165-L181`, `src/routes/analytics.rs:L53-L56,L153-L178,L326-L390,L493-L496,L649-L658`, `src/routes/admin.rs:L99-L101,L454-L571`, `src/consumers.rs:L12-L45,L95-L162`, `src/http/auth.rs:L276-L288`, `src/http/quota.rs:L320-L431`, `src/cache/keys.rs:L31-L37`, `src/cache/l1.rs:L88-L119`, `src/cache/l2.rs:L186-L216`, `src/fetcher.rs:L42-L69,L321-L390`, `src/config.rs:L727-L751`, `src/jobs/mod.rs:L20-L71`, `src/jobs/ladder.rs:L221-L321`, `src/jobs/archive.rs:L309-L419`, `src/jobs/analytics.rs:L354-L520`, `docs/design/12-object-storage-and-hosting.md:L83-L156`, `docs/design/07-deployment.md:L183`, `docs/_meta/scratch/{jobs-and-observability,persistence,siblings,WRITING-BRIEF}.md`, `README.md`
**Last verified:** 2026-10-22 against commit `6eda84e`

# Adding a Riot call

How to teach the proxy a new Riot API method: the registry row in `src/riot/endpoints.rs`, where every number in it must come from, the tests that pin the table, and how to record a replay fixture without leaking the key.

## What the registry is

`ENDPOINTS` is the single table of every Riot method the proxy calls: its host family, path, query parameters, TTLs and cache tier (`src/riot/endpoints.rs:1-3`, `91-285`). A method id is Riot's rate-limit granularity: the limiter buckets on it and the cache key embeds it, so ids must stay stable even if a URL changes (`src/riot/endpoints.rs:5-6`). Routes, jobs, the dev CLI and the lane table all look methods up by id (`Endpoint::by_id`, `src/riot/endpoints.rs:325`).

## The `ep(` row

`ep(id, host, path_template, query, override_key, soft_ttl, persist_l2, negative)` builds one `Endpoint` (`src/riot/endpoints.rs:64-86`):

| Field | Type | Meaning | Source of the value |
|---|---|---|---|
| `id` | `&str` | `family.method` in v1's naming (`account.byPuuid`); doubles as the limiter's method bucket, `method_scope_key = id` (`src/riot/endpoints.rs:47-48`, `79`) | v1 `src/riot/endpoints.ts` for carried-over methods; the same style for new ones |
| `host` | `HostKind::{Platform, Regional, Account}` | `{platform}.api.riotgames.com`, `{region}.api.riotgames.com` (match-v5), or the three account-v1 clusters with `sea` sent to `asia` (`src/riot/endpoints.rs:16-26`) | the developer portal's routing for the method |
| `path_template` | `&str` | Riot's path with `{name}` segments filled in order by `Endpoint::path` (`src/riot/endpoints.rs:43-44`, `362-384`); values are `encodeURIComponent`-encoded (`src/riot/endpoints.rs:407-420`) | the developer portal, verbatim |
| `query` | `&[&str]` | the query parameters the method accepts; `RiotRequest::query` refuses others (`src/riot/endpoints.rs:45-46`) | the developer portal |
| `override_key` | `&str` | the key under which `CACHE_TTL_OVERRIDES=key=seconds` replaces the soft TTL of every method sharing it (`src/riot/endpoints.rs:49-50`, `424-444`, `476-497`); existing keys: `account, summoner, league, ladder, matchIds, match, timeline, spectator, mastery, rotations, status` | reuse a group's key when the method belongs to it (ladder reads share `ladder`, `src/riot/endpoints.rs:133-134`) |
| `soft_ttl` | `Option<Duration>` via `secs(n)` | how long a copy is fresh; `None` means **immutable** (`src/riot/endpoints.rs:51-52`, `59-61`, `83`). Hard TTL is soft × `STALE_MULTIPLIER` (4) with stale-while-revalidate on, else equal (`src/riot/endpoints.rs:13-14`, `476-497`) | see [Where TTLs come from](#where-ttls-come-from) |
| `persist_l2` | `bool` | written through to the SQLite `cache` table so it survives a restart (`src/riot/endpoints.rs:53-54`) | design/04's rule: persist if the TTL is at least an hour **or** it costs more than one upstream call to rebuild (`docs/design/04-data-and-cache.md:384`); its table wins over the rule (rotations, ADR-016) |
| `negative` | `NegTtl::{None, Default, Account}` | whether an upstream 404 is cached and for how long: `NEG_TTL_SECONDS` (30) or `NEG_TTL_ACCOUNT_SECONDS` (300) (`src/riot/endpoints.rs:28-37`; `src/config.rs:732-733`) | v1 §8.3: account lookups get the long one; list-shaped methods that cannot 404 get `None` |

The row goes at the **end** of `ENDPOINTS` and its id at the end of `V2_ADDED` (`src/riot/endpoints.rs:287-288`): the registry is v1's `METHOD_IDS` order followed by v2's additions, and the `parity` test asserts exactly that against `docs/contract/v1-endpoints.txt` (`src/riot/endpoints.rs:538-548`; the contract file lists the 16 v1 ids). Comment the row the way the two v2 rows are: which task, why that host and tier (`src/riot/endpoints.rs:257-284`).

## Where TTLs come from

Never from memory (`docs/IMPLEMENTATION.md:18`; CLAUDE.md "Never invent Riot semantics"). In order:

1. `docs/design/04-data-and-cache.md` §TTLs (`:392-407`): the table the registry's `documented_ttl_table` test pins (`src/riot/endpoints.rs:550-578`). If the method is there, use its row.
2. v1's `src/riot/endpoints.ts` at `c86e631` (cloned as `../riot-proxy-v1`): ids, paths, host families, TTLs and override keys were ported from it (`src/riot/endpoints.rs:1-3`).
3. The Riot developer portal: the method's documented behaviour, routing and how often its data changes.

A method none of the three covers is a question for the owner, not a guess: add a design/04 row in the same pull request, state the reasoning in the ADR, and mark anything unconfirmed. The same goes for `negative` and `persist_l2`.

### Immutable means archive, not cache

`soft_ttl: None` sets `immutable: true` (`src/riot/endpoints.rs:83`). Only `match.byId` and `match.timeline` are immutable, and they are not cached at all: the fetcher serves them from the archive (`X-Cache: ARCHIVE`) and the archive stores them forever (`src/riot/endpoints.rs:185-204`; `docs/design/04-data-and-cache.md:396`). An override on an immutable method is ineffective and `serve` warns about it (`src/riot/endpoints.rs:500-512`). A new immutable method therefore needs archive work (`src/archive/`), not just a row; that is a task of its own.

## Routing and the limiter: nothing to configure

- **Targets.** `Endpoint::target_for_platform` maps a platform to itself, its match-v5 region or its account cluster; `target_for_region` returns `None` for platform-hosted methods (`src/riot/endpoints.rs:386-404`). `Target::scope()` is the limiter's app scope, the platform or region string (`src/riot/endpoints.rs:316-321`). The platform-to-region table is `src/riot/routing.rs:108-121`.
- **Requests.** `RiotRequest::new(ep, target, &params)?.query(k, Some(v))?`; `RiotRequest::account(ep, &params)` for an `Account` method that may pick a cluster (`PROGRESS.md:403`; `src/routes/riot.rs:43-47`).
- **Limits.** The limiter learns every window from Riot's own headers: `X-App-Rate-Limit` and `X-Method-Rate-Limit` with their `-Count` pairs, `X-Rate-Limit-Type` and `Retry-After` (`src/riot/limiter/headers.rs:1-15`). Until the first answer it assumes the development-key app limits, 20 per 1 s and 100 per 120 s (`src/riot/limiter/headers.rs:31-40`). The method bucket is keyed by `method_scope_key`, which is the id; a new row gets its own bucket on first use. There is no per-method configuration.
- **Jobs.** If a background job will call the method, `jobs::lanes::of` must map that kind to the method so the claim respects the limiter (`src/jobs/lanes.rs:44-64`); see [adding-a-job.md](./adding-a-job.md).
- **Dev CLI.** `riot get` resolves a method by id or by slug (`account.byRiotId` is `account/by-riot-id`), so the new id works at once: `just riot clash/player-by-puuid euw1 <puuid>` (`src/cli/riot.rs:63-81`; `justfile:45-47`).

## Unit tests to extend

All in `src/riot/endpoints.rs`'s `mod tests`; run them with `cargo test --lib riot::endpoints`:

| Test | Lines | What to change |
|---|---|---|
| `parity` | 538-548 | nothing, once the id is appended to `V2_ADDED` |
| `documented_ttl_table` | 550-578 | add `(id, soft, hard)`; it asserts `table.len() + 2 == ENDPOINTS.len()` |
| `matches_and_timelines_are_immutable` | 597-606 | only if the method is immutable |
| `host_families` | 609-637 | add the id under its family |
| `l2_membership_matches_design_04` | 638-657 | add the id to the persisted or non-persisted list |
| `negative_ttls_follow_v1` | 659-677 | add the id under its `NegTtl` |
| `ladder_reads_share_one_override_key` | 680-696 | only for a ladder read |
| `every_endpoint_group_resolves_to_the_right_host` | 823 onwards | add the id to its group |

`paths_fill_and_encode_like_encode_uri_component` (`:742-780`) and `targets_resolve_to_the_right_host` (`:781-810`) are worth a case when the path has an unusual segment or the host family is `Account`.

## Recording a replay fixture

Replay fixtures are real exchanges served back by wiremock (`tests/fixtures/replay/README.md:1-4`; `tests/replay.rs:1-4`). Record one with the dev CLI, which needs a real key in `./.env`:

```bash
OUT=tests/fixtures/replay/<scenario>
cargo run -q --features dev-cli -- riot record --out $OUT <endpoint-id-or-slug> <platform-or-region> [PARAMS]... [-q k=v]...
```

What `riot record` does (`src/cli/record.rs:1-7`, `18-29`, `106-158`):

- appends `NN-<method>.json` (request, status, headers) and `NN-<method>.body` (the bytes, verbatim) to `--out`, `NN` being the count of existing `.json` files plus one;
- keeps only the headers the proxy reads: `content-type`, the four rate-limit headers, `x-rate-limit-type`, `retry-after` (`KEPT_HEADERS`);
- replaces every occurrence of the key with `RGAPI-REDACTED` and **refuses to keep a file** that still contains the key or any `RGAPI-` followed by 20 or more `[0-9a-f-]` characters, the same pattern CI greps for (`.github/workflows/ci.yml:52-53`).

Then: document the scenario in `tests/fixtures/replay/README.md` as `cold-lookup` is (`:6-20`), mount it in a test the way `tests/replay.rs` does (`load` and `mount`, `:25-60`), and expect the replay snapshot to change; review it deliberately (`tests/fixtures/replay/README.md:22`). A synthetic variant (an edited status or header) is marked `"synthetic": true`, as `429-typed-application` is; a real application 429 is never provoked because it is an accountable violation of the key (`tests/fixtures/replay/README.md:24-26`). PUUIDs in fixtures are Riot's encrypted ids for this key and are fine to keep; names and tag lines of real players are already public data, but prefer the fixtures' existing player.

## Worked example (hypothetical)

A `clash.playerByPuuid` method for a Clash route ([adding-an-endpoint.md](./adding-an-endpoint.md)). The TTL and path are placeholders, see the TODO below.

```rust
// src/riot/endpoints.rs, appended to ENDPOINTS (hypothetical)
    // v2 (CLASH-01). clash-v1 players by PUUID: a platform read like summoner-v4.
    // TTL and tier: <design/04 row or ADR reference here>.
    ep(
        "clash.playerByPuuid",
        OnPlatform,
        "/lol/clash/v1/players/by-puuid/{puuid}",
        &[],
        "clash",
        secs(300),
        false,
        NegDefault,
    ),
];

pub const V2_ADDED: &[&str] = &["league.expEntries", "account.regionByPuuid", "clash.playerByPuuid"];
```

Then the tests above (`documented_ttl_table` gains `("clash.playerByPuuid", 300, 1200)`), a design/04 §TTLs row, `.env.example`'s `CACHE_TTL_OVERRIDES` comment if the key is new, an ADR, and the route.

> **TODO (unverified):** the real clash-v1 path, its query parameters, whether a 404 is the right negative-cache case, and a TTL grounded in how Clash registrations change. The example values above were not checked against the developer portal or v1 (which never called clash-v1: it is absent from `docs/contract/v1-endpoints.txt`).

## Checklist

1. Row appended to `ENDPOINTS`, id appended to `V2_ADDED`, comment says where the numbers come from.
2. design/04 §TTLs row (and `.env.example` if a new override key).
3. The eight registry tests updated; `cargo test --lib riot::endpoints` green.
4. `jobs::lanes::of` if a job will call it.
5. A replay fixture if a test needs real bytes; README updated; no key in it.
6. ADR, `PROGRESS.md`, `docs/reference/riot-api-usage.md`.

---
**Sources:** `src/riot/endpoints.rs:L1-L6`, `L13-L86`, `L91-L135`, `L185-L204`, `L257-L288`, `L316-L325`, `L362-L420`, `L424-L444`, `L476-L512`, `L538-L578`, `L597-L696`, `L742-L823`, `src/riot/routing.rs:L108-L121`, `src/riot/limiter/headers.rs:L1-L40`, `src/jobs/lanes.rs:L44-L64`, `src/cli/riot.rs:L63-L81`, `src/cli/record.rs:L1-L29`, `L106-L158`, `src/routes/riot.rs:L43-L47`, `src/config.rs:L732-L733`, `docs/contract/v1-endpoints.txt`, `docs/design/04-data-and-cache.md:L384`, `L392-L407`, `docs/IMPLEMENTATION.md:L18`, `tests/fixtures/replay/README.md:L1-L26`, `tests/replay.rs:L1-L60`, `.github/workflows/ci.yml:L52-L53`, `justfile:L45-L47`, `PROGRESS.md:L403`
**Last verified:** 2026-10-26 against commit `c9e1b33`

# Gaps: everything marked TODO (unverified)

Every `> **TODO (unverified):**` marker in this documentation set, collected in one place and ordered by how much it matters, with the file and line it lives on and a suggested way to close it. Regenerate the raw list with `python3 docs/_meta/tools/gaps.py` (only the blockquote form counts; prose that mentions the phrase does not).

Counts on 2026-10-20: 50 markers across 29 files.

## 1. Production state that lives only on the box

These matter most: the docs describe production from the design record and dated session notes, not from the host. One SSH session by the owner closes all of them.

| Where | Gap | How to resolve |
|---|---|---|
| `deployment/overview.md:83`, `deployment/install.md:84`, `deployment/security.md:103`, `integration/environments.md:27` | The live `/etc/systemd/system/riot-proxy.service` is not in the repository; the design's unit (`docs/design/07-deployment.md` §Option C) is assumed | Commit the unit to `deploy/` (secrets are in the env file, not the unit) and reproduce it in `install.md` |
| `deployment/configuration.md:198`, `operations/performance.md:69`, `integration/environments.md:27` | `/etc/riot-proxy/env` was not read; the "last known" values are session notes from 2026-10-10 to 2026-10-18 (`JOB_CONCURRENCY`, `BACKUP_KEEP`, `ARCHIVE_BODY_LOCAL_DAYS` among them) | Record the non-secret variables in `deployment/configuration.md` with the date |
| `deployment/backup-and-restore.md:53`, `deployment/overview.md:83` | `riot-proxy-offsite.{service,timer}` and its `verify.conf` drop-in (the 6-hourly backup copy to R2) exist only on the box | Commit them beside `riot-proxy-replica.*` in `deploy/`; add them to `deploy/test.sh` |
| `integration/service-communication.md:104`, `deployment/install.md:161` | The site's production `PROXY_URL` and the name of its consumer key | Record the key name (never the key) in the site's README or here |
| `operations/runbook.md:165`, `integration/monitoring-hooks.md:77`, `integration/monitoring-hooks.md:83`, `deployment/overview.md:135` | The live `/etc/ninja-monitor/config.toml` on both boxes; design/12 and ninja-monitor's example disagree on thresholds; whether the unit has `SupplementaryGroups=riot-proxy` or an ACL on `/var/lib/riot-proxy`; whether `ninja-ingest` is deployed | Commit a redacted copy of the config to ninja-monitor's repo or `deploy/` |
| `deployment/security.md:44`, `deployment/security.md:118` | Whether the Onidel key is bucket-scoped; the R2 token's IP filter (a session note); whether R2 versioning or a deletion grace is on | Check the two consoles; note the answer in `deploy/README.md` §1 |
| `deployment/install.md:23` | No host package manifest; `unattended-upgrades` and `chrony` are provisioning intent | `dpkg -l` on the box, then a short list in `install.md` |
| `workflows/release-process.md:70` | The exact deploy commands the owner runs for a release | Paste the owner's procedure into `release-process.md` |
| `workflows/ci.md:29` | Whether `ui` is now a required check on `main` | GitHub branch protection settings |

## 2. Measurements the repository does not hold

| Where | Gap | How to resolve |
|---|---|---|
| `deployment/capacity.md:26`, `:52` | The proxy's RSS and CPU or PSI readings on the 6-vCPU host; only 2-vCPU figures exist | Read ninja-monitor's local API once under load; add a dated row |
| `deployment/capacity.md:101`, `operations/runbook.md:128` | Whether Onidel counts ingress against the 6 TB; the site's traffic; how outbound traffic is measured on the box (nothing in the repo does) | Onidel console or `vnstat`; record the method in the runbook |
| `operations/performance.md:34` | Production `/champions` latency after PERF-03 (PROGRESS says done without a number) | One timed curl on the box |
| `data/migrations.md:65` | Production timings for V0026 and V0027 | From the deploy journal |

## 3. Behaviour not pinned down in code or tests

| Where | Gap | How to resolve |
|---|---|---|
| `integration/object-storage.md:186` | OBJ-06 (orphan-pack sweep, compaction, `ARCHIVE_COMPACT_BELOW`) is designed but not implemented; an orphan pack from a crash mid-offload stays in the bucket | Build OBJ-06, or document the manual sweep |
| `integration/object-storage.md:156` | `object_store`'s effective timeouts and retry count (crate defaults; nothing set in `src/archive/objects.rs`) | Read the pinned crate's defaults or set them explicitly |
| `deployment/upgrade-and-rollback.md:77` | What an older binary does against a database with newer migrations in `refinery_schema_history` | Test it once against a backup; document, or guard in `db::migrate` |
| `data/migrations.md:14`, `workflows/adding-a-migration.md:18` | What refinery 0.9 does when an applied migration file's checksum changes | Same test; or add a CI guard that migrations never change |
| `operations/runbook.md:54` | Whether `match_facts` under an old key scope are ever re-derived after a Riot-key rotation (`facts:reextract` selects on `facts_version` only) | Decide and document a rotation procedure; probably a task |
| `operations/runbook.md:113` | What the hourly `rclone sync` does when the Onidel source is unreachable (can it delete from R2?) | Read rclone's sync semantics for a listing failure; add `--max-delete` or a check to the unit |
| `api/endpoints.md:110`, `api/errors.md:96` | The exact response when a body exceeds the 1 MiB limit (axum's rejection, outside the envelope) | One test in `tests/http.rs` |
| `api/endpoints-riot-lol.md:119` | Whether `ServeDir` sends `Last-Modified` and honours `If-Modified-Since` on filled images | One test in `tests/ddragon.rs` |
| `deployment/security.md:31` | Whether the `http` span's `path` includes the query string, so a `?token=` key could land in journald | Read `src/app.rs:109-116`'s extractor; if it does, strip the query or drop `?token=` support outside `/v1/ws` |
| `deployment/security.md:97` | No Dependabot, Renovate, `cargo audit` or `cargo deny` in the repo | Decide a policy; add a workflow if wanted |
| `workflows/adding-an-endpoint.md:82`, `workflows/testing.md:74` | How insta snapshots are meant to be accepted (`cargo insta` is mentioned nowhere) | Document the chosen flow in `testing.md` and the PR template |

## 4. Documentation of shapes and examples

| Where | Gap | How to resolve |
|---|---|---|
| `api/endpoints-riot-lol.md:38` | `GET /v1/riot/accounts/region/by-puuid/{puuid}` has no typed 200 body in the OpenAPI document | Add `AccountRegionDTO` to `RIOT_BODIES` in `src/routes/docs.rs` |
| `api/endpoints-admin.md:89`, `:101` | Field names of the job activity views, `stats::Snapshot` and `jobs::timelines::Progress` are given by schema reference only | Read `src/jobs/activity.rs`, `src/stats.rs`, `src/jobs/timelines.rs`; or point readers at `components.schemas` in `openapi.json` |
| `workflows/adding-an-endpoint.md:9`, `adding-a-riot-call.md:107`, `adding-a-job.md:154`, `adding-a-migration.md:109` | The worked examples use a hypothetical clash-v1 endpoint whose path, TTL and fields were not checked against the portal | Keep them labelled hypothetical, or replace with a real past change (for example ADR-095's `league.expEntries`) |
| `workflows/development-setup.md:202` | The first-run output of a release binary built without `dev-cli` was not captured | Run a release asset once |
| `reference/dependencies.md:65` | The Node package lists for `tests/dom` and `acceptance` | Read the two lockfiles if wanted |
| `reference/riot-api-usage.md:96` | The repo calls the production host's key "the production key" while ADRs describe a personal key's limits; whether a Riot production key exists | Owner statement (session notes say: no production key yet) |
| `integration/contract.md:96` | Whether the site's unseen remote commits regenerated against 2.7.0 | `git fetch` the site repo |
| `integration/environments.md:46` | Whether Proxmox VM 102 (the dormant LAN proxy) still exists stopped or was deleted | Owner's Proxmox host |

## Code and existing documentation that disagree

Found while writing; not gaps in these docs but worth a fix in the repository.

| What | Where | Recommendation |
|---|---|---|
| `JOB_KIND_LIMITS` default: `.env.example:34` and `README.md` show `aggregate:analytics=1`; the code default is `aggregate:analytics=1,archive:offload=1` (`src/config.rs:114`) | `deployment/configuration.md` | Update `.env.example` and `README.md` |
| The generated OpenAPI document emits `operationId: champions` twice (`src/routes/analytics.rs:1031`, `src/routes/players.rs:1005`), which fails Redocly's lint; the docs copy is patched | `api/openapi.json`, `_meta/doc-maintenance.md` | Rename one handler, for example `player_champions`, and re-snapshot |
| `docs/design/07-deployment.md:218` says request logs carry `x_cache` and `upstream_ms`; the observed line carries `latency`, `status` and the span's `request_id`, `method`, `path`, `consumer` | `operations/observability.md` | Fix design/07 or add the fields to the response log |
| `docs/design/12` describes the offload job in the bulk lane, a counter seeded at boot, store errors ending the job `failed`, and `ARCHIVE_COMPACT_BELOW`; the code has no lane, seeds nothing, retries, and has no compaction | `integration/object-storage.md` | Add an "as built" note to design/12 for OBJ-03; build or drop OBJ-06 |
| `docs/design/04-data-and-cache.md:387` describes `limiter_state.windows` as `count/reset_at`; the code stores sliding-log stamps (`src/riot/limiter/persist.rs:37-51`) | `data/schema.md` | Fix the comment in design/04 |
| `docs/design/06` scheduler backoff "2^attempts × 30 s" is ambiguous; `attempts` includes the current run, so the first retry waits about 60 s | `architecture/concurrency-and-rate-limiting.md` | Say so in design/06 |
| `docs/IMPLEMENTATION.md:381` (SITE-07) says the site serves images from the proxy's mirror; the site's `src/lib/assets.ts` points the browser at Riot's CDN and CommunityDragon | `integration/service-communication.md` | Update the task note; the `/ddragon/*` routes are unused by the site |
| `docs/DECISIONS.md:1734` (ADR-166) says the site "revalidates every call (ADR-099)"; the site sends no conditional requests | `integration/service-communication.md` | Reword ADR-166's context |
| `docs/design/README.md:5` still says "Proposal / design draft" and calls design/13 a proposal; `docs/adr/` holds only `.gitkeep`; `docs/img/p7-06-*.png` are orphaned v1 screenshots; `docs/design/02-stack-options.md:92` links to a heading that no longer exists in design/08 | repository docs | Housekeeping |
| `ops/prometheus-alerts.yml` keeps v1 wording ("Lua accounting", `rl:*`) though its metric names are current | `operations/observability.md` | Reword the comments |
| `README.md` §Dev VM duplicates `deploy/proxmox/README.md` | | Link instead |

---
**Sources:** every `TODO (unverified)` marker under `docs/` on 2026-10-19; `docs/_meta/scratch/*.md` "design vs code" sections
**Last verified:** 2026-10-21 against commit `32c4672`

# Documentation changelog

Dated log of changes to this documentation set. One line per change; the newest first.

## 2026-10-21

- Release 2.8.0: `api/openapi.json` `info.version` is `2.8.0`; the version examples in `deploy/proxmox` and `workflows/release-process.md` follow.
- THR-03 (ADR-182): incremental analytics. `aggregate:delta`, V0029 (`analytics_counted`, `analytics_retract`, `analytics_stamped`, the `match_facts_tier_moved` trigger, `analytics_runs.kind`), `AGGREGATE_DELTA_S`, `AGGREGATE_DELTA_TARGET_MS` and `AGGREGATE_DELTA_BATCH`, the slice stamp in every analytics `ETag` but `/patches`, `kind` on `lastRuns[]` and `proxy_aggregate_runs_total`, the four `aggregate_delta_*` metrics and the boot rebuilds of unstamped ladders are in `data/{schema,schema-analytics,migrations,models,caching}.md`, `deployment/{configuration,upgrade-and-rollback}.md`, `operations/{runbook,troubleshooting,observability,performance}.md` (runbook §19 and troubleshooting §29–§31 are new), `api/{endpoints-admin,endpoints-players-analytics,pagination-filtering-and-versioning,examples}.md`, `api/openapi.json` (`RunSummary.kind`, by hand), `architecture/{overview,data-flow,concurrency-and-rate-limiting}.md`, `integration/environments.md`, `workflows/{adding-a-job,adding-a-migration,development-setup,testing}.md`, `reference/glossary.md` and `_meta/inventory.md` (29 migrations, next free V0030, 22 job kinds, `docs/thr/`). The captured example `ETag` `W/"HwbkHH…"` is recomputed as `W/"pG-2Xj…"` for the added part. Line citations into the changed `src/` files follow the moved code across the set.
- Release 2.7.2: `api/openapi.json` `info.version` is `2.7.2`; the version example in `deploy/proxmox` follows.

## 2026-10-20

- WR-02 (ADR-180): `/readyz` reports writer progress instead of queueing behind a running write; its new `writerBusy`, `writerBusyMs`, `writerQueued` fields, the `sqlite_write_seconds`, `sqlite_write_wait_seconds` and `sqlite_write_queue` metrics, the `slow SQLite write` warning and `sqliteWriter` in `/v1/admin/metrics` are in `api/endpoints.md`, `api/examples.md`, `api/openapi.json`, `operations/{observability,runbook,troubleshooting}.md`, `integration/monitoring-hooks.md`, `deployment/{capacity,install,upgrade-and-rollback}.md`, `architecture/request-lifecycle.md`, `reference/{faq,dependencies}.md` and `docs/design/{04-data-and-cache,07-deployment,metrics}.md`. Troubleshooting §1 is rewritten (a rebuild no longer makes `sqlite:false`; a busy `false` is a stalled writer), which closes its gap; line citations into `src/db/mod.rs`, `src/stats.rs`, `src/metrics.rs` and `src/routes/health.rs` follow the moved code.

- WR-01 (ADR-179): V0028 `jobs_crawl_matches` added to `data/schema.md` and `data/migrations.md` (with its build time); the migration count and next free number updated in `workflows/adding-a-migration.md` and `_meta/inventory.md`.
- Release 2.7.1: `api/openapi.json` `info.version` is `2.7.1`; the version example in `deploy/proxmox` follows.

## 2026-10-19

- The set became a CI gate (ADR-178): `docs/_meta/tools/{linkcheck.py,docs-guard.py,mermaidcheck.sh,gaps.py,openapi-check.py}` and a `docs` job in `.github/workflows/ci.yml`; the `test` job compares the generated OpenAPI document with `docs/api/openapi.json`. `docs/workflows/ci.md` and `doc-maintenance.md` describe it; `PROGRESS.md` gained the DOC series.
- A `ninja-docs` repository was scaffolded beside this one (deploy kit with a pull-based updater, `sources.toml`, `bump.yml`, the build specification) so the published docs follow this repository's releases.
- Initial generation of the set against commit `52fa67d`, re-verified at `52fa67d` (version 2.7.0) on branch `docs/DOC-01-documentation-set`: discovery inventories under `_meta/scratch/`, then `README.md`, `architecture/` (5 pages and 10 decision records), `integration/` (6), `api/` (openapi.json and 7 pages), `data/` (5), `workflows/` (9), `deployment/` (7), `operations/` (4), `reference/` (4), `_meta/` (4).
- `api/openapi.json` generated with `riot-proxy spec`; the duplicate `operationId: champions` renamed to `player_champions` in the docs copy (the generator still emits it; see `gaps.md`).
- A `## Documentation` section added to the top-level `README.md`.
- Verification performed: `cargo build --all-features`; `cargo test --all-features --no-fail-fast` at `7491ff6` (44 binaries, 938 passed, 0 failed, 5 ignored) and again at `52fa67d` after merging `origin/main` (CI-04, CI-05, #226: 5 binaries, 939 passed, 0 failed, 5 ignored); a scratch `serve` with a fake key probed for health, docs, dashboard, errors; Redocly lint of `openapi.json` (0 errors, 6 warnings); relative link check; Mermaid render check of every block; five source-footer spot checks. Results recorded in the final report and summarised in `gaps.md`.

---
**Sources:** this documentation set
**Last verified:** 2026-10-21 against commit `f8384be`

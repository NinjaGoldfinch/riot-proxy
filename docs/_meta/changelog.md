# Documentation changelog

Dated log of changes to this documentation set. One line per change; the newest first.

## 2026-10-20

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
**Last verified:** 2026-10-20 against commit `a11df6c`

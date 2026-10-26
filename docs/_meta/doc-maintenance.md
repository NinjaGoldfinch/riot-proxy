# Keeping these docs current

Which documents to touch when which code changes, how to regenerate the generated artefacts, how to check links and diagrams, and the checklist to run before a release.

## Conventions

- One H1 per file, a one- or two-sentence summary under it, CommonMark only, Mermaid in fenced blocks, relative links.
- Every file ends with a `**Sources:**` line citing files and line ranges and a `**Last verified:** YYYY-MM-DD against commit \`<sha>\`` line. Update both when you re-verify a file, even if the prose did not change.
- Anything not evidenced in the repository is written as `> **TODO (unverified):** …` and listed in [gaps.md](./gaps.md). Grep for the marker before a release:

  ```bash
  grep -rn "TODO (unverified)" docs/ --include=*.md | grep -v _meta/gaps.md
  ```

- British/NZ spelling, present tense, second person for procedures. No em-dashes.
- The repository's own records stay the authority: `docs/design/*` for design, `docs/DECISIONS.md` for decisions, `PROGRESS.md` for task status. These docs describe what is built and link to them; do not duplicate a design section, summarise and link.

## What to update when

| When you change | Update |
|---|---|
| A route, its parameters, response shape or auth | `docs/api/openapi.json` (regenerate), `api/endpoints*.md`, `api/examples.md` if the walkthrough touches it, `integration/contract.md` if the site depends on it, `src/ui/showcase.html` or its `notShowcased` list (the repo's own rule) |
| An error code or message | `api/errors.md`, `integration/contract.md` |
| A configuration variable | `deployment/configuration.md` (and `.env.example`, which a test checks against `VARS`), `operations/runbook.md` if it is a lever |
| A migration | `data/schema.md`, `data/migrations.md`, `data/models.md` if a model changed, `workflows/adding-a-migration.md` (the next free number) |
| A job kind, tick or priority | `architecture/concurrency-and-rate-limiting.md`, `operations/runbook.md`, `workflows/adding-a-job.md` |
| Limiter, cache or fetcher behaviour | `architecture/request-lifecycle.md`, `architecture/concurrency-and-rate-limiting.md`, `data/caching.md`, `reference/riot-api-usage.md` |
| A Riot endpoint | `reference/riot-api-usage.md`, `data/caching.md` (TTLs), `workflows/adding-a-riot-call.md` |
| A metric, log line or health body | `operations/observability.md`, `integration/monitoring-hooks.md`, `docs/design/metrics.md` (a unit test checks it) |
| Object storage layout, lifecycle or credentials | `integration/object-storage.md`, `deployment/backup-and-restore.md`, `deployment/security.md`, `deploy/README.md` |
| A workflow file | `workflows/ci.md`, `workflows/release-process.md` |
| The production host, ports, units or tunnel | `deployment/overview.md`, `deployment/install.md`, `integration/environments.md`, `deployment/capacity.md` |
| A dependency bump with a reason | `reference/dependencies.md` |
| A decision | `docs/DECISIONS.md` first (the ADR number), then `architecture/decisions/` if it reshapes the system |
| Anything | `_meta/changelog.md` (one dated line), and the file's `Last verified` footer |

## Regenerating the OpenAPI document

```bash
cargo run -q -- spec > docs/api/openapi.json
npx -y @redocly/cli lint docs/api/openapi.json
```

The generator emits `operationId: champions` twice (`GET /v1/lol/analytics/champions` and `GET /v1/players/{puuid}/champions`, from the handler function names in `src/routes/analytics.rs:1031` and `src/routes/players.rs:1005`). Until the code renames one of them, patch the docs copy after regenerating:

```bash
python3 - <<'EOF2'
import json; p='docs/api/openapi.json'; d=json.load(open(p))
d['paths']['/v1/players/{puuid}/champions']['get']['operationId']='player_champions'
json.dump(d,open(p,'w'),indent=2,ensure_ascii=False); open(p,'a').write('\n')
EOF2
```

Then cross-check the route table against the document (the repo's own `tests/openapi.rs` asserts `/openapi.json` equals `spec()`; `scripts/compare-openapi.py OLD NEW --prefix /v1/` lists operations missing from NEW).

## Checking links

```bash
cd docs && find . -name '*.md' -print0 | xargs -0 -n1 npx -y markdown-link-check -q -c /dev/null
```

Or, with no network, the checked-in script, which resolves every relative `](…)` target and `#anchor` against the file's directory using GitHub's heading slugs:

```bash
python3 docs/_meta/tools/linkcheck.py docs
```

## Checking Mermaid

Every fenced `mermaid` block must render with mermaid-cli. The checked-in script extracts every block under a directory and renders each one (`docs/_meta/tools/mermaidcheck.sh docs`); `docs/_meta/tools/gaps.py` lists every `TODO (unverified)` marker for `gaps.md`. On this dev box Chromium is not on the default path:

```bash
cat > /tmp/pp.json <<'EOF2'
{"executablePath":"/home/dev/.cache/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell","args":["--no-sandbox","--disable-gpu"]}
EOF2
# extract each block to a .mmd and render it
LD_LIBRARY_PATH=$(cat ~/.local/chromelibs/libpath) FONTCONFIG_FILE=~/.local/chromelibs/fonts.conf \
PUPPETEER_SKIP_DOWNLOAD=1 npx -y @mermaid-js/mermaid-cli@11 -p /tmp/pp.json -i block.mmd -o block.svg
```

On a machine with Chromium installed, `npx -y @mermaid-js/mermaid-cli -i block.mmd -o block.svg` is enough. Keep to `flowchart`, `sequenceDiagram`, `erDiagram` and `stateDiagram-v2`; quote labels with punctuation; stay under about 25 nodes.

## When code changes

The set is kept current in the same pull request as the code it describes. Before opening a PR that touches `src/`, `deploy/`, `.github/`, `Cargo.toml` or `.env.example`:

1. `git log --oneline <newest Last verified sha>..HEAD -- src deploy .github Cargo.toml .env.example` lists what moved since the docs were last verified.
2. Update the pages in the table above for each change; refresh their `Last verified` line to the new commit.
3. Run the three scripts in `docs/_meta/tools/` and `cargo run -q -- spec` against `docs/api/openapi.json`.
4. Add a line to `changelog.md` and reconcile `gaps.md`.

## The CI gate

The `docs` job in `.github/workflows/ci.yml` (ADR-178) runs the link check, `docs-guard.py` (a page whose `Sources` cite a changed file must change in the same pull request; code changes need a docs change; footers must be refreshed and name real commits), the Mermaid render and the `gaps.md` count; the `test` job compares the generated OpenAPI document with `docs/api/openapi.json` using `openapi-check.py`. Run the same locally before pushing:

```bash
python3 docs/_meta/tools/linkcheck.py docs
python3 docs/_meta/tools/docs-guard.py origin/main HEAD
docs/_meta/tools/mermaidcheck.sh docs
cargo run -q -- spec > /tmp/openapi.json && python3 docs/_meta/tools/openapi-check.py /tmp/openapi.json docs/api/openapi.json
```

## Pre-release documentation checklist

1. `cargo run -q -- spec` diff against `docs/api/openapi.json` is empty apart from `info.version` and the patched `operationId`.
2. `grep -rn "TODO (unverified)"` output matches `gaps.md`.
3. Every file touched this release has today's `Last verified` line and the new commit.
4. `deployment/configuration.md` lists every name in `src/config.rs` `VARS` (the config tests enforce `.env.example` parity; mirror it here).
5. `data/migrations.md` ends at the newest `V<nnnn>` file.
6. Link check and Mermaid render pass.
7. Add a line to `changelog.md`.

## The planned docs web server

These files are written for a static site generator served from the dev box. The intended layout is `docs/README.md` as the home page and the directory structure as the navigation; every page is CommonMark with ATX headings, fenced code and Mermaid blocks, so MkDocs with the Material theme (`pymdownx.superfences` with a `mermaid` custom fence) or Docusaurus (`@docusaurus/theme-mermaid`) both render it. A minimal `mkdocs.yml` whose `nav` mirrors the table of contents in `docs/README.md` is the suggested next step; it is not created here.

## Evidence inventories

The files under [scratch/](./scratch/) are the Phase 0 inventories the set was written from: `routes.md`, `config.md`, `riot-and-cache.md`, `jobs-and-observability.md`, `persistence.md`, `siblings.md`, `tests-tooling-history.md`, plus `WRITING-BRIEF.md` (the writers' shared contract) and `openapi-generated.json` (the unpatched generator output). They are dense, cite line numbers that drift with every change, and are not meant for readers; refresh or delete them when the pages that cite them are re-verified.

---
**Sources:** `docs/_meta/scratch/WRITING-BRIEF.md`, `src/routes/analytics.rs:L1031`, `src/routes/players.rs:L1005`, `tests/openapi.rs`, `scripts/compare-openapi.py`, `src/config/tests.rs:L434-L453`
**Last verified:** 2026-10-26 against commit `c9e1b33`

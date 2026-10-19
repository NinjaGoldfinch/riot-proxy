# Writing brief for the riot-proxy documentation set (DOC-01)

Read this whole file before writing any document. It is the shared contract for every writer.

## Facts about the platform (verified 2026-10-19 at commit 7491ff6, version 2.7.0)

- **riot-proxy**: Rust 1.98.1 single binary (axum 0.8, tokio, rusqlite/SQLite WAL, moka, reqwest, object_store). Repo github.com/NinjaGoldfinch/riot-proxy (public). Owns the Riot key, rate limiting, caching, match/timeline archive, ladder crawl, analytics. Issues consumer keys (`rpx_…`, 36 chars; bearer or `?token=`; scopes `read`/`admin`; quota per minute, default 600). Known key names: `admin-main` (owner's dashboard key), the site's production key name is not recorded (TODO).
- **ninjagoldfinch.lol**: private repo `NinjaGoldfinch/ninjagoldfinch-lol`, SvelteKit 3 / Svelte 5 / adapter-node, pnpm 12.10.1, Node >= 22.18. Runs `node build` under systemd unit `ninjagoldfinch-lol.service` on the same host, `127.0.0.1:3000`, env `/etc/ninjagoldfinch-lol/env` (`PROXY_URL`, `PROXY_KEY`, …). Talks only to riot-proxy over loopback (13 GET operations, all server-side, `Authorization: Bearer`). Browser never calls riot-proxy; images come straight from ddragon.leagueoflegends.com and raw.communitydragon.org. No CI (hand deploy: git pull, pnpm install, pnpm build, systemctl restart). The public repo `NinjaGoldfinch/ninjagoldfinch.lol` is a GitHub Pages placeholder (CNAME + riot.txt).
- **ninja-monitor**: Rust agent (v0.2.1) per host, systemd unit `ninja-monitor.service`, samples host CPU/mem/disk and per-systemd-service cgroup metrics every 15 s, alerts to a Discord webhook only, local API on 127.0.0.1:9090. It does NOT scrape riot-proxy's /healthz, /readyz or /metrics; it watches `riot-proxy.service` and `/var/lib/riot-proxy`. Runs on both boxes. Optional push to `ninja-ingest` (deployment unverified).
- **Production host**: Onidel Sydney VPS `ngf-prod-syd1` (plan ONI-4: 6 vCPU, 12 GB RAM, 120 GB NVMe (115 GB usable), 6 TB/month), Ubuntu 26.04 LTS, login user `ninja`, key-only SSH, ufw 22 only, no Docker. Processes: `riot-proxy.service` (`/usr/local/bin/riot-proxy serve`, user `riot-proxy`, env `/etc/riot-proxy/env`, data `/var/lib/riot-proxy/data/riot-proxy.db`, `HOST=127.0.0.1`, `PORT=8080`), `ninjagoldfinch-lol.service` (127.0.0.1:3000), `cloudflared` (token-installed Cloudflare Tunnel: `ninjagoldfinch.lol` → :3000, `api.ninjagoldfinch.lol` → :8080, `admin.ninjagoldfinch.lol` → :8080 behind Cloudflare Access One-time PIN; a WAF rule blocks `/metrics`, `/dashboard*`, `/dev*`, `/v1/admin*` on `api.`), `ninja-monitor.service`, `riot-proxy-replica.timer` (hourly rclone sync to R2), `riot-proxy-offsite.timer` (every 6 h rclone copy of backups/ to R2, hand-made, not in repo). Never say "sydney01". Old box `ngf-mon-syd1` (OVH, 2 core/3.7 GB) runs ninja-monitor and is the planned Prometheus/Grafana host (not done).
- **Object storage**: Onidel S3 endpoint `https://s3.ap-southeast-2.onidel.cloud`, region `ap-southeast-2`, bucket `riot-proxy-archive` (system of record for packed match/timeline bodies: `packs/matches/<yyyy>/<mm>/<ULID>.pack`, `packs/timelines/…`, daily `index/packs-<date>.jsonl.zst` + `index/locations-<date>.jsonl.zst`; lifecycle: `index/` expires 14 d, packs kept forever). Cloudflare R2 bucket `riot-proxy-archive` = replica (hourly `rclone sync`, token scoped to that bucket, `index/` expires 21 d, multipart abort 1 d); R2 bucket `ngf-backups` prefix `riot-proxy/` = SQLite backups (14-day expiry). The proxy only ever talks to the Onidel bucket via `ARCHIVE_OBJECT_URL=s3://riot-proxy-archive` + `ARCHIVE_OBJECT_ENDPOINT` + `ARCHIVE_OBJECT_ACCESS_KEY`/`SECRET_KEY`; R2 is reached only by rclone from the box. Restore = point `ARCHIVE_OBJECT_URL`'s endpoint at R2. Two stores because Onidel is same-DC/cheap primary and R2 is the off-vendor copy.
- **Dormant LAN dev proxy**: `http://192.168.68.46:8080` was the Proxmox dev VM (VM 102) running the `:edge` image with `AUTH_DISABLED`; it is not running now. Document as optional/dormant. Local development must work against a locally started `riot-proxy serve`.
- **CI**: three workflows (ci.yml, edge.yml, release.yml); nothing deploys to production; the owner installs the release binary by hand. Already written: `docs/workflows/ci.md`, `docs/workflows/release-process.md`, `docs/workflows/code-style-and-conventions.md`, `docs/reference/dependencies.md`.
- **Riot key**: a personal key (limits 100 per 2 min, 20 per 1 s, i.e. ~0.83 req/s sustained); no production key yet. Never print or guess key values.

## Evidence inventories (read the ones relevant to your files; cite from them, they already carry file:line)

- `docs/_meta/scratch/routes.md` — every route, auth, errors, headers, WS protocol
- `docs/_meta/scratch/config.md` — every variable, precedence, CLI, build.rs
- `docs/_meta/scratch/riot-and-cache.md` — Riot client, endpoints, routing, limiter, fetcher, caches
- `docs/_meta/scratch/jobs-and-observability.md` — boot sequence, scheduler, jobs, ladder crawl, events, metrics, logging, health
- `docs/_meta/scratch/persistence.md` — migrations, every table, SQLite setup, object storage, packs (may still be being written; wait for it if your files need it)
- `docs/_meta/scratch/siblings.md` — the site and ninja-monitor boundary, cross-service call table
- `docs/_meta/scratch/tests-tooling-history.md` — tests, scripts, deploy kit, docs map, full ADR index, git history
- Design authority: `docs/design/*.md`, `docs/DECISIONS.md`, `deploy/README.md`, `deploy/proxmox/README.md`, `README.md`, `.env.example`.

Evidence only. If something is not in the repo, write exactly: `> **TODO (unverified):** <what is unknown and where you looked>` on its own line (blockquote). Never guess. Never invent Riot semantics. Never read `.env`.

## Writing standards (strict)

- CommonMark for MkDocs/Docusaurus: ATX headings, ONE H1 per file, fenced code with language tags (`bash`, `json`, `ini`, `yaml`, `sql`, `rust`, `text`, `mermaid`), relative links between docs (`../api/errors.md`), no HTML.
- Start with the H1 and a one- or two-sentence summary of what the reader gets.
- End EVERY file with exactly this footer (three dashes line, then two bold lines):

  ```
  ---
  **Sources:** `path/file.rs:L10-L42`, `path/other.md`
  **Last verified:** 2026-10-19 against commit `7491ff6`
  ```

- Cite `file:line` ranges inline for non-obvious behaviour. Numbers in diagrams must match the code and cite the line in prose.
- Tables for reference material, prose for explanation, numbered lists for procedures (each step one command or one checkable action). Example payloads use realistic shapes with fake ids (puuid like `AbC…` 78 chars of placeholder, match id `OC1_700000001`, keys `rpx_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAM`); never real PUUIDs, keys or personal data.
- British/NZ spelling (optimise, behaviour, licence). Present tense. Second person for procedures, third person for descriptions. No marketing, no filler. No em-dashes; use commas, colons or full stops.
- Keep files focused; split anything over ~400 lines.
- Mermaid: `flowchart`, `sequenceDiagram`, `erDiagram`, `stateDiagram-v2`. One-sentence caption ABOVE each diagram, short prose explanation BELOW. Consistent node/participant names across all diagrams: `Browser`, `ninjagoldfinch.lol`, `riot-proxy`, `ninja-monitor`, `Riot API`, `Cache`, `DB`, `Onidel Object Storage`, `Cloudflare R2`, `GitHub Actions`, `Cloudflare Tunnel`, `Data Dragon CDN`. Label edges with protocol/action and auth where relevant (`HTTP · Bearer rpx_…`, `HTTPS · X-Riot-Token`). Under 25 nodes per diagram. Avoid HTML labels; `<br/>` inside quoted labels is acceptable. Avoid very new syntax. Quote labels that contain punctuation.
- Config names, routes, flags and files in backticks. Error codes in backticks (`RATE_LIMITED`).
- Do not commit. Do not modify anything outside `docs/`. Do not touch the files already written.

## File map (so relative links resolve)

```
docs/README.md
docs/architecture/{overview,components,request-lifecycle,data-flow,concurrency-and-rate-limiting}.md
docs/architecture/decisions/ADR-*.md (index at docs/architecture/decisions/README.md)
docs/integration/{service-communication,contract,authentication,object-storage,monitoring-hooks,environments}.md
docs/api/{openapi.json,endpoints,errors,pagination-filtering-and-versioning,examples}.md
docs/data/{schema,models,caching,migrations}.md
docs/workflows/{development-setup,adding-an-endpoint,adding-a-riot-call,adding-a-job,adding-a-migration,testing,release-process,ci,code-style-and-conventions}.md
docs/deployment/{overview,install,configuration,upgrade-and-rollback,backup-and-restore,capacity,security}.md
docs/operations/{runbook,troubleshooting,observability,performance}.md
docs/reference/{glossary,riot-api-usage,dependencies,faq}.md
docs/_meta/{inventory,doc-maintenance,gaps,changelog}.md
```

Existing repo docs you may link to: `../design/07-deployment.md`, `../DECISIONS.md`, `../IMPLEMENTATION.md`, `../../deploy/README.md`, `../../README.md` (adjust `..` depth to your file's location).

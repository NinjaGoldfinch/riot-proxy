# Environments

Production on the shared Onidel VPS, local development, and the dormant LAN dev proxy: base URLs, which keys and buckets each uses, and what differs between them.

## Summary

| | Production | Local development | Proxmox dev VM (dormant) |
|---|---|---|---|
| Host | `ngf-prod-syd1`, Onidel Sydney, Ubuntu 26.04, 6 vCPU / 12 GB / 120 GB | your machine | Debian 13 VM 102 on the owner's Proxmox host |
| Binary | release asset `riot-proxy-linux-amd64` under `riot-proxy.service` | `cargo run -- serve` (`just dev`) | `ghcr.io/ninjagoldfinch/riot-proxy:edge` in Docker, updated every 2 min |
| Base URL | `http://127.0.0.1:8080` on the box; `https://api.ninjagoldfinch.lol` publicly; `https://admin.ninjagoldfinch.lol` for operators | `http://127.0.0.1:8080` | `http://192.168.68.46:8080` (LAN only) |
| `ENV` | `production` | `development` (default) | `development` |
| Auth | consumer keys; Cloudflare Access on `admin.`; WAF blocks admin paths on `api.` | keys from the bootstrap banner, or `AUTH_DISABLED=true` | `AUTH_DISABLED=true` |
| Riot key | the owner's personal key in `/etc/riot-proxy/env` | your own personal key in `.env` | the owner's key in `/opt/riot-proxy/.env` |
| Data | `/var/lib/riot-proxy/data/riot-proxy.db` | `./data/riot-proxy.db` | `/opt/riot-proxy/data` (the container's `/data`) |
| Object storage | Onidel `riot-proxy-archive`; R2 replica; R2 `ngf-backups` for SQLite backups | none (bodies stay in SQLite), or `ARCHIVE_OBJECT_URL=file:///…` | none |
| Pages | `/docs` and `/dashboard` on `admin.`; `/dev` never | `/dev`, `/dev/showcase`, `/docs`, `/dashboard` | `/dev`, `/docs`, `/dashboard` |
| Background work | crawls of `oc1,kr,euw1,na1` daily (last known), polls, offload | whatever `.env` enables; `LADDER_CRAWL_S=0` by default | real Riot calls |
| Monitoring | ninja-monitor on the box, Discord alerts | none | none |

## Production

The box runs riot-proxy, ninjagoldfinch.lol and `cloudflared` as systemd services, plus ninja-monitor and two rclone timers. The proxy and the site bind loopback only; the tunnel is the only ingress and there are no inbound ports (`docs/design/12-object-storage-and-hosting.md` §As executed). `TRUST_PROXY=true` lets the proxy see the real client address behind `cloudflared`; `ADMIN_IP_ALLOWLIST` stays empty. Prometheus and Grafana on `ngf-mon-syd1` are planned, not deployed. The topology, ports and directories are in [../deployment/overview.md](../deployment/overview.md); the configuration in force, as far as it is recorded, is in [../deployment/configuration.md](../deployment/configuration.md).

Last known production settings come from session notes dated 2026-10-10 to 2026-10-18, not from the box: `LADDER_PLATFORMS=oc1,kr,euw1,na1`, `LADDER_CRAWL_S=86400`, `LADDER_TIER_FLOOR=MASTER`, `LADDER_BACKFILL_LIMIT=100`, `ARCHIVE_TIMELINES=true`, `BULK_USAGE_CEILING=0.80`, `AGGREGATE_PATCH_LIMIT=0`, `HOST=127.0.0.1`, the Onidel bucket variables.

> **TODO (unverified):** `/etc/riot-proxy/env` and `/etc/systemd/system/riot-proxy.service` on `ngf-prod-syd1` were not read; the unit is assumed to be the template in `docs/design/07-deployment.md` §Option C.

## Local development

Everything works against a locally started proxy; no shared dev instance exists any more.

1. `cp .env.example .env`, set `RIOT_API_KEY` to your own personal key from the Riot developer portal.
2. `just dev`. The first run migrates `./data/riot-proxy.db` and prints the bootstrap admin key on stderr.
3. Open `http://127.0.0.1:8080/dev` (explorer), `/dev/showcase` (example frontend), `/docs`, `/dashboard`.
4. For frontend work without keys, add `AUTH_DISABLED=true` to `.env`; for a quiet box add `LADDER_CRAWL_S=0` and `JOB_CONCURRENCY=1`.
5. To point ninjagoldfinch.lol at it, set `PROXY_URL=http://127.0.0.1:8080` in the site's `.env` (and `PROXY_KEY` only if auth is on).
6. To exercise the offload locally, set `ARCHIVE_OBJECT_URL=file:///absolute/dir` (allowed outside production; `file://` and `http://` endpoints are refused when `ENV=production`, `src/config.rs:990-999`).

The mock Riot in `acceptance/helpers/mock-riot.ts` and the replay fixtures under `tests/fixtures/replay/` let the test suites run with no Riot key at all. Full steps in [../workflows/development-setup.md](../workflows/development-setup.md).

## The Proxmox dev VM

`deploy/proxmox/create-vm.sh` builds a Debian VM that runs the `:edge` image and pulls a new one within two minutes of each push to `main`, rolling back an image that fails its healthcheck (`deploy/proxmox/README.md`). VM 102 at `192.168.68.46` served as the site's `PROXY_URL` for frontend work until mid October 2026 and is not running now; the site's README on its `origin/main` says there is no shared dev instance. Treat it as optional: recreate it with the script when a LAN instance is wanted, and never assume it in documentation or defaults.

> **TODO (unverified):** whether VM 102 still exists on the Proxmox host in a stopped state or was deleted; the repo records only its address and that it is gone from the site's configuration (`docs/IMPLEMENTATION.md:434`, ninjagoldfinch-lol handoff of 2026-10-15).

## What differs and why it matters

| Difference | Effect |
|---|---|
| `ENV=production` | refuses `AUTH_DISABLED`, `RIOT_BASE_URL`, `DDRAGON_BASE_URL`, `file://` object URLs and `http://` endpoints; forces `/dev` off; requires both object-storage keys with an `s3://` URL (`src/config.rs:658-684,987-999`) |
| Key scope | cache keys and facts are scoped by the hash of the Riot key, so a local database filled with one key is invisible to another (`src/cache/keys.rs:24-37`) |
| Object storage | locally, bodies never leave SQLite unless a `file://` store is set; in production bodies older than 14 days exist only in the bucket and its replica |
| Limits | the same personal-key limits apply everywhere; two instances with the same key share Riot's budget without knowing it, which is why the dev VM and production should not run the same key at full load |
| Logs | JSON in production (journald); pretty on a terminal locally |

---
**Sources:** `docs/design/12-object-storage-and-hosting.md` §As executed, `docs/design/07-deployment.md` §Option C, `deploy/proxmox/README.md`, `README.md` §First run, `.env.example`, `src/config.rs:L658-L684,L987-L999`, `src/cache/keys.rs:L24-L37`, `docs/_meta/scratch/siblings.md` §1.2, §1.8, `docs/IMPLEMENTATION.md:L434`
**Last verified:** 2026-10-20 against commit `a11df6c`

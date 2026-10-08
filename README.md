# riot-proxy

A single static binary in front of the [Riot Games API](https://developer.riotgames.com/). It keeps your Riot key off your clients and stays inside Riot's rate limits. It caches what can be cached, archives matches, tracks players, crawls the ranked ladder and serves champion analytics. All state lives in one SQLite file. There is no Redis, no Postgres and no Node.

v2 is a Rust rewrite of v1. Its HTTP API, headers, error codes and metric names are the same as v1's, so existing consumers, dashboards and alerts keep working. To move a running v1 deployment across, see [docs/CUTOVER.md](docs/CUTOVER.md).

## Install

Pick one:

- **Binary:** download it from the [latest release](https://github.com/NinjaGoldfinch/riot-proxy/releases) and check it against `sha256sums`.
  - `riot-proxy-linux-amd64` and `riot-proxy-linux-arm64` are static (musl) and run on any Linux.
  - `riot-proxy-darwin-arm64` is for local development on Apple silicon.
- **Docker:** `ghcr.io/ninjagoldfinch/riot-proxy:2`, a scratch image holding only the binary.
  ```yaml
  # docker-compose.yml
  services:
    riot-proxy:
      image: ghcr.io/ninjagoldfinch/riot-proxy:2
      restart: unless-stopped
      env_file: .env
      ports: ["8080:8080"]
      volumes: ["./data:/data"]
  ```
- **From source:** `cargo build --release`. The toolchain is pinned in `rust-toolchain.toml`. For a static Linux binary, run `just musl` (it needs `musl-tools`).

## Dev VM on Proxmox

This creates a Debian VM that runs the `:edge` image and updates itself within about 2 minutes of each push to `main`. It needs Proxmox VE 8 or later. Run these on the Proxmox host, as root:

```bash
# once: let the 'local' storage hold cloud-init snippets
#   Datacenter → Storage → local → Content → add "Snippets"

curl -fsSL https://github.com/NinjaGoldfinch/riot-proxy/archive/refs/heads/main.tar.gz | tar xz
cd riot-proxy-main/deploy/proxmox
./create-vm.sh --ssh-keys ~/.ssh/my-laptop.pub            # DHCP, next free VM id
# or: ./create-vm.sh --ssh-keys key.pub --vmid 210 --ip 192.168.68.50/22 --gw 192.168.68.1
# or: ./create-vm.sh --generate-key                        # a new login keypair just for this VM
```

`--generate-key` writes the keypair to `/root/.ssh/riot-proxy/<name>-<vmid>` on the host and prints the exact `ssh -i` commands for logging in from the host and from your own machine.

Docker is installed on first boot, which takes a few minutes. Then:

```bash
qm guest cmd <vmid> network-get-interfaces     # its address
ssh riot@<vm-ip>                               # <vm-ip> is the address above, e.g. 192.168.68.46
# with --generate-key, from the Proxmox host:
#   ssh -i /root/.ssh/riot-proxy/riot-proxy-dev-<vmid> riot@<vm-ip>
# or from your laptop, after scp-ing that file to ~/.ssh/ and chmod 600:
#   ssh -i ~/.ssh/riot-proxy-dev-<vmid> riot@<vm-ip>
nano /opt/riot-proxy/.env                      # set RIOT_API_KEY=
sudo riot-proxy-update                         # start now instead of waiting for the timer
docker compose -f /opt/riot-proxy/compose.yaml logs | grep -i "admin key"   # the bootstrap admin key, shown once
```

Open `http://<vm>:8080/dev`, `/docs` or `/dashboard`. [deploy/proxmox/README.md](deploy/proxmox/README.md) covers the options (`./create-vm.sh --help`, `--dry-run`, `--generate-key`), how to pin a version, and how to rebuild the VM.

## First run

```bash
cp .env.example .env          # set RIOT_API_KEY
./riot-proxy serve
```

On first start the database is created and migrated under `DATA_DIR` (default `./data`). A bootstrap admin key is printed once (`bootstrap admin key (shown once): rpx_…`), so save it. Then mint a key for each consumer:

```bash
./riot-proxy key create --name my-website            # scopes: read (default), admin
./riot-proxy key list
./riot-proxy key revoke my-website
```

Consumers send their key as `Authorization: Bearer rpx_…` (WebSockets may use `?token=`):

```bash
curl -H "Authorization: Bearer $KEY" \
  http://localhost:8080/v1/riot/accounts/by-riot-id/Hide%20on%20bush/KR1
```

Account lookups need no region. The proxy picks whichever account-v1 cluster has rate-limit room (`asia`, then `americas`, then `europe`). Add a region (`…/by-riot-id/europe/…`) to pin one.

Once running, these pages are served:

- `/docs`: the OpenAPI reference for every route.
- `/dashboard`: the operational dashboard (it needs an admin key).
- `/dev`: a dev explorer that can call every endpoint and shows the raw responses (design/10). It is never served when `ENV=production`.

## Configuration

Values come from CLI flags, then the environment, then `.env`, then the built-in defaults. [`.env.example`](.env.example) documents every variable. The ones most deployments touch:

| Variable | Default | Meaning |
|---|---|---|
| `RIOT_API_KEY` | — | **Required.** Never commit it. |
| `ENV` | `development` | `production` refuses `AUTH_DISABLED` and turns `/dev` off |
| `HOST` / `PORT` | `0.0.0.0` / `8080` | Listen address |
| `DATA_DIR` | `./data` | SQLite, Data Dragon mirror, backups |
| `LOG_LEVEL` / `LOG_FORMAT` | `info` / `json` (`pretty` on a terminal) | Logging |
| `JOB_CONCURRENCY` | `8` | Background jobs run at once |
| `CACHE_TTL_OVERRIDES` | — | e.g. `league=120,spectator=20` (seconds) |
| `CACHE_L1_MAX_MB` | `128` | In-memory cache budget |
| `BULK_USAGE_CEILING` | `0.80` | Share of each rate-limit bucket that background work may use |
| `ARCHIVE_TIMELINES` | `false` | Archive jobs also fetch match timelines (large). Timelines fetched through the API are archived either way |
| `LADDER_CRAWL_S` / `LADDER_TIER_FLOOR` | `0` / `MASTER` | Scheduled ladder crawls (0 = on demand) and their depth |
| `ADMIN_IP_ALLOWLIST` | — | IPs and CIDRs allowed to reach `/v1/admin/*` |
| `TRUST_PROXY` | `false` | Take the client address from `X-Forwarded-For`. Turn on only behind a reverse proxy that sets it |
| `BOOTSTRAP_ADMIN_KEY` | — | Use this admin key on first run instead of generating one |
| `DEV_UI` / `DOCS_UI` / `DASHBOARD_UI` | on (`/dev` always off in production) | The three pages |

## Operations

### HTTPS

The proxy serves plain HTTP only. It is meant for services on the same host or a private network. If callers reach it over a network you don't trust, put Caddy or nginx in front of `:8080` to terminate HTTPS, and set `TRUST_PROXY=true` so the admin allowlist sees the real client. `TLS=true`, from the removed built-in TLS, refuses to start (ADR-067).

### systemd

Use the unit in [docs/design/07](docs/design/07-deployment.md#option-c--systemd-no-containers). Upgrading is replacing the binary and restarting it. Migrations apply on boot.

### Health and metrics

| Endpoint | Purpose |
|---|---|
| `/healthz` | Process up. `riot-proxy healthcheck` calls it, which suits a scratch image with no curl |
| `/readyz` | Database writable and limiter restored |
| `/metrics` | Prometheus, with v1's metric names, so v1's Grafana dashboard and alerts import unchanged |

Logs are JSON on stdout. Every request carries a `request_id`, echoed as `X-Request-Id`.

### Backups

A daily maintenance job writes a consistent snapshot to `$DATA_DIR/backups/` and keeps 14. You can also run one by hand:

```bash
./riot-proxy backup /path/to/out.db   # safe while serving
```

To restore, stop the service, put the file back as `$DATA_DIR/riot-proxy.db`, and start it.

### Migrating from v1

Import v1's archive and players from its Postgres dump:

```bash
./riot-proxy migrate-v1 --from v1.dump
```

Consumer keys do not migrate; mint new ones. The full sequence is in [docs/CUTOVER.md](docs/CUTOVER.md).

## Development

```bash
just dev          # serve with .env
just test         # cargo test
just lint         # fmt + clippy
just acceptance   # v1's black-box suite against a mock Riot
```

- The design lives in [docs/design/](docs/design/).
- Decisions are in [docs/DECISIONS.md](docs/DECISIONS.md).
- The plan is in [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md).

riot-proxy isn't endorsed by Riot Games and doesn't reflect the views or opinions of Riot Games or anyone officially involved in producing or managing Riot Games properties.

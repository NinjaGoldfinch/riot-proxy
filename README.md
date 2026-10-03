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
  http://localhost:8080/v1/riot/accounts/by-riot-id/asia/Hide%20on%20bush/KR1
```

Once running, these pages are served:

- `/docs`: the OpenAPI reference for every route.
- `/dashboard`: the operational dashboard (it needs an admin key).
- `/dev`: a browser client, off when `ENV=production`.

## Configuration

Values come from CLI flags, then the environment, then `.env`, then the built-in defaults. [`.env.example`](.env.example) documents every variable. The ones most deployments touch:

| Variable | Default | Meaning |
|---|---|---|
| `RIOT_API_KEY` | — | **Required.** Never commit it. |
| `ENV` | `development` | `production` refuses `AUTH_DISABLED` and turns `/dev` off |
| `HOST` / `PORT` | `0.0.0.0` / `8080` | Listen address |
| `DATA_DIR` | `./data` | SQLite, Data Dragon mirror, backups, ACME state |
| `DEFAULT_PLATFORM` | `euw1` | Platform when a request names none |
| `LOG_LEVEL` / `LOG_FORMAT` | `info` / `json` (`pretty` on a terminal) | Logging |
| `JOB_CONCURRENCY` | `8` | Background jobs run at once |
| `CACHE_TTL_OVERRIDES` | — | e.g. `league=120,spectator=20` (seconds) |
| `CACHE_L1_MAX_MB` | `128` | In-memory cache budget |
| `BULK_USAGE_CEILING` | `0.80` | Share of each rate-limit bucket that background work may use |
| `ARCHIVE_TIMELINES` | `false` | Also archive match timelines (large) |
| `LADDER_CRAWL_S` / `LADDER_TIER_FLOOR` | `0` / `MASTER` | Scheduled ladder crawls (0 = on demand) and their depth |
| `ADMIN_IP_ALLOWLIST` | — | IPs and CIDRs allowed to reach `/v1/admin/*` |
| `BOOTSTRAP_ADMIN_KEY` | — | Use this admin key on first run instead of generating one |
| `TLS` / `TLS_DOMAIN` / `ACME_EMAIL` | off | Built-in HTTPS (below) |
| `DEV_UI` / `DOCS_UI` / `DASHBOARD_UI` | on (`/dev` off in production) | The three pages |

## Operations

### HTTPS

You have two options:

- **Built-in:** `riot-proxy serve --tls --domain api.example.com --acme-email you@example.com` obtains and renews a Let's Encrypt certificate into `$DATA_DIR/acme`.
  - It serves HTTPS on `TLS_PORT` (443) and redirects HTTP on `TLS_REDIRECT_PORT` (80). On systemd it needs `CAP_NET_BIND_SERVICE`.
  - Plain HTTP stays on `127.0.0.1:PORT` for the healthcheck.
  - `/metrics` and `/readyz` answer private addresses only.
- **Reverse proxy:** put Caddy or nginx in front of `:8080` instead.

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

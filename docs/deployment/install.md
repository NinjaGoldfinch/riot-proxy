# Installing riot-proxy on a fresh server

A step-by-step install of the proxy on Ubuntu 26.04 as production runs it: a static binary under systemd, no containers, state in one directory, bodies in an S3 bucket. The topology this reproduces is in [overview.md](./overview.md); the variables are in [configuration.md](./configuration.md).

Every command below is run on the server as the login user with `sudo`. Nothing here prints a secret: key values go into root-only files through an editor or a pipe, never onto a command line (`deploy/README.md:51-52`; `docs/design/12-object-storage-and-hosting.md:594-601`).

## 1. OS packages

The proxy itself needs nothing: the binary is static and carries its migrations, UI pages, OpenAPI document and root certificates (`docs/design/07-deployment.md:15`). The host tooling around it does:

| Package | Why | Evidence |
|---|---|---|
| `rclone` | the replica unit runs `/usr/bin/rclone`; the offsite backup timer and the restore drill use it too | `deploy/riot-proxy-replica.service:16`; `deploy/README.md:82-84,151-152` |
| `sqlite3` | the reclaim runbook, `quick_check`, `wal_checkpoint`, and the `max(pack_id)` query after a restore | `deploy/README.md:138,244-249`; `docs/design/07-deployment.md:196,211` |
| `curl` | fetching the release asset and checking `/healthz` and `/readyz` | `docs/workflows/release-process.md` steps 1 and 6 |

```bash
sudo apt-get update && sudo apt-get install -y rclone sqlite3 curl
```

Do not install Docker: production runs no containers (`docs/design/12-object-storage-and-hosting.md:794-807`). Ubuntu's `rclone` 1.60 needs `no_head = true` for R2 (step 9).

> **TODO (unverified):** the repository has no package manifest for the host; the list above is what the runbooks invoke. `unattended-upgrades` and `chrony` are named as provisioning intent (`docs/design/12-object-storage-and-hosting.md:556-558`) but not confirmed installed.

## 2. User, group and directories

```bash
sudo useradd --system --home /var/lib/riot-proxy --shell /usr/sbin/nologin riot-proxy
sudo mkdir -p /var/lib/riot-proxy/data /etc/riot-proxy
sudo chown -R riot-proxy:riot-proxy /var/lib/riot-proxy
```

These are the commands the host move used (`docs/design/12-object-storage-and-hosting.md:585-587`). `useradd --system` creates the group `riot-proxy` with the user. `/var/lib/riot-proxy` is the unit's `WorkingDirectory` and its only writable path; `data/` under it is `DATA_DIR` (`docs/design/07-deployment.md:84,88-89`; `deploy/README.md:122`).

## 3. The binary

Fetch the release asset and verify it against the `sha256sums` the release workflow publishes (`.github/workflows/release.yml:179-194`; `README.md:11-12`):

```bash
v=2.7.0
curl -fsSLO https://github.com/NinjaGoldfinch/riot-proxy/releases/download/v$v/riot-proxy-linux-amd64
curl -fsSLO https://github.com/NinjaGoldfinch/riot-proxy/releases/download/v$v/sha256sums
sha256sum -c --ignore-missing sha256sums
sudo install -m 0755 riot-proxy-linux-amd64 /usr/local/bin/riot-proxy
/usr/local/bin/riot-proxy --version      # riot-proxy 2.7.0
```

`riot-proxy-linux-amd64` is a static musl build; `riot-proxy-linux-arm64` is the ARM equivalent (`README.md:12`). Keep the previous binary beside it on upgrades: [upgrade-and-rollback.md](./upgrade-and-rollback.md).

## 4. The systemd unit

This is the unit from `docs/design/07-deployment.md:74-95` (Option C), which the production box copied from the previous host (`docs/design/12-object-storage-and-hosting.md:589-592`):

```ini
# /etc/systemd/system/riot-proxy.service
[Unit]
Description=riot-proxy
After=network-online.target

[Service]
User=riot-proxy
EnvironmentFile=/etc/riot-proxy/env
ExecStart=/usr/local/bin/riot-proxy serve
WorkingDirectory=/var/lib/riot-proxy
Restart=always
RestartSec=2
# hardening
ProtectSystem=strict
ReadWritePaths=/var/lib/riot-proxy
PrivateTmp=yes
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

`ExecStart` has no `--data-dir`, so `DATA_DIR=/var/lib/riot-proxy/data` must be in the env file (the default is `./data`, relative to the working directory, `src/config.rs:622`). The `rp` helper in `deploy/README.md:121-122` passes `--data-dir /var/lib/riot-proxy/data` explicitly for one-off commands.

```bash
sudo install -m 0644 riot-proxy.service /etc/systemd/system/
sudo systemctl daemon-reload
```

> **TODO (unverified):** the unit actually installed on `ngf-prod-syd1` is not in the repository. The text above is the design's; whether the live file differs (for example in `Wants=network-online.target` or extra hardening) was not checked.

## 5. The environment file

Create `/etc/riot-proxy/env` root-only and fill it in an editor (`docs/design/12-object-storage-and-hosting.md:594-601` streams it with `umask 077`; systemd reads it as root before dropping to `riot-proxy`):

```bash
sudo install -m 0600 /dev/null /etc/riot-proxy/env && sudo editor /etc/riot-proxy/env
```

The production-relevant variables, with placeholders. The file is parsed with node-dotenv rules; an empty value means "use the default" (`src/config.rs:826-869,871-877`).

```ini
RIOT_API_KEY=<the Riot key, never anywhere else>
ENV=production
HOST=127.0.0.1
PORT=8080
DATA_DIR=/var/lib/riot-proxy/data
LOG_FORMAT=json
TRUST_PROXY=true
ADMIN_IP_ALLOWLIST=
JOB_CONCURRENCY=<2-4 on a small host; see capacity.md>
BACKUP_KEEP=<1 when an offsite timer prunes; see backup-and-restore.md>
ARCHIVE_OBJECT_URL=s3://riot-proxy-archive
ARCHIVE_OBJECT_ENDPOINT=https://s3.ap-southeast-2.onidel.cloud
ARCHIVE_OBJECT_REGION=ap-southeast-2
ARCHIVE_OBJECT_ACCESS_KEY=<Onidel access key id>
ARCHIVE_OBJECT_SECRET_KEY=<Onidel secret key>
ARCHIVE_BODY_LOCAL_DAYS=14
LADDER_PLATFORMS=<csv of platforms to crawl, or empty>
LADDER_CRAWL_S=<seconds between crawls, 0 for on demand>
LADDER_TIER_FLOOR=MASTER
ARCHIVE_TIMELINES=true
BULK_USAGE_CEILING=0.80
AGGREGATE_PATCH_LIMIT=<4, or 0 for every patch>
BOOTSTRAP_ADMIN_KEY=
```

`ADMIN_IP_ALLOWLIST` stays empty on purpose: Cloudflare Access is the gate on `admin.`, and behind the tunnel an allowlist of `127.0.0.1` refuses every admin call (`docs/design/12-object-storage-and-hosting.md:868-873`). `BOOTSTRAP_ADMIN_KEY` may be left empty: `serve` then mints the first admin key and prints it once (`src/cli/serve.rs:57-68`).

What `ENV=production` enforces: `AUTH_DISABLED` refused, `/dev` never served, `RIOT_BASE_URL` and `DDRAGON_BASE_URL` refused, `file://` object URLs refused, `http://` endpoints refused, both object-storage keys required with an `s3://` URL (`src/config.rs:658-684,987-999`). `HOST=127.0.0.1` keeps port 8080 off the public interface; the default is `0.0.0.0` (`docs/design/12-object-storage-and-hosting.md:551-556`; `src/config.rs:699`). The full reference, defaults and last-known production values are in [configuration.md](./configuration.md).

Check the file loads without starting the service. Every subcommand validates the configuration first and prints `error: Invalid environment configuration:` with one line per problem, exit 1 (`src/cli/mod.rs:82-88`; `src/config.rs:115-116,785-789`):

```bash
sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
  /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data migrate     # <path>: applied V0001, … or up to date
```

`migrate` creates the database and applies every migration (`src/cli/mod.rs:144-168`); `serve` would do the same on its first boot (`src/db/mod.rs:148-155`).

## 6. Enable, start, verify

```bash
sudo systemctl enable --now riot-proxy
sudo journalctl -u riot-proxy -n 50 --no-pager
curl -s localhost:8080/healthz      # {"ok":true}
curl -s localhost:8080/readyz       # {"ok":true,"sqlite":true,"limiter":true,…}
```

In the journal, in order: `database ready`, `bootstrap admin consumer created`, then on stderr a block `bootstrap admin key (shown once): rpx_…` with `Scopes read,admin. Store it now; it cannot be recovered.` (only when `BOOTSTRAP_ADMIN_KEY` was not set), `limiter checkpoint restored` or `limiter checkpoint could not be read; starting fresh`, `L1 warmed from L2`, `object store configured store=s3://riot-proxy-archive at https://…`, `listening addr=127.0.0.1:8080`, `starting job workers` (`src/cli/serve.rs:52-148,319-324,399-405`; `docs/_meta/scratch/jobs-and-observability.md:11-31`). Store the admin key now; only its sha256 is kept (`src/consumers.rs:105-107,154-157`).

`/readyz` answers 503 with the same body while the limiter is not yet restored, while another process holds the database's write lock, or while a running write has stalled; a long write that makes progress keeps it 200 (`src/routes/health.rs:119-200`).

## 7. Consumer keys

Mint a read key for the site and keep the admin key for the dashboard. The `rp` helper from `deploy/README.md:121-122` runs the binary as the service user with its environment:

```bash
rp() { sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
         /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data "$@"; }
rp key create --name ninjagoldfinch-lol               # scopes read (default), quota 600/min
rp key list
```

`key create` prints the key once (`API KEY   rpx_…`, 36 characters) and never again (`src/cli/key.rs:46-57`; `src/consumers.rs:95-103`). Keys are also mintable at `POST /v1/admin/consumers` with the admin key (`docs/_meta/scratch/routes.md:137-140`).

> **TODO (unverified):** the name of the consumer key the site uses in production is not recorded anywhere in either repository (`docs/_meta/scratch/siblings.md:150`); `ninjagoldfinch-lol` above is a placeholder name.

## 8. Object storage credentials

Three credentials, three places (`deploy/README.md:20-28`):

1. **Onidel access key** for bucket `riot-proxy-archive`: created in the Onidel console, scoped to that bucket (`docs/design/12-object-storage-and-hosting.md:544-548`). It goes into `/etc/riot-proxy/env` as `ARCHIVE_OBJECT_ACCESS_KEY` and `ARCHIVE_OBJECT_SECRET_KEY` (step 5) and into the `[onidel]` remote of `rclone.conf` (step 9). Keys without a URL are accepted and inert, so they can go in before the bucket exists (`src/config/tests.rs:870-876`).
2. **R2 API token for the replica**: Object Read & Write on R2 bucket `riot-proxy-archive` only, so it cannot touch `ngf-backups` (`deploy/README.md:26-28`). Only `rclone` uses it (`[r2]` remote).
3. **R2 API token for backups**: scoped to `ngf-backups`, used by the hand-made offsite timer through `/etc/rclone/r2.conf` (`docs/design/12-object-storage-and-hosting.md:815-818`; `deploy/README.md:151-152`).

Set the lifecycle rules once in each console: Onidel `index/` expires after 14 days; R2 `index/` after 21 days and multipart abort at 1 day; nothing on `packs/` (`deploy/README.md:29-47`).

## 9. rclone and the replica timer

`/etc/riot-proxy/rclone.conf`, `root:root`, mode 0600, filled in an editor (`deploy/README.md:49-72`):

```ini
[onidel]
type = s3
provider = Other
endpoint = https://s3.ap-southeast-2.onidel.cloud
region = ap-southeast-2
access_key_id = <Onidel access key id>
secret_access_key = <Onidel secret key>
no_check_bucket = true

[r2]
type = s3
provider = Cloudflare
endpoint = https://<account id>.r2.cloudflarestorage.com
access_key_id = <R2 token id>
secret_access_key = <R2 token secret>
no_check_bucket = true
no_head = true
```

`no_check_bucket` stops a bucket-scoped token failing on a create-bucket probe; `no_head` works around rclone 1.60 sending `HEAD …?versionId=` that R2 answers with 501 (`deploy/README.md:74-77`). Check both remotes, then install the units from the repository (`deploy/README.md:79-100`):

```bash
sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1
sudo rclone --config /etc/riot-proxy/rclone.conf lsf r2:riot-proxy-archive --max-depth 1
sudo rclone --config /etc/riot-proxy/rclone.conf sync onidel:riot-proxy-archive r2:riot-proxy-archive --dry-run
sudo install -m 0644 deploy/riot-proxy-replica.service deploy/riot-proxy-replica.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start riot-proxy-replica.service          # the first sync, in the foreground
sudo journalctl -u riot-proxy-replica -n 20 --no-pager
sudo systemctl enable --now riot-proxy-replica.timer
systemctl list-timers riot-proxy-replica.timer
```

The service loads `rclone.conf` as a systemd credential and runs as a `DynamicUser` with `ProtectSystem=strict`, `ProtectHome=yes`, `NoNewPrivileges=yes` and idle I/O priority; the timer fires at 17 past each hour with up to 2 min of random delay and catches up a missed run (`deploy/riot-proxy-replica.service:12-33`; `deploy/riot-proxy-replica.timer:6-8`). The offsite backup timer has no file in the repository; see [backup-and-restore.md](./backup-and-restore.md).

## 10. Coordinating with the site on the same host

The site is installed separately (its own repository and README; `docs/design/12-object-storage-and-hosting.md:609-641`). What the two must agree on:

| Item | Value | Evidence |
|---|---|---|
| Port 8080 | reserved for the proxy on loopback; the site binds `127.0.0.1:3000` | `docs/design/12-object-storage-and-hosting.md:551-556,806-807` |
| `PROXY_URL` in `/etc/ninjagoldfinch-lol/env` | `http://127.0.0.1:8080` | `docs/design/12-object-storage-and-hosting.md:629` |
| `PROXY_KEY` in the same file | a read-scope key from step 7, sent as `Authorization: Bearer` on every call | `docs/_meta/scratch/siblings.md:255-275` |
| Start order | the proxy first; the site's requests fail with its own 503 page until the proxy answers | `docs/_meta/scratch/siblings.md:255` |
| Tunnel routes | `ninjagoldfinch.lol` to `:3000`, `api.` and `admin.` to `:8080`, set in the Cloudflare dashboard | `docs/design/12-object-storage-and-hosting.md:893-903` |

Revoking the site's key (`rp key revoke <name>`) takes effect within the auth cache's 60 s, or at once through the admin API (`src/http/auth.rs:30,263-265`).

## Docker alternative (not production)

For a non-production host, `docker-compose.yml` runs the published image with `.env` and a `./data` volume (`docker-compose.yml:1-11`; `README.md:14-24`):

```bash
cp .env.example .env            # set RIOT_API_KEY
GIT_SHA=$(git rev-parse HEAD) docker compose up -d --build   # or omit --build to pull ghcr.io/ninjagoldfinch/riot-proxy:2
docker compose logs | grep -i "admin key"                     # the bootstrap admin key, shown once
```

The image is `FROM scratch` with the static binary, `DATA_DIR=/data`, port 8080 and a `HEALTHCHECK` that runs `riot-proxy healthcheck` (`Dockerfile:19-26`). `healthcheck` loads the configuration first, so `RIOT_API_KEY` must be in the container environment (`src/cli/healthcheck.rs`; `docs/_meta/scratch/config.md:215`). The Proxmox dev VM in `deploy/proxmox/` follows the `:edge` image; it is optional and currently dormant (`README.md:27-58`).

---
**Sources:** `docs/design/07-deployment.md:L15,L74-L95`, `docs/design/12-object-storage-and-hosting.md:L544-L558,L585-L641,L794-L807,L815-L818,L893-L903`, `deploy/README.md:L20-L100,L121-L122,L151-L152`, `deploy/riot-proxy-replica.service:L12-L33`, `deploy/riot-proxy-replica.timer:L6-L8`, `.github/workflows/release.yml:L179-L194`, `README.md:L11-L24,L27-L58`, `Dockerfile:L19-L26`, `docker-compose.yml:L1-L11`, `src/config.rs:L115-L116,L622,L658-L684,L699,L826-L877,L987-L999`, `src/cli/mod.rs:L82-L88,L144-L168`, `src/cli/serve.rs:L52-L148,L319-L324,L399-L405`, `src/cli/key.rs:L46-L57`, `src/consumers.rs:L95-L107,L154-L157`, `src/http/auth.rs:L30,L263-L265`, `src/routes/health.rs:L119-L200`, `docs/_meta/scratch/siblings.md`, `docs/_meta/scratch/routes.md:L137-L140`
**Last verified:** 2026-10-20 against commit `b7b363c`

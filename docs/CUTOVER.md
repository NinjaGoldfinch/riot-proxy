# Cut-over: v1 → v2

The runbook for moving a running v1 deployment (the `docker-compose.prod.yml` stack: api, worker, Redis, Postgres, Caddy) to v2. It follows [design/08 §Cut-over](design/08-migration-plan.md#cut-over):

1. Deploy v2 next to v1.
2. Import v1's archive.
3. Move consumers across one at a time.
4. Watch v1's traffic reach zero.
5. Switch v1 off, keeping its data for 14 days.

Nothing here is irreversible until step 7. Until then, rolling back means pointing a consumer at v1 again.

## 0. Before you start

- [ ] A v2 release: the `riot-proxy-linux-amd64` or `riot-proxy-linux-arm64` binary from the [releases page](https://github.com/NinjaGoldfinch/riot-proxy/releases), checked against `sha256sums`, or the `ghcr.io/ninjagoldfinch/riot-proxy:2` image.
- [ ] **The same `RIOT_API_KEY` as v1.**
  - Facts and players are filed under a key scope derived from the key. With the same key, v1's encrypted ids stay valid and `migrate-v1` needs no `--key-scope`.
  - With a different key, pass `--key-scope <v1's scope>` in step 2. The import prints the players' stored scope, so you can read it there.
- [ ] A hostname or port for v2 that does not disturb v1, e.g. `api-v2.example.com` or `:8081`. Consumers switch by base URL in step 4.
- [ ] A list of every downstream project that calls v1, with its owner. Each one needs a new key; v1's keys are not migrated.

## 1. Deploy v2 next to v1

Use the deployment options in [design/07](design/07-deployment.md) and the [README](../README.md#install). For a v2 container on the same Docker host as v1:

```bash
mkdir -p /srv/riot-proxy-v2 && cd /srv/riot-proxy-v2
cat > .env <<'ENV'
ENV=production
RIOT_API_KEY=…                # v1's key
ENV
cat > docker-compose.yml <<'YAML'
services:
  riot-proxy:
    image: ghcr.io/ninjagoldfinch/riot-proxy:2
    restart: unless-stopped
    env_file: .env
    ports: ["127.0.0.1:8081:8080"]
    volumes: ["./data:/data"]
YAML
```

Do not start it yet. Step 2 runs against an empty data directory first, so the archive is in place before the first boot and its tracked players are polled from the start.

Carry over the v1 settings you changed from their defaults. The names are the same. `REDIS_URL`, `SF_LOCK_MS` and `DEFAULT_PLATFORM` no longer exist (v2 requires `platform` on player and admin routes, ADR-065; set `LADDER_PLATFORMS` explicitly if v1 crawled its default platform), `DATABASE_URL` is now optional, and `NODE_ENV` becomes `ENV` ([design/07 §Configuration](design/07-deployment.md#configuration)).

During the overlap, leave **`LADDER_CRAWL_S=0`** on v2 and do not start crawls by hand. Both proxies share one Riot key, each with its own limiter. The shorter the overlap and the less bulk work either one runs, the less they compete for the key's budget.

## 2. Import v1's archive

On the v1 host, dump the two tables `migrate-v1` reads, as plain SQL:

```bash
cd /path/to/riot-proxy-v1
docker compose -f docker-compose.prod.yml exec -T postgres \
  pg_dump -U proxy -d riotproxy --data-only -t matches -t players > v1-data.sql
```

This plain form needs no `pg_restore` on the v2 host.

v1's nightly `pg-backup` dumps (`riotproxy-<ts>.dump`, custom format) work too: `migrate-v1` ignores the tables it does not use. A custom-format dump is read through `pg_restore`, though, and that must be **version 18 or later**, the version v1's Postgres runs (ADR-060).

Copy the file across. On the v2 host, with v2 stopped:

```bash
# binary
./riot-proxy --data-dir /srv/riot-proxy-v2/data migrate-v1 --from v1-data.sql
# container
docker compose run --rm -v "$PWD/v1-data.sql:/v1-data.sql:ro" riot-proxy migrate-v1 --from /v1-data.sql
```

Read the report:

- `imported N matches …, M timelines, P players`: compare N and P with v1's `SELECT count(*) FROM matches` and `FROM players`.
- `skipped <id>: <why>`: bodies v2 cannot archive, e.g. a match with no `gameEndTimestamp`. They are listed and do not fail the run.
- `note: players carry key scope(s) …`: the key differs from v1's. Re-run with `--key-scope` (§0).
- Consumers are counted and not imported.

The import is idempotent, so you can re-run it at any time. Step 5 re-runs it to pick up what v1 archived during the overlap.

## 3. Start v2 and verify it

```bash
docker compose up -d          # or: ./riot-proxy serve
docker compose logs riot-proxy | grep "bootstrap admin key (shown once)"
```

- [ ] Store the bootstrap admin key (`rpx_…`) in your password manager. It is shown once.
- [ ] `curl -fsS http://127.0.0.1:8081/readyz`: the database is writable and the limiter restored.
- [ ] `/dashboard` with the admin key: jobs are draining, and the limiter shows the key's buckets with no Riot 429s.
- [ ] `/docs` lists the routes your consumers use.
- [ ] A spot check through v2 of a player and a match you know from v1. Their bodies should match v1's.
- [ ] Optional: v1's acceptance suite in live mode against v2. It spends the key's quota.
  ```bash
  cd acceptance && ACCEPTANCE_LIVE=1 ACCEPTANCE_BASE_URL=http://127.0.0.1:8081 \
    ACCEPTANCE_API_KEY=rpx_… ACCEPTANCE_RIOT_ID='Name#TAG' RIOT_API_KEY=… npm test
  ```
- [ ] Prometheus scrapes v2's `/metrics`. The metric names are v1's, so `ops/grafana/riot-proxy-dashboard.json` and `ops/prometheus-alerts.yml` apply unchanged.

Expose v2 the way v1 was exposed: add a site to v1's Caddyfile that points at v2's port, and set `TRUST_PROXY=true` in v2's `.env` so the admin allowlist sees the real client (ADR-068). If services call v2 directly with no proxy, leave it off. v2 has no built-in TLS ([README](../README.md#https)).

## 4. Move consumers, one at a time

For each downstream project:

1. Mint its key on v2:
   ```bash
   docker compose exec riot-proxy /riot-proxy key create --name <project>   # scopes: read (default); add --scopes read,admin only for tools that need /v1/admin
   ```
   The `rpx_…` key is printed once. Hand it to the project's owner over a secure channel.
2. The project switches its base URL to v2 and its key to the new one, then deploys.
3. Watch for at least one of the project's normal usage cycles:
   - v2's logs show its requests: every request span carries `consumer=<project>`.
   - v2's `/dashboard` shows no rise in 4xx or 5xx.
   - On v1, `rate(proxy_requests_total[5m])` drops by about that project's share.
4. If anything is wrong, switch the project back to v1, which is still running unchanged, and investigate.

Revoke nothing on v1 yet.

## 5. Watch v1 reach zero

When every project has moved, v1's request rate should be zero:

```promql
sum(rate(proxy_requests_total{job="riot-proxy-v1"}[15m]))
```

Use whatever job label your scrape config gives v1. Anything still calling v1 is a consumer that was missed.

v1 does not log which consumer made a request: its request logging is off, and its `consumers` table has no last-used column. To find a straggler, disable v1's remaining keys one at a time with v1's own admin API, then wait for the rate to drop or for an owner to report a 401:

```bash
curl -X DELETE -H "Authorization: Bearer $V1_ADMIN_KEY" https://<v1-host>/v1/admin/consumers/<id>
```

v1's `GET /v1/admin/consumers` lists the ids. Then go back to step 4 for that project. v1 has no route to re-enable a key. To undo a disable, run `UPDATE consumers SET disabled_at = NULL WHERE id = '<id>';` in v1's Postgres. Take this step deliberately.

Once v1's rate has been zero for a full day:

1. Dump and import again (step 2), so v2 has the matches v1 archived during the overlap. Upserts make this safe; already-imported matches change nothing.
2. Turn on v2's scheduled crawls if you use them (`LADDER_CRAWL_S`), and restart v2.

## 6. Switch v1 off

```bash
cd /path/to/riot-proxy-v1
docker compose -f docker-compose.prod.yml exec -T postgres \
  pg_dump -U proxy -d riotproxy -Fc > riotproxy-final.dump      # the last full backup
docker compose -f docker-compose.prod.yml down                   # no -v: volumes stay
```

- [ ] Keep `riotproxy-final.dump` and the `pg-data` volume for **14 days**.
- [ ] If v2 took over v1's hostname, point DNS or the Caddy site at v2 now. Consumers already use v2's URL, so this only matters for stragglers and bookmarks.
- [ ] Update dashboards and alerts to v2's scrape job.

## 7. Decommission (after 14 days)

If nothing needed v1's data:

```bash
cd /path/to/riot-proxy-v1
docker compose -f docker-compose.prod.yml down -v    # removes pg-data, redis-data, ddragon
```

- [ ] Archive `riotproxy-final.dump` off-box, or delete it.
- [ ] Remove v1's Caddy site, DNS records, scrape job and alerts.
- [ ] Remove the v1 deploy directory.

## Rollback

- **During steps 1–5:** move the affected consumers back to v1. v1 kept running and kept its keys, so nothing else is needed.
- **During step 6's 14 days:**
  1. `docker compose -f docker-compose.prod.yml up -d` in v1's directory.
  2. Move consumers back.
  3. v1's Postgres is as it was at shutdown. Anything archived only by v2 since then is not in it.
- **After step 7:** there is no v1 to return to. Restore v2 from its own daily backups (`$DATA_DIR/backups/`, [README](../README.md#backups)).

## Sign-off

| Step | Done | By | Date |
|---|---|---|---|
| 0. Prerequisites | | | |
| 1. v2 deployed | | | |
| 2. Archive imported (matches / players) | | | |
| 3. v2 verified | | | |
| 4. Consumers moved (list) | | | |
| 5. v1 at zero; final import | | | |
| 6. v1 off; final dump kept until … | | | |
| 7. v1 decommissioned | | | |

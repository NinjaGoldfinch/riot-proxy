# Upgrade and rollback

How a new release reaches `ngf-prod-syd1`, what the proxy does on its first boot after an upgrade, and how to go back. The automated part of the pipeline is in [../workflows/release-process.md](../workflows/release-process.md); this page starts where that one hands off.

## Where automation stops

`release.yml` builds the binaries, writes `sha256sums`, publishes the GitHub release and pushes the image tags (`.github/workflows/release.yml:179-194`). Nothing in the repository deploys to production: once the GitHub release is published, a person downloads the asset and installs it (`docs/workflows/release-process.md`, "Deploying to production"). The dev VM follows the `:edge` image on its own and is not part of this (`docs/workflows/release-process.md`, "The dev VM").

## What an upgrade does on boot

The first `serve` of a new binary runs the boot sequence in `src/cli/serve.rs:43-416` (`docs/_meta/scratch/jobs-and-observability.md:7-36`). The steps that matter for an upgrade:

| Step | What happens | Evidence |
|---|---|---|
| Migrations | `Db::open` runs every pending embedded migration, grouped in one transaction, before readers open or the writer thread starts; already-applied versions are skipped. There is no "migrations applied" log line, only `database ready` afterwards, or `DbError::Migration` on failure | `src/db/mod.rs:148-155,343-345`; `docs/_meta/scratch/jobs-and-observability.md:295` |
| Direction | forward-only: `src/db/migrations/` holds `V0001` to `V0029` with no down files; rebuild-style migrations copy into `_vNN` tables, drop and rename | `docs/_meta/scratch/persistence.md:17,21-51` |
| Limiter | the checkpoint in `limiter_state` is restored (`limiter checkpoint restored rows=`); a row older than 120 s restores every window as full, so the first requests after a long stop wait rather than burst | `src/cli/serve.rs:76-80`; `src/riot/limiter/persist.rs:23,92-94` |
| Caches | L1 is warmed from every unexpired L2 row and expired L2 rows are swept (`L1 warmed from L2 entries=`) | `src/cli/serve.rs:86-93`; `src/cache/l2.rs:186-216` |
| Running jobs | `recover()` sets every job left `running` by the previous process back to `pending`, keeping its attempts (`re-queued jobs a previous process left running jobs=`) | `src/cli/serve.rs:279-286`; `src/jobs/scheduler.rs:963-973` |
| Catch-ups | boot also queues `archive:match` for stored matches missing their timeline, `facts:reextract` if any match has an older facts version, `tiers:backfill` for unstamped facts, analytics rebuilds for unsplit ladders, with `AGGREGATE_DELTA_S` > 0 one full rebuild per configured ladder not yet stamped (after V0029, every one: about 58 minutes of writer time on production by the 2026-10-20 runs; see [../operations/runbook.md](../operations/runbook.md#19-analytics-deltas-rebuilds-and-recompute-now)), and one `archive:offload` | `src/cli/serve.rs:292-340` |
| Ticks | every tick fires immediately at boot: `maintenance` (and so the daily backup, if today's is missing), `ddragon:sync`, `names:backfill`, the poll fan-outs, `aggregate:delta` (unless `AGGREGATE_DELTA_S=0`) and, when enabled, `ladder:crawl` | `src/jobs/ticks.rs:172,200-204`; `docs/_meta/scratch/jobs-and-observability.md:32` |

On the way down, `SIGTERM` drains requests for up to 10 s, running jobs get 10 s to finish or are aborted and stay `running` for the next boot's `recover`, then the limiter checkpoint and the L2 queue are flushed (`limiter checkpoint and L2 flushed; stopped`) (`src/app.rs:35,240-253`; `src/cli/serve.rs:405-419`; `src/jobs/scheduler.rs:1291-1300`). Downtime is the restart plus any migration; `V0025` took 3 s on production, `V0026` and `V0027` applied together at the first start of v2.7.0 (session notes, 2026-10-15 and 2026-10-19).

## Pre-upgrade checklist

1. Read the release notes and the release commit body for migrations (`docs/workflows/release-process.md`, "Changelog"). A migration that rebuilds a large table (`V0013`, `V0017`, `V0020`, `V0021` did) needs room for a copy of that table.
2. Free disk: a manual backup needs at least 1.2 × (database + WAL) free, the same guard the daily backup applies (`src/jobs/maintenance.rs:34,294-306`; `docs/design/07-deployment.md:177`). Check `df -h /var/lib/riot-proxy` against `ls -la /var/lib/riot-proxy/data/riot-proxy.db*`, or `GET /v1/admin/stats` for `sqlite_db_bytes`, `sqlite_wal_bytes` and `data_dir_free_bytes` (`docs/design/07-deployment.md:216`).
3. No crawl mid-flight: `GET /v1/admin/ladder/crawls` lists crawls and their status (`docs/_meta/scratch/routes.md:187`). A running crawl survives a restart (its legs are jobs, re-queued by `recover`), but a quiet moment keeps the manual backup and the restart short.
4. `GET /v1/admin/stats` and `GET /v1/admin/jobs/stats` (`docs/_meta/scratch/routes.md:157,168`): note pending and failed counts so a regression after the upgrade is visible.
5. `GET /v1/admin/info`: note `version` and `gitSha` (`src/routes/admin.rs:2485-2512`).
6. Confirm the last offsite backup reached R2 (`sudo journalctl -u riot-proxy-offsite -n 20`), so a rollback has something to restore from.

## Upgrade procedure

These follow `docs/workflows/release-process.md`, `docs/design/07-deployment.md:225-231` and `deploy/README.md:121-122`.

1. Download the asset and verify it:

   ```bash
   v=X.Y.Z
   curl -fsSLO https://github.com/NinjaGoldfinch/riot-proxy/releases/download/v$v/riot-proxy-linux-amd64
   curl -fsSLO https://github.com/NinjaGoldfinch/riot-proxy/releases/download/v$v/sha256sums
   sha256sum -c --ignore-missing sha256sums
   ```

2. Keep the running binary as the rollback copy: `sudo cp /usr/local/bin/riot-proxy /usr/local/bin/riot-proxy.<old version>` (production keeps `riot-proxy.2.5.0` and the like; `docs/workflows/release-process.md` step 2).
3. If the release carries a migration, take a manual backup while serving (the same `VACUUM INTO` as the daily one, `src/cli/backup.rs:10-21`):

   ```bash
   rp() { sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
            /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data "$@"; }
   rp backup /var/lib/riot-proxy/data/pre-$v.db      # OUT must not exist; prints <out>: <size> bytes
   ```

4. Install: `sudo install -m 0755 riot-proxy-linux-amd64 /usr/local/bin/riot-proxy`.
5. Restart: `sudo systemctl restart riot-proxy`.
6. Verify:

   ```bash
   sudo journalctl -u riot-proxy -n 50 --no-pager     # database ready … listening … starting job workers
   curl -s localhost:8080/readyz                        # {"ok":true,…}
   curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/info | jq '.version,.gitSha'
   ```

   A boot-queued analytics rebuild holds the writer for minutes; `/readyz` stays 200 through it with `"writerBusy":true` and `writerBusyMs` climbing (WR-02; `src/routes/health.rs:148-200`). Before WR-02 it answered `"sqlite":false` until `analytics recomputed` (session note, 2026-10-15).
7. Delete `pre-$v.db` once the next daily backup has run and reached R2; delete the old binary after a few days of stable running (`reports/OBJ implementation handoff.md:23`).

## Rolling back the binary

When the new version misbehaves and no migration needs undoing:

1. `sudo systemctl stop riot-proxy`.
2. `sudo cp /usr/local/bin/riot-proxy.<old version> /usr/local/bin/riot-proxy`.
3. `sudo systemctl start riot-proxy`, then the verification above.

> **TODO (unverified):** how an older binary behaves when the database already carries migrations it does not embed. `refinery` is run with `set_grouped(true)` (`src/db/mod.rs:343-345`) and nothing in `src/db/mod.rs` handles a history newer than the embedded set; whether it errors or ignores the extra versions was not exercised. Treat "new migration, then downgrade" as the restore case below.

## Rolling back a migration

Migrations are forward-only, so undoing one means restoring the pre-upgrade backup (`docs/design/07-deployment.md:231`). The backup must come from before the upgrade: the daily backup runs first in `maintenance`, which is why it precedes the day's other steps, and step 3 above exists for the same reason.

1. Stop the service, put the old binary back (above, steps 1 and 2).
2. Restore the file: [backup-and-restore.md](./backup-and-restore.md), "Restoring the database". With the service stopped, fold the current file's WAL in before moving it aside; never delete a non-empty `-wal` (`deploy/README.md:246-265`).
3. Start, verify, then rebuild the pack map for packs written since the backup: `rp packs reindex --since <max pack_id in the restored file>` (`deploy/README.md:136-138`).

What a restore costs, by table:

| State | After restoring an older file | Evidence |
|---|---|---|
| L2 cache (`cache`) | rows written since the backup are gone; the next request re-fetches. L1 is warmed from whatever L2 rows the backup holds | `src/cache/l2.rs:186-216` |
| `jobs` | jobs queued since the backup are gone; the ticks re-queue polls, `maintenance`, `ddragon:sync` and (if enabled) `ladder:crawl` at the next boot, and `archive:match` dedupes on match id, so a crawl re-discovers what it lost. `done` rows older than 7 days are trimmed daily anyway | `src/jobs/ticks.rs:172`; `src/jobs/archive.rs:186-191`; `src/jobs/maintenance.rs:36,237-240` |
| `limiter_state` | the row is older than 120 s, so every window restores as full and the limiter is conservative for the first windows | `src/riot/limiter/persist.rs:23,92-94` |
| `consumers` | a key minted after the backup no longer exists; mint it again and update the site's `PROXY_KEY`. A key revoked after the backup is live again: revoke it again | `src/consumers.rs:228-283` |
| `matches`, `timelines`, facts, analytics | archived since the backup: gone from SQLite, re-fetched from Riot when asked or re-crawled. Bodies already packed are still in the bucket but unreferenced until the reindex | `docs/design/12-object-storage-and-hosting.md:938-946` |
| `body_packs`, `body_locations` | behind by every pack written since; `packs reindex --since` reads only those packs and records them. Packs the restored rows do not reference are harmless orphans in the bucket | `deploy/README.md:131,136-138`; `docs/DECISIONS.md:1722-1730` (ADR-165) |
| Bucket | untouched by a restore; `packs/` is never expired, `index/` snapshots roll over 14 days on Onidel and 21 on R2 | `deploy/README.md:31-36` |
| Data Dragon mirror | on disk, not in the database; unaffected | `docs/_meta/scratch/persistence.md:406-412` |

Rolling back only the object-storage feature is configuration, not a restore: unset `ARCHIVE_OBJECT_URL` and offloaded rows read as misses and re-fetch from Riot; the bucket keeps everything and re-setting the URL restores reads (`docs/design/12-object-storage-and-hosting.md:941-943`).

---
**Sources:** `.github/workflows/release.yml:L179-L194`, `docs/workflows/release-process.md`, `docs/design/07-deployment.md:L177,L216,L225-L231`, `docs/design/12-object-storage-and-hosting.md:L938-L946`, `deploy/README.md:L121-L138,L246-L265`, `src/cli/serve.rs:L43-L416`, `src/cli/backup.rs:L10-L21`, `src/db/mod.rs:L148-L155,L343-L345`, `src/jobs/scheduler.rs:L963-L973,L1291-L1300`, `src/jobs/ticks.rs:L172,L200-L204`, `src/jobs/maintenance.rs:L34,L36,L237-L240,L294-L306`, `src/jobs/archive.rs:L186-L191`, `src/riot/limiter/persist.rs:L23,L92-L94`, `src/cache/l2.rs:L186-L216`, `src/consumers.rs:L228-L283`, `src/routes/health.rs:L148-L200`, `src/routes/admin.rs:L2485-L2512`, `src/app.rs:L35,L240-L253`, `docs/DECISIONS.md:L1722-L1730`, `reports/OBJ implementation handoff.md:L23`, `docs/_meta/scratch/jobs-and-observability.md`, `docs/_meta/scratch/persistence.md`, `docs/_meta/scratch/routes.md:L157,L168,L187`
**Last verified:** 2026-10-26 against commit `220cdef`

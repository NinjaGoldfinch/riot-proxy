# Upgrade and rollback

How a new release reaches `ngf-prod-syd1`, what the proxy does on its first boot after an upgrade, and how to go back. The automated part of the pipeline is in [../workflows/release-process.md](../workflows/release-process.md); this page starts where that one hands off.

## Where automation stops

`release.yml` builds the binaries and the deploy tarball, writes `sha256sums`, publishes the GitHub release and pushes the image tags (`.github/workflows/release.yml:262-279`), then deploys it to staging and smoke-tests it there. `deploy.yml` puts a release on production, but only when the owner starts it and approves it on the `prod` environment, and only a version that passed on staging (`staging/smoke`) or was live in production before (`prod/deployed`); the host restarts the service, waits for `/readyz` to report the new version and rolls back by itself if it does not (`docs/workflows/release-process.md`, "Staging" and "Deploying to production"; DEP-03, ADR-196; DEP-05, ADR-198). The dev VM follows the `:edge` image on its own and is not part of this (`docs/workflows/release-process.md`, "The dev VM").

## What an upgrade does on boot

The first `serve` of a new binary runs the boot sequence in `src/cli/serve.rs:94-592` (`docs/_meta/scratch/jobs-and-observability.md:7-36`). It binds, logs `listening` and accepts once the limiter and caches below are ready, so `/readyz` answers 200 from then on; the running-jobs recovery and the catch-ups run after that, behind the server, and the job workers and ticks start only when they finish (`boot catch-up done ms=`, then `starting job workers`; ADR-209). The steps that matter for an upgrade:

| Step | What happens | Evidence |
|---|---|---|
| Migrations | `Db::open` runs every pending embedded migration, grouped in one transaction, before readers open or the writer thread starts; already-applied versions are skipped. There is no "migrations applied" log line, only `database ready` afterwards, or `DbError::Migration` on failure | `src/db/mod.rs:194-202,533-535`; `docs/_meta/scratch/jobs-and-observability.md:295` |
| Direction | forward-only: `src/db/migrations/` holds `V0001` to `V0029` with no down files; rebuild-style migrations copy into `_vNN` tables, drop and rename | `docs/_meta/scratch/persistence.md:17,21-51` |
| Limiter | the checkpoint in `limiter_state` is restored (`limiter checkpoint restored rows=`); a row older than 120 s restores every window as full, so the first requests after a long stop wait rather than burst | `src/cli/serve.rs:134-138`; `src/riot/limiter/persist.rs:23,92-94` |
| Caches | L1 is warmed from every unexpired L2 row and expired L2 rows are swept (`L1 warmed from L2 entries=`) | `src/cli/serve.rs:144-151`; `src/cache/l2.rs:194-225` |
| Running jobs | `recover()` sets every job left `running` by the previous process back to `pending`, keeping its attempts (`re-queued jobs a previous process left running jobs=`); one interrupted on its last (fifth) attempt is `failed` with `interrupted on its last attempt: the process stopped while it ran` instead, so a job that kills the process cannot crash-loop it (`failed jobs a previous process left running on their last attempt jobs=n`, warn; REV-04, ADR-189) | `src/cli/serve.rs`; `src/jobs/scheduler.rs` (`Scheduler::recover`) |
| Catch-ups | boot also queues `archive:match` for stored matches missing their timeline, `facts:reextract` if any match has an older facts version, `tiers:backfill` when a match-id prefix has never been swept or has unstamped facts past its cursor (ADR-213; the first boot after V0033 always does, and that sweep reads every match row once), analytics rebuilds for unsplit ladders, with `AGGREGATE_DELTA_S` > 0 one full rebuild per configured ladder not yet stamped (after V0029, every one: about 58 minutes of writer time on production by the 2026-10-20 runs; see [../operations/runbook.md](../operations/runbook.md#19-analytics-deltas-rebuilds-and-recompute-now)), and one `archive:offload`. These run while the server already answers; the whole catch-up logs `boot catch-up done ms=` (before AVL-01 it ran before the server accepted, 1.3-5 minutes on production, ADR-209) | `src/cli/serve.rs:501-592` |
| Ticks | the poll fan-outs and `aggregate:delta` (unless `AGGREGATE_DELTA_S=0`) fire at once on every boot. `maintenance` (and so the daily backup, if today's is missing), `ddragon:sync`, `names:backfill` and, when enabled, `ladder:crawl`, `aggregate:analytics` and `timelines:backfill` (per ladder or region) fire at once only when overdue: their last run plus their period is past, or nothing is on record, as on a fresh database. Otherwise each logs `tick deferred kind=… next_in_s=…` and waits; a restart inside the day runs no `maintenance` (ADR-214) | `src/jobs/ticks.rs:217-382` |

On the way down, `SIGTERM` drains requests for up to 10 s, running jobs get 10 s to finish or are aborted and stay `running` for the next boot's `recover`, then the limiter checkpoint, the writes still running behind their requests (AVL-02) and the L2 queue are flushed (`limiter checkpoint and L2 flushed; stopped`) (`src/app.rs:35,309-322`; `src/cli/serve.rs:467-492,595`; `src/jobs/scheduler.rs:1350-1367`). A `SIGTERM` during the boot catch-up stops it where it is (`boot catch-up stopped; job workers not started`, `reason=shutdown signal`) and drains, and blocking work it abandoned gets at most 10 s before the process exits (`src/cli/mod.rs`, `BLOCKING_GRACE`; ADR-209). Downtime is the restart plus any migration; `V0025` took 3 s on production, `V0026` and `V0027` applied together at the first start of v2.7.0 (session notes, 2026-10-15 and 2026-10-19).

## Pre-upgrade checklist

1. Read the release notes and the release commit body for migrations (`docs/workflows/release-process.md`, "Changelog"). A migration that rebuilds a large table (`V0013`, `V0017`, `V0020`, `V0021` did) needs room for a copy of that table.
2. Free disk: a manual backup needs at least 1.2 × (database + WAL) free, the same guard the daily backup applies (`src/jobs/maintenance.rs:36,294-306`; `docs/design/07-deployment.md:181`). Check `df -h /var/lib/riot-proxy` against `ls -la /var/lib/riot-proxy/data/riot-proxy.db*`, or `GET /v1/admin/stats` for `sqlite_db_bytes`, `sqlite_wal_bytes` and `data_dir_free_bytes` (`docs/design/07-deployment.md:220`).
3. No crawl mid-flight: `GET /v1/admin/ladder/crawls` lists crawls and their status (`docs/_meta/scratch/routes.md:187`). A running crawl survives a restart (its legs are jobs, re-queued by `recover`), but a quiet moment keeps the manual backup and the restart short.
4. `GET /v1/admin/stats` and `GET /v1/admin/jobs/stats` (`docs/_meta/scratch/routes.md:157,168`): note pending and failed counts so a regression after the upgrade is visible.
5. `GET /v1/admin/info`: note `version` and `gitSha` (`src/routes/admin.rs:2501-2528`).
6. Confirm the last offsite backup reached R2 (`sudo journalctl -u riot-proxy-offsite -n 20`), so a rollback has something to restore from.

## Upgrade procedure

These follow `docs/workflows/release-process.md`, `docs/design/07-deployment.md:229-235` and `deploy/README.md:121-122`.

1. If the release carries a migration, take a manual backup while serving, before starting the deploy (the same `VACUUM INTO` as the daily one, `src/cli/backup.rs:10-21`). `/usr/local/bin/riot-proxy` is a symlink to the live release, so `rp` runs the binary that serves:

   ```bash
   v=X.Y.Z
   rp() { sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
            /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data "$@"; }
   rp backup /var/lib/riot-proxy/data/pre-$v.db      # OUT must not exist; prints <out>: <size> bytes
   ```

2. Check that the version passed on staging (the release run's `staging` job, or `gh api repos/NinjaGoldfinch/riot-proxy/commits/vX.Y.Z/status`); a production deploy refuses it otherwise. Deploy: `ngf-deploy riot-proxy` (or `-v vX.Y.Z`), then approve the run (`docs/workflows/release-process.md`, "Deploying to production"). The host waits up to its health wait (`HEALTH_WAIT` in `/etc/ninja-deploy/riot-proxy.conf`) for `/readyz` to report the new version; set it to cover the slowest migration a release brings, because a rollback after a migration has applied starts a binary that refuses the database ("Rolling back the binary", below).
3. Verify:

   ```bash
   sudo journalctl -u riot-proxy -n 50 --no-pager     # database ready … listening … starting job workers
   curl -s localhost:8080/readyz                        # {"ok":true,…,"version":"X.Y.Z"}
   curl -s -H "Authorization: Bearer $ADMIN_KEY" localhost:8080/v1/admin/info | jq '.version,.gitSha'
   ```

   A boot-queued analytics rebuild runs for minutes, holding the writer one patch's step at a time (seconds each, up to about 12 s on THR-03's synthetic archive; AVL-05, ADR-215), and a restart resumes it at the step it reached; `/readyz` stays 200 through it with `"writerBusy":true` and `writerBusyMs` climbing (WR-02; `src/routes/health.rs:152-204`). Before WR-02 it answered `"sqlite":false` until `analytics recomputed` (session note, 2026-10-15).
4. Delete `pre-$v.db` once the next daily backup has run and reached R2. Old releases need no cleanup: the host keeps the last five in `/srv/riot-proxy/releases/`.

## Rolling back the binary

When the new version misbehaves and no migration needs undoing, deploy the previous tag: `ngf-deploy riot-proxy -v v<old version>`, then approve. It is still on the host, so nothing is uploaded; the restart and the health check are the same as for an upgrade. The gate lets it through because it carries `prod/deployed` from when it was live; check with `gh api repos/NinjaGoldfinch/riot-proxy/commits/v<old version>/status`. v2.9.0 and v2.10.0 were live before the gate and carry no status of their own: until the owner marks them by hand (`docs/workflows/release-process.md`, "Deploying to production"), a rollback to them is refused without `break_glass`. Without GitHub, the same step on the host: `sudo -u deploy env SSH_ORIGINAL_COMMAND="activate v<old version>" /usr/local/sbin/ninja-deploy riot-proxy` (`ninja-deploy list` shows what is on disk).

A binary older than the database's newest migration does not start. `migrate` runs refinery with its defaults (`src/db/mod.rs:551-555`), and refinery aborts with `MissingVersion` when the history in `refinery_schema_history` holds a version the binary does not embed (`abort_missing` defaults to true: refinery-core 0.9.2 `src/runner.rs:240-241`, `src/traits/mod.rs:26-28`). So after a release that migrated, going back is the restore below, not a redeploy; the same holds for the automatic rollback, which is why the health wait must cover the migration. A rollback that interrupts a migration is safe: the migrations apply in one transaction (`set_grouped(true)`), which the stopped process never commits.

## Rolling back a migration

Migrations are forward-only, so undoing one means restoring the pre-upgrade backup (`docs/design/07-deployment.md:235`). The backup must come from before the upgrade: the daily backup runs first in `maintenance`, which is why it precedes the day's other steps, and step 1 above exists for the same reason.

1. `sudo systemctl stop riot-proxy`.
2. Restore the file: [backup-and-restore.md](./backup-and-restore.md), "Restoring the database". With the service stopped, fold the current file's WAL in before moving it aside; never delete a non-empty `-wal` (`deploy/README.md:246-265`).
3. Start the old release on the restored file by deploying its tag ("Rolling back the binary", above), verify, then rebuild the pack map for packs written since the backup: `rp packs reindex --since <max pack_id in the restored file>` (`deploy/README.md:136-138`).

What a restore costs, by table:

| State | After restoring an older file | Evidence |
|---|---|---|
| L2 cache (`cache`) | rows written since the backup are gone; the next request re-fetches. L1 is warmed from whatever L2 rows the backup holds | `src/cache/l2.rs:194-225` |
| `jobs` | jobs queued since the backup are gone; the ticks re-queue the polls at the next boot, and `maintenance`, `ddragon:sync` and (if enabled) `ladder:crawl` when the restored `done` rows and crawl log say they are due, and `archive:match` dedupes on match id, so a crawl re-discovers what it lost. `done` rows older than 7 days are trimmed daily anyway | `src/jobs/ticks.rs:217-382`; `src/jobs/archive.rs:186-191`; `src/jobs/maintenance.rs:38,337-380` |
| `limiter_state` | the row is older than 120 s, so every window restores as full and the limiter is conservative for the first windows | `src/riot/limiter/persist.rs:23,92-94` |
| `consumers` | a key minted after the backup no longer exists; mint it again and update the site's `PROXY_KEY`. A key revoked after the backup is live again: revoke it again | `src/consumers.rs:228-283` |
| `matches`, `timelines`, facts, analytics | archived since the backup: gone from SQLite, re-fetched from Riot when asked or re-crawled. Bodies already packed are still in the bucket but unreferenced until the reindex | `docs/design/12-object-storage-and-hosting.md:938-946` |
| `body_packs`, `body_locations` | behind by every pack written since; `packs reindex --since` reads only those packs and records them. Packs the restored rows do not reference are harmless orphans in the bucket | `deploy/README.md:131,136-138`; `docs/DECISIONS.md:1722-1730` (ADR-165) |
| Bucket | untouched by a restore; `packs/` is never expired, `index/` snapshots roll over 14 days on Onidel and 21 on R2 | `deploy/README.md:31-36` |
| Data Dragon mirror | on disk, not in the database; unaffected | `docs/_meta/scratch/persistence.md:406-412` |

Rolling back only the object-storage feature is configuration, not a restore: unset `ARCHIVE_OBJECT_URL` and offloaded rows read as misses and re-fetch from Riot; the bucket keeps everything and re-setting the URL restores reads (`docs/design/12-object-storage-and-hosting.md:941-943`).

---
**Sources:** `.github/workflows/release.yml:L262-L279`, `.github/workflows/deploy.yml`, `docs/workflows/release-process.md`, `docs/design/07-deployment.md:L181,L220,L229-L235`, `docs/design/12-object-storage-and-hosting.md:L941-L949`, `deploy/README.md:L121-L138,L246-L265`, `src/cli/serve.rs:L73-L568`, `src/cli/mod.rs`, `src/cli/backup.rs:L10-L21`, `src/db/mod.rs:L152-L171,L483-L487`, refinery-core 0.9.2 `src/runner.rs:L240-L241` and `src/traits/mod.rs:L26-L28`, `src/jobs/scheduler.rs:L983-L1014,L1350-L1367`, `src/jobs/ticks.rs:L341,L359-L380`, `src/jobs/maintenance.rs:L36,L38,L337-L380,L294-L306`, `src/jobs/archive.rs:L186-L191`, `src/riot/limiter/persist.rs:L23,L92-L94`, `src/cache/l2.rs:L194-L225`, `src/consumers.rs:L228-L283`, `src/routes/health.rs:L152-L204`, `src/routes/admin.rs:L2501-L2528`, `src/app.rs:L35,L309-L322`, `docs/DECISIONS.md:L1722-L1730`, `docs/_meta/scratch/jobs-and-observability.md`, `docs/_meta/scratch/persistence.md`, `docs/_meta/scratch/routes.md:L157,L168,L187`
**Last verified:** 2026-11-09 against commit `7531561`

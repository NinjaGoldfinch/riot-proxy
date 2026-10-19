# Backup and restore

What state matters, the four copies that exist (daily `VACUUM INTO`, the offsite copy to R2, the bucket replica, the index snapshots), how to restore each, and what to do when the Onidel bucket is gone. The commands for the bucket side are in `deploy/README.md`; this page says when to use which.

## What state matters

| State | Where | Durable copy | Disposable? | Evidence |
|---|---|---|---|---|
| Everything but raw bodies: consumers, players, matches and timelines metadata, facts, analytics, jobs, limiter checkpoint, metrics history, the pack map (`body_packs`, `body_locations`) | `riot-proxy.db` | daily backup in `backups/`, copied to R2 `ngf-backups` | no: the system of record | `docs/_meta/scratch/persistence.md:375-377` |
| Raw match and timeline bodies once freed from SQLite | Onidel bucket `riot-proxy-archive` (`packs/`) | hourly R2 replica | no: once `free` runs, SQLite holds `X''` and the pack is the only copy | `docs/_meta/scratch/persistence.md:377`; `src/db/migrations/V0023__body_packs.sql:3-5` |
| Bodies still inside the local window | `matches.body_zstd`, `timelines.body_zstd` | in the daily backup until freed; packed within hours of archiving | re-fetchable from Riot if lost | `docs/design/12-object-storage-and-hosting.md:208-212` |
| `body_packs` and `body_locations` | SQLite | three copies: the backups, the daily `index/` snapshots in the bucket, and each pack's own trailing index | rebuildable with `packs reindex` | `src/archive/recovery.rs:1-15`; `deploy/README.md:109-115` |
| `limiter_state` | SQLite | in the backup; a row older than 120 s restores every window as full | effectively yes: a stale checkpoint is conservative, not wrong | `src/riot/limiter/persist.rs:23,92-94` |
| L1 and L2 caches | memory and the `cache` table | none needed | yes: a miss goes to Riot | `src/cache/l2.rs:186-216` |
| Data Dragon mirror | `$DATA_DIR/ddragon/` | none needed | yes: `ddragon:sync` re-fetches JSON and images refill on first request | `docs/_meta/scratch/persistence.md:406-412` |
| `metrics_history`, recent-request rings | SQLite (24 h) and memory | none | yes | `src/jobs/maintenance.rs:38,241-244`; `src/http/recent.rs:92-100` |

The database is never written to the Onidel bucket, and raw bodies are never in `ngf-backups`; the two stores hold different things (`docs/design/12-object-storage-and-hosting.md:424-428`). R2 backs up both: it replicates the packs and it holds the SQLite backups. Onidel backs up nothing; it is the primary for bodies only.

## Copy 1: the daily `VACUUM INTO` backup

The `maintenance` job, ticked every 86 400 s and first at every boot, runs the backup as its first step (`src/jobs/ticks.rs:116-117`; `src/jobs/maintenance.rs:215-232`):

- Statement `VACUUM INTO ?1` from a separate read-only connection on a blocking thread, so the writer is not held (`src/jobs/maintenance.rs:57-76`). The copy is consistent and compact (freed pages are not copied).
- Path `$DATA_DIR/backups/riot-proxy-<YYYY-MM-DD>.db`, UTC date, one per day; today's existing file skips the step (`src/jobs/maintenance.rs:40-41,78-84,280-283`).
- Written as `backups/.riot-proxy-<date>.db.partial` and renamed when complete; a leftover partial is removed first and on failure (`src/jobs/maintenance.rs:287-291,307-321`).
- Free-space guard: skipped with the warning `backup skipped: not enough disk` when the backup directory's file system has less than 1.2 × (database + WAL) free; `BACKUP_FREE_FACTOR` (`src/jobs/maintenance.rs:34,294-306`; `docs/DECISIONS.md:1459-1464`, ADR-136).
- Retention: the newest `BACKUP_KEEP` files matching `riot-proxy-DDDD-DD-DD.db` are kept, default 14, `0` takes none and prunes none (`src/config.rs:729`; `src/jobs/maintenance.rs:86-105,219-231`).
- A failed copy is logged (`backup failed; maintenance goes on`) and the trims, sweep and checkpoint still run (`src/jobs/maintenance.rs:314-322`).
- Status: `GET /v1/admin/backups` lists the files, `BACKUP_KEEP`, free space and the last run; `POST /v1/admin/maintenance/run` queues the job now (`docs/design/07-deployment.md:181`; `src/jobs/maintenance.rs:127-139`).

On production `BACKUP_KEEP=1` because the offsite timer prunes to one local copy and the disk cannot hold several (`docs/design/12-object-storage-and-hosting.md:819-830`; `reports/OBJ implementation handoff.md:20`). A session note of 2026-10-18 records the nightly backup being skipped by the 1.2 × guard with the database at 53.8 GB; see [capacity.md](./capacity.md).

A manual copy at any time, same statement (`src/cli/backup.rs:10-21`; the `rp` helper is in [install.md](./install.md)):

```bash
rp backup /var/lib/riot-proxy/data/manual-$(date -u +%F).db     # OUT must not exist
```

The copy is finished only when the command returns and the file starts with `SQLite format 3`; a copy still being written has a blank first page (`docs/design/07-deployment.md:204`; `deploy/README.md:240-243`).

## Copy 2: the offsite copy to R2 (`riot-proxy-offsite.timer`)

A hand-made systemd timer on the box, not in this repository (`docs/design/12-object-storage-and-hosting.md:808-826`; `docs/design/07-deployment.md:182`):

- every 6 hours: `rclone copy` of `$DATA_DIR/backups` to R2 bucket `ngf-backups`, prefix `riot-proxy/`, then `rclone check --one-way`, then delete all but the newest local backup (`tail -n +2`);
- config `/etc/rclone/r2.conf` with `no_head = true` (Ubuntu's rclone 1.60.1 sends `HEAD …?versionId=` that R2 answers with 501);
- R2 lifecycle: objects deleted after 14 days, multipart abort at 1 day; the API token is scoped to `ngf-backups` only;
- the first backup taken on the new box, 21.6 GB, reached R2 and passed `rclone check` on 2026-10-13.

Because `rclone copy` skips files already present, a day's backup is uploaded once, after which the three 6-hourly runs find nothing to do.

> **TODO (unverified):** the unit and timer text of `riot-proxy-offsite.{service,timer}` and its `verify.conf` drop-in are on the box only (`docs/design/12-object-storage-and-hosting.md:808-810`; `docs/_meta/scratch/persistence.md:431`). The description above is design/12's account of it; the exact `ExecStart` lines, the failure hook and the user it runs as were not read.

## Copy 3: the bucket replica (`riot-proxy-replica.timer`)

`deploy/riot-proxy-replica.service` runs `rclone sync onidel:riot-proxy-archive r2:riot-proxy-archive --metadata --transfers 16` at 17 past each hour, `Persistent=true`, as a `DynamicUser` with `rclone.conf` loaded as a credential (`deploy/riot-proxy-replica.service:12-19`; `deploy/riot-proxy-replica.timer:6-8`). `--metadata` carries each pack's `x-amz-meta-sha256`, which `packs reindex` reads instead of hashing the object (`deploy/README.md:103`). The hour is how long a new pack has a single provider, since `free` only runs after the local window anyway (`docs/design/12-object-storage-and-hosting.md:361-366`). Check it is whole:

```bash
sudo rclone --config /etc/riot-proxy/rclone.conf check onidel:riot-proxy-archive r2:riot-proxy-archive --one-way
```

"hashes could not be checked" for multipart packs is expected; sizes are compared (`deploy/README.md:103`). The proxy never reads R2 (`docs/design/12-object-storage-and-hosting.md:372-374`).

## Copy 4: index snapshots

`archive:index-snapshot`, queued after each `maintenance` run when a bucket is configured, streams `body_packs` and `body_locations` as zstd JSON lines to `index/packs-<date>.jsonl.zst` and `index/locations-<date>.jsonl.zst` with metadata `last-pack` and `rows` (`src/archive/recovery.rs:207-271`; `src/jobs/maintenance.rs:204-211`). `rp packs snapshot` writes one now (`src/cli/packs.rs:55-66`). Snapshots expire after 14 days on Onidel and 21 on R2 (`deploy/README.md:31-35`). A damaged snapshot fails its frame checksum and `restore-index` falls back to the day before (`docs/DECISIONS.md:1722-1730`, ADR-165).

## Restoring the database

Stop, replace the file, start; then catch the pack map up (`README.md:142`; `docs/design/07-deployment.md:179`; `deploy/README.md:136-138`). Each step checks before the next; never delete a non-empty `-wal`, it holds committed writes (`deploy/README.md:246-265`).

1. Get the backup: the newest local one in `/var/lib/riot-proxy/data/backups/`, or from R2:

   ```bash
   sudo rclone --config /etc/rclone/r2.conf lsf r2:ngf-backups/riot-proxy/ | tail -1
   sudo rclone --config /etc/rclone/r2.conf copyto r2:ngf-backups/riot-proxy/<newest> /var/lib/riot-proxy/data/restore.db
   ```

2. Check it is a finished database: `sudo head -c 16 /var/lib/riot-proxy/data/restore.db | grep -q 'SQLite format 3' && echo "header ok"`, and `sudo -u riot-proxy sqlite3 -readonly /var/lib/riot-proxy/data/restore.db "PRAGMA quick_check;"` (tens of minutes on a large file).
3. `sudo systemctl stop riot-proxy`, then `sudo fuser -v /var/lib/riot-proxy/data/riot-proxy.db*` must print nothing.
4. Fold the current file's WAL in and move it aside:

   ```bash
   cd /var/lib/riot-proxy/data
   sudo -u riot-proxy sqlite3 -cmd '.timeout 10000' riot-proxy.db "PRAGMA wal_checkpoint(TRUNCATE);"   # 0|0|0
   ls -la riot-proxy.db*                                                     # -wal absent or 0 bytes
   sudo -u riot-proxy mv riot-proxy.db riot-proxy.old.db
   [ ! -s riot-proxy.db-wal ] && sudo rm -f riot-proxy.db-wal riot-proxy.db-shm   # only an empty WAL
   ```

5. `sudo -u riot-proxy mv restore.db riot-proxy.db` (the file must be owned by `riot-proxy`).
6. `sudo systemctl start riot-proxy`; `curl -s localhost:8080/readyz`. Migrations newer than the backup apply on this boot (`src/db/mod.rs:148-155`).
7. Catch the pack map up with the packs written after the backup was taken:

   ```bash
   sudo systemctl stop riot-proxy
   sudo -u riot-proxy sqlite3 -readonly riot-proxy.db "SELECT max(pack_id) FROM body_packs"
   rp packs reindex --since <that pack id>        # reindexed N packs, M bodies
   sudo systemctl start riot-proxy
   ```

   `reindex` is idempotent; an unreadable pack is listed and the command exits non-zero after recording the rest (`src/cli/packs.rs:32-35,70-83`). If the restored map is damaged or `body_packs` is empty, use `rp packs restore-index` instead: it loads the newest readable snapshot and reindexes the packs after it (`deploy/README.md:130,139`).
8. Keep `riot-proxy.old.db` until the next daily backup has run and reached R2.

What is lost between the backup and the restore, table by table, is in [upgrade-and-rollback.md](./upgrade-and-rollback.md), "Rolling back a migration". Keys minted since the backup must be minted again.

## When the Onidel bucket is gone

The replica is a full copy of the bucket, so the proxy can read from it directly (`deploy/README.md:140-142`; `docs/design/12-object-storage-and-hosting.md:372-374`):

1. Edit `/etc/riot-proxy/env`: `ARCHIVE_OBJECT_ENDPOINT=https://<account id>.r2.cloudflarestorage.com`, `ARCHIVE_OBJECT_REGION=auto`, and the R2 token's id and secret in `ARCHIVE_OBJECT_ACCESS_KEY` / `ARCHIVE_OBJECT_SECRET_KEY`. `ARCHIVE_OBJECT_URL` stays `s3://riot-proxy-archive`; the bucket has the same name on both.
2. `sudo systemctl restart riot-proxy`; the boot log shows `object store configured store=s3://riot-proxy-archive at https://<account id>.r2.cloudflarestorage.com`.
3. Stop the replica timer (`sudo systemctl disable --now riot-proxy-replica.timer`): its source is gone, and a `sync` from an empty source would delete the replica.

The token the proxy then uses must be Object Read & Write on `riot-proxy-archive`, since the offload keeps writing new packs. There is no R2-specific code in the proxy; the only accommodation is equal 8 MiB multipart parts (`docs/_meta/scratch/persistence.md:378`).

## The quarterly restore drill

`deploy/README.md:144-220` has the drill in full. In summary, on the box in `/var/tmp/restore-drill`, against R2 with a **read-only** token so the drill's own boot-time `archive:offload` cannot write into the replica:

1. Copy the newest backup from `ngf-backups` into a scratch data directory.
2. Start a proxy on `127.0.0.1:8081` with `ENV=development`, `AUTH_DISABLED=true`, a fake Riot key, `JOB_CONCURRENCY=1`, `ARCHIVE_BODY_LOCAL_DAYS=365` and a 1 GiB pack target; read one offloaded match through `/v1/lol/matches/{region}/{id}` and confirm `X-Cache: ARCHIVE`.
3. Empty `body_locations` and `body_packs` in the scratch copy, run `packs restore-index`, and confirm the rows match the backup's up to its newest pack.
4. Remove the scratch directory and note the date in `PROGRESS.md`.

The drill passed on 2026-10-15: the 2026-10-14 backup served a body from R2 with `X-Cache: ARCHIVE`, and `restore-index` (snapshot of 2026-10-14, 351 packs plus 2 newer) rebuilt both tables identical to production's (`PROGRESS.md:335`).

## Reclaiming space after the offload

Freeing a body empties its blob; the pages go to SQLite's free list and the file does not shrink. Once `archive_offload_pending{step="free"}` is 0 for both kinds, copy the database compactly with `rp backup` and swap it in with the gated steps in `deploy/README.md:222-270` (tmux, `fuser`, header check, `quick_check`, checkpoint, swap). The 2026-10-14 incident, a swap made while the copy was still being written, is recounted there; nothing was lost, and the gates exist because of it.

---
**Sources:** `src/jobs/maintenance.rs:L34-L41,L57-L105,L127-L139,L204-L232,L280-L322`, `src/jobs/ticks.rs:L116-L117`, `src/cli/backup.rs:L10-L21`, `src/cli/packs.rs:L32-L83`, `src/archive/recovery.rs:L1-L15,L207-L271`, `src/riot/limiter/persist.rs:L23,L92-L94`, `src/cache/l2.rs:L186-L216`, `src/db/mod.rs:L148-L155`, `src/db/migrations/V0023__body_packs.sql:L3-L5`, `src/config.rs:L729`, `deploy/README.md:L12-L47,L103-L152,L156-L160,L222-L270`, `deploy/riot-proxy-replica.service:L12-L19`, `deploy/riot-proxy-replica.timer:L6-L8`, `docs/design/07-deployment.md:L175-L206`, `docs/design/12-object-storage-and-hosting.md:L208-L212,L359-L434,L808-L830,L938-L946`, `docs/DECISIONS.md:L1459-L1464,L1722-L1730`, `README.md:L134-L142`, `PROGRESS.md:L335`, `reports/OBJ implementation handoff.md:L20`, `docs/_meta/scratch/persistence.md`
**Last verified:** 2026-10-20 against commit `b7b363c`

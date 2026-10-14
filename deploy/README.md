# Deploy

| Path | What |
|---|---|
| `proxmox/` | the dev VM that follows `main` (OPS-01/02, its own README) |
| `riot-proxy-replica.{service,timer}` | hourly `rclone sync` of the archive bucket to its R2 replica (OBJ-04) |
| `test.sh` | checks the replica units (CI's `ops` job) |

Everything below is for production (`ngf-prod-syd1`) and is run by the owner.
The design is docs/design/12 (§Replica and backups); this is the how.

## Archive bucket and replica

Raw match and timeline bodies live in packs in the Onidel bucket
`riot-proxy-archive` (OBJ-01..03). R2 holds a copy, made hourly by
`rclone sync` from the box. The proxy never reads the replica; it is for the
day the Onidel bucket or account is gone, and restoring means pointing
`ARCHIVE_OBJECT_URL` at it.

### 1. Buckets, keys and lifecycle rules

Once, in each console:

- **Onidel** `riot-proxy-archive` (Sydney): the access key the proxy uses
  (`ARCHIVE_OBJECT_ACCESS_KEY` / `_SECRET_KEY`) also serves `rclone`.
- **R2** `riot-proxy-archive`: location hint Oceania if offered. An API token
  with Object Read & Write on **this bucket only** (not the account's other
  buckets, so it can't touch `ngf-backups`).
- **Lifecycle rules:**

  | Bucket | Prefix | Rule |
  |---|---|---|
  | Onidel `riot-proxy-archive` | `index/` | expire after 14 days |
  | R2 `riot-proxy-archive` | `index/` | expire after 21 days, longer than Onidel's so it never deletes a snapshot the sync still mirrors; a safety net if the sync stops |
  | R2 `riot-proxy-archive` | whole bucket | abort incomplete multipart uploads after 1 day |
  | both | `packs/` | **none**: packs are kept forever; only compaction (OBJ-06) deletes one |

  R2 sets the rules under the bucket's Settings, Object lifecycle rules (a new
  bucket comes with a multipart-abort rule; set it to 1 day). Onidel documents
  lifecycle support in its knowledge base. If its console has no form for it,
  the S3 API takes the rule, with the proxy's key in the AWS CLI's environment:

  ```bash
  aws --endpoint-url https://s3.ap-southeast-2.onidel.cloud --region ap-southeast-2 \
    s3api put-bucket-lifecycle-configuration --bucket riot-proxy-archive \
    --lifecycle-configuration '{"Rules":[{"ID":"index-14d","Filter":{"Prefix":"index/"},"Status":"Enabled","Expiration":{"Days":14}}]}'
  ```

### 2. rclone

`/etc/riot-proxy/rclone.conf`, `root:root`, mode 0600. Fill the keys in an
editor, never on a command line:

```ini
[onidel]
type = s3
provider = Other
endpoint = https://s3.ap-southeast-2.onidel.cloud
region = ap-southeast-2
access_key_id = …
secret_access_key = …
no_check_bucket = true

[r2]
type = s3
provider = Cloudflare
endpoint = https://<account id>.r2.cloudflarestorage.com
access_key_id = …
secret_access_key = …
no_check_bucket = true
no_head = true
```

`no_check_bucket` stops rclone trying to create a bucket a scoped token may
not see. `no_head` is the workaround from the move (design/12 §As executed):
Ubuntu's rclone 1.60 sends `HEAD …?versionId=` after each upload, which R2
answers with 501.

Check both remotes before installing the timer:

```bash
sudo rclone --config /etc/riot-proxy/rclone.conf lsf onidel:riot-proxy-archive --max-depth 1
sudo rclone --config /etc/riot-proxy/rclone.conf lsf r2:riot-proxy-archive --max-depth 1
sudo rclone --config /etc/riot-proxy/rclone.conf sync onidel:riot-proxy-archive r2:riot-proxy-archive --dry-run
```

### 3. The hourly unit

```bash
sudo install -m 0644 deploy/riot-proxy-replica.service deploy/riot-proxy-replica.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start riot-proxy-replica.service          # the first sync, in the foreground
sudo journalctl -u riot-proxy-replica -n 20 --no-pager
sudo systemctl enable --now riot-proxy-replica.timer
systemctl list-timers riot-proxy-replica.timer
```

It runs at 17 past each hour, catches up a run missed while the box was down
(`Persistent=true`), and takes the config as a systemd credential, so `rclone`
runs as a throwaway user that can read nothing else. If a temporary cron line
was set up before this unit existed (design/12 step 8), remove it.

`--metadata` copies each pack's `x-amz-meta-sha256`, which `packs reindex` reads instead of hashing the whole object. `rclone check` reports "hashes could not be checked" for packs uploaded in parts (their ETags aren't MD5) and compares sizes; that is expected.

To check the replica is whole: `sudo rclone --config /etc/riot-proxy/rclone.conf check onidel:riot-proxy-archive r2:riot-proxy-archive --one-way`.

## Index snapshots and recovery

The map of where each body is (`body_packs`, `body_locations`) is in SQLite and
its backups. Two more copies are in the bucket, and replicate with it:

- `archive:index-snapshot` writes `index/packs-<date>.jsonl.zst` and
  `index/locations-<date>.jsonl.zst` once a day, after `maintenance`.
  `riot-proxy packs snapshot` writes one now.
- Every pack ends with an index of its own bodies.

They run as the service does: as `riot-proxy`, with its environment file, so
the keys are never on a command line. A shell function saves typing:

```bash
rp() { sudo systemd-run --quiet --wait --pipe --uid=riot-proxy -p EnvironmentFile=/etc/riot-proxy/env \
         /usr/local/bin/riot-proxy --data-dir /var/lib/riot-proxy/data "$@"; }
sudo systemctl stop riot-proxy        # restore-index and reindex: with the service stopped
rp packs restore-index
sudo systemctl start riot-proxy
```

| Command | Does |
|---|---|
| `packs restore-index` | loads the newest snapshot that reads cleanly (a corrupt or half-written one is passed over for the day before), then reads only the packs written after it. With no readable snapshot, it reindexes every pack |
| `packs reindex [--since <pack id>]` | reads the trailer and index of every pack, or of those after `<pack id>`, and records them. Safe to repeat; a pack that cannot be read is listed and the command exits non-zero after recording the rest |
| `packs snapshot` | today's snapshot, now |

Which to use:

- **Database lost, backup fine:** restore the backup (design/07 §Backups &
  restore), then `packs reindex --since <newest pack_id in the backup>`
  (`sqlite3 riot-proxy.db "SELECT max(pack_id) FROM body_packs"`).
- **Backup lost or its map damaged:** `packs restore-index`.
- **Onidel gone:** point `ARCHIVE_OBJECT_URL`'s endpoint and keys at R2
  (`ARCHIVE_OBJECT_ENDPOINT=https://<account id>.r2.cloudflarestorage.com`,
  `ARCHIVE_OBJECT_REGION=auto`) and restart. Nothing else changes.

### Restore drill (quarterly)

On the box, in a scratch directory, against R2. It touches neither the live
database nor the Onidel bucket; `/var/tmp` needs room for one backup.

```bash
d=/var/tmp/restore-drill; sudo rm -rf "$d"; sudo mkdir -p "$d/data"; cd "$d"
sudo rclone --config /etc/rclone/r2.conf lsf r2:ngf-backups/riot-proxy/ | tail -1      # the newest backup
sudo rclone --config /etc/rclone/r2.conf copyto r2:ngf-backups/riot-proxy/<newest> data/riot-proxy.db
sudo install -m 0600 /dev/null env && sudo editor env
```

`env` holds the following. Its keys are a **read-only** R2 token for
`riot-proxy-archive` (Object Read on that bucket only), typed in the editor
and never echoed: the drill's `serve` queues `archive:offload` at boot like
any other, and must not be able to write into the replica. The window and
pack size keep that run from finding anything to pack as well.

```ini
ENV=development
AUTH_DISABLED=true
RIOT_API_KEY=RGAPI-restore-drill-not-a-key
HOST=127.0.0.1
PORT=8081
JOB_CONCURRENCY=1
ARCHIVE_OBJECT_URL=s3://riot-proxy-archive
ARCHIVE_OBJECT_ENDPOINT=https://<account id>.r2.cloudflarestorage.com
ARCHIVE_OBJECT_REGION=auto
ARCHIVE_BODY_LOCAL_DAYS=365
ARCHIVE_PACK_TARGET_BYTES=1073741824
ARCHIVE_OBJECT_ACCESS_KEY=…
ARCHIVE_OBJECT_SECRET_KEY=…
```

```bash
drill() { sudo systemd-run --quiet --wait --pipe -p EnvironmentFile="$d/env" \
            /usr/local/bin/riot-proxy --data-dir "$d/data" "$@"; }
```

The fake Riot key means anything not in the archive fails; nothing reaches
Riot with the real key, and the live limiter is untouched.

1. **A body reads from R2.** Use an offloaded match. Before the first bodies
   are freed (14 days after the offload starts), the backup has none, so
   free a packed one in the scratch copy by hand (the row keeps its
   location, so the read must go to the bucket):

   ```bash
   read -r region id < <(sudo sqlite3 -readonly -separator ' ' data/riot-proxy.db \
     "SELECT region, match_id FROM matches WHERE offloaded_at IS NOT NULL LIMIT 1")
   if [ -z "$id" ]; then
     read -r region id < <(sudo sqlite3 -readonly -separator ' ' data/riot-proxy.db \
       "SELECT m.region, m.match_id FROM matches m JOIN body_locations l ON l.match_id = m.match_id AND l.kind = 'match' LIMIT 1")
     sudo sqlite3 data/riot-proxy.db "UPDATE matches SET body_zstd = X'', offloaded_at = 1 WHERE match_id = '$id'"
   fi
   echo "$region $id"
   sudo systemd-run --unit=restore-drill -p EnvironmentFile="$d/env" /usr/local/bin/riot-proxy --data-dir "$d/data" serve
   until curl -sf localhost:8081/healthz >/dev/null; do sleep 2; done
   curl -s -D - -o /dev/null "localhost:8081/v1/lol/matches/$region/$id" | grep -iE '^(HTTP|x-cache)'   # 200, X-Cache: ARCHIVE
   sudo systemctl stop restore-drill
   ```

2. **The map comes back.**

   ```bash
   q="SELECT * FROM body_locations WHERE pack_id <= (SELECT max(pack_id) FROM body_packs) ORDER BY 1, 2"
   sudo sqlite3 -readonly data/riot-proxy.db "$q" | sha256sum > before
   sudo sqlite3 data/riot-proxy.db "DELETE FROM body_locations; DELETE FROM body_packs;"
   drill packs restore-index
   sudo sqlite3 -readonly data/riot-proxy.db "$q" | sha256sum | diff - before && echo "map restored"
   ```

   `before` is taken while the backup's own map is there, so its
   `max(pack_id)` is the backup's newest pack. The restore also brings back
   packs written after the backup; the query leaves them out of the
   comparison.
3. `cd /; sudo rm -rf /var/tmp/restore-drill`, and note the date in PROGRESS.md.

## Reclaiming the file after the offload

Freeing a body empties its `body_zstd` and puts its pages on SQLite's free
list; the file does not shrink. Once `archive_offload_pending{step="free"}`
is 0 for both kinds, copy the database compactly and swap it in, as
design/07 §Reclaiming space describes. On production:

```bash
rp backup /var/lib/riot-proxy/data/riot-proxy.compact.db   # while serving; needs free space ≈ the live data (rp: §Index snapshots and recovery)
sudo systemctl stop riot-proxy
cd /var/lib/riot-proxy/data
sudo -u riot-proxy mv riot-proxy.db riot-proxy.old.db && sudo rm -f riot-proxy.db-wal riot-proxy.db-shm
sudo -u riot-proxy mv riot-proxy.compact.db riot-proxy.db
sudo systemctl start riot-proxy
curl -s localhost:8080/readyz
```

Expect `sqlite_db_bytes` under 8 GB and `sqlite_freelist_bytes` near zero
(OBJ-04's done-when). Keep `riot-proxy.old.db` until the next nightly backup
has run and reached R2, then delete it. Do it in a quiet moment: what is
written between the copy and the stop is lost (the crawl re-fetches it).

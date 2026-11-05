#!/usr/bin/env bash
# Seed staging riot-proxy with production's newest daily backup (STG-04,
# ADR-203). Run on the staging host (ngf-mon-syd1) as root; safe to rerun,
# each run replaces staging's database with a fresh copy.
#
#   sudo ./staging-seed.sh --dry-run     # what it would do, nothing changed
#   sudo ./staging-seed.sh               # asks before stopping staging
#   sudo ./staging-seed.sh --yes         # no question
#
# It refuses before touching anything unless /etc/riot-proxy/env pins
# production's key scope (KEY_SCOPE), any bucket is read-only
# (ARCHIVE_OBJECT_READ_ONLY=true), ENV is not production, the installed
# binary knows those settings, the newest backup is recent, and the disk has
# room. Then it stops the service, replaces the database and staging's own
# backups with the download, scrubs what must not carry over (production's
# consumer keys, its job queue, the response cache, the limiter checkpoint,
# metrics history, tracking flags, running crawls), starts the service and
# waits for /readyz to report the pinned scope.
set -euo pipefail

# Paths and names; the defaults are staging's. Tests point them elsewhere.
env_file=${RP_ENV_FILE:-/etc/riot-proxy/env}
service=${RP_SERVICE:-riot-proxy.service}
binary=${RP_BINARY:-/srv/riot-proxy/current/riot-proxy}
owner=${RP_USER:-riot-proxy}
ready_url=${RP_READY_URL:-http://127.0.0.1:8080/readyz}
ready_wait=${RP_READY_WAIT:-1800}
rclone_config=${SEED_RCLONE_CONFIG:-/etc/riot-proxy/seed-rclone.conf}
remote=${SEED_REMOTE:-r2:ngf-backups/riot-proxy}
max_age_days=${SEED_MAX_AGE_DAYS:-3}
# Room left after the copy for the WAL, the day's writes and a capped crawl.
headroom=${SEED_HEADROOM_BYTES:-$((4 * 1024 * 1024 * 1024))}
lock=${SEED_LOCK:-/run/riot-proxy-seed.lock}
require_root=${SEED_REQUIRE_ROOT:-1}

dry_run=0 yes=0 allow_old=0
for arg in "$@"; do
  case $arg in
    --dry-run) dry_run=1 ;;
    --yes) yes=1 ;;
    --allow-old) allow_old=1 ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
  esac
done

die() { echo "refused: $*" >&2; exit 1; }
say() { echo "== $*"; }
gib() { awk -v b="$1" 'BEGIN { printf "%.1f GiB", b / 1073741824 }'; }

[[ $require_root == 0 || $EUID -eq 0 ]] || die "run as root (sudo)"
for tool in rclone sqlite3 curl systemctl df awk flock; do
  command -v "$tool" >/dev/null || die "$tool is not installed (sqlite3: apt-get install -y sqlite3)"
done
exec 9>"$lock"
flock -n 9 || die "another staging-seed is running"

# --- the environment file: read, never sourced -------------------------------
[[ -r $env_file ]] || die "cannot read $env_file"
setting() {
  local line
  line=$(grep -E "^$1=" "$env_file" | tail -n 1 || true)
  printf '%s' "${line#*=}"
}
key_scope=$(setting KEY_SCOPE)
object_url=$(setting ARCHIVE_OBJECT_URL)
read_only=$(setting ARCHIVE_OBJECT_READ_ONLY)
data_dir=$(setting DATA_DIR)
data_dir=${data_dir:-/var/lib/riot-proxy/data}
backup_keep=$(setting BACKUP_KEEP)
backup_keep=${backup_keep:-14}
[[ $(setting ENV) != production ]] || die "ENV=production in $env_file: this is not a staging host"
[[ $key_scope =~ ^[0-9a-f]{8}$ ]] ||
  die "KEY_SCOPE is not set in $env_file; set it to production's /readyz keyScope, or every production row is invisible"
if [[ -n $object_url && $read_only != true ]]; then
  die "ARCHIVE_OBJECT_URL is set without ARCHIVE_OBJECT_READ_ONLY=true: staging would write production's bucket"
fi
# A binary from before STG-01/02 would ignore both settings.
for name in KEY_SCOPE ARCHIVE_OBJECT_READ_ONLY; do
  grep -qa "$name" "$binary" 2>/dev/null || die "$binary does not know $name; deploy a release that has it first"
done
[[ $(setting LADDER_CRAWL_MATCH_LIMIT) =~ ^[1-9][0-9]*$ ]] ||
  echo "warning: LADDER_CRAWL_MATCH_LIMIT is not set; a crawl started on staging is not capped" >&2

# --- the newest backup -------------------------------------------------------
rc() { rclone --config "$rclone_config" "$@"; }
listing=$(rc lsf --files-only --format "sp" --separator " " "$remote/") || die "cannot list $remote"
newest=$(awk '$2 ~ /^riot-proxy-[0-9]{4}-[0-9]{2}-[0-9]{2}\.db$/ { print $2, $1 }' <<<"$listing" | sort | tail -n 1)
[[ -n $newest ]] || die "no riot-proxy-YYYY-MM-DD.db under $remote"
read -r name size <<<"$newest"
day=${name#riot-proxy-}
day=${day%.db}
age_days=$((($(date -u +%s) - $(date -u -d "$day" +%s)) / 86400))
if ((age_days > max_age_days && !allow_old)); then
  die "the newest backup, $name, is $age_days days old (production's backup may be skipping on disk space); --allow-old to use it anyway"
fi

# --- room --------------------------------------------------------------------
db="$data_dir/riot-proxy.db"
mkdir -p "$data_dir"
reclaim=0
for f in "$db" "$db-wal" "$db-shm" "$data_dir"/backups/riot-proxy-*.db; do
  if [[ -f $f ]]; then reclaim=$((reclaim + $(stat -c %s "$f"))); fi
done
avail=$(df -B1 --output=avail "$data_dir" | tail -n 1 | tr -d ' ')
need=$((size + headroom))
if ((backup_keep > 0)); then
  # Staging's own daily backup needs 1.2 x the database free (maintenance).
  need=$((need + size * 12 / 10))
fi
say "newest backup: $name, $(gib "$size")"
say "disk: $(gib "$avail") free + $(gib "$reclaim") from the database and backups this replaces"
say "needed: $(gib "$need") (the copy, $(gib "$headroom") headroom$( ((backup_keep > 0)) && echo ", 1.2 x for BACKUP_KEEP=$backup_keep"))"
if ((avail + reclaim < need)); then
  die "not enough disk on $data_dir. $( ((backup_keep > 0)) && echo "Set BACKUP_KEEP=0 in $env_file (staging is reseeded, not restored), or")free space on the box, or reclaim production's database first (deploy/README.md, Reclaiming the file)"
fi

if ((dry_run)); then
  say "dry run: would stop $service, replace $db with $name, scrub it, and start $service"
  exit 0
fi
if ((!yes)); then
  read -r -p "Stop $service and replace staging's database with $name? Type yes: " answer
  [[ $answer == yes ]] || die "not confirmed"
fi

# --- replace -----------------------------------------------------------------
trap 'echo "failed: $service may be stopped; rerun this script, or: systemctl start $service" >&2' ERR
say "stopping $service"
systemctl stop "$service"
rm -f "$db" "$db-wal" "$db-shm" "$db.seed" "$data_dir"/backups/riot-proxy-*.db
say "downloading $name"
rc copyto "$remote/$name" "$db.seed"
[[ $(stat -c %s "$db.seed") == "$size" ]] || die "the download is $(stat -c %s "$db.seed") bytes, the backup $size"
[[ $(head -c 15 "$db.seed") == "SQLite format 3" ]] || die "$name is not a SQLite database"

# Production's keys, queue and caches must not carry over; its players stay
# readable but untracked, so no poll asks Riot about a production PUUID.
say "scrubbing"
now_ms=$(($(date -u +%s) * 1000))
sqlite3 -bail "$db.seed" <<SQL
BEGIN;
DELETE FROM consumers;
DELETE FROM jobs;
DELETE FROM cache;
DELETE FROM limiter_state;
DELETE FROM metrics_history;
DELETE FROM rank_lookup_queue;
UPDATE players SET tracked = 0 WHERE tracked = 1;
UPDATE ladder_crawls SET status = 'cancelled', finished_at = $now_ms WHERE status = 'running';
DELETE FROM crawl_legs;
DELETE FROM crawl_match_ids;
COMMIT;
SQL
mv "$db.seed" "$db"
chown "$owner:$owner" "$db"
chmod 0640 "$db"

# --- start -------------------------------------------------------------------
started=$(date -u +%s)
since=$(date -u '+%Y-%m-%d %H:%M:%S UTC')
say "starting $service; the first boot migrates the copy if this release is newer than production's"
systemctl start "$service"
body=
until body=$(curl -fsS --max-time 5 "$ready_url" 2>/dev/null); do
  (($(date -u +%s) - started < ready_wait)) ||
    die "$ready_url not ready after ${ready_wait}s; see journalctl -u $service --since '$since'"
  sleep 5
done
trap - ERR
seen=$(grep -o '"keyScope":"[0-9a-f]*"' <<<"$body" | cut -d'"' -f4 || true)
[[ $seen == "$key_scope" ]] || die "/readyz reports keyScope '$seen', not KEY_SCOPE $key_scope"
say "ready after $(($(date -u +%s) - started))s, keyScope $seen, seeded from $name"
if [[ -z $(setting BOOTSTRAP_ADMIN_KEY) ]]; then
  say "new admin key: journalctl -u $service --since '$since' | grep -A2 'shown once'"
fi

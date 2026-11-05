#!/usr/bin/env bash
# Tests for deploy/staging-seed.sh (STG-04, ADR-203): rclone, systemctl and
# curl are stubs over a temporary directory; sqlite3 is the real one. Each
# refusal leaves staging untouched; a seed replaces and scrubs the database,
# starts the service and checks /readyz's keyScope; a second seed works the
# same. Run by deploy/test.sh (CI's `ops` job).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
seed="$here/staging-seed.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

scope=0123abcd
today=$(date -u +%F)
mkdir -p "$tmp/stubs" "$tmp/remote" "$tmp/data/backups"

# rclone: `lsf` lists $tmp/remote as "size name", `copyto` copies from it.
cat >"$tmp/stubs/rclone" <<EOF
#!/usr/bin/env bash
[[ \$1 == --config ]] && shift 2
case \$1 in
  lsf) for f in "$tmp/remote"/*; do [[ -f \$f ]] && echo "\$(stat -c %s "\$f") \$(basename "\$f")"; done ;;
  copyto) cp "$tmp/remote/\${2##*/}" "\$3" ;;
  *) exit 9 ;;
esac
EOF
# systemctl: logged; `start` makes /readyz answer.
cat >"$tmp/stubs/systemctl" <<EOF
#!/usr/bin/env bash
echo "\$*" >>"$tmp/systemctl.log"
[[ \$1 == start ]] && touch "$tmp/started"
[[ \$1 == stop ]] && rm -f "$tmp/started"
exit 0
EOF
cat >"$tmp/stubs/curl" <<EOF
#!/usr/bin/env bash
[[ -f "$tmp/started" ]] || exit 7
echo "{\"ok\":true,\"keyScope\":\"\$(cat "$tmp/served-scope")\"}"
EOF
chmod +x "$tmp/stubs"/*
echo "$scope" >"$tmp/served-scope"
printf 'riot-proxy binary with KEY_SCOPE and ARCHIVE_OBJECT_READ_ONLY\n' >"$tmp/binary"

# A backup with production's rows, as a VACUUM INTO copy would hold them.
backup() {
  rm -f "$tmp/remote"/*
  sqlite3 "$tmp/remote/riot-proxy-$1.db" <<'SQL'
CREATE TABLE consumers (id TEXT PRIMARY KEY);
CREATE TABLE jobs (id TEXT PRIMARY KEY, state TEXT);
CREATE TABLE cache (key TEXT PRIMARY KEY);
CREATE TABLE limiter_state (scope TEXT);
CREATE TABLE metrics_history (at INTEGER);
CREATE TABLE rank_lookup_queue (puuid TEXT);
CREATE TABLE players (key_scope TEXT, puuid TEXT, tracked INTEGER);
CREATE TABLE ladder_crawls (id TEXT, status TEXT, finished_at INTEGER);
CREATE TABLE crawl_legs (crawl_id TEXT);
CREATE TABLE crawl_match_ids (crawl_id TEXT);
CREATE TABLE matches (match_id TEXT PRIMARY KEY);
INSERT INTO consumers VALUES ('prod-site'), ('prod-admin');
INSERT INTO jobs VALUES ('j1', 'pending'), ('j2', 'done');
INSERT INTO cache VALUES ('0123abcd:account.byRiotId:x');
INSERT INTO limiter_state VALUES ('app');
INSERT INTO metrics_history VALUES (1);
INSERT INTO rank_lookup_queue VALUES ('p1');
INSERT INTO players VALUES ('0123abcd', 'p1', 1), ('0123abcd', 'p2', 0);
INSERT INTO ladder_crawls VALUES ('c1', 'running', NULL), ('c0', 'completed', 5);
INSERT INTO crawl_legs VALUES ('c1');
INSERT INTO crawl_match_ids VALUES ('c1');
INSERT INTO matches VALUES ('KR_1'), ('KR_2');
SQL
}

env_file() {
  cat >"$tmp/env" <<EOF
ENV=development
RIOT_API_KEY=RGAPI-not-a-key
DATA_DIR=$tmp/data
KEY_SCOPE=$scope
ARCHIVE_OBJECT_URL=s3://riot-proxy-archive
ARCHIVE_OBJECT_READ_ONLY=true
BACKUP_KEEP=0
LADDER_CRAWL_MATCH_LIMIT=10000
EOF
}

run() {
  PATH="$tmp/stubs:$PATH" RP_ENV_FILE="$tmp/env" RP_BINARY="$tmp/binary" RP_USER="$(id -un)" \
    RP_READY_URL=http://stub/readyz RP_READY_WAIT=20 SEED_LOCK="$tmp/lock" SEED_REQUIRE_ROOT=0 \
    SEED_RCLONE_CONFIG=/dev/null SEED_HEADROOM_BYTES="${SEED_HEADROOM_BYTES:-1024}" "$seed" "$@" >"$tmp/out" 2>&1
}
q() { sqlite3 "$tmp/data/riot-proxy.db" "$1"; }

# Staging's own database and a backup of it, which a seed replaces.
fresh_staging() {
  rm -f "$tmp/data"/riot-proxy.db* "$tmp/systemctl.log" "$tmp/started"
  sqlite3 "$tmp/data/riot-proxy.db" "CREATE TABLE staging_only (x); INSERT INTO staging_only VALUES (1);"
  echo old >"$tmp/data/backups/riot-proxy-2026-01-01.db"
}

untouched() {
  [[ ! -f $tmp/systemctl.log ]] && [[ $(q "SELECT count(*) FROM staging_only") == 1 ]]
}

refused() {
  local name=$1 expect=$2; shift 2
  fresh_staging
  if run "$@"; then
    fail "$name: exited 0"
  elif ! grep -q "$expect" "$tmp/out"; then
    fail "$name: no '$expect' in: $(cat "$tmp/out")"
  elif ! untouched; then
    fail "$name: staging was touched"
  else
    ok "$name"
  fi
}

backup "$today"

env_file
sed -i '/^KEY_SCOPE=/d' "$tmp/env"
refused "no KEY_SCOPE" "KEY_SCOPE is not set" --yes

env_file
sed -i 's/^KEY_SCOPE=.*/KEY_SCOPE=0123ABCD/' "$tmp/env"
refused "a KEY_SCOPE that is not 8 lowercase hex" "KEY_SCOPE is not set" --yes

env_file
sed -i '/^ARCHIVE_OBJECT_READ_ONLY=/d' "$tmp/env"
refused "a writable bucket" "without ARCHIVE_OBJECT_READ_ONLY=true" --yes

env_file
sed -i 's/^ENV=.*/ENV=production/' "$tmp/env"
refused "a production env file" "ENV=production" --yes

env_file
printf 'an older binary\n' >"$tmp/binary"
refused "a binary without the settings" "does not know KEY_SCOPE" --yes
printf 'riot-proxy binary with KEY_SCOPE and ARCHIVE_OBJECT_READ_ONLY\n' >"$tmp/binary"

backup 2026-01-02
refused "an old backup" "days old" --yes
backup "$today"

fresh_staging
if SEED_HEADROOM_BYTES=$((1 << 60)) run --yes; then fail "no room: exited 0"
elif grep -q "not enough disk" "$tmp/out" && untouched; then ok "no room"
else fail "no room: $(cat "$tmp/out")"; fi

env_file
sed -i 's/^BACKUP_KEEP=.*/BACKUP_KEEP=1/' "$tmp/env"
fresh_staging
run --dry-run || true
if grep -q "1.2 x for BACKUP_KEEP=1" "$tmp/out"; then ok "BACKUP_KEEP counts a backup's room"; else fail "BACKUP_KEEP: $(cat "$tmp/out")"; fi
env_file

fresh_staging
if run --dry-run && grep -q "dry run: would stop" "$tmp/out" && untouched; then ok "a dry run changes nothing"
else fail "dry run: $(cat "$tmp/out")"; fi

fresh_staging
if run </dev/null; then fail "unconfirmed: exited 0"
elif untouched; then ok "nothing happens without yes"
else fail "unconfirmed: staging was touched"; fi

seeded() {
  local name=$1
  if ! run --yes; then fail "$name: $(cat "$tmp/out")"; return; fi
  local got
  got=$(q "SELECT (SELECT count(*) FROM consumers), (SELECT count(*) FROM jobs), (SELECT count(*) FROM cache),
             (SELECT count(*) FROM limiter_state), (SELECT count(*) FROM metrics_history),
             (SELECT count(*) FROM rank_lookup_queue), (SELECT count(*) FROM players WHERE tracked = 1),
             (SELECT count(*) FROM players), (SELECT status FROM ladder_crawls WHERE id = 'c1'),
             (SELECT count(*) FROM crawl_legs) + (SELECT count(*) FROM crawl_match_ids),
             (SELECT count(*) FROM matches)")
  if [[ $got != "0|0|0|0|0|0|0|2|cancelled|0|2" ]]; then fail "$name: scrubbed rows $got"
  elif q "SELECT 1 FROM staging_only" >/dev/null 2>&1; then fail "$name: the old database is still there"
  elif [[ -e $tmp/data/backups/riot-proxy-2026-01-01.db || -e $tmp/data/riot-proxy.db.seed ]]; then fail "$name: old backup or partial download left"
  elif [[ $(tr '\n' ' ' <"$tmp/systemctl.log") != "stop riot-proxy.service start riot-proxy.service " ]]; then fail "$name: systemctl $(cat "$tmp/systemctl.log")"
  elif ! grep -q "keyScope $scope, seeded from riot-proxy-$today.db" "$tmp/out"; then fail "$name: $(cat "$tmp/out")"
  elif ! grep -q "new admin key: journalctl" "$tmp/out"; then fail "$name: no admin key hint"
  else ok "$name"; fi
}
fresh_staging
seeded "a seed replaces, scrubs and starts"
rm -f "$tmp/systemctl.log"
echo old >"$tmp/data/backups/riot-proxy-2026-01-01.db"
sqlite3 "$tmp/data/riot-proxy.db" "CREATE TABLE staging_only (x);"
seeded "a second seed does the same"

fresh_staging
echo ffffffff >"$tmp/served-scope"
if run --yes; then fail "scope mismatch: exited 0"
elif grep -q "not KEY_SCOPE $scope" "$tmp/out"; then ok "a served scope other than KEY_SCOPE fails"
else fail "scope mismatch: $(cat "$tmp/out")"; fi
echo "$scope" >"$tmp/served-scope"

((fails == 0)) || { echo "$fails failed" >&2; exit 1; }
echo "staging-seed: all passed"

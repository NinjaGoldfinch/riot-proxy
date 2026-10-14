#!/usr/bin/env bash
# Tests for the archive replica units (OBJ-04): the unit files parse, and the
# unit's own rclone command, pointed at two local directories, does what an
# hourly sync must (a --dry-run changes nothing; a real run mirrors the source,
# deletions included). Run by CI's `ops` job; locally: deploy/test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
check() { local name=$1; shift; if "$@"; then ok "$name"; else fail "$name"; fi; }

unit="$here/riot-proxy-replica.service"
timer="$here/riot-proxy-replica.timer"

check "shellcheck" shellcheck "$here/test.sh"

rclone=$(command -v rclone || true)
if [[ -z $rclone ]]; then
  echo "FAIL rclone is not installed" >&2
  exit 1
fi

# --- the units parse ---------------------------------------------------------
if command -v systemd-analyze >/dev/null; then
  if [[ -x /usr/bin/rclone ]]; then
    check "systemd-analyze verify" systemd-analyze verify "$unit" "$timer"
  else
    # verify insists the ExecStart binary exists; point it at this rclone.
    mkdir "$tmp/units"
    sed "s|^ExecStart=/usr/bin/rclone |ExecStart=$rclone |" "$unit" > "$tmp/units/riot-proxy-replica.service"
    cp "$timer" "$tmp/units/"
    check "systemd-analyze verify (rclone at $rclone)" \
      systemd-analyze verify "$tmp/units/riot-proxy-replica.service" "$tmp/units/riot-proxy-replica.timer"
  fi
else
  echo "skip systemd-analyze verify (systemd-analyze not installed)"
fi
check "the timer runs hourly" grep -qx 'OnCalendar=\*-\*-\* \*:17:00' "$timer"
check "the timer catches up after downtime" grep -qx 'Persistent=true' "$timer"
check "the sync runs from Onidel to R2" bash -c "grep -qx 'Environment=REPLICA_SOURCE=onidel:riot-proxy-archive' '$unit' && grep -qx 'Environment=REPLICA_DEST=r2:riot-proxy-archive' '$unit'"
check "the sync carries object metadata" grep -q '^ExecStart=.* --metadata ' "$unit"
check "rclone.conf comes in as a credential" grep -qx 'LoadCredential=rclone.conf:/etc/riot-proxy/rclone.conf' "$unit"

# --- the unit's rclone command against two local directories ----------------
exec_start=$(sed -n 's/^ExecStart=//p' "$unit")
src="$tmp/onidel"
dst="$tmp/r2"
mkdir -p "$tmp/creds" "$src/packs/matches/2026/10" "$src/index" "$dst/packs/matches/2026/10" "$dst/index"
: > "$tmp/creds/rclone.conf"
printf 'pack one' > "$src/packs/matches/2026/10/01JA.pack"
printf 'pack two' > "$src/packs/matches/2026/10/01JB.pack"
printf 'snapshot' > "$src/index/packs-2026-10-14.jsonl.zst"
printf 'pack one' > "$dst/packs/matches/2026/10/01JA.pack"
printf 'expired' > "$dst/index/packs-2026-09-01.jsonl.zst"

# shellcheck disable=SC2016 # the unit's literal ${…}, replaced here
line=${exec_start//'${REPLICA_SOURCE}'/$src}
# shellcheck disable=SC2016
line=${line//'${REPLICA_DEST}'/$dst}
line=${line//'%d'/$tmp/creds}
line=${line/#\/usr\/bin\/rclone /$rclone }
read -ra cmd <<< "$line"

tree() { (cd "$1" && find . -type f -print0 | sort -z | xargs -0 sha256sum); }
before=$(tree "$dst")
"${cmd[@]}" --dry-run > "$tmp/dry.log" 2>&1 || { cat "$tmp/dry.log" >&2; fail "the dry run exits 0"; }
check "the dry run changes nothing" test "$before" = "$(tree "$dst")"
check "the dry run would copy the new pack and snapshot" bash -c \
  "grep -q '01JB.pack: Skipped copy' '$tmp/dry.log' && grep -q 'packs-2026-10-14.jsonl.zst: Skipped copy' '$tmp/dry.log'"
check "the dry run would delete what the source no longer has" grep -q 'packs-2026-09-01.jsonl.zst: Skipped delete' "$tmp/dry.log"
check "the dry run leaves the pack both have alone" bash -c "! grep -q '01JA.pack: Skipped' '$tmp/dry.log'"

"${cmd[@]}" > "$tmp/run.log" 2>&1 || { cat "$tmp/run.log" >&2; fail "the sync exits 0"; }
check "a real run mirrors the source" diff -r "$src" "$dst"

if ((fails)); then
  echo "$fails failed" >&2
  exit 1
fi
echo "all passed"

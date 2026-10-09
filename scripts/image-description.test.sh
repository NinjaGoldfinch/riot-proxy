#!/usr/bin/env bash
# Tests for scripts/image-description.sh (OPS-05) against a throwaway git repo,
# and for scripts/check-image-description.sh against hand-made OCI tarballs.
# Run by CI's `ops` job; locally: scripts/image-description.test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
desc="$here/image-description.sh"
checker="$here/check-image-description.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
same() { if [ "$2" = "$3" ]; then ok "$1"; else fail "$1"; printf '  want: %s\n  got:  %s\n' "$3" "$2" >&2; fi; }

check_shellcheck() { shellcheck "$desc" "$checker" "$here/image-description.test.sh"; }
if check_shellcheck; then ok "shellcheck"; else fail "shellcheck"; fi

cd "$tmp"
git init -q -b main
git config user.email t@example.invalid
git config user.name t
git config commit.gpgSign false
git config tag.gpgSign false
commit() { git commit -q --allow-empty -m "$1"; }

commit "first (#1)"
git tag -m "v1.0.0" v1.0.0
commit "SITE-01 — report when each part was last read (#2)"
commit "DEV-27 — the writer keeps stats fresh (#3)"
git tag -m "v1.1.0" v1.1.0
base=$(git rev-parse --short HEAD~1)

same "a tag names the range and lists newest first" \
  "$("$desc" "riot-proxy 1.1.0" v1.1.0 v1.0.0)" \
  "riot-proxy 1.1.0. Since v1.0.0: DEV-27 — the writer keeps stats fresh (#3); SITE-01 — report when each part was last read (#2)"

same "a commit names the range by its short sha" \
  "$("$desc" "edge build" HEAD "$(git rev-parse HEAD~1)")" \
  "edge build. Since $base: DEV-27 — the writer keeps stats fresh (#3)"

same "no previous image lists only the head commit" \
  "$("$desc" "edge build" HEAD)" \
  "edge build: DEV-27 — the writer keeps stats fresh (#3)"

same "an unknown previous commit lists only the head commit" \
  "$("$desc" "edge build" HEAD 0123456789abcdef0123456789abcdef01234567)" \
  "edge build: DEV-27 — the writer keeps stats fresh (#3)"

git checkout -q -b side v1.0.0
commit "abandoned (#9)"
side=$(git rev-parse HEAD)
git checkout -q main
same "a previous commit that is not an ancestor lists only the head commit" \
  "$("$desc" "edge build" HEAD "$side")" \
  "edge build: DEV-27 — the writer keeps stats fresh (#3)"

same "the same commit says there are no changes" \
  "$("$desc" "edge build" HEAD HEAD)" \
  "edge build. Since $(git rev-parse --short HEAD): no changes"

for i in $(seq 1 40); do commit "TASK-$i — a change with a reasonably long subject line (#$((i + 10)))"; done
long=$("$desc" "riot-proxy 1.2.0" HEAD v1.1.0)
bytes=$(printf '%s' "$long" | LC_ALL=C wc -c)
if [ "$bytes" -le 512 ]; then ok "a long list fits in 512 bytes ($bytes)"; else fail "a long list fits in 512 bytes ($bytes)"; fi
case "$long" in
  "riot-proxy 1.2.0. Since v1.1.0: TASK-40 — "*) ok "a long list starts with the newest commit" ;;
  *) fail "a long list starts with the newest commit: $long" ;;
esac
shown=$(printf '%s' "$long" | grep -o 'TASK-[0-9]* —' | wc -l)
left=$(printf '%s' "$long" | sed -n 's/.*; and \([0-9]*\) more$/\1/p')
same "a long list counts every commit it leaves out" "$((shown + left))" 40

# --- check-image-description.sh --------------------------------------------
# An OCI layout tarball whose index.json points at one blob: <name> <blob json>.
oci() {
  local dir="$tmp/oci-$1" digest
  mkdir -p "$dir/blobs/sha256"
  printf '%s' "$2" > "$dir/blob"
  digest=$(sha256sum "$dir/blob" | cut -d' ' -f1)
  mv "$dir/blob" "$dir/blobs/sha256/$digest"
  printf '{"schemaVersion":2,"manifests":[{"digest":"sha256:%s"}]}' "$digest" > "$dir/index.json"
  tar -cf "$tmp/$1.tar" -C "$dir" index.json blobs
}
index_type=application/vnd.oci.image.index.v1+json
oci good "{\"mediaType\":\"$index_type\",\"annotations\":{\"org.opencontainers.image.description\":\"edge: a — b (#1)\"}}"
oci none "{\"mediaType\":\"$index_type\",\"annotations\":{}}"
oci single '{"mediaType":"application/vnd.oci.image.manifest.v1+json","annotations":{"org.opencontainers.image.description":"edge: a — b (#1)"}}'

if "$checker" "$tmp/good.tar" "edge: a — b (#1)" >/dev/null; then ok "check: the index's description passes"; else fail "check: the index's description passes"; fi
if ! "$checker" "$tmp/good.tar" "edge: something else" 2>/dev/null; then ok "check: a different description fails"; else fail "check: a different description fails"; fi
if ! "$checker" "$tmp/none.tar" "edge: a — b (#1)" 2>/dev/null; then ok "check: an index without one fails"; else fail "check: an index without one fails"; fi
if ! "$checker" "$tmp/single.tar" "edge: a — b (#1)" 2>/dev/null; then ok "check: a lone manifest fails"; else fail "check: a lone manifest fails"; fi

if [ "$fails" -gt 0 ]; then echo "$fails failed" >&2; exit 1; fi
echo "all passed"

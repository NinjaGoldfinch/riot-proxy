#!/usr/bin/env bash
# Tests for scripts/edge-tags.sh (INC-02) against a throwaway clone of a
# throwaway bare `origin`, so `git fetch origin main` works offline.
# Run by CI's `ops` job; locally: scripts/edge-tags.test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tags="$here/edge-tags.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
same() { if [ "$2" = "$3" ]; then ok "$1"; else fail "$1"; printf '  want: %s\n  got:  %s\n' "$3" "$2" >&2; fi; }

if shellcheck "$tags" "$here/edge-tags.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi

git init -q --bare -b main "$tmp/origin.git"
git clone -q "$tmp/origin.git" "$tmp/work" 2>/dev/null
cd "$tmp/work"
git config user.email t@example.invalid
git config user.name t
git config commit.gpgSign false
# A commit on main, pushed the way a merged PR lands.
land() { git commit -q --allow-empty -m "$1" && git push -q origin HEAD:main && git rev-parse HEAD; }

a=$(land "SITE-04 — first (#132)")
b=$(land "SITE-05 — second (#133)")

# stdout of edge-tags.sh, with stderr in $tmp/log
run() { "$tags" "$1" 2>"$tmp/log"; }

same "the head of main gets edge and sha-…" "$(run "$b")" "sha=sha-${b::7}
edge=true"
same "the head of main says so" "$(cat "$tmp/log")" \
  "edge: ${b::7} is the head of main; pushing :edge and :sha-${b::7}"

same "an older commit gets sha-… only" "$(run "$a")" "sha=sha-${a::7}
edge=false"
same "an older commit says :edge stays" "$(cat "$tmp/log")" \
  "edge: ${a::7} is no longer the head of main (${b::7}); pushing :sha-${a::7} only, :edge stays"

same "a short or symbolic name is resolved first" "$(run "${b::10}")" "sha=sha-${b::7}
edge=true"

# main moves on in another clone while this run builds: the check fetches again.
git clone -q "$tmp/origin.git" "$tmp/other" 2>/dev/null
(
  cd "$tmp/other"
  git -c user.email=t@example.invalid -c user.name=t -c commit.gpgSign=false \
    commit -q --allow-empty -m "DEV-29 — third (#134)"
  git push -q origin HEAD:main
)
c=$(git -C "$tmp/other" rev-parse HEAD)
same "a commit that was the head when the run started, but isn't now, gets sha-… only" \
  "$(run "$b")" "sha=sha-${b::7}
edge=false"
same "it names the new head" "$(cat "$tmp/log")" \
  "edge: ${b::7} is no longer the head of main (${c::7}); pushing :sha-${b::7} only, :edge stays"

# A pull request's merge commit is never on main.
git checkout -q -b pr origin/main
git commit -q --allow-empty -m "Merge pull request into main"
pr=$(git rev-parse HEAD)
same "a commit that isn't on main gets sha-… only" "$(run "$pr")" "sha=sha-${pr::7}
edge=false"

if run 0123456789abcdef0123456789abcdef01234567 >/dev/null; then
  fail "an unknown commit fails"
else
  ok "an unknown commit fails"
fi

git remote set-url origin "$tmp/missing.git"
if out=$(run "$c" 2>/dev/null); then
  fail "a main that can't be fetched fails"
else
  same "a main that can't be fetched fails and picks nothing" "$out" ""
fi
git remote set-url origin "$tmp/origin.git"

# --- accept: a late run for an older commit leaves :edge on the newer image ---
# A registry in a file: one "<tag> <commit>" line per tag; a push moves the
# tags edge-tags.sh picks, as metadata-action + build-push-action do.
registry="$tmp/registry"
: > "$registry"
publish() {
  local out t
  local -a push
  out=$(run "$1")
  push=("$(sed -n 's/^sha=//p' <<<"$out")")
  if [ "$(sed -n 's/^edge=//p' <<<"$out")" = true ]; then push+=(edge); fi
  for t in "${push[@]}"; do
    grep -v "^$t " "$registry" > "$registry.new" || true
    echo "$t $1" >> "$registry.new"
    mv "$registry.new" "$registry"
  done
}
tag_of() { sed -n "s/^$1 //p" "$registry"; }

publish "$c"           # the newest commit's run publishes first
publish "$a"           # then GitHub starts the run for an older commit late
publish "$b"
same "a late run leaves :edge on the newer image" "$(tag_of edge)" "$c"
same "the late runs still push their sha- tags" "$(tag_of "sha-${a::7}") $(tag_of "sha-${b::7}")" "$a $b"

if [ "$fails" -gt 0 ]; then echo "$fails failed" >&2; exit 1; fi
echo "all passed"

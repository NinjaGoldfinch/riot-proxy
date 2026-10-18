#!/usr/bin/env bash
# Tests for scripts/docker-paths.sh (CI-01).
# Run by CI's `ops` job; locally: scripts/docker-paths.test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
paths="$here/docker-paths.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
same() { if [ "$2" = "$3" ]; then ok "$1"; else fail "$1"; printf '  want: %s\n  got:  %s\n' "$3" "$2" >&2; fi; }

if shellcheck "$paths" "$here/docker-paths.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi

# stdout of docker-paths.sh for the given lines, with stderr in $tmp/log
run() { printf '%s' "$1" | "$paths" 2>"$tmp/log"; }

for f in Dockerfile .dockerignore docker-compose.yml Cargo.toml Cargo.lock build.rs \
  rust-toolchain.toml .github/workflows/ci.yml scripts/docker-paths.sh; do
  same "$f builds" "$(run "src/lib.rs
$f
")" "build=true"
done
same "the reason names the file" "$(cat "$tmp/log")" \
  "docker: scripts/docker-paths.sh changed; building the source Dockerfile"

same "source and docs only skip" "$(run "src/lib.rs
src/routes/analytics.rs
docs/DECISIONS.md
tests/analytics.rs
")" "build=false"
same "the skip says why" "$(cat "$tmp/log")" \
  "docker: nothing the source Dockerfile depends on changed; build-musl covers the source"
same "no changes skip" "$(run "")" "build=false"
same "a last line without a newline counts" "$(run "src/lib.rs
Cargo.lock")" "build=true"
same "a path is matched whole, not by suffix" "$(run "deploy/Dockerfile
docs/Cargo.toml
")" "build=false"
same "another workflow skips" "$(run ".github/workflows/edge.yml
")" "build=false"

if [ "$fails" -gt 0 ]; then
  echo "$fails failed" >&2
  exit 1
fi
echo "all passed"

#!/usr/bin/env bash
# CI-01 (ADR-173). Says whether a pull request can change what the source
# `Dockerfile` builds, so CI's `docker` job skips its 6–8 minute build when
# it can't. Reads the changed paths on stdin, one per line:
#
#   git diff --name-only HEAD^1 HEAD | scripts/docker-paths.sh
#
# and prints a GITHUB_OUTPUT line on stdout, `build=true` or `build=false`,
# with the reason on stderr. Source edits don't count: `build-musl` compiles
# the same code with the same release profile on every pull request, and every
# push to `main` runs the full `docker` job.
set -euo pipefail

# What the Dockerfile reads beyond the source tree, or what decides how it builds.
paths=(
  Dockerfile
  .dockerignore
  docker-compose.yml
  Cargo.toml
  Cargo.lock
  build.rs
  rust-toolchain.toml
  .github/workflows/ci.yml
  scripts/docker-paths.sh
)

while IFS= read -r file || [ -n "$file" ]; do
  for p in "${paths[@]}"; do
    if [ "$file" = "$p" ]; then
      echo "build=true"
      echo "docker: $file changed; building the source Dockerfile" >&2
      exit 0
    fi
  done
done

echo "build=false"
echo "docker: nothing the source Dockerfile depends on changed; build-musl covers the source" >&2

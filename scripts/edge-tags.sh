#!/usr/bin/env bash
# INC-02 (ADR-114). Picks the tags the `edge` workflow pushes for a commit:
# `sha-<short>` always, and `edge` only while the commit is still the head of
# `main`. A run GitHub starts late, after newer commits have published, then
# leaves `:edge` on the newer image instead of moving it back.
#
#   scripts/edge-tags.sh <commit>
#
# Fetches `main` from `origin` first, so call it right before the push. Prints
# GITHUB_OUTPUT lines on stdout:
#
#   sha=sha-<first 7 hex digits>    (what metadata-action's type=sha makes)
#   edge=true|false
#
# and one line on stderr saying which it chose and why. Exits non-zero if
# `main` can't be fetched: a guess either way could move `:edge` backwards or
# freeze it.
set -euo pipefail

commit=${1:?usage: edge-tags.sh <commit>}

full=$(git rev-parse --verify -q "$commit^{commit}") || {
  echo "edge: $commit is not a commit" >&2
  exit 1
}
short=${full::7}

if ! git fetch -q --no-tags origin +refs/heads/main:refs/remotes/origin/main; then
  echo "edge: could not fetch main from origin" >&2
  exit 1
fi
head=$(git rev-parse --verify refs/remotes/origin/main)

echo "sha=sha-$short"
if [ "$full" = "$head" ]; then
  echo "edge=true"
  echo "edge: $short is the head of main; pushing :edge and :sha-$short" >&2
else
  echo "edge=false"
  echo "edge: $short is no longer the head of main (${head::7}); pushing :sha-$short only, :edge stays" >&2
fi

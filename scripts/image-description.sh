#!/usr/bin/env bash
# OPS-05 (ADR-107). Prints the one-line description GHCR shows on an image's
# package page: what the image is, then the commits it adds over the image
# before it, newest first. edge.yml and release.yml put it in the image's
# `org.opencontainers.image.description` label and annotations.
#
#   scripts/image-description.sh <title> <to> [<from>]
#
# <from> is the previous image's commit or tag. When it is empty, unknown, or
# not an ancestor of <to>, only <to>'s own subject is listed. GHCR keeps 512
# characters, so the list stops early and says how many commits it left out.
set -euo pipefail

title=${1:?usage: image-description.sh <title> <to> [<from>]}
to=${2:?usage: image-description.sh <title> <to> [<from>]}
from=${3:-}
max=512

if [ -n "$from" ] && git rev-parse -q --verify "$from^{commit}" >/dev/null \
    && git merge-base --is-ancestor "$from" "$to"; then
  if git show-ref -q --verify "refs/tags/$from"; then since=$from; else since=$(git rev-parse --short "$from"); fi
  mapfile -t subjects < <(git log --format=%s "$from..$to")
  out="$title. Since $since:"
else
  mapfile -t subjects < <(git log -1 --format=%s "$to")
  out="$title:"
fi

if [ ${#subjects[@]} -eq 0 ]; then
  printf '%s no changes\n' "$out"
  exit 0
fi

# Lengths are in bytes (LC_ALL=C), so a subject's em dash can't push it past the limit.
export LC_ALL=C
sep=" "
for i in "${!subjects[@]}"; do
  left=$((${#subjects[@]} - i - 1))
  more=""
  [ "$left" -gt 0 ] && more="; and $left more"
  if [ $((${#out} + ${#sep} + ${#subjects[$i]} + ${#more})) -gt "$max" ]; then
    printf '%s%sand %d more\n' "$out" "$sep" $((${#subjects[@]} - i))
    exit 0
  fi
  out="$out$sep${subjects[$i]}"
  sep="; "
done
printf '%s\n' "$out"

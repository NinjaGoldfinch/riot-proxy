#!/usr/bin/env bash
# OPS-05 (ADR-106). Fails unless an image's index carries the description GHCR
# shows, as an `org.opencontainers.image.description` annotation.
#
#   scripts/check-image-description.sh <image-ref | oci-tarball> <want>
#
# A pushed image is read from the registry; a pull request's build, from the
# OCI tarball build-push-action wrote.
set -euo pipefail

src=${1:?usage: check-image-description.sh <image-ref | oci-tarball> <want>}
want=${2:?usage: check-image-description.sh <image-ref | oci-tarball> <want>}

if [ -f "$src" ]; then
  digest=$(tar -xOf "$src" index.json | jq -r '.manifests[0].digest')
  index=$(tar -xOf "$src" "blobs/${digest/://}")
else
  index=$(docker buildx imagetools inspect --raw "$src")
fi

kind=$(jq -r '.mediaType' <<<"$index")
if [ "$kind" != application/vnd.oci.image.index.v1+json ]; then
  echo "::error::expected an OCI image index, got $kind" >&2
  exit 1
fi
got=$(jq -r '.annotations["org.opencontainers.image.description"] // empty' <<<"$index")
if [ "$got" != "$want" ]; then
  printf '::error::the index description is wrong\n  want: %s\n  got:  %s\n' "$want" "$got" >&2
  exit 1
fi
echo "index description: $got"

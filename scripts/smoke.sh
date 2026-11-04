#!/usr/bin/env bash
# Smoke test of a running riot-proxy (DEP-05, ADR-198): the release is up, is the
# version asked for, and answers through its own layers, with no call to Riot.
# Staging runs on a development key that expires every 24 hours; a test that
# needed Riot would fail every staging deploy once it lapsed, and the gate would
# then hold every version back from production. So it checks only what the
# process answers by itself:
#
#   /healthz        200 {"ok":true}
#   /readyz         200, "ok":true and the version
#   /openapi.json   200 and the version (the spec the binary serves)
#   /metrics        200 and Prometheus text
#   /v1/lol/status  401 UNAUTHORIZED with an X-Request-Id: the router, the
#                   auth layer and the error shape answer, before any Riot call
#
# deploy.yml runs it against staging after each staging deploy, and its result is
# the commit status `staging/smoke` that production requires. release.yml's
# `image` job runs it against the release image. scripts/smoke.test.sh tests it.
#
#   scripts/smoke.sh <base-url> <version>
#
# With CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET set, every request sends
# them as a Cloudflare Access service token, read from a file so they never
# appear in a command line.
set -euo pipefail

base=${1:-}
version=${2:-}
if [[ -z $base || -z $version ]]; then
  echo "usage: scripts/smoke.sh <base-url> <version>" >&2
  exit 2
fi
base=${base%/}
version=${version#v}
# Tries per check, and the pause between them (seconds).
tries=${SMOKE_TRIES:-3}
pause=${SMOKE_PAUSE:-3}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "smoke: FAIL $*" >&2; exit 1; }

curl_args=(-sS --max-time 20)
if [[ -n ${CF_ACCESS_CLIENT_ID:-} || -n ${CF_ACCESS_CLIENT_SECRET:-} ]]; then
  [[ -n ${CF_ACCESS_CLIENT_ID:-} && -n ${CF_ACCESS_CLIENT_SECRET:-} ]] ||
    fail "set both CF_ACCESS_CLIENT_ID and CF_ACCESS_CLIENT_SECRET, or neither"
  (
    umask 077
    printf 'CF-Access-Client-Id: %s\nCF-Access-Client-Secret: %s\n' \
      "$CF_ACCESS_CLIENT_ID" "$CF_ACCESS_CLIENT_SECRET" > "$tmp/access"
  )
  curl_args+=(-H "@$tmp/access")
fi

# get <path> <want-status>: prints the status. The body goes to $tmp/body, the
# headers to $tmp/headers. Tried up to $tries times until the status is the one wanted.
get() {
  local status='' attempt
  for ((attempt = 1; attempt <= tries; attempt++)); do
    status=$(curl "${curl_args[@]}" -o "$tmp/body" -D "$tmp/headers" -w '%{http_code}' \
      "$base$1" 2>"$tmp/error") || status=000
    [[ $status == "$2" ]] && break
    ((attempt == tries)) || sleep "$pause"
  done
  echo "$status"
}

# expect <path> <want-status>: fails with what came back instead.
expect() {
  local status
  status=$(get "$1" "$2")
  [[ $status == "$2" ]] && return
  case $status in
  3* | 403) fail "$1 answered $status: Cloudflare Access refused the request; check the service token" ;;
  000) fail "$1 did not answer: $(cat "$tmp/error")" ;;
  *) fail "$1 answered $status, not $2: $(head -c 300 "$tmp/body")" ;;
  esac
}

body_has() { grep -qE -- "$2" "$tmp/body" || fail "$1: the body has no $2: $(head -c 300 "$tmp/body")"; }

expect /healthz 200
[[ $(cat "$tmp/body") == '{"ok":true}' ]] || fail "/healthz answered $(cat "$tmp/body")"
echo "smoke: ok /healthz"

expect /readyz 200
body_has /readyz '"ok":true'
grep -qF "\"version\":\"$version\"" "$tmp/body" ||
  fail "/readyz reports $(grep -oE '"version":"[^"]*"' "$tmp/body" || echo 'no version'), not $version"
echo "smoke: ok /readyz $version"

expect /openapi.json 200
grep -qE "\"version\": *\"${version//./\\.}\"" "$tmp/body" || fail "/openapi.json does not describe $version"
echo "smoke: ok /openapi.json $version"

expect /metrics 200
body_has /metrics '^# TYPE '
echo "smoke: ok /metrics"

# No client key: the auth layer refuses it, so Riot is never asked.
expect /v1/lol/status/oc1 401
body_has /v1/lol/status/oc1 '"code":"UNAUTHORIZED"'
grep -qi '^x-request-id: ' "$tmp/headers" || fail "/v1/lol/status/oc1 has no X-Request-Id"
echo "smoke: ok /v1/lol/status/oc1 refuses a request without a key"

echo "smoke: $base passed at $version"

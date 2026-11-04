#!/usr/bin/env bash
# Tests for scripts/smoke.sh (DEP-05, ADR-198) against a stub server that answers
# as riot-proxy does, or as one way it can be wrong. release.yml's `image` job
# runs smoke.sh against the real binary. Run by CI's `ops` job; needs python3.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
smoke="$here/scripts/smoke.sh"
tmp=$(mktemp -d)
pid=''
trap '[[ -n $pid ]] && kill "$pid" 2>/dev/null; rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

cat > "$tmp/stub.py" <<'PY'
import http.server, json, sys
mode, counter = sys.argv[1], {"readyz": 0}
pretty = json.dumps({"openapi": "3.1.0", "info": {"title": "riot-proxy", "version": "2.10.0"}}, indent=2)

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, status, body, ctype="application/json", headers=()):
        self.send_response(status)
        self.send_header("content-type", ctype)
        if mode != "no-request-id":
            self.send_header("x-request-id", "01TEST")
        for k, v in headers: self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body.encode())
    def do_GET(self):
        p = self.path
        if mode == "access-redirect":
            return self.send(302, "", headers=[("location", "https://x.cloudflareaccess.com/")])
        if mode == "access-forbidden":
            return self.send(403, "Forbidden", "text/plain")
        if mode == "needs-token" and (self.headers.get("CF-Access-Client-Id") != "id"
                                      or self.headers.get("CF-Access-Client-Secret") != "secret"):
            return self.send(302, "", headers=[("location", "https://x.cloudflareaccess.com/")])
        if p == "/healthz":
            return self.send(200, '{"ok":true}')
        if p == "/readyz":
            counter["readyz"] += 1
            if mode == "slow-ready" and counter["readyz"] < 2 or mode == "never-ready":
                return self.send(503, '{"ok":false,"sqlite":false,"limiter":true,"version":"2.10.0"}')
            return self.send(200, '{"ok":true,"sqlite":true,"limiter":true,"keyScope":"d63166ad","version":"2.10.0"}')
        if p == "/openapi.json":
            return self.send(200, pretty.replace("2.10.0", "2.9.0") if mode == "old-spec" else pretty)
        if p == "/metrics":
            return self.send(200, "# HELP x y\n# TYPE x counter\nx 0\n", "text/plain")
        if p.startswith("/v1/lol/status/"):
            if mode == "auth-open":
                return self.send(200, "{}")
            return self.send(401, '{"error":{"code":"UNAUTHORIZED","message":"Missing or invalid API key","requestId":"01TEST"}}')
        return self.send(404, '{"error":{"code":"NOT_FOUND"}}')

s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY

# run <name> <stub mode> <version> <want exit> <want in output> [env...]: starts
# the stub, runs smoke.sh against it for that version, checks the exit code and
# the output.
run() {
  local name=$1 mode=$2 version=$3 want=$4 grep_for=$5 port code=0
  shift 5
  python3 "$tmp/stub.py" "$mode" > "$tmp/port" &
  pid=$!
  for _ in $(seq 50); do [[ -s $tmp/port ]] && break; sleep 0.1; done
  port=$(cat "$tmp/port")
  env SMOKE_PAUSE=0 "$@" "$smoke" "http://127.0.0.1:$port/" "$version" > "$tmp/out" 2>&1 || code=$?
  kill "$pid" && wait "$pid" 2>/dev/null || true
  pid=''
  : > "$tmp/port"
  if [[ $code == "$want" ]] && grep -qF -- "$grep_for" "$tmp/out"; then ok "$name"; else
    fail "$name (exit $code, want $want and '$grep_for'):"; sed 's/^/     /' "$tmp/out" >&2
  fi
}

run "a release that answers as riot-proxy does passes" good v2.10.0 0 "passed at 2.10.0"
run "/readyz that comes up on the second try passes" slow-ready v2.10.0 0 "passed at 2.10.0"
run "/readyz that never comes up fails" never-ready v2.10.0 1 "/readyz answered 503, not 200"
run "another version than the one asked for fails" good v2.11.0 1 "reports \"version\":\"2.10.0\", not 2.11.0"
run "a spec of another version fails" old-spec v2.10.0 1 "/openapi.json does not describe 2.10.0"
run "an auth layer that lets a keyless request through fails" auth-open v2.10.0 1 "/v1/lol/status/oc1 answered 200, not 401"
run "a response without X-Request-Id fails" no-request-id v2.10.0 1 "has no X-Request-Id"
run "an Access login redirect names the service token" access-redirect v2.10.0 1 "Cloudflare Access refused the request"
run "an Access 403 names the service token" access-forbidden v2.10.0 1 "Cloudflare Access refused the request"
run "the service token is sent as Access's headers" needs-token v2.10.0 0 "passed at 2.10.0" \
  CF_ACCESS_CLIENT_ID=id CF_ACCESS_CLIENT_SECRET=secret
run "half a service token is refused" good v2.10.0 1 "set both" CF_ACCESS_CLIENT_ID=id

code=0
SMOKE_TRIES=1 "$smoke" http://127.0.0.1:1 v2.10.0 > "$tmp/out" 2>&1 || code=$?
if [[ $code == 1 ]] && grep -q "/healthz did not answer" "$tmp/out"; then ok "nothing listening fails"; else fail "nothing listening fails"; fi
code=0
"$smoke" > "$tmp/out" 2>&1 || code=$?
if [[ $code == 2 ]] && grep -q usage "$tmp/out"; then ok "no arguments prints the usage"; else fail "no arguments prints the usage"; fi

if command -v shellcheck >/dev/null; then
  if shellcheck "$smoke" "$here/scripts/smoke.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi
fi
[[ $fails -eq 0 ]] || { echo "$fails failed" >&2; exit 1; }

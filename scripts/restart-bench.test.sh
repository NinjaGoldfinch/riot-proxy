#!/usr/bin/env bash
# Tests for scripts/restart-bench.sh (AVL-00, ADR-208) without systemd:
# systemctl, curl and hostname are stubs over a temporary directory, and the
# journal is a file (BENCH_JOURNAL_FILE) that the systemctl stub writes as
# `journalctl -o short-unix` would. Run by deploy/test.sh (CI's `ops` job).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bench="$here/scripts/restart-bench.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

puuid=$(printf 'a%.0s' {1..78})
key='bench-test-key'
mkdir -p "$tmp/stubs"

# systemctl: logged. Stopping writes the old process's last line (or systemd's
# SIGKILL lines in mode `killed`); starting writes the new one's boot lines,
# timestamped a little ahead, and in mode `gap` the tier check's line too.
cat >"$tmp/stubs/systemctl" <<EOF
#!/usr/bin/env bash
echo "\$*" >>"$tmp/systemctl.log"
mode=\$(cat "$tmp/mode")
line() { echo "\$(awk -v t="\$(date +%s.%N)" -v d="\$1" 'BEGIN { printf "%.6f", t + d }') stg riot-proxy[7]: \$2" >>"$tmp/journal"; }
case \$1 in restart | stop)
  if [[ \$mode == killed ]]; then
    line 0 "riot-proxy.service: State 'stop-sigterm' timed out. Killing."
    line 0 "riot-proxy.service: Main process exited, code=killed, status=9/KILL"
  else
    line 0 '{"level":"INFO","message":"limiter checkpoint and L2 flushed; stopped"}'
  fi ;;
esac
case \$1 in restart | start)
  : >"$tmp/calls"
  line 0.3 '{"level":"INFO","message":"listening","addr":"127.0.0.1:8080"}'
  [[ \$mode == gap ]] && line 0.5 '{"level":"INFO","message":"queued tiers:backfill for matches archived without tier stamps"}'
  line 0.6 '{"level":"INFO","message":"starting job workers","workers":8}' ;;
esac
exit 0
EOF
# curl: prints the status as -w '%{http_code}' would. Mode `gap` answers
# nothing to the first 15 /healthz probes; mode `no-profile` answers 503
# to the profile. The profile needs the key, sent as a header file.
cat >"$tmp/stubs/curl" <<EOF
#!/usr/bin/env bash
echo "\$*" >>"$tmp/curl.log"
mode=\$(cat "$tmp/mode") url=\${*: -1} auth=''
while ((\$#)); do [[ \$1 == -H ]] && auth=\$(cat "\${2#@}"); shift; done
case \$url in
  */healthz)
    echo x >>"$tmp/calls"
    if [[ \$mode == gap && \$(wc -l <"$tmp/calls") -le 15 ]]; then printf 000; exit 7; fi
    printf 200 ;;
  */readyz) printf 200 ;;
  */v1/players/$puuid/profile\?platform=oc1)
    if [[ \$mode == no-profile ]]; then printf 503
    elif [[ \$auth == "Authorization: Bearer $key" ]]; then printf 200
    else printf 401; fi ;;
  *) printf 404 ;;
esac
EOF
cat >"$tmp/stubs/hostname" <<EOF
#!/usr/bin/env bash
cat "$tmp/host"
EOF
chmod +x "$tmp/stubs"/*

# run <name> <mode> <host> <want exit> <want in output...> -- [bench args]
run() {
  local name=$1 code=0 want_code=$4 g
  echo "$2" >"$tmp/mode"
  echo "$3" >"$tmp/host"
  shift 4
  local greps=()
  while [[ $1 != -- ]]; do greps+=("$1"); shift; done
  shift
  rm -f "$tmp/systemctl.log" "$tmp/curl.log" "$tmp/journal"
  touch "$tmp/journal" "$tmp/calls"
  PATH="$tmp/stubs:$PATH" BENCH_JOURNAL_FILE="$tmp/journal" BENCH_POLL=0.05 BENCH_TIMEOUT="${BENCH_TIMEOUT:-5}" \
    "$bench" "$@" >"$tmp/out" 2>&1 || code=$?
  if [[ $code != "$want_code" ]]; then fail "$name: exit $code, want $want_code"; sed 's/^/     /' "$tmp/out" >&2; return; fi
  for g in "${greps[@]}"; do
    if ! grep -qE -- "$g" "$tmp/out"; then fail "$name: no '$g' in"; sed 's/^/     /' "$tmp/out" >&2; return; fi
  done
  ok "$name"
}
systemctl_was() { [[ $(tr '\n' ' ' <"$tmp/systemctl.log" 2>/dev/null) == "$1" ]] || fail "systemctl ran: $(cat "$tmp/systemctl.log" 2>/dev/null)"; }
args=(riot-proxy http://127.0.0.1:8080/ "$puuid" oc1)

export ADMIN_KEY=$key
run "a restart is timed beside the baseline" good ngf-mon-syd1 0 \
  'old process stopped +[0-9.]+ s +24 s, or SIGKILL at 90 s' 'start -> listening +[0-9.]+ s' \
  'queued tiers:backfill +not logged' 'starting job workers +[0-9.]+ s' 'first /healthz 200 +[0-9.]+ s' \
  'first /readyz 200 +[0-9.]+ s' 'first profile 200 +[0-9.]+ s +never' '90-300 s \(the gap\)' -- "${args[@]}"
systemctl_was "restart riot-proxy "
if grep -q "$key" "$tmp/curl.log"; then fail "the key is on curl's command line"; else ok "the key is sent from a file"; fi
if grep -q -- "--max-time 30 -H @" "$tmp/curl.log"; then ok "the profile probe gives up after 30 s"; else fail "profile max-time"; fi
run "a listen gap and the tier check's line are reported" gap ngf-mon-syd1 0 \
  'queued tiers:backfill +[0-9.]+ s' 'listening -> first /healthz 200 +[0-9.]+ s' -- "${args[@]}"
run "--stop times a clean stop and starts the service again" good ngf-mon-syd1 0 \
  'SIGTERM -> stopped \(--stop\) +[0-9.]+ s +24 s' -- --stop "${args[@]}"
systemctl_was "restart riot-proxy stop riot-proxy start riot-proxy "
run "a stop that systemd ends with SIGKILL says so" killed ngf-mon-syd1 0 \
  'old process stopped +SIGKILL at [0-9.]+ s' 'stopped \(--stop\) +SIGKILL at [0-9.]+ s' -- --stop "${args[@]}"
BENCH_TIMEOUT=1 run "a profile that never answers 200 fails" no-profile ngf-mon-syd1 1 \
  'first profile 200 +never \(in 1 s\)' 'first /healthz 200 +[0-9.]+ s' -- "${args[@]}"
run "a production host is refused" good ngf-prod-syd1 2 'looks like production' -- "${args[@]}"
if [[ -f $tmp/systemctl.log ]]; then fail "production: systemctl ran"; else ok "production: nothing restarted"; fi
run "a PUUID that is not one is refused" good ngf-mon-syd1 2 'is not a PUUID' -- riot-proxy http://x abc oc1
run "a platform that is not one is refused" good ngf-mon-syd1 2 'not a platform id' -- riot-proxy http://x "$puuid" 'oc1&x'
run "too few arguments prints the usage" good ngf-mon-syd1 2 'usage|ADMIN_KEY=' -- riot-proxy
unset ADMIN_KEY
run "no ADMIN_KEY is refused" good ngf-mon-syd1 2 'set ADMIN_KEY' -- "${args[@]}"
if [[ -f $tmp/systemctl.log ]]; then fail "no key: systemctl ran"; else ok "no key: nothing restarted"; fi

if command -v shellcheck >/dev/null; then
  if shellcheck "$bench" "$here/scripts/restart-bench.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi
fi
[[ $fails -eq 0 ]] || { echo "$fails failed" >&2; exit 1; }

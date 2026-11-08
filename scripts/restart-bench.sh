#!/usr/bin/env bash
# Restart benchmark (AVL-00, ADR-208). Run it on staging (ngf-mon-syd1), never
# on production: it restarts the service, and with --stop stops it once more.
# It times one restart from the journal and from probes, and prints the times
# beside the production restart of 2026-11-08 (IMPLEMENTATION.md §Post-release
# — AVL):
#
#   SIGTERM -> old process stopped     the journal's `stopped` line, or SIGKILL
#   start -> listening                 the journal's `listening` line
#   start -> queued tiers:backfill     the journal's line, if logged (AVL-06)
#   start -> starting job workers      the journal's line (the catch-ups ended)
#   start -> first /healthz 200        probed (listening -> /healthz is the gap)
#   start -> first /readyz 200         probed
#   start -> first profile 200         GET /v1/players/<puuid>/profile?platform=
#   --stop: SIGTERM -> stopped         `systemctl stop`, timed, then started again
#
#   ADMIN_KEY=... sudo -E scripts/restart-bench.sh [--stop] <service> <base-url> <puuid> <platform>
#   e.g. sudo -E scripts/restart-bench.sh --stop riot-proxy http://127.0.0.1:8080 <puuid> oc1
#
# ADMIN_KEY is any key with the `read` scope; it is sent from a file, never on
# a command line. BENCH_TIMEOUT (600 s, production's HEALTH_WAIT) bounds the
# wait; BENCH_POLL (0.2 s) is the probe interval. Exit 1 when a probe never
# answered 200 in time, 2 on a usage error or refusal. Tests:
# scripts/restart-bench.test.sh (BENCH_JOURNAL_FILE stands in for journalctl).
set -euo pipefail

usage() { sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; }
die() { echo "restart-bench: $*" >&2; exit 2; }

stop=0
args=()
for arg in "$@"; do
  case $arg in
    --stop) stop=1 ;;
    -h | --help) usage; exit 0 ;;
    -*) die "unknown option $arg (try --help)" ;;
    *) args+=("$arg") ;;
  esac
done
((${#args[@]} == 4)) || { usage >&2; exit 2; }
service=${args[0]}
base=${args[1]%/}
puuid=${args[2]}
platform=${args[3]}
timeout=${BENCH_TIMEOUT:-600}
poll=${BENCH_POLL:-0.2}

[[ -n ${ADMIN_KEY:-} ]] || die "set ADMIN_KEY to a key with the read scope (sudo -E keeps it)"
# 60-128 url-safe characters, as the route validates (src/http/validate.rs).
[[ $puuid =~ ^[A-Za-z0-9_-]{60,128}$ ]] || die "$puuid is not a PUUID"
[[ $platform =~ ^[a-z0-9]+$ ]] || die "$platform is not a platform id (e.g. oc1)"
host=$(hostname)
[[ $host != *prod* ]] || die "$host looks like production; run this on staging only"
for tool in curl systemctl awk; do command -v "$tool" >/dev/null || die "$tool is not installed"; done
[[ -n ${BENCH_JOURNAL_FILE:-} ]] || command -v journalctl >/dev/null || die "journalctl is not installed"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(umask 077 && printf 'Authorization: Bearer %s\n' "$ADMIN_KEY" > "$tmp/auth")

LISTENING='"message":"listening"'
WORKERS='"message":"starting job workers"'
TIERS='"message":"queued tiers:backfill for matches archived without tier stamps"'
STOPPED='"message":"limiter checkpoint and L2 flushed; stopped"'
KILLED="status=9/KILL|State 'stop-sigterm' timed out"

now() { date +%s.%N; }
# The unit's journal as `<unix time> <host> <ident>: <line>` (journalctl -o short-unix).
journal() {
  if [[ -n ${BENCH_JOURNAL_FILE:-} ]]; then cat -- "$BENCH_JOURNAL_FILE"
  else journalctl -u "$service" -o short-unix --no-pager --since "@${t0%.*}" 2>/dev/null; fi
}
# first <since> <regex>: the time of the first journal line at or after since that matches.
first() { journal | awk -v t="$1" -v re="$2" '$1 + 0 >= t + 0 && $0 ~ re { print $1; exit }'; }
# settle <since> <regex>: as first, giving journald up to 2 s to catch up.
settle() {
  local at='' _
  for _ in {1..10}; do at=$(first "$1" "$2") && [[ -n $at ]] && break; sleep 0.2; done
  echo "$at"
}
# probe <path> <max-time> [curl args]: the HTTP status, 000 when nothing answered.
probe() {
  local status
  status=$(curl -s -o /dev/null -w '%{http_code}' --max-time "$2" "${@:3}" "$base$1" 2>/dev/null) || status=000
  echo "$status"
}
since() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.1f s", b - a }'; }
past() { awk -v a="$(now)" -v b="$1" 'BEGIN { exit !(a >= b) }'; }

t0=$(now)
echo "restart-bench: restarting $service on $host ($base) at $(date -u +%FT%TZ)"
systemctl restart "$service"
t_start=$(now)
t_old_stop=$(settle "$t0" "$STOPPED")
t_old_kill=$(first "$t0" "$KILLED")
deadline=$(awk -v t="$t_start" -v w="$timeout" 'BEGIN { printf "%.6f", t + w }')

t_listen='' t_tiers='' t_workers='' t_health='' t_ready='' t_profile=''
profile="/v1/players/$puuid/profile?platform=$platform"
while :; do
  [[ -n $t_listen ]] || t_listen=$(first "$t_start" "$LISTENING")
  [[ -n $t_workers ]] || t_workers=$(first "$t_start" "$WORKERS")
  [[ -n $t_tiers ]] || t_tiers=$(first "$t_start" "$TIERS")
  if [[ -z $t_health && $(probe /healthz 2) == 200 ]]; then t_health=$(now); fi
  if [[ -z $t_ready && $(probe /readyz 3) == 200 ]]; then t_ready=$(now); fi
  # The site gives up on a request after 30 s; so does this probe.
  if [[ -n $t_health && -z $t_profile && $(probe "$profile" 30 -H "@$tmp/auth") == 200 ]]; then
    t_profile=$(now)
  fi
  [[ -n $t_listen && -n $t_workers && -n $t_health && -n $t_ready && -n $t_profile ]] && break
  past "$deadline" && break
  sleep "$poll"
done

t_term='' t_stopped='' t_killed='' t_gone=''
if ((stop)); then
  t_term=$(now)
  systemctl stop "$service"
  t_gone=$(now)
  t_stopped=$(settle "$t_term" "$STOPPED")
  t_killed=$(first "$t_term" "$KILLED")
  systemctl start "$service"
fi

# exit_time <sigterm> <stopped> <killed> <returned>: how the process ended, and when.
exit_time() {
  if [[ -n $3 ]]; then echo "SIGKILL at $(since "$1" "$3")"
  elif [[ -n $2 ]]; then since "$1" "$2"
  elif [[ -n $4 ]]; then echo "$(since "$1" "$4") (no stop line)"
  else echo "no stop line"; fi
}
# at <from> <to> <missing>: the interval, or what to print when there is none.
at() { if [[ -n $1 && -n $2 ]]; then since "$1" "$2"; else echo "$3"; fi; }
never="never (in ${timeout} s)"
row() { printf '%-36s %-24s %s\n' "$1" "$2" "$3"; }

echo
row "" "this run" "2026-11-08 baseline"
row "SIGTERM -> old process stopped" "$(exit_time "$t0" "$t_old_stop" "$t_old_kill" "$t_start")" "24 s, or SIGKILL at 90 s"
row "start -> listening" "$(at "$t_start" "$t_listen" "not logged")" "-"
row "start -> queued tiers:backfill" "$(at "$t_start" "$t_tiers" "not logged")" "logged on every boot"
row "start -> starting job workers" "$(at "$t_start" "$t_workers" "not logged")" "-"
row "start -> first /healthz 200" "$(at "$t_start" "$t_health" "$never")" "-"
row "listening -> first /healthz 200" "$(at "$t_listen" "$t_health" "-")" "90-300 s (the gap)"
row "start -> first /readyz 200" "$(at "$t_start" "$t_ready" "$never")" "4 m 58 s (DEP-03, 2026-11-04)"
row "start -> first profile 200" "$(at "$t_start" "$t_profile" "$never")" "never"
if ((stop)); then
  row "SIGTERM -> stopped (--stop)" "$(exit_time "$t_term" "$t_stopped" "$t_killed" "$t_gone")" "24 s, or SIGKILL at 90 s"
fi

[[ -n $t_health && -n $t_ready && -n $t_profile ]] || exit 1

#!/usr/bin/env bash
# Tests for the build identity release.yml embeds (STS-01, ADR-203): GET /status
# reports the tag's version only for a build from a tag, and `dev` otherwise, so
# `RELEASE_VERSION` must be the tag on a tag push and empty on a pull request,
# and every release build must check what `riot-proxy build-info` prints.
# Run by CI's `ops` job.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

python3 - "$here/.github/workflows/release.yml" <<'PY' || fails=$((fails + 1))
import sys, yaml
release = yaml.safe_load(open(sys.argv[1]))
builds = {name: job for name, job in release["jobs"].items()
          if any("cargo build --release" in str(st.get("run", "")) for st in job.get("steps", []))}

def checks_identity(job):
    return any('build-info' in str(st.get("run", "")) and '${GIT_SHA:0:7}' in str(st.get("run", ""))
               and 'expected=dev' in str(st.get("run", "")) and 'GITHUB_REF_TYPE" = tag' in str(st.get("run", ""))
               for st in job["steps"])

checks = {
    "RELEASE_VERSION is the tag on a tag push and empty otherwise":
        release["env"].get("RELEASE_VERSION") == "${{ github.ref_type == 'tag' && github.ref_name || '' }}",
    "there are release builds to check": len(builds) >= 2,
}
for name, job in sorted(builds.items()):
    checks[f"{name} checks the version and commit build-info prints"] = checks_identity(job)
bad = 0
for name, passed in checks.items():
    print(("ok   " if passed else "FAIL ") + name)
    bad += not passed
sys.exit(1 if bad else 0)
PY

# The check release.yml runs, against a binary built here: `dev` without a tag.
bin=${RIOT_PROXY_BIN:-$here/target/debug/riot-proxy}
if [[ -x $bin ]]; then
    sha=$(git -C "$here" rev-parse HEAD)
    if "$bin" build-info | grep -q "^{\"version\":\"dev\",\"commit\":\"${sha:0:7}\","; then
        ok "a local build reports dev and the checkout's commit"
    else
        fail "a local build reports dev and the checkout's commit: $("$bin" build-info)"
    fi
fi

if command -v shellcheck >/dev/null; then
    if shellcheck "$here/scripts/release-identity.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi
fi
[[ $fails -eq 0 ]] || { echo "$fails failed" >&2; exit 1; }

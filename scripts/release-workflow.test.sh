#!/usr/bin/env bash
# Tests for .github/workflows/release.yml's build identity (STS-01, design/14,
# ADR-203): every cargo build of a release binary gets RELEASE_VERSION, the
# tag's version on a tag build and empty otherwise, so /status says `dev` off a
# tag; nothing else sets it; and the image job checks the startup line carries
# it. Run by CI's `ops` job.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

python3 - "$here/.github/workflows/release.yml" "$here/.github/workflows/ci.yml" <<'PY'
import sys, yaml
release = yaml.safe_load(open(sys.argv[1]))
ci_text = open(sys.argv[2]).read()
want = "${{ github.ref_type == 'tag' && needs.version.outputs.version || '' }}"
fails = 0

def check(name, ok):
    global fails
    print(("ok   " if ok else "FAIL ") + name, file=sys.stdout if ok else sys.stderr)
    fails += 0 if ok else 1

builds = [
    (job_name, st)
    for job_name, job in release["jobs"].items()
    for st in job.get("steps", [])
    if str(st.get("run", "")).startswith("cargo build --release")
]
check("the Linux and macOS jobs build release binaries",
      sorted({j for j, _ in builds}) == ["build", "build-darwin"])
for job_name, st in builds:
    check(f"{job_name}: RELEASE_VERSION is the version on a tag build only",
          st.get("env", {}).get("RELEASE_VERSION") == want)
    check(f"{job_name}: the build depends on the version job",
          "version" in release["jobs"][job_name]["needs"])
check("the workflow does not set RELEASE_VERSION for every job",
      "RELEASE_VERSION" not in release.get("env", {}))
check("no job sets RELEASE_VERSION job-wide",
      all("RELEASE_VERSION" not in job.get("env", {}) for job in release["jobs"].values()))
check("CI's builds never set it, so they report dev", "RELEASE_VERSION" not in ci_text)

image = {st.get("name"): st for st in release["jobs"]["image"]["steps"]}
line = image.get("the startup log line names the version and commit", {})
check("the image job checks the startup line: the version, else dev",
      line.get("env", {}).get("WANT") == "${{ github.ref_type == 'tag' && needs.version.outputs.version || 'dev' }}"
      and '"version' in line.get("run", "") and "GIT_SHA:0:7" in line.get("run", ""))
sys.exit(1 if fails else 0)
PY

#!/usr/bin/env bash
# Tests for .github/workflows/deploy.yml (DEP-03, ADR-196; DEP-05, ADR-198). It
# deploys to real hosts, so CI checks what it can without deploying: it deploys
# the asset release.yml builds, checked against the checksum file release.yml
# writes, on the Proxmox runner behind the chosen environment, through the shared
# action at its moving major tag; production requires staging/smoke or
# prod/deployed and sets prod/deployed; staging is smoke-tested and records
# staging/smoke; and release.yml calls it for staging with the secrets passed on.
# Run by CI's `ops` job.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

python3 - "$here/.github/workflows/deploy.yml" "$here/.github/workflows/release.yml" <<'PY' || fails=$((fails + 1))
import re, sys, yaml
deploy = yaml.safe_load(open(sys.argv[1]))
release_text = open(sys.argv[2]).read()
release = yaml.safe_load(release_text)
env = deploy["env"]
on = deploy[True]
job = deploy["jobs"]["deploy"]
steps = job["steps"]
step = steps[0]
w = step["with"]
by_name = {st.get("name"): st for st in steps}
smoke = by_name.get("Smoke test staging", {})
record = by_name.get("Record staging/smoke", {})
checkout = next((st for st in steps if str(st.get("uses", "")).startswith("actions/checkout@")), {})
staging = release["jobs"].get("staging", {})
attach = release["jobs"]["attach-darwin"]
check_run = deploy["jobs"]["check"]["steps"][0]["run"]
checks = {
    "the asset is the tarball release.yml builds":
        re.search(r"tgz=dist/" + re.escape(env["ASSET"]) + r"\b", release_text) is not None,
    "the checksums are the file release.yml writes":
        f"> {env['CHECKSUMS']}" in release_text,
    "it runs on the Proxmox runner": job["runs-on"] == ["self-hosted", "linux", "proxmox"],
    "the job waits on the chosen environment": job["environment"] == "${{ inputs.env }}",
    "a dispatch offers staging and prod, staging first":
        on["workflow_dispatch"]["inputs"]["env"]["options"] == ["staging", "prod"]
        and on["workflow_dispatch"]["inputs"]["env"]["default"] == "staging",
    "it can be called with env and version":
        set(on["workflow_call"]["inputs"]) == {"env", "version"}
        and all(i["required"] and i["type"] == "string" for i in on["workflow_call"]["inputs"].values()),
    "a called env other than staging or prod is refused before the environment":
        job["needs"] == "check" and "staging | prod)" in check_run and "exit 1" in check_run,
    "it calls ninja-actions/deploy@v1": step["uses"] == "NinjaGoldfinch/ninja-actions/deploy@v1",
    "the action gets the asset and the checksum file": w["asset"] == "${{ env.ASSET }}" and w["checksums"] == "${{ env.CHECKSUMS }}",
    "the key comes from the environment's secret": w["ssh-key"] == "${{ secrets.DEPLOY_SSH_KEY }}",
    "prod requires staging/smoke or prod/deployed":
        w["require-status"] == "${{ inputs.env == 'prod' && 'staging/smoke prod/deployed' || '' }}",
    "prod sets prod/deployed": w["set-status"] == "${{ inputs.env == 'prod' && 'prod/deployed' || '' }}",
    "break_glass is a dispatch input passed as the unverified reason":
        on["workflow_dispatch"]["inputs"]["break_glass"]["default"] == ""
        and w["unverified-reason"] == "${{ inputs.break_glass }}",
    "deploys queue per environment on the job, never cancel":
        "concurrency" not in deploy
        and job["concurrency"] == {"group": "deploy-${{ inputs.env }}", "cancel-in-progress": False},
    "read-only token, plus statuses for the gate":
        deploy["permissions"] == {"contents": "read"}
        and job["permissions"] == {"contents": "read", "statuses": "write"},
    "staging is smoke-tested with scripts/smoke.sh through Access":
        smoke.get("if") == "inputs.env == 'staging'"
        and checkout.get("if") == "inputs.env == 'staging'"
        and smoke.get("run") == 'scripts/smoke.sh "$STAGING_URL" "$VERSION"'
        and smoke["env"]["CF_ACCESS_CLIENT_ID"] == "${{ secrets.CF_ACCESS_CLIENT_ID }}"
        and smoke["env"]["CF_ACCESS_CLIENT_SECRET"] == "${{ secrets.CF_ACCESS_CLIENT_SECRET }}"
        and env["STAGING_URL"] == "https://riot-proxy-staging.ninjagoldfinch.com",
    "the smoke test's result is recorded as staging/smoke on the version's commit":
        record.get("if", "").startswith("always() && inputs.env == 'staging'")
        and "steps.smoke.outcome == 'success' || steps.smoke.outcome == 'failure'" in record.get("if", "")
        and "context=staging/smoke" in record.get("run", "")
        and 'commits/$VERSION" --jq .sha' in record.get("run", "")
        and 'state="$OUTCOME"' in record.get("run", ""),
    "release.yml deploys every release to staging after publishing it":
        staging.get("uses") == "./.github/workflows/deploy.yml"
        and staging.get("needs") == "release"
        and staging.get("with") == {"env": "staging", "version": "${{ github.ref_name }}"},
    "release.yml passes the secrets on, or the deploy key arrives empty": staging.get("secrets") == "inherit",
    "release.yml's staging job may set statuses":
        staging.get("permissions") == {"contents": "read", "statuses": "write"},
    "sha256sums is replaced only after the staging deploy has read it":
        "staging" in attach["needs"] and "!cancelled()" in attach["if"],
    "the release image passes the smoke test":
        'scripts/smoke.sh http://127.0.0.1:8080 "v$VERSION"' in release_text,
}
bad = 0
for name, passed in checks.items():
    print(("ok   " if passed else "FAIL ") + name)
    bad += not passed
sys.exit(1 if bad else 0)
PY

if command -v shellcheck >/dev/null; then
    if shellcheck "$here/scripts/deploy-workflow.test.sh"; then ok "shellcheck"; else fail "shellcheck"; fi
fi
[[ $fails -eq 0 ]] || { echo "$fails failed" >&2; exit 1; }

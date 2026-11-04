#!/usr/bin/env bash
# Tests for .github/workflows/deploy.yml (DEP-03, ADR-196). It only runs by hand
# against production, so CI checks what it can without deploying: it deploys the
# asset release.yml builds, checks it against the checksum file release.yml
# writes, runs on the Proxmox runner behind the chosen environment, and calls
# the shared action at its moving major tag. Run by CI's `ops` job.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fails=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }

python3 - "$here/.github/workflows/deploy.yml" "$here/.github/workflows/release.yml" <<'PY' || fails=$((fails + 1))
import re, sys, yaml
deploy = yaml.safe_load(open(sys.argv[1]))
release_text = open(sys.argv[2]).read()
env = deploy["env"]
job = deploy["jobs"]["deploy"]
step = job["steps"][0]
checks = {
    "the asset is the tarball release.yml builds":
        re.search(r"tgz=dist/" + re.escape(env["ASSET"]) + r"\b", release_text) is not None,
    "the checksums are the file release.yml writes":
        f"> {env['CHECKSUMS']}" in release_text,
    "it runs on the Proxmox runner": job["runs-on"] == ["self-hosted", "linux", "proxmox"],
    "the job waits on the chosen environment": job["environment"] == "${{ inputs.env }}",
    "every choice of environment is a real one": set(deploy[True]["workflow_dispatch"]["inputs"]["env"]["options"]) <= {"prod", "staging"},
    "it calls ninja-actions/deploy@v1": step["uses"] == "NinjaGoldfinch/ninja-actions/deploy@v1",
    "the action gets the asset and the checksum file": step["with"]["asset"] == "${{ env.ASSET }}" and step["with"]["checksums"] == "${{ env.CHECKSUMS }}",
    "the key comes from the environment's secret": step["with"]["ssh-key"] == "${{ secrets.DEPLOY_SSH_KEY }}",
    "deploys to one environment queue, never cancel": deploy["concurrency"]["cancel-in-progress"] is False,
    "read-only token": deploy["permissions"] == {"contents": "read"},
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

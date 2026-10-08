#!/usr/bin/env bash
# Tests for the Proxmox dev VM kit (OPS-02). No Proxmox host or Docker daemon needed:
# create-vm.sh runs in --dry-run / --print-vendor-data, and riot-proxy-update runs
# against a fake `docker` on PATH. Run by CI's `ops` job; locally: deploy/proxmox/test.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ok() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
check() { local name=$1; shift; if "$@"; then ok "$name"; else fail "$name"; fi; }

# --- shellcheck --------------------------------------------------------------
check "shellcheck" shellcheck "$here/create-vm.sh" "$here/test.sh" "$here/files/riot-proxy-update"

# --- vendor-data -------------------------------------------------------------
"$here/create-vm.sh" --print-vendor-data --user tester > "$tmp/vendor.yaml"
check "vendor-data starts with #cloud-config" grep -qx '#cloud-config' <(head -1 "$tmp/vendor.yaml")
check "vendor-data embeds every file byte-for-byte and adds the user to docker" python3 - "$tmp/vendor.yaml" "$here/files" <<'PY'
import base64, sys, yaml
doc = yaml.safe_load(open(sys.argv[1]))
want = {
    "/opt/riot-proxy/compose.yaml": "compose.yaml",
    "/opt/riot-proxy/env.example": "env.example",
    "/usr/local/bin/riot-proxy-update": "riot-proxy-update",
    "/etc/systemd/system/riot-proxy-update.service": "riot-proxy-update.service",
    "/etc/systemd/system/riot-proxy-update.timer": "riot-proxy-update.timer",
}
got = {f["path"]: f for f in doc["write_files"]}
assert set(got) == set(want), sorted(got)
for path, name in want.items():
    assert base64.b64decode(got[path]["content"]) == open(f"{sys.argv[2]}/{name}", "rb").read(), path
assert got["/usr/local/bin/riot-proxy-update"]["permissions"] == "0755"
assert {"docker.io", "docker-compose", "qemu-guest-agent"} <= set(doc["packages"])
assert ["usermod", "-aG", "docker", "tester"] in doc["runcmd"]
assert any("riot-proxy-update.timer" in c for c in doc["runcmd"][-1])
PY
if command -v cloud-init >/dev/null; then
  check "cloud-init schema accepts the vendor-data" cloud-init schema -c "$tmp/vendor.yaml"
else
  echo "skip cloud-init schema (cloud-init not installed)"
fi
check "a bad --user is refused" bash -c "! '$here/create-vm.sh' --print-vendor-data --user 'x;rm' 2>/dev/null"

# --- dry run -----------------------------------------------------------------
echo "ssh-ed25519 AAAAtest tester@example" > "$tmp/keys"
"$here/create-vm.sh" --dry-run --ssh-keys "$tmp/keys" --vmid 123 --ip 192.168.1.50/24 --gw 192.168.1.1 > "$tmp/dry"
check "dry run creates the VM" grep -q '^qm create 123 --name riot-proxy-dev ' "$tmp/dry"
check "dry run imports the Debian 13 image" grep -q 'scsi0 local-lvm:0\\,import-from=/var/tmp/riot-proxy-images/debian-13-genericcloud-amd64.qcow2' "$tmp/dry"
check "dry run attaches the vendor snippet" grep -q 'cicustom vendor=local:snippets/riot-proxy-dev-123.yaml' "$tmp/dry"
check "dry run sets a static address" grep -q 'ipconfig0 ip=192.168.1.50/24\\,gw=192.168.1.1' "$tmp/dry"
check "dry run starts the VM" grep -q '^qm start 123' "$tmp/dry"
"$here/create-vm.sh" --dry-run --ssh-keys "$tmp/keys" --vmid 123 --no-start > "$tmp/dry2"
check "--no-start leaves it stopped" bash -c "! grep -q '^qm start' '$tmp/dry2'"
check "dhcp is the default" grep -q 'ipconfig0 ip=dhcp' "$tmp/dry2"
check "--ssh-keys or --generate-key is required" bash -c "! '$here/create-vm.sh' --dry-run >/dev/null 2>&1"
check "a bad --name is refused" bash -c "! '$here/create-vm.sh' --dry-run --ssh-keys '$tmp/keys' --name '../x' >/dev/null 2>&1"

# --- generated keys ----------------------------------------------------------
"$here/create-vm.sh" --dry-run --generate-key --vmid 123 > "$tmp/gen"
check "--generate-key makes an ed25519 key named after the VM" grep -q "^ssh-keygen -q -t ed25519 -N '' -C riot@riot-proxy-dev-123 -f /root/.ssh/riot-proxy/riot-proxy-dev-123" "$tmp/gen"
check "--generate-key gives the VM only the new public key" grep -q 'sshkeys /root/.ssh/riot-proxy/riot-proxy-dev-123.pub ' "$tmp/gen"
check "--generate-key says how to log in" grep -q 'ssh -i ~/.ssh/riot-proxy-dev-123 riot@<vm>' "$tmp/gen"
"$here/create-vm.sh" --dry-run --generate-key --key-dir "$tmp/k" --name dev2 --vmid 124 > "$tmp/gen2"
check "--key-dir and --name place the key" grep -qF -- "-C riot@dev2-124 -f $tmp/k/dev2-124 " "$tmp/gen2"
"$here/create-vm.sh" --dry-run --generate-key --ssh-keys "$tmp/keys" --vmid 123 > "$tmp/gen3"
check "--generate-key with --ssh-keys passes a combined file" bash -c "grep -q -- '--sshkeys /' '$tmp/gen3' && ! grep -q -- '--sshkeys .*\.pub ' '$tmp/gen3' && ! grep -q -- '--sshkeys $tmp/keys ' '$tmp/gen3'"
check "plain --ssh-keys generates nothing" bash -c "! grep -q ssh-keygen '$tmp/dry'"
check "--generate-key makes the key after the download" bash -c "[ \$(grep -n '^curl' '$tmp/gen' | cut -d: -f1) -lt \$(grep -n '^ssh-keygen' '$tmp/gen' | cut -d: -f1) ]"

# --- a real run against fake Proxmox tools -----------------------------------
mkdir -p "$tmp/pve" "$tmp/pve/snippets"
for t in id qm pvesh pvesm curl; do
  cat > "$tmp/pve/$t" <<FAKE
#!/usr/bin/env bash
echo "$t \$*" >> "$tmp/pve.log"
case "$t \$*" in
  "id -u") echo 0 ;;
  "pvesh get /cluster/nextid") echo 777 ;;
  "pvesm status"*) printf 'Name Type\nlocal dir\n' ;;
  "pvesm path"*) echo "$tmp/pve/snippets/vendor.yaml" ;;
  curl*SHA512SUMS) echo "\$(sha512sum < "$tmp/img/debian-13-genericcloud-amd64.qcow2" | cut -d' ' -f1)  debian-13-genericcloud-amd64.qcow2" ;;
  curl*) [ -f "$tmp/pve/curl-fails" ] && exit 22; out=\$(sed -n 's/.*-o \([^ ]*\).*/\1/p' <<< "\$*"); echo image > "\$out" ;;
esac
FAKE
  chmod +x "$tmp/pve/$t"
done
fake_run() { PATH="$tmp/pve:$PATH" "$here/create-vm.sh" --generate-key --key-dir "$tmp/pvekeys" --image-dir "$tmp/img"; }
touch "$tmp/pve/curl-fails"
fake_run > /dev/null 2> "$tmp/run-err" || true
check "a failed download leaves no key behind" test ! -e "$tmp/pvekeys/riot-proxy-dev-777"
check "the download is announced" grep -q 'create-vm: downloading the Debian 13 cloud image' "$tmp/run-err"
rm "$tmp/pve/curl-fails"
fake_run > "$tmp/run-out" 2> "$tmp/run-err"
check "a real run makes the key and the VM" bash -c "test -f '$tmp/pvekeys/riot-proxy-dev-777.pub' && grep -q 'qm set 777 --ciuser riot --sshkeys $tmp/pvekeys/riot-proxy-dev-777.pub' '$tmp/pve.log'"
check "a real run reports each step" bash -c "for s in checking generating creating starting; do grep -q \"create-vm: \$s\" '$tmp/run-err' || exit 1; done"
check "a second run refuses before downloading" bash -c ": > '$tmp/pve.log'; ! PATH='$tmp/pve:$PATH' '$here/create-vm.sh' --generate-key --key-dir '$tmp/pvekeys' --image-dir '$tmp/img' 2>/dev/null && ! grep -q '^curl' '$tmp/pve.log'"

mkdir -p "$tmp/real"
check "the generated keys differ per VM" bash -c "ssh-keygen -q -t ed25519 -N '' -f '$tmp/real/a' && ssh-keygen -q -t ed25519 -N '' -f '$tmp/real/b' && ! cmp -s '$tmp/real/a.pub' '$tmp/real/b.pub'"
check "a static --ip needs --gw" bash -c "! '$here/create-vm.sh' --dry-run --ssh-keys '$tmp/keys' --ip 10.0.0.2/24 >/dev/null 2>&1"

# --- riot-proxy-update against a fake docker ---------------------------------
mkdir -p "$tmp/bin" "$tmp/stack"
cat > "$tmp/bin/docker" <<'FAKE'
#!/usr/bin/env bash
# Fake docker: logs each call; `compose images -q` reports the image id in $STATE.
echo "$*" >> "$FAKE_LOG"
case "$*" in
  "compose images -q riot-proxy") cat "$STATE" ;;
  "compose pull --quiet") [ -f "$STATE.next" ] && mv "$STATE.next" "$STATE.pulled" ;;
  "compose up -d --remove-orphans") [ -f "$STATE.pulled" ] && mv "$STATE.pulled" "$STATE" ;;
  "compose config --images") echo ghcr.io/ninjagoldfinch/riot-proxy:edge ;;
  "image inspect"*) echo abc1234 ;;
esac
exit 0
FAKE
chmod +x "$tmp/bin/docker"
update() { PATH="$tmp/bin:$PATH" RIOT_PROXY_DIR="$tmp/stack" FAKE_LOG="$tmp/log" STATE="$tmp/state" "$here/files/riot-proxy-update"; }

: > "$tmp/log"; echo old > "$tmp/state"
cp "$here/files/env.example" "$tmp/stack/.env"
update 2> "$tmp/err"
check "without a key it does nothing" test ! -s "$tmp/log"
check "without a key it says why" grep -q 'no RIOT_API_KEY' "$tmp/err"

sed -i 's/^RIOT_API_KEY=$/RIOT_API_KEY=test-key-not-real/' "$tmp/stack/.env"
update > "$tmp/out"
check "with a key it pulls and ups" grep -qx 'compose up -d --remove-orphans' "$tmp/log"
check "an unchanged image is not reported or pruned" bash -c "! grep -q 'now running' '$tmp/out' && ! grep -q 'image prune' '$tmp/log'"

: > "$tmp/log"; echo new > "$tmp/state.next"
update > "$tmp/out"
check "a new image is reported with its revision" grep -qx 'riot-proxy-update: now running ghcr.io/ninjagoldfinch/riot-proxy:edge (abc1234)' "$tmp/out"
check "a new image prunes the old one" grep -q '^image prune -f' "$tmp/log"

# --- compose file ------------------------------------------------------------
if docker compose version >/dev/null 2>&1; then
  cp "$here/files/compose.yaml" "$tmp/stack/"
  check "compose follows :edge by default" bash -c "cd '$tmp/stack' && sed -i '/^RIOT_PROXY_TAG/d' .env && docker compose config --images | grep -qx ghcr.io/ninjagoldfinch/riot-proxy:edge"
  check "RIOT_PROXY_TAG pins the image" bash -c "cd '$tmp/stack' && echo RIOT_PROXY_TAG=sha-abc1234 >> .env && docker compose config --images | grep -qx ghcr.io/ninjagoldfinch/riot-proxy:sha-abc1234"
else
  echo "skip compose config (docker compose not installed)"
fi

[ "$fails" = 0 ] || { echo "$fails failed" >&2; exit 1; }
echo "all passed"

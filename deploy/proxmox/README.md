# Dev VM on Proxmox

A Debian 13 VM running riot-proxy in Docker that follows `main`: every push to
`main` publishes `ghcr.io/ninjagoldfinch/riot-proxy:edge` (`.github/workflows/edge.yml`),
and a timer in the VM pulls it every 2 minutes and restarts the container if it
changed. `:edge` only moves forwards: a workflow run for a commit that is no
longer the head of `main` (GitHub sometimes starts one late) pushes only its
`:sha-<short>` tag. Plain HTTP on your LAN, `ENV=development`, real Riot API.
See ADR-069, ADR-070 and ADR-114.

Needs Proxmox VE 8 or later (`qm set --scsi0 …,import-from=`).

## Create the VM

On the Proxmox host, as root:

```bash
# once: let the 'local' storage hold cloud-init snippets
#   Datacenter → Storage → local → Content → add "Snippets"

curl -fsSL https://github.com/NinjaGoldfinch/riot-proxy/archive/refs/heads/main.tar.gz | tar xz
cd riot-proxy-main/deploy/proxmox
./create-vm.sh --ssh-keys ~/.ssh/my-laptop.pub            # DHCP, next free VM id
# or: ./create-vm.sh --ssh-keys key.pub --vmid 210 --ip 192.168.68.50/22 --gw 192.168.68.1
# or: ./create-vm.sh --generate-key                        # a new keypair just for this VM
```

`--generate-key` writes an ed25519 keypair (no passphrase) to
`/root/.ssh/riot-proxy/<name>-<vmid>` on the host, so every VM gets its own login key.
On the host, log in with that full path (`ssh -i /root/.ssh/riot-proxy/<name>-<vmid> riot@<vm-ip>`).
To log in from your own machine, `scp` the file to its `~/.ssh/` and `chmod 600` it.
The script prints both commands. Add `--ssh-keys` as well to let your usual key in too.
It refuses to overwrite an existing key, so on a rebuild delete the old one first. Each VM's SSH *host* keys are already unique; cloud-init makes them on first boot.

`./create-vm.sh --help` lists the options: storage (`local-lvm`), bridge (`vmbr0`),
2 cores, 2 GB RAM and a 16 GB disk by default. `--dry-run` prints the `qm` commands
without running anything; `--print-vendor-data` shows what the VM installs.

First boot installs Docker and takes a few minutes. Then:

```bash
qm guest cmd <vmid> network-get-interfaces     # its address
ssh riot@<vm-ip>                               # <vm-ip> is the address above
# with --generate-key: ssh -i /root/.ssh/riot-proxy/riot-proxy-dev-<vmid> riot@<vm-ip>
nano /opt/riot-proxy/.env                      # set RIOT_API_KEY=
sudo riot-proxy-update                         # start now instead of waiting for the timer
docker compose -f /opt/riot-proxy/compose.yaml logs | grep -i "admin key"   # the bootstrap admin key, shown once
```

Open `http://<vm>:8080/docs` (or `/dev`, `/dashboard`).

Set only the variables you need in `.env`. Don't copy a laptop `.env` that sets
`DATA_DIR`, `PORT` or `HOST`: the container expects `/data` and port 8080.

## Updating

Nothing to do: push to `main`, wait for the `edge` workflow (a few minutes), and
the VM picks it up within 2 minutes. When several commits land close together,
`:edge` ends on the newest; a run whose commit is no longer the head logs
"`:edge` stays" and pushes `:sha-<short>` only. To check or force it:

```bash
systemctl list-timers riot-proxy-update.timer
journalctl -u riot-proxy-update -n 20          # "now running …:edge (<commit>)" on each update
sudo riot-proxy-update                         # pull now
```

A new image has to pass its Docker healthcheck within 120 seconds (ADR-115). The
image's check first runs 30 s after start, so a good update takes about that long.
If the container doesn't report `healthy` in time, or keeps exiting, the updater
puts it back on the image it replaced (tagged `riot-proxy:previous` before each
pull), logs `error: rejected …:edge (<commit>): …; rolled back to riot-proxy:previous (<commit>)`,
and notes the bad image in `/opt/riot-proxy/.rejected` so later runs leave it alone.
The next image published is tried as usual. On a first install there is nothing
to go back to, so a bad image is logged and left running. Old images are pruned
only after a healthy update; `riot-proxy:previous` is kept.

```bash
journalctl -u riot-proxy-update -p err         # rollbacks only
cat /opt/riot-proxy/.rejected                  # the image ID and commit it won't deploy again
sudo rm /opt/riot-proxy/.rejected && sudo riot-proxy-update   # try that image again
```

While a rollback holds, update with `sudo riot-proxy-update`, not `docker compose up`:
compose alone would start the rejected image again. To give the check longer, run
`sudo systemctl edit riot-proxy-update` and add `[Service]` / `Environment=RIOT_PROXY_HEALTH_TIMEOUT=300`.

To hold a version, set `RIOT_PROXY_TAG` in `/opt/riot-proxy/.env` to `sha-<short commit>`
or a release such as `2.0.0`, then run `sudo riot-proxy-update`. Set it back
to `edge` to follow `main` again. To stop updates: `sudo systemctl disable --now riot-proxy-update.timer`.

Debian security updates install themselves (`unattended-upgrades`).

## Files

| In the VM | From |
|---|---|
| `/opt/riot-proxy/compose.yaml` | `files/compose.yaml` |
| `/opt/riot-proxy/.env` | `files/env.example`, then yours; never overwritten |
| `/opt/riot-proxy/data/` | SQLite, backups, ddragon (the container's `/data`) |
| `/usr/local/bin/riot-proxy-update` + `riot-proxy-update.{service,timer}` | `files/` |

Changes to these files reach a VM only when it is recreated or they are copied in;
the image updates itself.

## Updating an existing VM

To copy in the current `compose.yaml` and `riot-proxy-update` from `main` (here VM 102),
on the Proxmox host as root (through the guest agent, no SSH needed):

```bash
qm guest exec 102 -- bash -c 'set -e; u=https://raw.githubusercontent.com/NinjaGoldfinch/riot-proxy/main/deploy/proxmox/files; curl -fsSL "$u/compose.yaml" -o /opt/riot-proxy/compose.yaml; curl -fsSL "$u/riot-proxy-update" -o /usr/local/bin/riot-proxy-update; chmod 755 /usr/local/bin/riot-proxy-update'
qm guest exec 102 -- /usr/local/bin/riot-proxy-update
```

`exitcode` 0 in each reply means it worked. Copy `compose.yaml` first: the new
updater's rollback needs its `RIOT_PROXY_IMAGE` line. The `.service` and `.timer`
files haven't changed since OPS-02.

## Rebuild from scratch

```bash
qm stop <vmid> && qm destroy <vmid> --purge     # deletes the VM and its data
./create-vm.sh --ssh-keys key.pub --vmid <vmid>
# with --generate-key: rm /root/.ssh/riot-proxy/riot-proxy-dev-<vmid>{,.pub} first
```

Keep `data/` first if you want the archive: `scp -r riot@<vm>:/opt/riot-proxy/data .`

## Tests

`deploy/proxmox/test.sh` (CI job `ops`): shellcheck, cloud-init schema, the
embedded files, the dry-run `qm` commands, and `riot-proxy-update` against a fake `docker`
(healthy update, rollback, the rejected image left alone, first install).

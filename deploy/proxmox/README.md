# Dev VM on Proxmox

A Debian 13 VM running riot-proxy in Docker that follows `main`: every push to
`main` publishes `ghcr.io/ninjagoldfinch/riot-proxy:edge` (`.github/workflows/edge.yml`),
and a timer in the VM pulls it every 2 minutes and restarts the container if it
changed. Plain HTTP on your LAN, `ENV=development`, real Riot API. See ADR-069 and ADR-070.

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
the VM picks it up within 2 minutes. To check or force it:

```bash
systemctl list-timers riot-proxy-update.timer
journalctl -u riot-proxy-update -n 20          # "now running …:edge (<commit>)" on each update
sudo riot-proxy-update                         # pull now
```

To hold a version, set `RIOT_PROXY_TAG` in `/opt/riot-proxy/.env` to `sha-<short commit>`
or a release such as `2.0.0-rc.3`, then run `sudo riot-proxy-update`. Set it back
to `edge` to follow `main` again. To stop updates: `sudo systemctl disable --now riot-proxy-update.timer`.

Debian security updates install themselves (`unattended-upgrades`).

## Files

| In the VM | From |
|---|---|
| `/opt/riot-proxy/compose.yaml` | `files/compose.yaml` |
| `/opt/riot-proxy/.env` | `files/env.example`, then yours; never overwritten |
| `/opt/riot-proxy/data/` | SQLite, backups, ddragon (the container's `/data`) |
| `/usr/local/bin/riot-proxy-update` + `riot-proxy-update.{service,timer}` | `files/` |

Changes to these files reach a VM only when it is recreated (or by copying them over by hand); the image updates itself.

## Rebuild from scratch

```bash
qm stop <vmid> && qm destroy <vmid> --purge     # deletes the VM and its data
./create-vm.sh --ssh-keys key.pub --vmid <vmid>
# with --generate-key: rm /root/.ssh/riot-proxy/riot-proxy-dev-<vmid>{,.pub} first
```

Keep `data/` first if you want the archive: `scp -r riot@<vm>:/opt/riot-proxy/data .`

## Tests

`deploy/proxmox/test.sh` (CI job `ops`): shellcheck, cloud-init schema, the
embedded files, the dry-run `qm` commands, and `riot-proxy-update` against a fake `docker`.

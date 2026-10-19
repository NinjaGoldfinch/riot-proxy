# ADR: a static binary under systemd behind a Cloudflare Tunnel

**Context.** The proxy is never the public edge; it serves services on the same host. The first production box (OVH, 2 cores, 38 GB disk) was replaced on 2026-10-13 by an Onidel Sydney ONI-4 after a hosting review found nothing cheaper for a workload bounded by the Riot key.

**Decision.** Production runs the release's static musl binary as `riot-proxy.service` on Ubuntu 26.04 with an environment file, a data directory and no containers (`docs/design/07-deployment.md` §Option C; the Docker image exists for other hosts and the dev VM). Built-in TLS was removed before 2.0.0; `TLS=true` refuses to boot (ADR-067). The proxy and the site listen on loopback only; a token-installed `cloudflared` publishes `api.ninjagoldfinch.lol` and `ninjagoldfinch.lol` outbound, and `admin.ninjagoldfinch.lol` sits behind Cloudflare Access with a WAF rule blocking the admin paths on `api.` (ADR-142, ADR-144). The old box became the monitoring host `ngf-mon-syd1`.

**Consequences.** A deploy is a file copy and a restart done by a person; nothing in CI touches the box. No inbound ports, no local certificates, no reverse proxy configuration to keep. `ADMIN_IP_ALLOWLIST` is useless behind the tunnel and stays empty; Access is the gate. `TRUST_PROXY=true` is required there so the proxy sees the client's address.

**Status.** Accepted, executed 2026-10-13 and 2026-10-14 (ADR-067 2026-10-05; ADR-142, ADR-144 2026-10-13).

**Evidence.** `docs/design/07-deployment.md`, `docs/design/12-object-storage-and-hosting.md` §As executed and §Admin hostname behind Access, `README.md` §HTTPS, `src/config.rs:668-673` (the `TLS` refusal).

---
**Sources:** `docs/DECISIONS.md:L792-L799,L1516-L1523,L1534-L1543`, `docs/design/07-deployment.md`, `docs/design/12-object-storage-and-hosting.md`, `src/config.rs:L668-L673`
**Last verified:** 2026-10-19 against commit `52fa67d`

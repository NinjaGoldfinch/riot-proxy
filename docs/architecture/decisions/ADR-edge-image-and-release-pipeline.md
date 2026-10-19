# ADR: `:edge` on every push, releases from tags, a dev VM that follows `main`

**Context.** The owner wanted a LAN box that always runs the latest `main` for frontend work, and releases that a person can install on the production box with a checksum.

**Decision.** Every push to `main` builds the static amd64 binary, packages it in a `FROM scratch` image and pushes `ghcr.io/ninjagoldfinch/riot-proxy:edge` and `:sha-<short>`; `:edge` only moves forward, so a late run for an older commit pushes only its sha tag (ADR-069, ADR-114). A `v*` tag builds three binaries, a multi-arch image tagged `<version>` and `2`, and a GitHub release with `sha256sums` (ADR-063). Each image's description lists the commits since the previous one (ADR-107). The Proxmox dev VM pulls `:edge` every two minutes and rolls back an image that fails its healthcheck (ADR-070, ADR-115). Pull requests run fmt, clippy, the tests under nextest, the DOM suites in their own job, the acceptance suite, the musl build with a 24 MiB cap, and the source Dockerfile only when the diff can change it (ADR-079, ADR-173 to ADR-175).

**Consequences.** Nothing deploys to production automatically; the hand-off is the published release (see [../../workflows/release-process.md](../../workflows/release-process.md)). A broken `main` reaches the dev VM within minutes but never production. The binary size is a CI gate, which shapes dependency choices.

**Status.** Accepted, in force (ADR-063 2026-10-04; ADR-069, ADR-070 2026-10-06; ADR-107, ADR-114, ADR-115 2026-10-09; ADR-173 to ADR-175 2026-10-19).

**Evidence.** `.github/workflows/{ci,edge,release}.yml`, `scripts/edge-tags.sh`, `scripts/image-description.sh`, `deploy/proxmox/`, `Dockerfile.release`.

---
**Sources:** `docs/DECISIONS.md:L731-L749,L807-L821,L1176-L1184,L1234-L1256,L1888-L1911`, `.github/workflows/ci.yml`, `.github/workflows/edge.yml`, `.github/workflows/release.yml`, `deploy/proxmox/README.md`
**Last verified:** 2026-10-19 against commit `52fa67d`

# Architecture decision records

The decisions that shape riot-proxy, one file each, in the Context / Decision / Consequences / Status / Evidence form. They summarise and point at the repository's own numbered record, `docs/DECISIONS.md` (ADR-001 to ADR-175 at the verified commit), which stays the authority.

| File | Decision | Repo ADRs |
|---|---|---|
| [ADR-single-process.md](./ADR-single-process.md) | One process, one static binary, no horizontal scaling | 001, 003 |
| [ADR-sqlite-only-store.md](./ADR-sqlite-only-store.md) | SQLite in WAL mode as the only default store; Postgres as a compile-only seam | 002, 004, 005, 062 |
| [ADR-consumer-keys.md](./ADR-consumer-keys.md) | Consumer keys with scopes and per-minute quotas; the no-auth dev mode | 012, 033, 034, 068 |
| [ADR-sliding-log-limiter.md](./ADR-sliding-log-limiter.md) | Header-driven sliding-log limiter, interactive over bulk, freeze on typed 429 | 021, 023, 024, 025, 026, 089, 126 |
| [ADR-timelines-always-archived.md](./ADR-timelines-always-archived.md) | Fetch and archive a match's timeline by default, together with the match | 085, 098, 118, 133 |
| [ADR-packs-in-object-storage.md](./ADR-packs-in-object-storage.md) | Raw bodies packed into an S3 bucket after a 14-day local window; R2 as the replica | 146, 162, 163, 164, 165 |
| [ADR-bare-metal-systemd.md](./ADR-bare-metal-systemd.md) | Production runs the static binary under systemd on one VPS behind a Cloudflare Tunnel; no Docker, no built-in TLS | 067, 142, 144 |
| [ADR-v1-parity-then-dropped.md](./ADR-v1-parity-then-dropped.md) | Byte-identical v1 headers, codes and metric names, until the owner retired v1 | 011, 014, 065, 066, 123, 152 |
| [ADR-durable-jobs-in-sqlite.md](./ADR-durable-jobs-in-sqlite.md) | A durable job table, lanes and yields instead of a queue broker | 047, 048, 089, 134, 137 |
| [ADR-edge-image-and-release-pipeline.md](./ADR-edge-image-and-release-pipeline.md) | `:edge` on every push to `main`, a tag-driven release, a dev VM that follows `main` | 063, 069, 070, 107, 114, 115, 173, 174, 175 |

Decisions recorded here were evidenced from the code and the history on 2026-10-19; where the repository's ADR and the code differ, the file says so.

---
**Sources:** `docs/DECISIONS.md`, `docs/_meta/scratch/tests-tooling-history.md` §6
**Last verified:** 2026-10-19 against commit `52fa67d`

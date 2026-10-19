# ADR: v1 parity as a constraint, then as a reference

**Context.** v2 was written to replace a running v1 without changing its consumers, dashboards or alerts. On 2026-10-04 the owner retired v1 for good.

**Decision.** The error envelope, the `X-Cache`, `X-Cache-Age`, `X-RateLimit-*` and `X-Request-Id` headers, the error codes and the `proxy_*` metric names are byte-identical to v1's; v2 additions use design names without the prefix (ADR-011, ADR-014). The acceptance suite is v1's black-box suite, run against a mock Riot (ADR-059). From RC-01 on, owner-requested changes may break the v1 contract and are recorded one ADR each: no default platform (ADR-065), account lookups without a region (ADR-066), one row per champion over every tier when no tier is given (ADR-123), the new dashboard (ADR-152). The contract artefacts in `docs/contract/` remain for the parity tests that still run.

**Consequences.** Consumers written for v1 largely work unchanged; the site was still regenerated from the 2.6.0 document. `CLAUDE.md`'s "byte-identical" rule still binds codes, headers and metric names unless the owner asks otherwise. The `cache` metric label and the `:param` route form are v1's even where v2's router would do otherwise (`src/app.rs:174-186`).

**Status.** Parity dropped as a constraint on 2026-10-04 (ADR-065); still the default where nothing asks for a change.

**Evidence.** `docs/DECISIONS.md` ADR-011, 014, 059, 065, 066, 123, 152; `acceptance/`; `scripts/compare-openapi.py`; `src/riot/endpoints.rs:544-554` (parity test).

---
**Sources:** `docs/DECISIONS.md:L57-L67,L87-L93,L639-L668,L760-L791,L1324-L1334,L1609-L1617`, `src/app.rs:L174-L186`, `src/riot/endpoints.rs:L544-L554`
**Last verified:** 2026-10-19 against commit `52fa67d`

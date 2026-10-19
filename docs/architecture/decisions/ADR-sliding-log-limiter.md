# ADR: a header-driven sliding-log limiter

**Context.** Riot publishes the live limits on every response (`X-App-Rate-Limit`, `X-Method-Rate-Limit`, their `-Count` twins) and expects clients to stay under them; an accountable 429 counts against the key. Background work (crawls, backfills) must never starve a person's request.

**Decision.** Windows are sliding logs of admission instants, as in v1 (ADR-023), reconfigured from the headers after every response and padded upward from the counts (ADR-024). A new scope starts with the personal-key bootstrap `20:1,100:120`. Acquire fails fast when the next free slot is beyond the caller's budget rather than waiting it out (ADR-025). Any typed 429 with a numeric `Retry-After` freezes the whole scope. Interactive callers have a 2 s budget; bulk callers may use 80 % of a window while anyone is active and 95 % once a scope has been idle for 30 s (ADR-126), and stand aside while an interactive request waits. The untyped service-429 backoff uses v1's numbers (ADR-021). The windows are checkpointed to SQLite every 10 s so a restart cannot over-admit (ADR-026).

**Consequences.** The limiter is exact, not approximate, at the cost of a `VecDeque` per window. Jobs that would wait yield their worker (ADR-089) rather than hold it. Where the code moved past `docs/design/05-rate-limiter.md` (backoff in the fetcher, one `Notify`, every typed 429 freezes), the design's "As built" section records it.

**Status.** Accepted, in force (ADR-021 to ADR-026 2026-09-25; ADR-089 2026-10-09; ADR-126 2026-10-11).

**Evidence.** `src/riot/limiter/{mod,bucket,headers,persist}.rs`, `src/riot/limiter/tests.rs` (24 ported v1 cases), `src/riot/limiter/proptest.rs`, `tests/limiter_restart.rs`, `tests/lanes.rs`.

---
**Sources:** `docs/DECISIONS.md:L132-L161,L1001-L1013,L1352-L1361`, `src/riot/limiter/mod.rs:L262-L300,L370-L431`, `src/riot/limiter/headers.rs:L30-L41`, `src/riot/limiter/persist.rs:L21-L25`, `docs/design/05-rate-limiter.md` §As built
**Last verified:** 2026-10-19 against commit `52fa67d`

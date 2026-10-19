# ADR: consumer keys, scopes and quotas

**Context.** Downstream services (the site, a dashboard operator) must reach the proxy without ever seeing the Riot key, and must be identifiable and limitable individually.

**Decision.** Each consumer gets a key `rpx_` + 24 random bytes in base64url (36 characters), shown once; only its SHA-256 is stored (`src/consumers.rs:12,95-107`). Keys carry scopes `read` and `admin` and a per-minute quota (default 600). Keys are sent as `Authorization: Bearer` or, for the WebSocket handshake and in fact every guarded route, as `?token=` (`src/http/auth.rs:141-156`). Lookups are cached 60 s (30 s for unknown keys); revocation is a soft delete that also drops the cache entry and closes the consumer's sockets with 4401 (`src/http/auth.rs:29-32`, `src/routes/admin.rs:248-270`). Admin routes additionally require the source IP to pass `ADMIN_IP_ALLOWLIST`, with `TRUST_PROXY` off by default so a forged `X-Forwarded-For` cannot pass it (ADR-068). `AUTH_DISABLED=true` runs every request as a synthetic `dev-local` consumer and is refused in production (`src/config.rs:658-662`). A first admin key is minted and printed once on first boot unless `BOOTSTRAP_ADMIN_KEY` is set.

**Consequences.** Key rotation is mint, switch, revoke. The site holds a `read` key; the owner's `admin-main` key drives the dashboard. Quota exhaustion is `429 QUOTA_EXCEEDED` and is never confused with Riot's limits (`503 RATE_LIMITED`). Because `?token=` is accepted everywhere, keys can appear in access logs of any proxy in front; on production the only hop is the Cloudflare Tunnel.

**Status.** Accepted, in force (ADR-012 2026-09-23; ADR-033, ADR-034; ADR-068 2026-10-05).

**Evidence.** `src/consumers.rs`, `src/http/auth.rs`, `src/http/quota.rs`, `src/cli/key.rs`, `tests/auth.rs`, `tests/cli.rs`.

---
**Sources:** `docs/DECISIONS.md:L68-L78,L212-L229,L800-L806`, `src/consumers.rs:L12,L95-L107`, `src/http/auth.rs:L29-L32,L141-L156`, `src/routes/admin.rs:L248-L270`, `src/config.rs:L658-L662`
**Last verified:** 2026-10-19 against commit `52fa67d`

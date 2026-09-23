-- P0-04: operational tables only. Archive tables arrive in V0002 (P5-01).
-- DDL verbatim from docs/design/04-data-and-cache.md §Schema. Timestamps are unix ms.

-- Consumers & auth ─────────────────────────────────────────────────────────
CREATE TABLE consumers (
  id            TEXT PRIMARY KEY,                 -- ulid
  name          TEXT NOT NULL UNIQUE,
  key_sha256    BLOB NOT NULL UNIQUE,             -- plaintext printed once, never stored
  scopes        TEXT NOT NULL,                    -- JSON array: ["read"] | ["read","admin"]
  quota_per_min INTEGER NOT NULL DEFAULT 600,
  created_at    INTEGER NOT NULL,
  revoked_at    INTEGER
);

-- Operational ───────────────────────────────────────────────────────────────
CREATE TABLE cache (                              -- L2, expensive tiers only
  key          TEXT PRIMARY KEY,                  -- already includes key_scope
  body         BLOB NOT NULL,
  status       INTEGER NOT NULL,                  -- 200 or 404 (negative)
  content_at   INTEGER NOT NULL,                  -- for X-Cache-Age (content, not fetch)
  soft_expires INTEGER NOT NULL,
  hard_expires INTEGER NOT NULL
);
CREATE INDEX cache_hard ON cache(hard_expires);

CREATE TABLE limiter_state (
  scope        TEXT PRIMARY KEY,                  -- "app:euw1" | "method:euw1:match.byId" | …
  windows      TEXT NOT NULL,                     -- JSON [{limit, seconds, count, reset_at}]
  frozen_until INTEGER,
  updated_at   INTEGER NOT NULL
);

CREATE TABLE jobs (
  id          TEXT PRIMARY KEY,                   -- ulid
  kind        TEXT NOT NULL,                      -- "archive:match" …
  dedupe_key  TEXT,                               -- UNIQUE while pending/running
  priority    INTEGER NOT NULL,                   -- lower runs first; see design 06
  payload     TEXT NOT NULL,                      -- JSON
  state       TEXT NOT NULL DEFAULT 'pending',    -- pending | running | done | failed
  attempts    INTEGER NOT NULL DEFAULT 0,
  run_after   INTEGER NOT NULL,                   -- backoff
  claimed_at  INTEGER, finished_at INTEGER, error TEXT
);
CREATE UNIQUE INDEX jobs_dedupe ON jobs(kind, dedupe_key) WHERE state IN ('pending','running');
CREATE INDEX jobs_claim ON jobs(state, run_after, priority) WHERE state = 'pending';

CREATE TABLE metrics_history (                    -- 1440 rows ≈ 24 h at 60 s; maintenance trims
  at    INTEGER PRIMARY KEY,
  point TEXT NOT NULL                             -- JSON snapshot
);

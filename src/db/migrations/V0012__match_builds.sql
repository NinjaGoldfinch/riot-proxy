-- BLD-01 (ADR-117): what each player bought and levelled, derived from the
-- archived timeline and the mirrored item.json by `builds:extract`. The key
-- scope is the one that owns the match's facts. A row below the current
-- BUILDS_VERSION is rewritten by the next extraction.
CREATE TABLE match_builds (
  match_id TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope TEXT NOT NULL,
  puuid TEXT NOT NULL,
  starter TEXT NOT NULL,                            -- JSON: item ids bought before 60 s, trinkets left out, sorted
  boots INTEGER,                                    -- the first boots bought, tier 2 or an upgrade
  items TEXT NOT NULL,                              -- JSON: finished items in purchase order, ≤ 6
  skills TEXT NOT NULL,                             -- the first 15 level-ups, "QWEQQRQ…"
  skill_order TEXT,                                 -- Q, W and E in the order they were maxed, "QWE"
  builds_version INTEGER NOT NULL,
  PRIMARY KEY (match_id, key_scope, puuid)
);

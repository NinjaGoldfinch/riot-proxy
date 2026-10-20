-- THR-03 (ADR-182): incremental analytics. Which rebuild steps count each
-- match, what a step counted for a player whose tier has moved since, and
-- which ladders' tables agree with their stamps. Empty tables, so the
-- migration reads no data: the first full rebuild of each ladder stamps it.

-- One bit per rebuild step (src/archive/analytics.rs STEPS): 1 champions,
-- 2 matchups, 4 items, 8 runes, 16 spells, 32 sets; 63 is counted. A match's
-- row is written, at 0, with its facts; `aggregate:delta` counts what is
-- missing.
CREATE TABLE analytics_counted (
  match_id  TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope TEXT NOT NULL,
  bits      INTEGER NOT NULL,
  PRIMARY KEY (match_id, key_scope)
) WITHOUT ROWID;
CREATE INDEX analytics_uncounted ON analytics_counted (key_scope, match_id) WHERE bits <> 63;

-- One row per step that counted a player whose tier then moved, with the tier
-- it counted them at: the next delta takes that count back.
CREATE TABLE analytics_retract (
  match_id  TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope TEXT NOT NULL,
  puuid     TEXT NOT NULL,
  step      INTEGER NOT NULL,                 -- one bit
  tier      TEXT,                             -- the tier that step counted the player at
  PRIMARY KEY (match_id, key_scope, puuid, step)
) WITHOUT ROWID;

-- A ladder takes deltas once a full rebuild has stamped it (`stamped_at`) and
-- nothing has rewritten a counted match's inputs since (`dirty_at` NULL).
CREATE TABLE analytics_stamped (
  key_scope  TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  stamped_at INTEGER,                         -- when the last full rebuild that stamped it started
  dirty_at   INTEGER,                         -- when a counted match was rewritten since
  PRIMARY KEY (key_scope, platform, queue)
) WITHOUT ROWID;

-- Every path that moves a counted player's tier (late_stamp, tiers::stamp)
-- records what each counting step saw. OR IGNORE keeps the first: a tier that
-- moves twice was counted at the oldest.
CREATE TRIGGER match_facts_tier_moved AFTER UPDATE OF tier ON match_facts
  WHEN OLD.tier IS NOT NEW.tier
BEGIN
  INSERT OR IGNORE INTO analytics_retract (match_id, key_scope, puuid, step, tier)
  SELECT OLD.match_id, OLD.key_scope, OLD.puuid, s.bit, OLD.tier
    FROM analytics_counted c
    JOIN (SELECT 1 AS bit UNION ALL SELECT 2 UNION ALL SELECT 4 UNION ALL SELECT 8
          UNION ALL SELECT 16 UNION ALL SELECT 32) s ON c.bits & s.bit
   WHERE c.match_id = OLD.match_id AND c.key_scope = OLD.key_scope;
END;

-- A delta's last run per ladder beside the rebuild's (eight rows at most).
CREATE TABLE analytics_runs_v29 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'rebuild',      -- rebuild | delta
  at INTEGER NOT NULL,                       -- when the run ended, unix ms
  status TEXT NOT NULL,                      -- completed | failed
  ms INTEGER NOT NULL,                       -- wall-clock length
  steps TEXT NOT NULL,                       -- JSON {step: seconds}, the steps that finished
  rows TEXT NOT NULL,                        -- JSON {table: rows written}
  games INTEGER NOT NULL,                    -- games in the ladder's stats; 0 for a failed run
  PRIMARY KEY (key_scope, platform, queue, kind)
);
INSERT INTO analytics_runs_v29 (key_scope, platform, queue, kind, at, status, ms, steps, rows, games)
SELECT key_scope, platform, queue, 'rebuild', at, status, ms, steps, rows, games FROM analytics_runs;
DROP TABLE analytics_runs;
ALTER TABLE analytics_runs_v29 RENAME TO analytics_runs;

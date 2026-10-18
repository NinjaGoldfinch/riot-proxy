-- FLT-04 (ADR-171): set builds against one lane opponent. A row is a laner's
-- build (match_builds, two or more finished items) in a lane held by one
-- player per team, the opponent another champion: champion_matchups' rule
-- (ADR-101, ADR-129), so a shared lane and a mirror give none. Keyed as
-- champion_matchups up to the opponent and as champion_builds after it, tier
-- and side last (ADR-129), so a read for one champion, lane and opponent seeks
-- through the remake. Only the 3rd item, boots, runes and spells are kept as
-- parts, to hold the rows down. Filled by rebuild_builds in the set builds'
-- transaction; not backfilled: the next rebuild fills the patches it covers.
CREATE TABLE champion_matchup_builds (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,  -- a lane: TOP … UTILITY, never ''
  opponent_id INTEGER NOT NULL,                      -- the other team's laner, never champion_id
  remake INTEGER NOT NULL,
  core TEXT NOT NULL,                                -- JSON: the first two finished items, "[a,b]"
  tier TEXT NOT NULL, side TEXT NOT NULL,            -- champion_id's player's
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, tier, side)
) WITHOUT ROWID;

CREATE TABLE champion_matchup_build_parts (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, opponent_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,
  part TEXT NOT NULL,   -- item3 | boots | runes | spells
  value TEXT NOT NULL,  -- an item id; "keystone:subStyle"; "a:b" with a <= b
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, core, part, value, tier, side)
) WITHOUT ROWID;

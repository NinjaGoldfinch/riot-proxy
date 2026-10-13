-- DSK-06 (ADR-140): the derived tables whose primary key is most or all of
-- the row are rebuilt WITHOUT ROWID. A rowid table keeps such a key twice:
-- once in the row and once in the automatic index that enforces it. On the
-- VPS those indexes were 460 MiB (match_builds 164, match_bans 63, the
-- analytics tables 234). A WITHOUT ROWID table is the key's b-tree itself.
--
-- Each table is copied in key order (V0017's pattern), so the new b-tree is
-- packed; the old pages go to the free list. Only champion_stats has a
-- secondary index; it is recreated and now carries the key's remaining
-- columns instead of a rowid, a few MiB. The writers delete and insert by
-- key already, so no code changes.

CREATE TABLE match_builds_v20 (
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
) WITHOUT ROWID;
INSERT INTO match_builds_v20 (match_id, key_scope, puuid, starter, boots, items, skills, skill_order, builds_version)
SELECT match_id, key_scope, puuid, starter, boots, items, skills, skill_order, builds_version
  FROM match_builds ORDER BY match_id, key_scope, puuid;
DROP TABLE match_builds;
ALTER TABLE match_builds_v20 RENAME TO match_builds;

CREATE TABLE match_bans_v20 (
  match_id TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  team_id INTEGER NOT NULL,
  pick_turn INTEGER NOT NULL,
  champion_id INTEGER NOT NULL,
  PRIMARY KEY (match_id, team_id, pick_turn)
) WITHOUT ROWID;
INSERT INTO match_bans_v20 (match_id, team_id, pick_turn, champion_id)
SELECT match_id, team_id, pick_turn, champion_id
  FROM match_bans ORDER BY match_id, team_id, pick_turn;
DROP TABLE match_bans;
ALTER TABLE match_bans_v20 RENAME TO match_bans;

CREATE TABLE analytics_slices_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, remake INTEGER NOT NULL,
  matches INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, remake)
) WITHOUT ROWID;
INSERT INTO analytics_slices_v20 (key_scope, platform, queue, tier, patch, remake, matches, computed_at)
SELECT key_scope, platform, queue, tier, patch, remake, matches, computed_at
  FROM analytics_slices ORDER BY key_scope, platform, queue, tier, patch, remake;
DROP TABLE analytics_slices;
ALTER TABLE analytics_slices_v20 RENAME TO analytics_slices;

CREATE TABLE analytics_match_totals_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  patch TEXT NOT NULL, remake INTEGER NOT NULL,
  matches INTEGER NOT NULL,                          -- distinct matches, every tier
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, remake)
) WITHOUT ROWID;
INSERT INTO analytics_match_totals_v20 (key_scope, platform, queue, patch, remake, matches, computed_at)
SELECT key_scope, platform, queue, patch, remake, matches, computed_at
  FROM analytics_match_totals ORDER BY key_scope, platform, queue, patch, remake;
DROP TABLE analytics_match_totals;
ALTER TABLE analytics_match_totals_v20 RENAME TO analytics_match_totals;

CREATE TABLE champion_stats_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, champion_id INTEGER NOT NULL,
  role TEXT NOT NULL,                                -- teamPosition, '' where Riot sends none
  remake INTEGER NOT NULL,
  side TEXT NOT NULL,                                -- blue | red; '' not split yet (V0017)
  games INTEGER NOT NULL, wins INTEGER NOT NULL,
  matches_picked INTEGER NOT NULL,                   -- distinct matches
  stated_games INTEGER NOT NULL,                     -- games with a K/D/A: the averages' denominator
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, assists INTEGER NOT NULL,
  cs INTEGER NOT NULL, gold INTEGER NOT NULL, damage INTEGER NOT NULL, vision INTEGER NOT NULL,
  duration_s INTEGER NOT NULL,                       -- length of the stated games
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, champion_id, role, remake, side)
) WITHOUT ROWID;
INSERT INTO champion_stats_v20 (key_scope, platform, queue, tier, patch, champion_id, role, remake, side, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at)
SELECT key_scope, platform, queue, tier, patch, champion_id, role, remake, side, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at
  FROM champion_stats ORDER BY key_scope, platform, queue, tier, patch, champion_id, role, remake, side;
DROP TABLE champion_stats;
ALTER TABLE champion_stats_v20 RENAME TO champion_stats;
CREATE INDEX champion_stats_slice ON champion_stats (key_scope, platform, queue, patch, tier, games);

CREATE TABLE champion_bans_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, champion_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  bans INTEGER NOT NULL,                             -- distinct matches it was banned in
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, champion_id, remake)
) WITHOUT ROWID;
INSERT INTO champion_bans_v20 (key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at)
SELECT key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at
  FROM champion_bans ORDER BY key_scope, platform, queue, tier, patch, champion_id, remake;
DROP TABLE champion_bans;
ALTER TABLE champion_bans_v20 RENAME TO champion_bans;

CREATE TABLE champion_ban_totals_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  patch TEXT NOT NULL, champion_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  bans INTEGER NOT NULL,                             -- distinct matches it was banned in, every tier
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, remake)
) WITHOUT ROWID;
INSERT INTO champion_ban_totals_v20 (key_scope, platform, queue, patch, champion_id, remake, bans, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, remake, bans, computed_at
  FROM champion_ban_totals ORDER BY key_scope, platform, queue, patch, champion_id, remake;
DROP TABLE champion_ban_totals;
ALTER TABLE champion_ban_totals_v20 RENAME TO champion_ban_totals;

CREATE TABLE champion_matchups_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, opponent_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_matchups_v20 (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side, games, wins, computed_at
  FROM champion_matchups ORDER BY key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side;
DROP TABLE champion_matchups;
ALTER TABLE champion_matchups_v20 RENAME TO champion_matchups;

CREATE TABLE champion_items_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, item_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_items_v20 (key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side, games, wins, computed_at
  FROM champion_items ORDER BY key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side;
DROP TABLE champion_items;
ALTER TABLE champion_items_v20 RENAME TO champion_items;

CREATE TABLE champion_runes_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  keystone_id INTEGER NOT NULL, sub_style_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_runes_v20 (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side, games, wins, computed_at
  FROM champion_runes ORDER BY key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side;
DROP TABLE champion_runes;
ALTER TABLE champion_runes_v20 RENAME TO champion_runes;

CREATE TABLE champion_spells_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  spell_a INTEGER NOT NULL, spell_b INTEGER NOT NULL,  -- spell_a <= spell_b
  remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_spells_v20 (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side, games, wins, computed_at
  FROM champion_spells ORDER BY key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side;
DROP TABLE champion_spells;
ALTER TABLE champion_spells_v20 RENAME TO champion_spells;

CREATE TABLE champion_builds_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,                                -- JSON: the first two finished items, "[a,b]"
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_builds_v20 (key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side, games, wins, computed_at
  FROM champion_builds ORDER BY key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side;
DROP TABLE champion_builds;
ALTER TABLE champion_builds_v20 RENAME TO champion_builds;

CREATE TABLE champion_build_parts_v20 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,
  part TEXT NOT NULL,   -- item3 | item4 | item5 | starter | boots | skill_order | runes | spells
  value TEXT NOT NULL,  -- an item id; starter's JSON; "QWE"; "keystone:subStyle"; "a:b" with a <= b
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side)
) WITHOUT ROWID;
INSERT INTO champion_build_parts_v20 (key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side, games, wins, computed_at
  FROM champion_build_parts ORDER BY key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side;
DROP TABLE champion_build_parts;
ALTER TABLE champion_build_parts_v20 RENAME TO champion_build_parts;

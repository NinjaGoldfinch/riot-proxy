-- FLT-01 (ADR-129): the per-participant analytics tables keyed by the row's
-- own player's tier (`match_tiers`, THR-02) and side ('blue' for team 100,
-- 'red' for team 200), so FLT-02 can filter builds and matchups by them.
-- Per-participant rows are disjoint across tiers and sides, so a read that
-- names neither sums them exactly. `champion_stats` had a tier already and
-- gains the side. Both go last in each key, so the reads, which name neither
-- yet, keep their key prefixes and their query plans.
--
-- No backfill from the facts: the stamps a split needs may not exist yet
-- (`tiers:backfill` runs after boot). Each table's rows are copied as they
-- are, with tier and side '' ("not split yet"). Reads sum over both, so
-- every route answers as before. Each ladder that had rows is listed in
-- `analytics_unsplit`: its next rebuild covers every patch, whatever
-- `AGGREGATE_PATCH_LIMIT` says, which replaces them all, and then takes the
-- ladder off the list. Boot queues that rebuild for each listed ladder.

CREATE TABLE analytics_unsplit (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  PRIMARY KEY (key_scope, platform, queue)
);
INSERT INTO analytics_unsplit (key_scope, platform, queue)
SELECT key_scope, platform, queue FROM champion_stats
UNION SELECT key_scope, platform, queue FROM champion_matchups
UNION SELECT key_scope, platform, queue FROM champion_items
UNION SELECT key_scope, platform, queue FROM champion_runes
UNION SELECT key_scope, platform, queue FROM champion_spells
UNION SELECT key_scope, platform, queue FROM champion_builds
UNION SELECT key_scope, platform, queue FROM champion_build_parts;

CREATE TABLE champion_stats_v17 (
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
);
INSERT INTO champion_stats_v17 (key_scope, platform, queue, tier, patch, champion_id, role, remake, side,
  games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s,
  computed_at)
SELECT key_scope, platform, queue, tier, patch, champion_id, role, remake, '',
  games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s,
  computed_at
  FROM champion_stats;
DROP TABLE champion_stats;
ALTER TABLE champion_stats_v17 RENAME TO champion_stats;
CREATE INDEX champion_stats_slice ON champion_stats (key_scope, platform, queue, patch, tier, games);

-- A matchup's tier and side are its own player's (`champion_id`'s); the
-- opponent's tier may differ.
CREATE TABLE champion_matchups_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, opponent_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, tier, side)
);
INSERT INTO champion_matchups_v17 (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake,
  tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, opponent_id, remake, '', '', games, wins, computed_at
  FROM champion_matchups;
DROP TABLE champion_matchups;
ALTER TABLE champion_matchups_v17 RENAME TO champion_matchups;

CREATE TABLE champion_items_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, item_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, item_id, remake, tier, side)
);
INSERT INTO champion_items_v17 (key_scope, platform, queue, patch, champion_id, role, item_id, remake,
  tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, item_id, remake, '', '', games, wins, computed_at
  FROM champion_items;
DROP TABLE champion_items;
ALTER TABLE champion_items_v17 RENAME TO champion_items;

CREATE TABLE champion_runes_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  keystone_id INTEGER NOT NULL, sub_style_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, tier, side)
);
INSERT INTO champion_runes_v17 (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id,
  remake, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake, '', '',
  games, wins, computed_at
  FROM champion_runes;
DROP TABLE champion_runes;
ALTER TABLE champion_runes_v17 RENAME TO champion_runes;

CREATE TABLE champion_spells_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  spell_a INTEGER NOT NULL, spell_b INTEGER NOT NULL,  -- spell_a <= spell_b
  remake INTEGER NOT NULL,
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, tier, side)
);
INSERT INTO champion_spells_v17 (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake,
  tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake, '', '',
  games, wins, computed_at
  FROM champion_spells;
DROP TABLE champion_spells;
ALTER TABLE champion_spells_v17 RENAME TO champion_spells;

CREATE TABLE champion_builds_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,                                -- JSON: the first two finished items, "[a,b]"
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core, tier, side)
);
INSERT INTO champion_builds_v17 (key_scope, platform, queue, patch, champion_id, role, remake, core,
  tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, remake, core, '', '', games, wins, computed_at
  FROM champion_builds;
DROP TABLE champion_builds;
ALTER TABLE champion_builds_v17 RENAME TO champion_builds;

CREATE TABLE champion_build_parts_v17 (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,
  part TEXT NOT NULL,   -- item3 | item4 | item5 | starter | boots | skill_order | runes | spells
  value TEXT NOT NULL,  -- an item id; starter's JSON; "QWE"; "keystone:subStyle"; "a:b" with a <= b
  tier TEXT NOT NULL, side TEXT NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, tier, side)
);
INSERT INTO champion_build_parts_v17 (key_scope, platform, queue, patch, champion_id, role, remake, core,
  part, value, tier, side, games, wins, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, remake, core, part, value, '', '',
  games, wins, computed_at
  FROM champion_build_parts;
DROP TABLE champion_build_parts;
ALTER TABLE champion_build_parts_v17 RENAME TO champion_build_parts;

-- P7-04 (owner decisions, ADR-056): v1's analytics in place of design/04's
-- three tables, and remakes kept apart so they can be counted or not.
--
-- Facts gain what v1's aggregates read (gold, damage, vision; bans per match)
-- and the match whether Riot flagged it a remake (any participant's
-- `gameEndedInEarlySurrender`). Both are filled on archive and by
-- facts:reextract (FACTS_VERSION 3); `remake` is NULL until then.
--
-- Every aggregate carries `remake` (0 or 1) as a key: a query excluding
-- remakes reads the 0 rows, one including them sums both. Nothing wrote
-- V0002's analytics tables, so they are replaced.
ALTER TABLE matches ADD COLUMN remake INTEGER;
-- The FACTS_VERSION that derived this match's rows; NULL before V0005.
-- facts:reextract walks the matches below the current version by this index,
-- and needs no cursor: a match it has done no longer matches.
ALTER TABLE matches ADD COLUMN facts_version INTEGER;
CREATE INDEX matches_facts_version ON matches (facts_version);
ALTER TABLE match_facts ADD COLUMN gold INTEGER;       -- goldEarned
ALTER TABLE match_facts ADD COLUMN damage INTEGER;     -- totalDamageDealtToChampions
ALTER TABLE match_facts ADD COLUMN vision INTEGER;     -- visionScore

CREATE TABLE match_bans (
  match_id TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  team_id INTEGER NOT NULL,
  pick_turn INTEGER NOT NULL,
  champion_id INTEGER NOT NULL,
  PRIMARY KEY (match_id, team_id, pick_turn)
);

DROP TABLE champion_stats;
DROP TABLE champion_matchups;
DROP TABLE champion_builds;

-- Distinct matches with a participant the ladder places at the tier: the
-- pick- and ban-rate denominator (v1).
CREATE TABLE analytics_slices (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, remake INTEGER NOT NULL,
  matches INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, remake)
);

CREATE TABLE champion_stats (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, champion_id INTEGER NOT NULL,
  role TEXT NOT NULL,                                -- teamPosition, '' where Riot sends none
  remake INTEGER NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL,
  matches_picked INTEGER NOT NULL,                   -- distinct matches
  stated_games INTEGER NOT NULL,                     -- games with a K/D/A: the averages' denominator
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, assists INTEGER NOT NULL,
  cs INTEGER NOT NULL, gold INTEGER NOT NULL, damage INTEGER NOT NULL, vision INTEGER NOT NULL,
  duration_s INTEGER NOT NULL,                       -- length of the stated games
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, champion_id, role, remake)
);
CREATE INDEX champion_stats_slice ON champion_stats (key_scope, platform, queue, patch, tier, games);

CREATE TABLE champion_bans (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  tier TEXT NOT NULL, patch TEXT NOT NULL, champion_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  bans INTEGER NOT NULL,                             -- distinct matches it was banned in
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, tier, patch, champion_id, remake)
);

-- No tier: a lane's two players can sit in different tiers (v1).
CREATE TABLE champion_matchups (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, opponent_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake)
);

CREATE TABLE champion_items (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, item_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, item_id, remake)
);

CREATE TABLE champion_runes (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  keystone_id INTEGER NOT NULL, sub_style_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, keystone_id, sub_style_id, remake)
);

CREATE TABLE champion_spells (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL,
  spell_a INTEGER NOT NULL, spell_b INTEGER NOT NULL,  -- spell_a <= spell_b
  remake INTEGER NOT NULL,
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b, remake)
);

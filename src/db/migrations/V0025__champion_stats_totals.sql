-- PERF-02 (ADR-168): champion_stats summed over tier, for the reads that
-- name no tier (/champions, a champion's summed row, /patches). Keyed as
-- champion_stats without its tier, so a read across every platform and
-- patch sums a fraction of the rows it used to. matches_picked is a plain
-- sum, as the tierless read computed it (ADR-123): the pick rate's
-- denominator comes from analytics_match_totals, not from this column.
--
-- Filled here from champion_stats in key order (V0017's pattern), so the
-- b-tree is packed; rebuild_champions rewrites it in the same transaction
-- as champion_stats, for the same patches.
--
-- champion_stats_slice (V0005) goes: every read seeks on the key since
-- PERF-01 (ADR-167), and with statistics the planner chose it, non-covering,
-- for a full read and ran 3x slower (ADR-166).

CREATE TABLE champion_stats_totals (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  patch TEXT NOT NULL, champion_id INTEGER NOT NULL,
  role TEXT NOT NULL,                                -- teamPosition, '' where Riot sends none
  remake INTEGER NOT NULL,
  side TEXT NOT NULL,                                -- blue | red; '' not split yet (V0017)
  games INTEGER NOT NULL, wins INTEGER NOT NULL,
  matches_picked INTEGER NOT NULL,                   -- summed over tier, as the tierless read did
  stated_games INTEGER NOT NULL,                     -- games with a K/D/A: the averages' denominator
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, assists INTEGER NOT NULL,
  cs INTEGER NOT NULL, gold INTEGER NOT NULL, damage INTEGER NOT NULL, vision INTEGER NOT NULL,
  duration_s INTEGER NOT NULL,                       -- length of the stated games
  computed_at INTEGER NOT NULL,                      -- the newest of the tiers' rows
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, side)
) WITHOUT ROWID;
INSERT INTO champion_stats_totals (key_scope, platform, queue, patch, champion_id, role, remake, side,
  games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
  duration_s, computed_at)
SELECT key_scope, platform, queue, patch, champion_id, role, remake, side,
  sum(games), sum(wins), sum(matches_picked), sum(stated_games), sum(kills), sum(deaths), sum(assists),
  sum(cs), sum(gold), sum(damage), sum(vision), sum(duration_s), max(computed_at)
  FROM champion_stats
 GROUP BY key_scope, platform, queue, patch, champion_id, role, remake, side
 ORDER BY key_scope, platform, queue, patch, champion_id, role, remake, side;
DROP INDEX champion_stats_slice;

-- FLT-03 (ADR-170): the pick- and ban-rate denominators of a tier range
-- (`minTier`). `analytics_slices` and `champion_bans` count a match once in
-- each tier it had a player in, so their sums over a range overcount, as
-- their sums over every tier did before V0015 (ADR-123). These count a match
-- once per floor: in every floor at or below its highest stamped tier.
-- UNKNOWN is no floor and raises none, so a match whose players are all
-- UNKNOWN is in no floor's total. floor_tier sits before patch, as tier does
-- in analytics_slices, so a read (one floor, by equality) seeks through
-- patch. Filled by rebuild_champions in its transaction from the same facts.
CREATE TABLE analytics_floor_totals (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  floor_tier TEXT NOT NULL,                          -- IRON … CHALLENGER: this tier and every one above
  patch TEXT NOT NULL, remake INTEGER NOT NULL,
  matches INTEGER NOT NULL,                          -- distinct matches with a player at or above floor_tier
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, floor_tier, patch, remake)
) WITHOUT ROWID;

CREATE TABLE champion_ban_floor_totals (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  floor_tier TEXT NOT NULL, patch TEXT NOT NULL, champion_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  bans INTEGER NOT NULL,                             -- distinct matches of the floor it was banned in
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, floor_tier, patch, champion_id, remake)
) WITHOUT ROWID;

-- Backfill every slice already aggregated, as rebuild_champions would count
-- it now (V0015's pattern): platform from the match id, queue from its queue
-- id, only matches archived by the slice's recompute (whose time is kept), a
-- fact counted as ladder_facts counts it (stamped, team 100 or 200). The tier
-- is the fact's stamp (V0019, DSK-04). Each match is reduced to its highest
-- floor once (v26_top); counts are taken per highest floor and only then
-- spread over the floors at or below it, so the big GROUP BY sorts per match
-- and champion, not per floor (5x faster on the synthetic archive, ADR-170).
CREATE TEMP TABLE v26_top AS
WITH floors(rank, tier) AS (VALUES (0, 'IRON'), (1, 'BRONZE'), (2, 'SILVER'), (3, 'GOLD'), (4, 'PLATINUM'),
       (5, 'EMERALD'), (6, 'DIAMOND'), (7, 'MASTER'), (8, 'GRANDMASTER'), (9, 'CHALLENGER')),
s AS (
  SELECT key_scope, platform, queue, patch, remake, max(computed_at) AS computed_at
    FROM analytics_slices GROUP BY key_scope, platform, queue, patch, remake
),
f AS (
  SELECT f.key_scope, lower(substr(m.match_id, 1, instr(m.match_id, '_') - 1)) AS platform,
    CASE m.queue_id WHEN 420 THEN 'RANKED_SOLO_5x5' WHEN 440 THEN 'RANKED_FLEX_SR' END AS queue,
    m.patch, coalesce(m.remake, 0) AS remake, m.match_id, m.archived_at, max(fl.rank) AS rank
    FROM match_facts f JOIN matches m ON m.match_id = f.match_id JOIN floors fl ON fl.tier = f.tier
   WHERE m.queue_id IN (420, 440) AND m.patch IS NOT NULL AND f.team_id IN (100, 200)
   GROUP BY f.key_scope, m.match_id
)
SELECT f.key_scope, f.platform, f.queue, f.patch, f.remake, f.match_id, f.rank, s.computed_at
  FROM f JOIN s ON s.key_scope = f.key_scope AND s.platform = f.platform AND s.queue = f.queue
   AND s.patch = f.patch AND s.remake = f.remake AND f.archived_at <= s.computed_at;

WITH floors(rank, tier) AS (VALUES (0, 'IRON'), (1, 'BRONZE'), (2, 'SILVER'), (3, 'GOLD'), (4, 'PLATINUM'),
       (5, 'EMERALD'), (6, 'DIAMOND'), (7, 'MASTER'), (8, 'GRANDMASTER'), (9, 'CHALLENGER')),
g AS MATERIALIZED (
  SELECT key_scope, platform, queue, patch, remake, rank, computed_at, count(*) AS n
    FROM temp.v26_top GROUP BY key_scope, platform, queue, patch, remake, rank
)
INSERT INTO analytics_floor_totals (key_scope, platform, queue, floor_tier, patch, remake, matches, computed_at)
SELECT g.key_scope, g.platform, g.queue, fl.tier, g.patch, g.remake, sum(g.n), max(g.computed_at)
  FROM g JOIN floors fl ON fl.rank <= g.rank
 GROUP BY g.key_scope, g.platform, g.queue, fl.tier, g.patch, g.remake;

WITH floors(rank, tier) AS (VALUES (0, 'IRON'), (1, 'BRONZE'), (2, 'SILVER'), (3, 'GOLD'), (4, 'PLATINUM'),
       (5, 'EMERALD'), (6, 'DIAMOND'), (7, 'MASTER'), (8, 'GRANDMASTER'), (9, 'CHALLENGER')),
g AS MATERIALIZED (
  SELECT t.key_scope, t.platform, t.queue, t.patch, b.champion_id, t.remake, t.rank, t.computed_at,
    count(DISTINCT t.match_id) AS n
    FROM temp.v26_top t JOIN match_bans b ON b.match_id = t.match_id
   GROUP BY t.key_scope, t.platform, t.queue, t.patch, b.champion_id, t.remake, t.rank
)
INSERT INTO champion_ban_floor_totals (key_scope, platform, queue, floor_tier, patch, champion_id, remake, bans, computed_at)
SELECT g.key_scope, g.platform, g.queue, fl.tier, g.patch, g.champion_id, g.remake, sum(g.n), max(g.computed_at)
  FROM g JOIN floors fl ON fl.rank <= g.rank
 GROUP BY g.key_scope, g.platform, g.queue, fl.tier, g.patch, g.champion_id, g.remake;

DROP TABLE temp.v26_top;

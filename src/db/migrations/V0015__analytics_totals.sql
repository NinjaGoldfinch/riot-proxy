-- SITE-08 (ADR-123): the pick- and ban-rate denominators of a champion row
-- summed over every tier. `analytics_slices` and `champion_bans` count a match
-- once in each tier it had a player in, so their sums over tiers overcount;
-- these count it once. Filled by `rebuild_champions` from the same facts,
-- grouped without the tier. Every column is a plain sum over disjoint matches,
-- like the other analytics tables.
CREATE TABLE analytics_match_totals (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  patch TEXT NOT NULL, remake INTEGER NOT NULL,
  matches INTEGER NOT NULL,                          -- distinct matches, every tier
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, remake)
);

CREATE TABLE champion_ban_totals (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL,
  patch TEXT NOT NULL, champion_id INTEGER NOT NULL, remake INTEGER NOT NULL,
  bans INTEGER NOT NULL,                             -- distinct matches it was banned in, every tier
  computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, remake)
);

-- Backfill every slice already aggregated, as `rebuild_champions` would count
-- it now. A rebuild replaces only the newest `AGGREGATE_PATCH_LIMIT` patches,
-- so without this the older patches it keeps would have no denominators. The
-- platform is the match id's prefix and the queue its ranked queue id (420,
-- 440), as the rebuild binds them. Only matches archived by the slice's
-- recompute count, so the totals agree with the stats beside them; the
-- slice's recompute time is kept.
WITH f AS (
  SELECT DISTINCT f.key_scope, lower(substr(m.match_id, 1, instr(m.match_id, '_') - 1)) AS platform,
    CASE m.queue_id WHEN 420 THEN 'RANKED_SOLO_5x5' WHEN 440 THEN 'RANKED_FLEX_SR' END AS queue,
    m.patch, coalesce(m.remake, 0) AS remake, m.match_id, m.archived_at
    FROM match_facts f JOIN matches m ON m.match_id = f.match_id
   WHERE m.queue_id IN (420, 440) AND m.patch IS NOT NULL
),
s AS (
  SELECT key_scope, platform, queue, patch, remake, max(computed_at) AS computed_at
    FROM analytics_slices GROUP BY key_scope, platform, queue, patch, remake
)
INSERT INTO analytics_match_totals (key_scope, platform, queue, patch, remake, matches, computed_at)
SELECT s.key_scope, s.platform, s.queue, s.patch, s.remake, count(*), s.computed_at
  FROM s JOIN f ON f.key_scope = s.key_scope AND f.platform = s.platform AND f.queue = s.queue
    AND f.patch = s.patch AND f.remake = s.remake AND f.archived_at <= s.computed_at
 GROUP BY s.key_scope, s.platform, s.queue, s.patch, s.remake;

WITH f AS (
  SELECT DISTINCT f.key_scope, lower(substr(m.match_id, 1, instr(m.match_id, '_') - 1)) AS platform,
    CASE m.queue_id WHEN 420 THEN 'RANKED_SOLO_5x5' WHEN 440 THEN 'RANKED_FLEX_SR' END AS queue,
    m.patch, coalesce(m.remake, 0) AS remake, m.match_id, m.archived_at
    FROM match_facts f JOIN matches m ON m.match_id = f.match_id
   WHERE m.queue_id IN (420, 440) AND m.patch IS NOT NULL
),
s AS (
  SELECT key_scope, platform, queue, patch, remake, max(computed_at) AS computed_at
    FROM analytics_slices GROUP BY key_scope, platform, queue, patch, remake
)
INSERT INTO champion_ban_totals (key_scope, platform, queue, patch, champion_id, remake, bans, computed_at)
SELECT s.key_scope, s.platform, s.queue, s.patch, b.champion_id, s.remake, count(DISTINCT f.match_id), s.computed_at
  FROM s JOIN f ON f.key_scope = s.key_scope AND f.platform = s.platform AND f.queue = s.queue
    AND f.patch = s.patch AND f.remake = s.remake AND f.archived_at <= s.computed_at
  JOIN match_bans b ON b.match_id = f.match_id
 GROUP BY s.key_scope, s.platform, s.queue, s.patch, b.champion_id, s.remake;

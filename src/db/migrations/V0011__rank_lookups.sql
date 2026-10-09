-- DEV-29 (ADR-110): looking up the ranks of archived players no ladder or
-- earlier lookup placed, so analytics stop counting them under UNKNOWN.

-- When each player was last looked up on a platform, whoever asked, ranked or
-- not. An unranked player has no player_ranks row, so this is what keeps the
-- lookup job from asking again before RANK_LOOKUP_RECHECK_S.
CREATE TABLE rank_lookups (
  key_scope TEXT NOT NULL,
  platform TEXT NOT NULL,
  puuid TEXT NOT NULL,
  looked_up_at INTEGER NOT NULL,                    -- unix ms
  PRIMARY KEY (key_scope, platform, puuid)
);
INSERT INTO rank_lookups (key_scope, platform, puuid, looked_up_at)
  SELECT key_scope, platform, puuid, max(fetched_at) FROM player_ranks
   GROUP BY key_scope, platform, puuid;

-- The players a ranks:lookup job plans to look up for one (platform, queue),
-- most archived games first. A lookup deletes its row; the job plans again
-- once the list is empty.
CREATE TABLE rank_lookup_queue (
  key_scope TEXT NOT NULL,
  platform TEXT NOT NULL,
  queue TEXT NOT NULL,                              -- RANKED_SOLO_5x5 | RANKED_FLEX_SR
  puuid TEXT NOT NULL,
  games INTEGER NOT NULL,                           -- archived games it would place
  PRIMARY KEY (key_scope, platform, queue, puuid)
);
CREATE INDEX rank_lookup_queue_next ON rank_lookup_queue (key_scope, platform, queue, games DESC);

-- DEV-28 (ADR-105): each player's rank as Riot last returned it from
-- league.entriesByPuuid, whoever asked (a profile, the passthrough route, a
-- rank poll). Analytics place a participant the ladder does not hold at this
-- tier, so a lookup of an Emerald player puts their games under EMERALD.
-- One row per ranked queue; a lookup that shows the player unranked in a
-- queue deletes that queue's row.
CREATE TABLE player_ranks (
  key_scope TEXT NOT NULL,
  platform TEXT NOT NULL,
  queue TEXT NOT NULL,                              -- league-v4 queueType, e.g. RANKED_SOLO_5x5
  puuid TEXT NOT NULL,
  tier TEXT NOT NULL,
  division TEXT NOT NULL,
  league_points INTEGER NOT NULL,
  fetched_at INTEGER NOT NULL,                      -- unix ms of the lookup
  PRIMARY KEY (key_scope, platform, queue, puuid)
);

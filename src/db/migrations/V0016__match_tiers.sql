-- THR-02 (ADR-127): each participant's tier as it was when their ranked match
-- was archived, so analytics no longer move a player's old games when they
-- change tier, and totals can be added to (THR-03). Kept apart from
-- `match_facts`: facts stay a pure derivation of the body, and
-- `facts:reextract` never touches this table.
--
-- One row per participant of a solo or flex match (`queue` is league-v4's
-- name, `platform` the match id's prefix, lower case). `tier` is the newer of
-- the ladder's and the last league lookup's for that platform and queue, else
-- 'UNKNOWN' (ADR-105). A re-archive never restamps. An 'UNKNOWN' row is
-- upgraded by a later rank for the same player when its match ended within
-- `TIER_LATE_STAMP_DAYS`; a known tier is never changed.
CREATE TABLE match_tiers (
  match_id   TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope  TEXT NOT NULL,
  puuid      TEXT NOT NULL,
  platform   TEXT NOT NULL,
  queue      TEXT NOT NULL,                       -- RANKED_SOLO_5x5 | RANKED_FLEX_SR
  tier       TEXT NOT NULL,                       -- a Riot tier, or UNKNOWN
  stamped_at INTEGER NOT NULL,                    -- unix ms
  PRIMARY KEY (match_id, puuid)
);
CREATE INDEX match_tiers_ladder ON match_tiers (key_scope, platform, queue, tier);
-- The late stamp's lookup: one player's unplaced rows.
CREATE INDEX match_tiers_unknown ON match_tiers (key_scope, platform, queue, puuid) WHERE tier = 'UNKNOWN';

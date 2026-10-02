-- P7-02 (owner decision, ADR-054): the ladder in v1's shape rather than
-- design/04's. Nothing wrote V0002's three ladder tables, so they are replaced
-- rather than altered.
--
-- ladder_crawls is the run log: a status beside the phase, and v1's counters
-- as columns. ladder_entries is the ladder itself, latest state per player per
-- ladder (v1 `league_entries`), and outlives the crawl rows. crawl_legs holds a
-- crawl's outstanding jobs (v1 kept the set in Redis): a job deletes its own
-- row, and whoever deletes the last one moves the crawl on.
DROP TABLE crawl_match_ids;
DROP TABLE ladder_entries;
DROP TABLE ladder_crawls;

CREATE TABLE ladder_crawls (
  id TEXT PRIMARY KEY,                              -- ULID
  key_scope TEXT NOT NULL,
  platform TEXT NOT NULL,
  queue TEXT NOT NULL,
  tier_floor TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'running',           -- running | completed | failed | cancelled
  phase TEXT NOT NULL DEFAULT 'enumerate',          -- enumerate | collect | archive
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  pages_fetched INTEGER NOT NULL DEFAULT 0,
  entries_seen INTEGER NOT NULL DEFAULT 0,
  players_discovered INTEGER NOT NULL DEFAULT 0,
  backfills_enqueued INTEGER NOT NULL DEFAULT 0,
  match_ids_seen INTEGER NOT NULL DEFAULT 0,
  matches_queued INTEGER NOT NULL DEFAULT 0,
  legs_failed INTEGER NOT NULL DEFAULT 0            -- legs that gave up: the crawl ends failed
);
-- One live crawl per ladder, enforced where two writers cannot both pass it (v1).
CREATE UNIQUE INDEX ladder_crawls_live ON ladder_crawls (key_scope, platform, queue)
  WHERE status = 'running';
CREATE INDEX ladder_crawls_recent ON ladder_crawls (key_scope, platform, queue, started_at);

CREATE TABLE ladder_entries (
  key_scope TEXT NOT NULL,
  platform TEXT NOT NULL,
  queue TEXT NOT NULL,
  puuid TEXT NOT NULL,
  tier TEXT NOT NULL,
  division TEXT NOT NULL,
  league_points INTEGER NOT NULL,
  wins INTEGER NOT NULL,
  losses INTEGER NOT NULL,
  veteran INTEGER NOT NULL DEFAULT 0,
  inactive INTEGER NOT NULL DEFAULT 0,
  fresh_blood INTEGER NOT NULL DEFAULT 0,
  hot_streak INTEGER NOT NULL DEFAULT 0,
  first_seen_crawl_id TEXT NOT NULL,                -- set once
  last_seen_crawl_id TEXT NOT NULL,                 -- restamped by every crawl that sees the player
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, puuid)
);
CREATE INDEX ladder_entries_ladder ON ladder_entries
  (key_scope, platform, queue, tier, division, league_points);
CREATE INDEX ladder_entries_last_seen ON ladder_entries (last_seen_crawl_id);
CREATE INDEX ladder_entries_puuid ON ladder_entries (key_scope, puuid);

CREATE TABLE crawl_legs (
  crawl_id TEXT NOT NULL REFERENCES ladder_crawls(id) ON DELETE CASCADE,
  leg TEXT NOT NULL,                                -- e.g. `ladder:walk:DIAMOND:I`
  cursor INTEGER,                                   -- a walk's next page
  PRIMARY KEY (crawl_id, leg)
);

CREATE TABLE crawl_match_ids (                      -- the "collect" set
  crawl_id TEXT NOT NULL REFERENCES ladder_crawls(id) ON DELETE CASCADE,
  match_id TEXT NOT NULL,
  PRIMARY KEY (crawl_id, match_id)
);

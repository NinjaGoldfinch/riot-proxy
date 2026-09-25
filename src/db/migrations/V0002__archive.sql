-- P5-01: the archive, players, analytics and ladder tables.
-- DDL verbatim from docs/design/04-data-and-cache.md §Schema (the operational
-- tables are V0001). Timestamps are unix ms; JSON lives in TEXT; bodies are zstd BLOBs.

-- Players ───────────────────────────────────────────────────────────────────
CREATE TABLE players (
  key_scope       TEXT NOT NULL,
  puuid           TEXT NOT NULL,
  platform        TEXT NOT NULL,
  game_name       TEXT, tag_line TEXT,
  tracked         INTEGER NOT NULL DEFAULT 0,
  last_seen_match_id TEXT,                        -- poll:matches cursor (v1 #46)
  backfill_state  TEXT,                           -- JSON: {done, cursor, limit}  (v1 #44)
  last_rank       TEXT,                           -- JSON snapshot for rank.changed diffs
  in_game_id      INTEGER,                        -- for game.started/ended transitions
  updated_at      INTEGER NOT NULL,
  PRIMARY KEY (key_scope, puuid)
);
CREATE INDEX players_tracked ON players(tracked) WHERE tracked = 1;

-- Archive (immutable, survives key rotation — match ids are not encrypted) ──
CREATE TABLE matches (
  match_id     TEXT PRIMARY KEY,                  -- e.g. OC1_123456789
  region       TEXT NOT NULL,
  patch        TEXT NOT NULL,                     -- "14.18"
  queue_id     INTEGER NOT NULL,
  game_end_ms  INTEGER NOT NULL,
  body_zstd    BLOB NOT NULL,                     -- match-v5 payload, zstd
  body_size    INTEGER NOT NULL,                  -- uncompressed, for stats
  archived_at  INTEGER NOT NULL
);
CREATE INDEX matches_patch_queue ON matches(patch, queue_id);
CREATE INDEX matches_end        ON matches(game_end_ms);

CREATE TABLE timelines (                          -- ARCHIVE_TIMELINES=true only
  match_id  TEXT PRIMARY KEY REFERENCES matches(match_id),
  body_zstd BLOB NOT NULL
);

CREATE TABLE match_facts (                        -- one row per participant; pure derivation of matches
  match_id     TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope    TEXT NOT NULL,
  puuid        TEXT NOT NULL,
  team_id      INTEGER NOT NULL,
  position     TEXT,
  champion_id  INTEGER NOT NULL,
  win          INTEGER NOT NULL,
  kills INTEGER, deaths INTEGER, assists INTEGER,
  items        TEXT,                              -- JSON [int]
  runes        TEXT,                              -- JSON
  summoners    TEXT,                              -- JSON [int,int]
  facts_version INTEGER NOT NULL,                 -- bump to trigger facts:reextract
  PRIMARY KEY (match_id, puuid)
);
CREATE INDEX facts_player ON match_facts(key_scope, puuid, match_id);
CREATE INDEX facts_champ  ON match_facts(champion_id);

-- Analytics (recomputed; safe to TRUNCATE) ──────────────────────────────────
CREATE TABLE champion_stats (
  patch TEXT, queue_id INTEGER, champion_id INTEGER, position TEXT,
  games INTEGER, wins INTEGER, bans INTEGER,
  PRIMARY KEY (patch, queue_id, champion_id, position)
);
CREATE TABLE champion_matchups (
  patch TEXT, queue_id INTEGER, position TEXT, champion_id INTEGER, vs_champion_id INTEGER,
  games INTEGER, wins INTEGER,
  PRIMARY KEY (patch, queue_id, position, champion_id, vs_champion_id)   -- v1 migration 0009 order
);
CREATE TABLE champion_builds (
  patch TEXT, queue_id INTEGER, champion_id INTEGER, position TEXT,
  build_hash TEXT, items TEXT, games INTEGER, wins INTEGER,
  PRIMARY KEY (patch, queue_id, champion_id, position, build_hash)
);

-- Ladder ────────────────────────────────────────────────────────────────────
CREATE TABLE ladder_crawls (
  id TEXT PRIMARY KEY, platform TEXT, queue TEXT,
  phase TEXT NOT NULL,                            -- enumerate | collect | archive | done
  tier_floor TEXT, started_at INTEGER, finished_at INTEGER,
  stats TEXT                                      -- JSON counters for /dashboard
);
CREATE TABLE ladder_entries (
  crawl_id TEXT REFERENCES ladder_crawls(id) ON DELETE CASCADE,
  key_scope TEXT, puuid TEXT, tier TEXT, division TEXT, lp INTEGER, wins INTEGER, losses INTEGER,
  PRIMARY KEY (crawl_id, puuid)
);
CREATE TABLE crawl_match_ids (                    -- the "collect" set
  crawl_id TEXT REFERENCES ladder_crawls(id) ON DELETE CASCADE,
  match_id TEXT, PRIMARY KEY (crawl_id, match_id)
);

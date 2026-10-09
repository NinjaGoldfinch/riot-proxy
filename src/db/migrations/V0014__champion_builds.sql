-- BLD-02 (ADR-120): set builds, aggregated from `match_builds` joined to the
-- ladder's facts by `rebuild_builds`. A build is a player's first two finished
-- items (`core`, "[a,b]"); only players with two or more count. Every column
-- is a plain sum over disjoint matches, like the other analytics tables.
CREATE TABLE champion_builds (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,                                -- JSON: the first two finished items, "[a,b]"
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core)
);

-- What the players of one build chose besides its core.
CREATE TABLE champion_build_parts (
  key_scope TEXT NOT NULL, platform TEXT NOT NULL, queue TEXT NOT NULL, patch TEXT NOT NULL,
  champion_id INTEGER NOT NULL, role TEXT NOT NULL, remake INTEGER NOT NULL,
  core TEXT NOT NULL,
  part TEXT NOT NULL,   -- item3 | item4 | item5 | starter | boots | skill_order | runes | spells
  value TEXT NOT NULL,  -- an item id; starter's JSON; "QWE"; "keystone:subStyle"; "a:b" with a <= b
  games INTEGER NOT NULL, wins INTEGER NOT NULL, computed_at INTEGER NOT NULL,
  PRIMARY KEY (key_scope, platform, queue, patch, champion_id, role, remake, core, part, value)
);

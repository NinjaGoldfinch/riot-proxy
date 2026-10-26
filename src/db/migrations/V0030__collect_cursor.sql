-- THR-04 (ADR-185): the collect cursor. A ladder player's wins + losses and
-- the time their last match-id walk began; a crawl skips players whose total
-- hasn't moved and asks the rest only for games since then. NULL until a
-- crawl's walk finishes (a first sight). players_skipped counts the players a
-- crawl left out. Added columns only: no row is rewritten.
ALTER TABLE ladder_entries ADD COLUMN collected_games INTEGER;
ALTER TABLE ladder_entries ADD COLUMN collected_at INTEGER;     -- unix ms
ALTER TABLE ladder_crawls ADD COLUMN players_skipped INTEGER NOT NULL DEFAULT 0;

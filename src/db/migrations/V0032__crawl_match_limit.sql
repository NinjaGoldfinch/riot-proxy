-- STG-03 (ADR-202): LADDER_CRAWL_MATCH_LIMIT caps the archived matches a
-- platform's crawls may bring it to. matches_skipped counts the ids a crawl's
-- archive hand-off left out because of it; match_limit is the cap in effect
-- when it first did, NULL while it never has. V0031 is LAD-02's. Added
-- columns only: no row is rewritten.
ALTER TABLE ladder_crawls ADD COLUMN matches_skipped INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ladder_crawls ADD COLUMN match_limit INTEGER;

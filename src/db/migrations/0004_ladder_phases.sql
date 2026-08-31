-- A crawl is three stages, not one: enumerate the ladder, collect every
-- discovered player's match ids, then fetch the matches behind them.
--
-- The ordering is the point. A match is shared by ten players, so walking a
-- player's history the moment they are discovered means the same game is
-- reachable from ten walks that run at ten different times — and a walk only
-- skips a match `filterUnarchived` can already see. Holding the ids until
-- every walk has produced them makes the whole crawl one de-duplicated set,
-- and each match is fetched exactly once.
--
-- `enumerate` is the default so a row written by an older deployment reads as
-- what it was: a crawl that never had stages.
ALTER TABLE "ladder_crawls"
	ADD COLUMN IF NOT EXISTS "phase" text DEFAULT 'enumerate' NOT NULL;
--> statement-breakpoint
-- Distinct ids the collect stage gathered, and how many of those were not
-- already in the archive. The pair is the dedup dividend, per run: a crawl
-- that saw 40 000 ids and queued 4 000 matches found nine tenths of them
-- stored already.
ALTER TABLE "ladder_crawls"
	ADD COLUMN IF NOT EXISTS "match_ids_seen" integer DEFAULT 0 NOT NULL;
--> statement-breakpoint
ALTER TABLE "ladder_crawls"
	ADD COLUMN IF NOT EXISTS "matches_queued" integer DEFAULT 0 NOT NULL;

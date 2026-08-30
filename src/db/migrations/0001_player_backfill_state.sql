-- #44 — "have we walked this player's history" is a fact about the player, not
-- something the archive can answer. Matches are shared between ten players, so
-- a player's recent games can be stored entirely because a teammate was walked.
--
-- No data migration on purpose: every existing player starts null and re-walks
-- on their next page-0 lookup. Those are precisely the players the old
-- archive-derived check was skipping, the walk is deduplicated per match by
-- `filterUnarchived`, and it runs at bulk priority behind BULK_USAGE_CEILING.
ALTER TABLE "players"
	ADD COLUMN IF NOT EXISTS "history_backfill_started_at" timestamp with time zone;
--> statement-breakpoint
ALTER TABLE "players"
	ADD COLUMN IF NOT EXISTS "history_backfilled_at" timestamp with time zone;
--> statement-breakpoint
ALTER TABLE "players"
	ADD COLUMN IF NOT EXISTS "history_backfill_depth" integer;

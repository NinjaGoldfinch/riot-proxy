-- #90 — the archive was write-only. `match_participants` knows which champion
-- won, `matches.queue_id` and `gameVersion` know which game it was, and
-- `league_entries` is what finally says at which tier it was played.
--
-- A table rather than a materialized view: the unit of work is one
-- (platform, queue) — what a crawl finishes — and REFRESH MATERIALIZED VIEW
-- has no WHERE clause, so a Korean crawl completing would recompute EUW too.
CREATE TABLE IF NOT EXISTS "champion_stats" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"tier" text NOT NULL,
	-- `gameVersion` major.minor. Data Dragon's third component is a Data Dragon
	-- build rather than a game one, so mapping onto its version list would still
	-- group by major.minor and would make this depend on the mirror.
	"patch" text NOT NULL,
	"champion_id" integer NOT NULL,
	"games" integer NOT NULL,
	"wins" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_stats_key_scope_platform_queue_tier_patch_champion_id_pk"
		PRIMARY KEY("key_scope","platform","queue","tier","patch","champion_id")
);
--> statement-breakpoint
-- The read route's shape: one (platform, queue, patch), optionally one tier,
-- ordered by how often a champion was played.
CREATE INDEX IF NOT EXISTS "champion_stats_slice_idx" ON "champion_stats"
	("key_scope","platform","queue","patch","tier","games");

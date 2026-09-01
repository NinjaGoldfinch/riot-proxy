-- #111 — champion_stats is recompute-only (§2.1 of the plan): every row is
-- rebuilt from the archive on the next crawl completion or admin trigger, so
-- there is nothing in it worth preserving across a shape change. Truncating
-- first means the new NOT NULL columns need no interim default, and the new
-- primary key (extended with `role`) has no stale row to conflict with.
TRUNCATE TABLE "champion_stats";
--> statement-breakpoint
ALTER TABLE "champion_stats"
	DROP CONSTRAINT "champion_stats_key_scope_platform_queue_tier_patch_champion_id_pk";
--> statement-breakpoint
ALTER TABLE "champion_stats"
	ADD COLUMN "role" text DEFAULT '' NOT NULL,
	ADD COLUMN "matches_picked" integer NOT NULL,
	ADD COLUMN "stated_games" integer NOT NULL,
	ADD COLUMN "kills" bigint NOT NULL,
	ADD COLUMN "deaths" bigint NOT NULL,
	ADD COLUMN "assists" bigint NOT NULL,
	ADD COLUMN "cs" bigint NOT NULL,
	ADD COLUMN "gold" bigint NOT NULL,
	ADD COLUMN "damage" bigint NOT NULL,
	ADD COLUMN "vision" bigint NOT NULL,
	ADD COLUMN "duration_s" bigint NOT NULL;
--> statement-breakpoint
ALTER TABLE "champion_stats"
	ADD CONSTRAINT "champion_stats_key_scope_platform_queue_tier_patch_champion_id_role_pk"
	PRIMARY KEY("key_scope","platform","queue","tier","patch","champion_id","role");
--> statement-breakpoint
-- Distinct matches per (tier, patch) this key scope has archived — the
-- pick-rate/ban-rate denominator L5 never computed.
CREATE TABLE IF NOT EXISTS "analytics_slices" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"tier" text NOT NULL,
	"patch" text NOT NULL,
	"matches" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "analytics_slices_key_scope_platform_queue_tier_patch_pk"
		PRIMARY KEY("key_scope","platform","queue","tier","patch")
);
--> statement-breakpoint
-- Per-tier ban counts. Bans are per-team and roleless, so this is its own
-- table rather than a role row of champion_stats (see the schema comment).
CREATE TABLE IF NOT EXISTS "champion_bans" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"tier" text NOT NULL,
	"patch" text NOT NULL,
	"champion_id" integer NOT NULL,
	"bans" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_bans_key_scope_platform_queue_tier_patch_champion_id_pk"
		PRIMARY KEY("key_scope","platform","queue","tier","patch","champion_id")
);

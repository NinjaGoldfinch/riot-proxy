-- #87 — the ladder needs somewhere to live. Rank was a transient Redis
-- snapshot used only for `rank.changed` detection; nothing persisted a league
-- entry, and nothing represented a crawl run at all.
CREATE TABLE IF NOT EXISTS "ladder_crawls" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"tier_floor" text NOT NULL,
	"status" text DEFAULT 'running' NOT NULL,
	"started_at" timestamp with time zone DEFAULT now() NOT NULL,
	"finished_at" timestamp with time zone,
	"pages_fetched" integer DEFAULT 0 NOT NULL,
	"entries_seen" integer DEFAULT 0 NOT NULL,
	"players_discovered" integer DEFAULT 0 NOT NULL,
	"backfills_enqueued" integer DEFAULT 0 NOT NULL
);
--> statement-breakpoint
-- Latest state, not history: one row per player per ladder, restamped by every
-- crawl that sees them. No foreign key to `ladder_crawls` — the crawl rows are
-- a run log a retention policy may prune, and the ladder must outlive its log.
CREATE TABLE IF NOT EXISTS "league_entries" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"puuid" text NOT NULL,
	"tier" text NOT NULL,
	"division" text NOT NULL,
	"league_points" integer NOT NULL,
	"wins" integer NOT NULL,
	"losses" integer NOT NULL,
	"veteran" boolean DEFAULT false NOT NULL,
	"inactive" boolean DEFAULT false NOT NULL,
	"fresh_blood" boolean DEFAULT false NOT NULL,
	"hot_streak" boolean DEFAULT false NOT NULL,
	"first_seen_crawl_id" uuid NOT NULL,
	"last_seen_crawl_id" uuid NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "league_entries_key_scope_platform_queue_puuid_pk"
		PRIMARY KEY("key_scope","platform","queue","puuid")
);
--> statement-breakpoint
-- One live crawl per ladder, enforced where two workers cannot both pass it.
CREATE UNIQUE INDEX IF NOT EXISTS "ladder_crawls_live_idx" ON "ladder_crawls"
	("key_scope","platform","queue") WHERE status = 'running';
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "ladder_crawls_recent_idx" ON "ladder_crawls"
	("key_scope","platform","queue","started_at");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "league_entries_ladder_idx" ON "league_entries"
	("key_scope","platform","queue","tier","division","league_points");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "league_entries_last_seen_idx" ON "league_entries"
	("last_seen_crawl_id");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "league_entries_puuid_idx" ON "league_entries"
	("key_scope","puuid");

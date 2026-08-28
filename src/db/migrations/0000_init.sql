-- §7.1 — initial schema.
CREATE TABLE IF NOT EXISTS "consumers" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"name" text NOT NULL,
	"key_hash" text NOT NULL,
	"scopes" text[] DEFAULT '{read}' NOT NULL,
	"quota_per_min" integer DEFAULT 600 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"disabled_at" timestamp with time zone,
	CONSTRAINT "consumers_key_hash_unique" UNIQUE("key_hash")
);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "players" (
	"puuid" text NOT NULL,
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"game_name" text,
	"tag_line" text,
	"tracked" boolean DEFAULT false NOT NULL,
	"last_seen_match_id" text,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "players_key_scope_puuid_pk" PRIMARY KEY("key_scope","puuid")
);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "matches" (
	"match_id" text PRIMARY KEY NOT NULL,
	"region" text NOT NULL,
	"data" jsonb NOT NULL,
	"timeline" jsonb,
	"fetched_at" timestamp with time zone DEFAULT now() NOT NULL,
	"queue_id" integer GENERATED ALWAYS AS ((data->'info'->>'queueId')::int) STORED,
	"game_end_ts" bigint GENERATED ALWAYS AS ((data->'info'->>'gameEndTimestamp')::bigint) STORED
);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "match_participants" (
	"match_id" text NOT NULL,
	"puuid" text NOT NULL,
	"champion_id" integer,
	"win" boolean,
	CONSTRAINT "match_participants_match_id_puuid_pk" PRIMARY KEY("match_id","puuid")
);
--> statement-breakpoint
ALTER TABLE "match_participants"
	ADD CONSTRAINT "match_participants_match_id_matches_match_id_fk"
	FOREIGN KEY ("match_id") REFERENCES "matches"("match_id") ON DELETE cascade ON UPDATE no action;
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "players_tracked_idx" ON "players" ("tracked","key_scope");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "matches_game_end_idx" ON "matches" ("game_end_ts");
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "match_participants_puuid_idx" ON "match_participants" ("puuid");
--> statement-breakpoint
-- Fast "was this player in this match" lookups straight off the archived JSON.
CREATE INDEX IF NOT EXISTS "matches_participants_gin" ON "matches"
	USING gin ((data->'metadata'->'participants'));

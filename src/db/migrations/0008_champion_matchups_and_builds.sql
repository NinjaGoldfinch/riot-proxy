-- #112 — the second-order aggregates: one lane matchup's record, and the
-- final items/runes/spells a champion ran. All four are pure
-- `match_participants` reads (plus one self-join for matchups); none opens
-- JSONB.
--
-- No tier dimension on any of these: cardinality is never the problem at this
-- scale (§3 of the plan), but 170 champions squared, by role, by tier would
-- shred every cell below significance — and for matchups specifically, the
-- two laners can sit in different tiers, so a per-tier attribution is
-- ill-defined regardless.
CREATE TABLE IF NOT EXISTS "champion_matchups" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"patch" text NOT NULL,
	"role" text NOT NULL,
	"champion_id" integer NOT NULL,
	"opponent_id" integer NOT NULL,
	"games" integer NOT NULL,
	"wins" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_matchups_key_scope_platform_queue_patch_role_champion_id_opponent_id_pk"
		PRIMARY KEY("key_scope","platform","queue","patch","role","champion_id","opponent_id")
);
--> statement-breakpoint
-- Final items only (`item0`-`item5`, trinket and empty slots excluded at
-- recompute time) — build order needs timelines, out of scope (§12).
CREATE TABLE IF NOT EXISTS "champion_items" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"patch" text NOT NULL,
	"champion_id" integer NOT NULL,
	"role" text DEFAULT '' NOT NULL,
	"item_id" integer NOT NULL,
	"games" integer NOT NULL,
	"wins" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_items_key_scope_platform_queue_patch_champion_id_role_item_id_pk"
		PRIMARY KEY("key_scope","platform","queue","patch","champion_id","role","item_id")
);
--> statement-breakpoint
CREATE TABLE IF NOT EXISTS "champion_runes" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"patch" text NOT NULL,
	"champion_id" integer NOT NULL,
	"role" text DEFAULT '' NOT NULL,
	"keystone_id" integer NOT NULL,
	"sub_style_id" integer NOT NULL,
	"games" integer NOT NULL,
	"wins" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_runes_key_scope_platform_queue_patch_champion_id_role_keystone_id_sub_style_id_pk"
		PRIMARY KEY("key_scope","platform","queue","patch","champion_id","role","keystone_id","sub_style_id")
);
--> statement-breakpoint
-- `spell_a`/`spell_b` are order-normalised (least/greatest) at recompute
-- time, so the two summoner-spell slots collapse to one row regardless of
-- which slot Riot happened to serialise each spell into.
CREATE TABLE IF NOT EXISTS "champion_spells" (
	"key_scope" text NOT NULL,
	"platform" text NOT NULL,
	"queue" text NOT NULL,
	"patch" text NOT NULL,
	"champion_id" integer NOT NULL,
	"role" text DEFAULT '' NOT NULL,
	"spell_a" integer NOT NULL,
	"spell_b" integer NOT NULL,
	"games" integer NOT NULL,
	"wins" integer NOT NULL,
	"computed_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "champion_spells_key_scope_platform_queue_patch_champion_id_role_spell_a_spell_b_pk"
		PRIMARY KEY("key_scope","platform","queue","patch","champion_id","role","spell_a","spell_b")
);

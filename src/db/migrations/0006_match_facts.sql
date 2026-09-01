-- #110 — `match_participants` was four columns through L5: `(match_id, puuid,
-- champion_id, win)`, so no aggregate could slice by lane or say anything
-- about performance, and `teams[].bans` sat unread in every archived body.
-- These are plain nullable columns with no default, so — unlike C1's
-- generated `matches.patch`/`game_duration` — this ALTER is metadata-only in
-- Postgres 11+ and does not rewrite the table.
ALTER TABLE "match_participants"
	ADD COLUMN IF NOT EXISTS "team_id" smallint,
	ADD COLUMN IF NOT EXISTS "team_position" text,
	ADD COLUMN IF NOT EXISTS "kills" smallint,
	ADD COLUMN IF NOT EXISTS "deaths" smallint,
	ADD COLUMN IF NOT EXISTS "assists" smallint,
	ADD COLUMN IF NOT EXISTS "cs" integer,
	ADD COLUMN IF NOT EXISTS "gold" integer,
	ADD COLUMN IF NOT EXISTS "damage" integer,
	ADD COLUMN IF NOT EXISTS "vision" integer,
	ADD COLUMN IF NOT EXISTS "item0" integer,
	ADD COLUMN IF NOT EXISTS "item1" integer,
	ADD COLUMN IF NOT EXISTS "item2" integer,
	ADD COLUMN IF NOT EXISTS "item3" integer,
	ADD COLUMN IF NOT EXISTS "item4" integer,
	ADD COLUMN IF NOT EXISTS "item5" integer,
	ADD COLUMN IF NOT EXISTS "keystone_id" integer,
	ADD COLUMN IF NOT EXISTS "sub_style_id" integer,
	ADD COLUMN IF NOT EXISTS "spell1" integer,
	ADD COLUMN IF NOT EXISTS "spell2" integer,
	ADD COLUMN IF NOT EXISTS "placement" smallint,
	ADD COLUMN IF NOT EXISTS "subteam_id" smallint;
--> statement-breakpoint
-- One row per ban. `championId: -1` (no pick made in that slot) is skipped at
-- extraction, so a row here always names a real champion. No `key_scope`,
-- same as `match_participants`: a ban is a fact about the match, not about a
-- player (§7.4).
CREATE TABLE IF NOT EXISTS "match_bans" (
	"match_id" text NOT NULL,
	"team_id" smallint NOT NULL,
	"pick_turn" smallint NOT NULL,
	"champion_id" integer NOT NULL,
	CONSTRAINT "match_bans_match_id_team_id_pick_turn_pk" PRIMARY KEY("match_id","team_id","pick_turn")
);
--> statement-breakpoint
ALTER TABLE "match_bans"
	ADD CONSTRAINT "match_bans_match_id_matches_match_id_fk"
	FOREIGN KEY ("match_id") REFERENCES "matches"("match_id") ON DELETE cascade ON UPDATE no action;
-- No extra index: the primary key's leading column already serves "every ban
-- in this match".

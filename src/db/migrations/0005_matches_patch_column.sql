-- #109 — the recompute parsed `gameVersion` out of `matches.data` per
-- participant row, unindexed. `patch` makes that a generated, stored, indexed
-- column instead, so `recomputeChampionStats` (and the later
-- `AGGREGATE_PATCH_LIMIT` bound) never open the JSONB body for it again. Null
-- `gameVersion` yields a null patch — the same exclusion the aggregate
-- applied before this column existed.
--
-- `game_duration` is seconds: every `gameVersion` this service can have
-- archived postdates Riot's 11.20 switch away from milliseconds, so there is
-- no unit to branch on.
--
-- Both are `GENERATED ALWAYS ... STORED`, so adding them rewrites every
-- existing row in `matches` once — on a large archive that rewrite, not the
-- index build, is this migration's real cost.
ALTER TABLE "matches"
	ADD COLUMN IF NOT EXISTS "patch" text GENERATED ALWAYS AS (
		split_part(data->'info'->>'gameVersion', '.', 1) || '.' ||
		split_part(data->'info'->>'gameVersion', '.', 2)
	) STORED;
--> statement-breakpoint
ALTER TABLE "matches"
	ADD COLUMN IF NOT EXISTS "game_duration" integer
	GENERATED ALWAYS AS ((data->'info'->>'gameDuration')::int) STORED;
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS "matches_queue_patch_idx" ON "matches" ("queue_id","patch");

-- The matchups primary key put `role` ahead of `champion_id`, which is the
-- opposite of how the table is read. `listChampionMatchups` always filters
-- `champion_id` and only optionally filters `role`, so the common request —
-- `GET /v1/lol/analytics/champions/{id}/matchups` with no role — could use
-- only the (key_scope, platform, queue, patch) prefix and then scanned and
-- sorted every champion's matchups for that patch.
--
-- Swapping the two columns puts the index in the same shape as the three
-- build tables, which had it right. There is no secondary index to add
-- instead: the table is recomputed wholesale and has exactly one read path.
--
-- Safe to do unguarded — `champion_matchups` is derived, and a recompute
-- rewrites every row of it for a (key_scope, platform, queue) anyway.
ALTER TABLE "champion_matchups"
	DROP CONSTRAINT "champion_matchups_key_scope_platform_queue_patch_role_champion_id_opponent_id_pk";
--> statement-breakpoint
ALTER TABLE "champion_matchups"
	ADD CONSTRAINT "champion_matchups_key_scope_platform_queue_patch_champion_id_role_opponent_id_pk"
		PRIMARY KEY("key_scope","platform","queue","patch","champion_id","role","opponent_id");

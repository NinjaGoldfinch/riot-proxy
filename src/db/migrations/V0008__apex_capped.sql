-- LAD-01 (ADR-097): the apex tiers whose league list came back at Riot's cap
-- (`RIOT_APEX_LIST_CAP`), so the crawl may be missing the bottom of that tier.
-- A JSON list of tiers in APEX_TIERS order, e.g. '["MASTER"]'; NULL when none.
ALTER TABLE ladder_crawls ADD COLUMN apex_capped TEXT;

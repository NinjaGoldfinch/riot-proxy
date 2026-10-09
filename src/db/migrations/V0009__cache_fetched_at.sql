-- SITE-01: when Riot last answered for a cached key, whether or not the bytes
-- changed (`X-Cache-Fetched-Age`). `content_at` stays the content's age.
-- Rows written before this read `content_at` in its place.
ALTER TABLE cache ADD COLUMN fetched_at INTEGER;

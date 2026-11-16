-- SITE-13 (ADR-219): each platform's last lol-status-v4 platform-data answer,
-- as the status poller read it. body and fetched_at are the last 200's and
-- are kept through a failure; failed_at and error are the last failure's
-- and are kept through a 200, so a reader compares the two stamps. A round
-- the limiter had no room for writes nothing. Not keyed by key_scope: the
-- feed is Riot's, the same whichever key read it. A rowid table: a body
-- carries each entry's titles and updates in every locale, not the small
-- row WITHOUT ROWID suits (ADR-140).
CREATE TABLE platform_status (
  platform   TEXT PRIMARY KEY,   -- Platform::as_str ('oc1')
  body       TEXT,               -- the last 200's body, JSON as Riot sent it; NULL before one
  fetched_at INTEGER,            -- unix ms of the last 200; NULL before one
  failed_at  INTEGER,            -- unix ms of the last failure; NULL before one
  error      TEXT                -- why the last failure failed; NULL before one
);

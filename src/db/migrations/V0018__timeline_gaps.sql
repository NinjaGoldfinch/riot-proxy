-- TL-02 (ADR-133): matches whose timeline Riot won't serve (a 404, as for a
-- game older than Riot keeps), so `timelines:backfill` doesn't ask again.
-- No foreign key, like `timelines` since V0013.
CREATE TABLE timeline_gaps (
  match_id  TEXT PRIMARY KEY,
  reason    TEXT NOT NULL,                        -- "not_found": Riot answered 404
  marked_at INTEGER NOT NULL                      -- unix ms
);

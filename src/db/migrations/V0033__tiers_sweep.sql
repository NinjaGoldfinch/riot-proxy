-- AVL-06 (ADR-213): how far tiers:backfill has swept each match-id prefix.
-- Every id up to swept_to in its prefix had its ranked facts stamped when the
-- sweep passed, and every match archived or re-extracted since is stamped in
-- its own transaction, so the boot check and the sweep read only the ids past
-- each cursor instead of walking the archive (which, rows carrying their
-- bodies, took minutes on every boot while ME1_ matches stayed unstamped).
-- No row, or swept_to '', is a prefix never swept. A change that leaves
-- ranked facts with tier NULL again resets the cursors (rows deleted or
-- swept_to ''). Keyed by the prefix alone, so WITHOUT ROWID (ADR-140).
CREATE TABLE tiers_sweep (
  prefix    TEXT PRIMARY KEY,   -- a match id's part before '_', as the id spells it ('EUW1')
  swept_to  TEXT NOT NULL,      -- the highest match id of the prefix a sweep has passed
  swept_at  INTEGER NOT NULL    -- unix ms
) WITHOUT ROWID;

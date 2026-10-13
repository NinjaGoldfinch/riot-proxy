-- OBJ-03 (ADR-164): a timeline's local window runs from its match's
-- `archived_at`, which sits after the body in each `matches` row. Without
-- this index, finding the timelines to free would read a match body per
-- timeline (about 2.5 GB on the first run, 300 MB a day after). With it, the
-- join reads the index alone. About 40 bytes a match. Building it reads
-- `archived_at` from every row once.
CREATE INDEX matches_archived ON matches(match_id, archived_at);

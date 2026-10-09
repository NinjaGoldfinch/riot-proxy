-- TL-01 (ADR-118): a timeline is stored whenever it is fetched, before its
-- match if need be. The foreign key to `matches` (V0002) dropped every timeline
-- that arrived first, which is the usual order when a client asks for a match
-- and its timeline at once. SQLite can't drop a constraint, so the table is
-- rebuilt without it. Nothing references `timelines`.
CREATE TABLE timelines_standalone (
  match_id  TEXT PRIMARY KEY,
  body_zstd BLOB NOT NULL
);
INSERT INTO timelines_standalone (match_id, body_zstd) SELECT match_id, body_zstd FROM timelines;
DROP TABLE timelines;
ALTER TABLE timelines_standalone RENAME TO timelines;

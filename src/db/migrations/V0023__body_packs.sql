-- OBJ-02 (design/12 §SQLite, ADR-146, ADR-163): raw bodies move to packs in
-- object storage. A body is local (`packed_at` NULL), packed (in a verified
-- pack and still in SQLite) or offloaded (`offloaded_at` set, `body_zstd` the
-- empty blob X'', which no zstd frame can be). Nothing is packed by this
-- migration; `archive:offload` (OBJ-03) does that.

-- One row per pack object. Its key is `packs/<kind>s/<yyyy>/<mm>/<pack_id>.pack`,
-- year and month from the ULID's time; `kind` is kept so the key survives
-- the pack's last body being deleted (OBJ-06).
CREATE TABLE body_packs (
  pack_id    TEXT PRIMARY KEY,                -- ULID
  kind       TEXT NOT NULL,                   -- match | timeline
  bytes      INTEGER NOT NULL,                -- object size
  bodies     INTEGER NOT NULL,
  live_bytes INTEGER NOT NULL,                -- bytes of bodies still referenced
  sha256     BLOB NOT NULL,                   -- of the whole object
  created_at INTEGER NOT NULL                 -- unix ms
);

-- Where each packed body is: its byte range in the pack and its digest.
CREATE TABLE body_locations (
  match_id TEXT NOT NULL,
  kind     TEXT NOT NULL,                     -- match | timeline
  pack_id  TEXT NOT NULL REFERENCES body_packs(pack_id),
  offset   INTEGER NOT NULL,
  len      INTEGER NOT NULL,
  sha256   BLOB NOT NULL,                     -- of the body's zstd bytes
  PRIMARY KEY (match_id, kind)
) WITHOUT ROWID;
CREATE INDEX body_locations_pack ON body_locations(pack_id);

-- Added columns are absent from existing rows' records, which read as NULL
-- without touching the body; the indexes below are built from that. The state
-- columns end each index although its WHERE implies them: SQLite still checks
-- a query's terms against the row unless the index holds the columns, and in
-- a row they sit after the body, so a count would read every body.
ALTER TABLE matches   ADD COLUMN packed_at    INTEGER;  -- unix ms; in a verified pack
ALTER TABLE matches   ADD COLUMN offloaded_at INTEGER;  -- unix ms; local copy freed
ALTER TABLE timelines ADD COLUMN packed_at    INTEGER;
ALTER TABLE timelines ADD COLUMN offloaded_at INTEGER;
CREATE INDEX matches_to_pack   ON matches(archived_at, packed_at)
  WHERE packed_at IS NULL;
CREATE INDEX matches_to_free   ON matches(archived_at, packed_at, offloaded_at)
  WHERE packed_at IS NOT NULL AND offloaded_at IS NULL;
CREATE INDEX timelines_to_pack ON timelines(match_id, packed_at)
  WHERE packed_at IS NULL;
CREATE INDEX timelines_to_free ON timelines(match_id, packed_at, offloaded_at)
  WHERE packed_at IS NOT NULL AND offloaded_at IS NULL;

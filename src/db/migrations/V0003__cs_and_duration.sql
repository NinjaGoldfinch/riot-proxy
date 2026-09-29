-- P5-04 (owner decision, ADR-043): CS per participant and game length, so the
-- champion pool can report CS per minute as v1 did. Nullable: rows archived
-- before this migration have neither until facts:reextract (FACTS_VERSION 2)
-- re-derives them from the stored bodies.
ALTER TABLE matches ADD COLUMN game_duration INTEGER;   -- seconds, info.gameDuration
ALTER TABLE match_facts ADD COLUMN cs INTEGER;          -- totalMinionsKilled + neutralMinionsKilled

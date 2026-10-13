-- DSK-04 (ADR-138, amends ADR-127): a participant's tier lives on their fact
-- row. `match_tiers` (V0016) repeated the fact's key (match id, puuid), a
-- platform the match id already names, a queue `matches.queue_id` already
-- holds, and a stamp time nothing read, under three indexes: 825 MiB on a
-- 2.3 M-row archive for one 8-byte value per row.
--
-- `tier` is NULL until the match is stamped (`tiers:backfill` finds those),
-- then a Riot tier or 'UNKNOWN'. It is the one fact column `facts:reextract`
-- never writes, so the stamp still outlives a re-extraction (ADR-127).
ALTER TABLE match_facts ADD COLUMN tier TEXT;
UPDATE match_facts SET tier = (SELECT t.tier FROM match_tiers t
  WHERE t.match_id = match_facts.match_id AND t.puuid = match_facts.puuid);
DROP TABLE match_tiers;

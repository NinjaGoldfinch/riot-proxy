-- DSK-07 (ADR-141): match_facts rebuilt WITHOUT ROWID. Its key (match id,
-- puuid) is 95 bytes that a rowid table stored twice, in the row and in the
-- key's automatic index: 253 MiB on a 2.3 M-row archive. Copied in key order
-- after DSK-05 packed the rows, so the new b-tree is dense.
--
-- facts_player is recreated: it holds the key's columns already, so it does
-- not grow. facts_champ is not: nothing reads facts by champion alone, and
-- in a WITHOUT ROWID table it would carry the 95-byte key per entry.
CREATE TABLE match_facts_v21 (
  match_id     TEXT NOT NULL REFERENCES matches(match_id) ON DELETE CASCADE,
  key_scope    TEXT NOT NULL,
  puuid        TEXT NOT NULL,
  team_id      INTEGER NOT NULL,
  position     TEXT,
  champion_id  INTEGER NOT NULL,
  win          INTEGER NOT NULL,
  kills INTEGER, deaths INTEGER, assists INTEGER,
  items        TEXT,                              -- JSON [int]
  runes        TEXT,                              -- JSON [primaryStyle, keystone, subStyle, [perks], [stat perks]|null]
  summoners    TEXT,                              -- JSON [int,int]
  facts_version INTEGER NOT NULL,                 -- bump to trigger facts:reextract
  cs INTEGER, gold INTEGER, damage INTEGER, vision INTEGER,
  tier         TEXT,                              -- V0019: the player's tier when the match was archived; NULL unstamped
  PRIMARY KEY (match_id, puuid)
) WITHOUT ROWID;
INSERT INTO match_facts_v21 (match_id, key_scope, puuid, team_id, position, champion_id, win, kills, deaths,
  assists, items, runes, summoners, facts_version, cs, gold, damage, vision, tier)
SELECT match_id, key_scope, puuid, team_id, position, champion_id, win, kills, deaths,
  assists, items, runes, summoners, facts_version, cs, gold, damage, vision, tier
  FROM match_facts ORDER BY match_id, puuid;
DROP TABLE match_facts;
ALTER TABLE match_facts_v21 RENAME TO match_facts;
CREATE INDEX facts_player ON match_facts (key_scope, puuid, match_id);

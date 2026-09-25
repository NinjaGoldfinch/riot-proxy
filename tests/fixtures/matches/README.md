# Match fixtures

Match-v5 bodies for the facts extraction tests (`src/archive/facts/tests.rs`, plan P5-03). They contain no API key; CI greps for `RGAPI-`. PUUIDs in them are encrypted per API key, so they identify nobody outside the key that fetched them.

| File | Source | What it covers |
|---|---|---|
| `ranked-solo.json` | real: `replay/cold-lookup/06` (KR ranked solo, queue 420, recorded 2026-09-24) | the ordinary case: ten players, five positions each side |
| `arena.json` | real: `replay/cold-lookup/09` (KR Arena, queue 1750) | 18 players, 2-player teams, `teamPosition: ""` for every player |
| `remake.json` | **derived** from `replay/cold-lookup/07` (KR ranked solo) | a remake with holes in it |

`remake.json` is a real match edited by a script rather than a recorded remake, which saves Riot calls hunting for one. The edits:
- the game is 185 s long;
- every player has `gameEndedInEarlySurrender: true`, team 100 surrendered and team 200 won;
- K/D/A and items are zeroed;
- participant 0 and 5 have `teamPosition: ""`, participant 1 has no `teamPosition`, participant 2 has no `perks`, participant 3 has no `summoner2Id`.

Replace it with a recorded remake if one turns up; the snapshots will show the difference.

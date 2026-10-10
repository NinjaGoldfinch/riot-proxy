# Build fixtures

For `builds:extract` (BLD-01, ADR-117; BLD-05, ADR-131; BLD-06, ADR-132): `src/archive/builds/tests.rs` and `tests/builds.rs`. BLD-04's showcase tests (`tests/showcase.mjs`) read the golden files too. No API key; CI greps for `RGAPI-`.

| File | Source | What it covers |
|---|---|---|
| `OC1_711969250.match.json` | real: OC1 ranked solo (queue 420, patch 16.19), fetched 2026-10-09, puuids replaced | the match the timeline belongs to, so its facts can be archived |
| `OC1_711969250.timeline.json` | real: that match's timeline, puuids replaced | every event a build reads, including an undone purchase (participant 4's 2031, participant 10's 1036 and 2021), an undone sale (participant 5's 1082), sales, and two `ITEM_PURCHASED` 3865 by `participantId: 0` |
| `item-16.19.1.json` | **derived** from Data Dragon 16.19.1's `item.json` | every item, cut down to `name`, `gold.total`, `gold.purchasable`, `into`, `from` and `tags` |
| `OC1_711969250.expected.json` | **derived** by a script that applies the definitions in IMPLEMENTATION.md §Post-release — BLD | per puuid: `starter`, `boots`, `items`, `skills`, `skillOrder` |
| `OC1_712417978.match.json` | real: OC1 ranked solo (queue 420, patch 16.20), from the dev VM's archive 2026-10-10, puuids replaced | the match the timeline belongs to; its `championId`s are what the Viego rule reads |
| `OC1_712417978.timeline.json` | real: that match's timeline, puuids replaced | Viego's possession (BLD-05): participant 2 (Viego) has two R level-ups at 1370679 ms, in the ms six of his items are destroyed, after possessing Nidalee. Participant 10 (Morgana) takes a real W at 1002026 ms, in the ms one of her items is destroyed. Participant 1 (Zed) has Triple Tonic: 14 points at level 13 |
| `item-16.20.1.json` | **derived** from Data Dragon 16.20.1's `item.json`, cut down as `item-16.19.1.json` | the catalogue for OC1_712417978's patch |
| `OC1_712417978.expected.json` | **derived** as `OC1_711969250.expected.json`, with Viego's `skills` (`"QWQEQRQEQEREE"`) and `skillOrder` (`"QEW"`) written by hand from the BLD-05 rule | per puuid, the same fields. Only Viego differs from the definitions before BLD-05 |
| `OC1_706102889.match.json` | real: OC1 ranked solo (queue 420, patch 16.14), from the dev VM's archive 2026-10-10, puuids replaced | the match the timeline belongs to; its `championId`s say who is Udyr |
| `OC1_706102889.timeline.json` | real: that match's timeline, puuids replaced | Udyr's skill order (BLD-06): participant 1 (Udyr, Triple Tonic) ends at level 20 with 21 points, R6 W6 E6 Q3, R first to 6, with points at levels 19 and 20. Participant 5 (Karma) has her R from level 1 |
| `item-16.14.1.json` | **derived** from Data Dragon 16.14.1's `item.json`, cut down as `item-16.19.1.json` | the catalogue for OC1_706102889's patch |
| `OC1_706102889.expected.json` | **derived** as the others, with Udyr's `skillOrder` (`"RWEQ"`) written by hand from the BLD-06 rule | per puuid, the same fields. Only Udyr differs from the definitions before BLD-06 (`"WEQ"`) |

Every puuid in the match and timeline was replaced with `bld-fixture-puuid-NN`, NN being the player's position in `metadata.participants`, the same in both files. Nothing else was changed.

The golden files were worked out from the definitions independently of `src/archive/builds.rs` and `showcase.html`, so the tests compare implementations, not a module with its own output. The script that derives them reproduces `OC1_711969250.expected.json` byte for byte. Each golden file goes with its own patch's item.json, and the Viego rule needs each player's `championId` from the match file. If a definition changes, change the golden files by hand and say why in the PR.

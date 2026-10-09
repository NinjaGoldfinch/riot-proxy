# Build fixtures

For `builds:extract` (BLD-01, ADR-117): `src/archive/builds/tests.rs` and `tests/builds.rs`. BLD-04's showcase tests read the golden file too. No API key; CI greps for `RGAPI-`.

| File | Source | What it covers |
|---|---|---|
| `OC1_711969250.match.json` | real: OC1 ranked solo (queue 420, patch 16.19), fetched 2026-10-09, puuids replaced | the match the timeline belongs to, so its facts can be archived |
| `OC1_711969250.timeline.json` | real: that match's timeline, puuids replaced | every event a build reads, including an undone purchase (participant 4's 2031, participant 10's 1036 and 2021), an undone sale (participant 5's 1082), sales, and two `ITEM_PURCHASED` 3865 by `participantId: 0` |
| `item-16.19.1.json` | **derived** from Data Dragon 16.19.1's `item.json` | every item, cut down to `name`, `gold.total`, `gold.purchasable`, `into`, `from` and `tags` |
| `OC1_711969250.expected.json` | **derived** by a script that applies the definitions in IMPLEMENTATION.md §Post-release — BLD | per puuid: `starter`, `boots`, `items`, `skills`, `skillOrder` |

Every puuid in the match and timeline was replaced with `bld-fixture-puuid-NN`, NN being the player's position in `metadata.participants`, the same in both files. Nothing else was changed.

The golden file was worked out from the definitions independently of `src/archive/builds.rs`, so the test compares two implementations, not a module with its own output. If a definition changes, change the golden file by hand and say why in the PR.

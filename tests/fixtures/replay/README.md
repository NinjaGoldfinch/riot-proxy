# Replay fixtures

Recorded Riot exchanges served by wiremock in `tests/replay.rs` (plan P3-06).
Each exchange is `NN-<method>.json` (request, status, the rate-limit headers the proxy reads) plus `NN-<method>.body` (the response bytes, verbatim). The API key is never recorded: `riot record` redacts it and refuses to keep anything key-shaped, and CI greps for `RGAPI-`.

## cold-lookup (real, recorded 2026-09-24)

A first lookup of `Hide on bush#KR1`: account (asia), summoner, league entries, top-3 mastery (kr), 5 match ids, and those 5 matches (asia). Re-record with:

```sh
OUT=tests/fixtures/replay/cold-lookup; rm -rf $OUT
R="cargo run -q --features dev-cli -- riot record --out $OUT"
$R account/by-riot-id asia 'Hide on bush' KR1
PUUID=$(python3 -c "import json;print(json.load(open('$OUT/01-account.byRiotId.body'))['puuid'])")
$R summoner/by-puuid kr "$PUUID"
$R league/entries-by-puuid kr "$PUUID"
$R mastery/top-by-puuid kr "$PUUID" -q count=3
$R match/ids-by-puuid asia "$PUUID" -q start=0 -q count=5
for M in $(python3 -c "import json;print(' '.join(json.load(open('$OUT/05-match.idsByPuuid.body'))))"); do $R match.byId asia "$M"; done
```

Re-recording changes the snapshot in `tests/snapshots/`; review it deliberately.

## 429-typed-application (synthetic)

Built from `cold-lookup/02` by editing the status and headers (`"synthetic": true`): an application-typed 429 with `Retry-After: 1`, then the recorded 200. A real application 429 is an accountable violation of the key's limits, so it is never provoked (owner decision).

## status-platform-data (real, recorded 2026-11-20, bodies only)

Three lol-status-v4 `platform-data` bodies, each listing one incident, as production's status poller (v2.11.0) wrote them to `STATUS_RECORDINGS_DIR`: `<platform>-incident-<id>.json` is Riot's body, verbatim (SITE-13, ADR-219). There is no request, status or header file beside a body, so `tests/replay.rs` does not serve them. `tests/openapi.rs` reads every `.json` in the directory and checks it against `PlatformDataDto`, required fields included (SITE-16, ADR-222).

| File | What it shows |
|---|---|
| `na1-incident-13878.json` | a `critical` incident with `updated_at: null` and no `maintenance_status` key |
| `br1-incident-8829.json` | an `info` incident with `maintenance_status: null`, titles in two locales |
| `me1-incident-10557.json` | an `info` incident with `maintenance_status: null`, titles in three locales |

`maintenances` is empty in all three: no maintenance entry is recorded. To add a body (one with a maintenance is the one worth having), copy a file from a deployment's `STATUS_RECORDINGS_DIR`, or keep the `.body` that `riot record status/platform-data <platform>` writes, under a `.json` name. A new file is checked by the same test with no code change.

## champion-rotations (real, recorded 2026-11-20, body only)

`oc1.json` is champion-v3's `champion-rotations` body for OC1: the body of `GET /v1/lol/rotations/oc1` on production (v2.11.0) on a `MISS`, which is Riot's, verbatim. It is `{"sr":[…],"newplayer":[…]}` and nothing else, as the portal's `ChampionInfo` lists it; v1's example (`freeChampionIds`, `freeChampionIdsForNewPlayers`, `maxNewPlayerLevel`) is not what Riot sends (ADR-217, amended for SITE-21). `tests/openapi.rs` checks every `.json` here against `ChampionInfo` (SITE-16, ADR-222). To add one: `riot record platform/champion-rotations <platform>`, keeping the `.body` under a `.json` name.

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

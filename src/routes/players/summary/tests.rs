//! Ported from v1 `test/match-summary.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::*;

const PUUID: &str = "PPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPPP";
const OTHER: &str = "QQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQ";

/// v1's fixture: the fields that matter plus enough bulk to prove it is dropped.
fn full_match(info: Value, participant: Value) -> Value {
    let challenges: serde_json::Map<String, Value> =
        (0..100).map(|i| (format!("challenge{i}"), json!(i))).collect();
    let mut me = json!({
        "puuid": PUUID, "win": true, "gameEndedInEarlySurrender": false, "championId": 64,
        "championName": "LeeSin", "champLevel": 16, "teamId": 100, "teamPosition": "JUNGLE",
        "individualPosition": "JUNGLE", "lane": "JUNGLE", "kills": 8, "deaths": 3, "assists": 11,
        "totalMinionsKilled": 42, "neutralMinionsKilled": 128, "goldEarned": 13240, "visionScore": 31,
        "totalDamageDealtToChampions": 21903, "totalDamageTaken": 30112,
        "item0": 3142, "item1": 6693, "item2": 3814, "item3": 3071, "item4": 3111, "item5": 0, "item6": 3364,
        "roleBoundItem": 1209,
    });
    let more = json!({
        "summoner1Id": 11, "summoner2Id": 4, "riotIdGameName": "Someone", "riotIdTagline": "OCE",
        "perks": {
            "statPerks": {"defense": 5002, "flex": 5008, "offense": 5005},
            "styles": [
                {"description": "primaryStyle", "style": 8000, "selections": [{"perk": 8010, "var1": 1}, {"perk": 9111}]},
                {"description": "subStyle", "style": 8300, "selections": [{"perk": 8306}]}
            ]
        },
        "challenges": challenges,
    });
    for (k, v) in more.as_object().unwrap() {
        me[k] = v.clone();
    }
    for (k, v) in participant.as_object().unwrap() {
        me[k] = v.clone();
    }
    let mut info_v = json!({
        "gameCreation": 1_756_000_000_000_i64, "gameStartTimestamp": 1_756_000_060_000_i64,
        "gameEndTimestamp": 1_756_001_894_000_i64, "gameDuration": 1834, "gameMode": "CLASSIC",
        "gameName": "teambuilder-match-1234567890", "gameType": "MATCHED_GAME",
        "gameVersion": "15.16.673.9260", "mapId": 11, "queueId": 420, "platformId": "OC1",
        "endOfGameResult": "GameComplete", "tournamentCode": "",
        "teams": [
            {"teamId": 100, "win": true, "bans": [{"championId": 64, "pickTurn": 1}], "objectives": {}},
            {"teamId": 200, "win": false, "bans": [], "objectives": {}}
        ],
        "participants": [me, {"puuid": OTHER, "win": false, "championId": 266, "championName": "Aatrox", "kills": 2}],
    });
    for (k, v) in info.as_object().unwrap() {
        info_v[k] = v.clone();
    }
    json!({
        "metadata": {"dataVersion": "2", "matchId": "OC1_1234567890", "participants": [PUUID, OTHER]},
        "info": info_v,
    })
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn summary(v: &Value, puuid: &str, id: &str) -> Option<Value> {
    summarise(&bytes(v), puuid, id).map(|s| serde_json::to_value(s).unwrap())
}

#[test]
fn keeps_the_requesting_players_line_and_nothing_else() {
    let s = summarise(&bytes(&full_match(json!({}), json!({}))), PUUID, "OC1_1234567890").unwrap();
    // Serialised text, so field order is checked too.
    let expected = json!({
        "matchId": "OC1_1234567890", "queueId": 420, "gameMode": "CLASSIC", "gameVersion": "15.16.673.9260",
        "gameCreation": 1_756_000_000_000_i64, "gameEndTimestamp": 1_756_001_894_000_i64, "gameDuration": 1834,
        "endOfGameResult": "GameComplete",
        "player": {
            "puuid": PUUID, "win": true, "gameEndedInEarlySurrender": false, "championId": 64,
            "championName": "LeeSin", "champLevel": 16, "teamId": 100, "teamPosition": "JUNGLE",
            "kills": 8, "deaths": 3, "assists": 11, "totalMinionsKilled": 42, "neutralMinionsKilled": 128,
            "goldEarned": 13240, "visionScore": 31, "totalDamageDealtToChampions": 21903,
            "item0": 3142, "item1": 6693, "item2": 3814, "item3": 3071, "item4": 3111, "item5": 0, "item6": 3364,
            "roleBoundItem": 1209,
            "summoner1Id": 11, "summoner2Id": 4,
            "perks": {"keystone": 8010, "primaryStyle": 8000, "subStyle": 8300}
        }
    });
    assert_eq!(serde_json::to_value(&s).unwrap(), expected);
    let text = serde_json::to_string(&s).unwrap();
    let order: Vec<usize> = [
        "\"matchId\"",
        "\"queueId\"",
        "\"player\"",
        "\"puuid\"",
        "\"item6\"",
        "\"roleBoundItem\"",
        "\"summoner1Id\"",
        "\"perks\"",
    ]
    .iter()
    .map(|k| text.find(k).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "v1's field order: {text}");
}

#[test]
fn drops_the_bulk_that_made_the_page_expensive() {
    let m = full_match(json!({}), json!({}));
    let text = serde_json::to_string(&summarise(&bytes(&m), PUUID, "OC1_1234567890").unwrap()).unwrap();
    for gone in ["challenge", "Aatrox", OTHER, "bans", "statPerks"] {
        assert!(!text.contains(gone), "{gone} leaked");
    }
    assert!(text.len() < bytes(&m).len() / 4);
}

#[test]
fn omits_what_riot_did_not_send_rather_than_nulling_it() {
    let sparse = json!({"metadata": {"matchId": "OC1_1"}, "info": {"participants": [{"puuid": PUUID, "championId": 64}]}});
    assert_eq!(
        summary(&sparse, PUUID, "OC1_1").unwrap(),
        json!({"matchId": "OC1_1", "player": {"puuid": PUUID, "championId": 64}})
    );
}

#[test]
fn falls_back_to_the_id_asked_for() {
    let anonymous = json!({"info": {"participants": [{"puuid": PUUID}]}});
    assert_eq!(summary(&anonymous, PUUID, "OC1_9").unwrap()["matchId"], "OC1_9");
}

#[test]
fn none_when_the_match_names_no_such_player() {
    let m = full_match(json!({}), json!({}));
    assert_eq!(summary(&m, &OTHER.replace('Q', "Z"), "OC1_1234567890"), None);
    assert_eq!(
        summary(&json!({"metadata": {"matchId": "OC1_1"}}), PUUID, "OC1_1"),
        None
    );
    assert_eq!(summary(&Value::Null, PUUID, "OC1_1"), None);
    assert_eq!(summary(&json!("not a match"), PUUID, "OC1_1"), None);
    assert_eq!(summarise(b"not json", PUUID, "OC1_1"), None);
}

#[test]
fn carries_the_arena_fields() {
    let arena = full_match(
        json!({"queueId": 1700, "gameMode": "CHERRY"}),
        json!({"placement": 2, "playerSubteamId": 4}),
    );
    let s = summary(&arena, PUUID, "OC1_1234567890").unwrap();
    assert_eq!(
        (s["queueId"].clone(), s["gameMode"].clone()),
        (json!(1700), json!("CHERRY"))
    );
    assert_eq!(
        (
            s["player"]["placement"].clone(),
            s["player"]["playerSubteamId"].clone()
        ),
        (json!(2), json!(4))
    );
}

#[test]
fn leaves_perks_out_rather_than_guessing() {
    let no_runes = full_match(json!({}), json!({"perks": {"statPerks": {}}}));
    let s = summary(&no_runes, PUUID, "OC1_1234567890").unwrap();
    assert!(s["player"].get("perks").is_none());
}

/// SITE-06: a bot laner's boots sit in the role quest slot, outside item0–6.
/// The recorded ranked game: Lucian (BOTTOM) has 3008 there, Braum (UTILITY)
/// a quest reward; an empty slot stays 0, as Riot sends it.
#[test]
fn copies_the_role_bound_item_as_riot_sends_it() {
    let ranked: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/fixtures/matches/ranked-solo.json"
    ))
    .unwrap();
    let by_champion = |name: &str| {
        let p = ranked["info"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["championName"] == name)
            .unwrap();
        let s = summary(&ranked, p["puuid"].as_str().unwrap(), "KR_8393343196").unwrap();
        s["player"]["roleBoundItem"].clone()
    };
    assert_eq!(by_champion("Lucian"), json!(3008));
    assert_eq!(by_champion("Braum"), json!(2055));

    let empty = full_match(json!({}), json!({"roleBoundItem": 0}));
    assert_eq!(
        summary(&empty, PUUID, "OC1_1").unwrap()["player"]["roleBoundItem"],
        json!(0)
    );
}

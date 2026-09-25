#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const RANKED: &[u8] = include_bytes!("../../../tests/fixtures/matches/ranked-solo.json");
const ARENA: &[u8] = include_bytes!("../../../tests/fixtures/matches/arena.json");
const REMAKE: &[u8] = include_bytes!("../../../tests/fixtures/matches/remake.json");

#[test]
fn ranked_solo_gives_ten_positioned_rows() {
    let facts = extract(RANKED).unwrap();
    assert_eq!(facts.len(), 10);
    assert!(facts.iter().all(|f| f.position.is_some()));
    assert_eq!(facts.iter().filter(|f| f.win).count(), 5);
    assert!(
        facts
            .iter()
            .all(|f| f.items.len() == 6 && f.runes.as_ref().is_some_and(|r| r.keystone.is_some()))
    );
    insta::assert_json_snapshot!("facts_ranked_solo", facts);
}

#[test]
fn arena_has_no_positions() {
    let facts = extract(ARENA).unwrap();
    assert_eq!(facts.len(), 18);
    assert!(facts.iter().all(|f| f.position.is_none()));
    insta::assert_json_snapshot!("facts_arena", facts);
}

#[test]
fn a_remake_with_holes_extracts_without_panicking() {
    let facts = extract(REMAKE).unwrap();
    assert_eq!(facts.len(), 10, "remakes are recorded, as v1 did");
    assert_eq!(
        (facts[0].position.as_deref(), facts[1].position.as_deref()),
        (None, None)
    );
    assert_eq!(facts[2].runes, None);
    assert_eq!(facts[3].summoners, None);
    assert!(facts.iter().all(|f| f.items == [0; 6]));
    insta::assert_json_snapshot!("facts_remake", facts);
}

#[test]
fn stored_json_columns() {
    let f = &extract(RANKED).unwrap()[0];
    assert_eq!(f.items_json(), serde_json::to_string(&f.items).unwrap());
    let runes: serde_json::Value = serde_json::from_str(&f.runes_json().unwrap()).unwrap();
    assert_eq!(
        runes.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["keystone", "perks", "primaryStyle", "statPerks", "subStyle"]
    );
    assert!(f.summoners_json().unwrap().starts_with('['));
}

#[test]
fn participants_missing_a_required_column_are_left_out() {
    let body = br#"{"info":{"participants":[
        {"puuid":"a","teamId":100,"championId":1,"win":true},
        {"teamId":100,"championId":2,"win":true},
        {"puuid":"","teamId":100,"championId":2,"win":true},
        {"puuid":"c","championId":3,"win":false},
        {"puuid":"d","teamId":200,"win":false},
        {"puuid":"e","teamId":200,"championId":5},
        {"puuid":"a","teamId":200,"championId":6,"win":false}
    ]}}"#;
    let facts = extract(body).unwrap();
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        facts[0],
        Fact {
            puuid: "a".into(),
            team_id: 100,
            position: None,
            champion_id: 1,
            win: true,
            kills: None,
            deaths: None,
            assists: None,
            items: vec![0; 6],
            runes: None,
            summoners: None,
        }
    );
}

#[test]
fn partial_rune_pages() {
    let body = br#"{"info":{"participants":[{"puuid":"a","teamId":100,"championId":1,"win":true,
        "perks":{"styles":[{"description":"subStyle","style":8300,"selections":[{"perk":8313}]}]}}]}}"#;
    let runes = extract(body).unwrap()[0].runes.clone().unwrap();
    assert_eq!(
        runes,
        Runes {
            primary_style: None,
            keystone: None,
            sub_style: Some(8300),
            perks: vec![8313],
            stat_perks: None,
        }
    );
}

#[test]
fn bodies_that_are_not_matches_are_errors() {
    assert!(extract(b"").is_err());
    assert!(extract(b"[]").is_err());
    assert!(extract(br#"{"metadata":{}}"#).is_err());
    assert_eq!(extract(br#"{"info":{}}"#).unwrap(), vec![]);
}

proptest::proptest! {
    #[test]
    fn never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
        let _ = extract(&bytes);
    }

    #[test]
    fn never_panics_on_json_participants(
        puuid in proptest::option::of("[a-z]{0,3}"),
        team in proptest::option::of(proptest::num::i64::ANY),
        pos in proptest::option::of("[A-Z]{0,6}"),
        champ in proptest::option::of(proptest::num::i64::ANY),
        win in proptest::option::of(proptest::bool::ANY),
    ) {
        let p = serde_json::json!({"puuid": puuid, "teamId": team, "teamPosition": pos, "championId": champ, "win": win});
        let body = serde_json::to_vec(&serde_json::json!({"info": {"participants": [p]}})).unwrap();
        let facts = extract(&body).unwrap();
        proptest::prop_assert!(facts.len() <= 1);
    }
}

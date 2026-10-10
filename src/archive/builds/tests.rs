#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

const TIMELINE: &[u8] = include_bytes!("../../../tests/fixtures/builds/OC1_711969250.timeline.json");
const ITEMS: &[u8] = include_bytes!("../../../tests/fixtures/builds/item-16.19.1.json");
/// The definitions worked out independently of this module (see the
/// fixtures' README); BLD-04's showcase helpers are held to it too.
const EXPECTED: &[u8] = include_bytes!("../../../tests/fixtures/builds/OC1_711969250.expected.json");
/// OC1_712417978 (patch 16.20): Viego possesses Nidalee and two of her ranks are
/// logged as his level-ups (BLD-05).
const POSSESSION_TIMELINE: &[u8] =
    include_bytes!("../../../tests/fixtures/builds/OC1_712417978.timeline.json");
const POSSESSION_MATCH: &[u8] = include_bytes!("../../../tests/fixtures/builds/OC1_712417978.match.json");
const POSSESSION_ITEMS: &[u8] = include_bytes!("../../../tests/fixtures/builds/item-16.20.1.json");
const POSSESSION_EXPECTED: &[u8] =
    include_bytes!("../../../tests/fixtures/builds/OC1_712417978.expected.json");
const VIEGO: i64 = 234;
const ZED: i64 = 238;

fn catalog() -> ItemCatalog {
    ItemCatalog::from_item_json(ITEMS).unwrap()
}

/// A one-player timeline (`participantId` 1, puuid `p1`) with these events.
fn timeline(events: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "metadata": {"matchId": "OC1_1"},
        "info": {
            "participants": [{"participantId": 1, "puuid": "p1"}],
            "frames": [{"events": events}],
        },
    }))
    .unwrap()
}

fn buy(item: i64, at: i64) -> Value {
    json!({"type": "ITEM_PURCHASED", "participantId": 1, "itemId": item, "timestamp": at})
}

fn level(slot: i64, at: i64) -> Value {
    json!({"type": "SKILL_LEVEL_UP", "participantId": 1, "skillSlot": slot, "levelUpType": "NORMAL", "timestamp": at})
}

fn destroyed(item: i64, at: i64) -> Value {
    json!({"type": "ITEM_DESTROYED", "participantId": 1, "itemId": item, "timestamp": at})
}

/// No champions known: every player is treated as any champion but Viego.
fn no_champions() -> HashMap<String, i64> {
    HashMap::new()
}

/// A match's puuid → championId, as `builds:extract` reads it from `match_facts`.
fn champions_of(match_json: &[u8]) -> HashMap<String, i64> {
    let m: Value = serde_json::from_slice(match_json).unwrap();
    m["info"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["puuid"].as_str().unwrap().to_string(),
                p["championId"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn only(events: Value) -> BuildFact {
    let mut rows = extract(&timeline(events), &catalog(), &no_champions());
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

/// [`only`], with the player on this champion.
fn only_as(champion: i64, events: Value) -> BuildFact {
    let champions = HashMap::from([("p1".to_string(), champion)]);
    let mut rows = extract(&timeline(events), &catalog(), &champions);
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

/// `rows` keyed by puuid, in the golden file's shape.
fn by_puuid(rows: &[BuildFact]) -> BTreeMap<String, Value> {
    rows.iter()
        .map(|b| {
            let mut v = serde_json::to_value(b).unwrap();
            v.as_object_mut().unwrap().remove("puuid");
            (b.puuid.clone(), v)
        })
        .collect()
}

#[test]
fn the_recorded_ranked_game_matches_the_golden_file() {
    // With its champions known, as builds:extract runs it: no Viego, so nothing changes.
    let match_json = include_bytes!("../../../tests/fixtures/builds/OC1_711969250.match.json");
    let rows = extract(TIMELINE, &catalog(), &champions_of(match_json));
    assert_eq!(rows.len(), 10);
    assert_eq!(rows, extract(TIMELINE, &catalog(), &no_champions()));
    let want: BTreeMap<String, Value> = serde_json::from_slice(EXPECTED).unwrap();
    assert_eq!(by_puuid(&rows), want);
    insta::assert_json_snapshot!("builds_ranked_solo", rows);
}

#[test]
fn the_possession_game_matches_its_golden_file() {
    let c = ItemCatalog::from_item_json(POSSESSION_ITEMS).unwrap();
    let rows = extract(POSSESSION_TIMELINE, &c, &champions_of(POSSESSION_MATCH));
    assert_eq!(rows.len(), 10);
    let want: BTreeMap<String, Value> = serde_json::from_slice(POSSESSION_EXPECTED).unwrap();
    assert_eq!(by_puuid(&rows), want);
    // Viego (participant 2): 13 points at level 13, R at 6 and 11 only.
    let viego = &rows[1];
    assert_eq!(viego.skills, "QWQEQRQEQEREE");
    assert_eq!(viego.skill_order.as_deref(), Some("QEW"));
    // Morgana (participant 10) keeps the W she took in the same ms as one ITEM_DESTROYED.
    assert_eq!(rows[9].skills, "QWEQQRQWQWRWW");
    // Without the champions, Viego keeps Nidalee's two R ranks: the rule needs them.
    let blind = extract(POSSESSION_TIMELINE, &c, &no_champions());
    assert_eq!(blind[1].skills, "QWQEQRQEQEREERR");
    assert_eq!(blind[2..], rows[2..]);
}

#[test]
fn a_viego_level_up_in_the_ms_of_his_item_destroyed_is_dropped() {
    let events = json!([
        level(1, 1_000),
        level(3, 2_000),
        // A possession: his inventory goes and two ranks are logged in the same ms.
        destroyed(6676, 900_000),
        destroyed(1001, 900_000),
        level(4, 900_000),
        level(4, 900_000),
        level(1, 950_000),
    ]);
    let b = only_as(VIEGO, events.clone());
    assert_eq!(b.skills, "QEQ");
    assert_eq!(b.skill_order.as_deref(), Some("QEW"));
    // One destroyed item is enough for Viego.
    let b = only_as(
        VIEGO,
        json!([level(2, 1_000), destroyed(2003, 5_000), level(1, 5_000)]),
    );
    assert_eq!(b.skills, "W");
}

#[test]
fn the_same_events_for_another_champion_are_kept() {
    let events = json!([
        level(1, 1_000),
        level(3, 2_000),
        destroyed(6676, 900_000),
        destroyed(1001, 900_000),
        level(4, 900_000),
        level(4, 900_000),
        level(1, 950_000),
    ]);
    assert_eq!(only_as(ZED, events.clone()).skills, "QERRQ");
    // A player whose champion isn't known is not Viego.
    assert_eq!(only(events).skills, "QERRQ");
}

#[test]
fn a_viego_level_up_with_no_item_destroyed_in_its_ms_is_kept() {
    let b = only_as(
        VIEGO,
        json!([
            level(1, 1_000),
            destroyed(6676, 899_999),
            level(4, 900_000),
            destroyed(6673, 900_001),
            // Another player's item destroyed in his millisecond doesn't count.
            {"type": "ITEM_DESTROYED", "participantId": 2, "itemId": 3036, "timestamp": 950_000},
            level(2, 950_000),
        ]),
    );
    assert_eq!(b.skills, "QRW");
}

#[test]
fn a_triple_tonic_player_keeps_the_level_nine_point() {
    // Triple Tonic (rune 8313) grants a point at level 9: two points seconds
    // apart, ending one above champLevel. Nothing caps points by level, Viego included.
    let mut ups: Vec<Value> = [1, 2, 3, 1, 1, 4, 1, 3, 1, 1, 3, 4, 3, 3]
        .iter()
        .enumerate()
        .map(|(k, &s)| level(s, 60_000 * (k as i64 + 1)))
        .collect();
    ups[9] = level(1, 60_000 * 9 + 4_000);
    for champion in [ZED, VIEGO] {
        let b = only_as(champion, Value::Array(ups.clone()));
        assert_eq!(b.skills, "QWEQQRQEQQEREE", "champion {champion}");
    }
    // The recorded Zed with Triple Tonic: 14 points at level 13.
    let c = ItemCatalog::from_item_json(POSSESSION_ITEMS).unwrap();
    let zed = &extract(POSSESSION_TIMELINE, &c, &champions_of(POSSESSION_MATCH))[0];
    assert_eq!(zed.skills.len(), 14);
}

#[test]
fn every_item_kept_in_the_recorded_game_is_finished() {
    let c = catalog();
    for b in extract(TIMELINE, &c, &no_champions()) {
        assert!(!b.items.is_empty(), "{} finished nothing", b.puuid);
        assert!(b.items.iter().all(|&i| c.is_finished(i)));
        assert!(b.boots.is_some_and(|i| c.is_boots(i)));
        assert!(b.skill_order.as_ref().is_some_and(|o| o.len() == 3));
    }
}

#[test]
fn the_catalogue_follows_the_definitions() {
    let c = catalog();
    // Trinity Force: finished. Long Sword: a component. Health Potion: a consumable.
    assert!(c.is_finished(3078));
    assert!(!c.is_finished(1036));
    assert!(!c.is_finished(2003));
    // Boots are never finished, whatever they cost.
    assert!(!c.is_finished(3047));
    // Base Boots aren't boots; tier 2 are; Gunmetal Greaves has no Boots tag
    // but is built from Berserker's Greaves.
    assert!(!c.is_boots(1001));
    assert!(c.is_boots(3006));
    assert!(c.is_boots(3172));
    assert!(!c.is_boots(3078));
    assert!(c.is_trinket(3340));
    assert!(ItemCatalog::from_item_json(b"not json").is_err());
    // No `data`: an empty catalogue, which `builds:extract` refuses to use.
    assert!(ItemCatalog::from_item_json(b"{}").unwrap().is_empty());
}

#[test]
fn an_undone_purchase_is_removed() {
    let b = only(json!([
        buy(3078, 600_000),
        buy(3071, 700_000),
        {"type": "ITEM_UNDO", "participantId": 1, "beforeId": 3071, "afterId": 0, "goldGain": 3000, "timestamp": 701_000},
        buy(3053, 800_000),
    ]));
    assert_eq!(b.items, vec![3078, 3053]);
}

#[test]
fn an_undo_removes_the_latest_earlier_purchase_only() {
    let b = only(json!([
        buy(2003, 10_000),
        buy(2003, 20_000),
        {"type": "ITEM_UNDO", "participantId": 1, "beforeId": 2003, "afterId": 0, "goldGain": 50, "timestamp": 21_000},
    ]));
    assert_eq!(b.starter, vec![2003]);
}

#[test]
fn an_undone_sale_changes_nothing_and_a_sold_item_stays() {
    let b = only(json!([
        buy(3078, 600_000),
        {"type": "ITEM_SOLD", "participantId": 1, "itemId": 3078, "timestamp": 650_000},
        {"type": "ITEM_UNDO", "participantId": 1, "beforeId": 0, "afterId": 3078, "goldGain": -2333, "timestamp": 651_000},
        buy(3071, 700_000),
    ]));
    assert_eq!(b.items, vec![3078, 3071]);
}

#[test]
fn boots_are_the_first_boots_bought_tagged_or_upgraded() {
    // Base Boots first, then tier 2: the tier 2 pair is the choice.
    assert_eq!(
        only(json!([buy(1001, 100_000), buy(3006, 400_000)])).boots,
        Some(3006)
    );
    // An upgrade with no Boots tag still counts.
    assert_eq!(
        only(json!([buy(1001, 100_000), buy(3172, 900_000)])).boots,
        Some(3172)
    );
    assert_eq!(only(json!([buy(1001, 100_000)])).boots, None);
}

#[test]
fn the_starter_is_cut_at_a_minute_without_the_trinket() {
    let b = only(json!([
        buy(3340, 1_000),
        buy(2003, 12_000),
        buy(1055, 10_000),
        buy(2003, 13_000),
        buy(1036, BUILD_STARTER_MS),
    ]));
    assert_eq!(b.starter, vec![1055, 2003, 2003]);
}

#[test]
fn participant_zero_and_evolutions_are_ignored() {
    let rows = extract(
        &serde_json::to_vec(&json!({"info": {
            "participants": [{"participantId": 0, "puuid": "nobody"}, {"participantId": 1, "puuid": "p1"}],
            "frames": [{"events": [
                {"type": "ITEM_PURCHASED", "participantId": 0, "itemId": 3865, "timestamp": 0},
                level(1, 1_000),
                {"type": "SKILL_LEVEL_UP", "participantId": 1, "skillSlot": 2, "levelUpType": "EVOLVE", "timestamp": 2_000},
            ]}],
        }}))
        .unwrap(),
        &catalog(),
        &no_champions(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].puuid, "p1");
    assert!(rows[0].starter.is_empty());
    assert_eq!(rows[0].skills, "Q");
}

#[test]
fn skill_order_ranks_by_points_and_breaks_ties_by_who_got_there_first() {
    // Q W E W E: W and E both reach 2, W first; Q has 1.
    let b = only(json!([
        level(1, 1),
        level(2, 2),
        level(3, 3),
        level(2, 4),
        level(3, 5)
    ]));
    assert_eq!(b.skill_order.as_deref(), Some("WEQ"));
    assert_eq!(b.skills, "QWEWE");
    // R doesn't take part; a skill never levelled goes last.
    let b = only(json!([level(3, 1), level(4, 2), level(3, 3)]));
    assert_eq!(b.skill_order.as_deref(), Some("EQW"));
    assert_eq!(only(json!([])).skill_order, None);
}

#[test]
fn skills_keep_the_first_fifteen_level_ups() {
    let ups: Vec<Value> = (0..18).map(|k| level(k % 3 + 1, k)).collect();
    assert_eq!(only(Value::Array(ups)).skills, "QWEQWEQWEQWEQWE");
}

#[test]
fn unknown_items_are_dropped_and_finished_items_capped_at_six() {
    let b = only(json!([
        buy(999_999, 5_000),
        buy(3078, 600_000),
        buy(3071, 700_000),
        buy(3053, 800_000),
        buy(6333, 900_000),
        buy(3026, 1_000_000),
        buy(3065, 1_100_000),
        buy(3742, 1_200_000),
    ]));
    assert!(b.starter.is_empty());
    assert_eq!(b.items, vec![3078, 3071, 3053, 6333, 3026, 3065]);
}

#[test]
fn a_malformed_timeline_gives_no_rows() {
    let c = catalog();
    assert!(extract(b"", &c, &no_champions()).is_empty());
    assert!(extract(b"{\"info\": 3}", &c, &no_champions()).is_empty());
    assert!(extract(b"{\"metadata\": {}}", &c, &no_champions()).is_empty());
    // A participant with no puuid is skipped, the rest kept.
    let rows = extract(
        br#"{"info": {"participants": [{"participantId": 1}, {"participantId": 2, "puuid": "p2"}], "frames": []}}"#,
        &c,
        &no_champions(),
    );
    assert_eq!(rows.len(), 1);
}

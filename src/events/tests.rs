#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::*;

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const AT: i64 = 1_790_247_623_902;

/// One of every variant, with the payloads v1 published.
fn every_event() -> Vec<Event> {
    let rank = |tier: &str, lp| Rank {
        tier: Some(tier.into()),
        rank: Some("I".into()),
        lp: Some(lp),
    };
    vec![
        Event::GameStarted {
            puuid: P.into(),
            platform: "kr".into(),
            game_id: 8_393_343_196,
            queue_id: Some(420),
            champion_id: Some(134),
        },
        Event::GameEnded {
            puuid: P.into(),
            platform: None,
            game_id: 8_393_343_196,
            queue_id: None,
            champion_id: None,
        },
        Event::RankChanged {
            puuid: P.into(),
            queue: "RANKED_SOLO_5x5".into(),
            before: Some(rank("GRANDMASTER", 980)),
            after: Some(rank("CHALLENGER", 1012)),
        },
        Event::MatchArchived {
            puuid: Some(P.into()),
            match_id: "KR_8393343196".into(),
            patch: Some("16.19".into()),
            participants: vec![P.into()],
        },
        Event::PatchNew {
            version: "16.19.1".into(),
        },
        Event::CrawlPhase {
            crawl_id: "01J9ZZZZZZZZZZZZZZZZZZZZZZ".into(),
            platform: "kr".into(),
            queue: "RANKED_SOLO_5x5".into(),
            phase: "collect".into(),
            stats: json!({"entries": 1100}),
        },
        Event::LadderCrawlCompleted {
            crawl_id: "01J9ZZZZZZZZZZZZZZZZZZZZZZ".into(),
            platform: "kr".into(),
            queue: "RANKED_SOLO_5x5".into(),
            entries: 1100,
            players: 1100,
            duration_s: 3600,
        },
        Event::AnalyticsUpdated {
            platform: "kr".into(),
            queue: "RANKED_SOLO_5x5".into(),
            duration_s: 12,
            tables: [("champion_stats".to_string(), 170)].into(),
        },
        Event::MetricsSnapshot(json!({"archive": {"matches": 5}})),
    ]
}

#[test]
fn every_variant_serialises_to_v1s_frame() {
    let frames: Vec<Value> = every_event()
        .iter()
        .map(|e| serde_json::from_str(&e.frame(AT)).unwrap())
        .collect();
    insta::assert_json_snapshot!("event_frames", frames);
}

#[test]
fn frames_keep_v1s_key_order() {
    let frame = every_event()[4].frame(AT);
    assert_eq!(
        frame,
        r#"{"op":"event","event":"patch.new","topic":"patch","at":1790247623902,"data":{"version":"16.19.1"}}"#
    );
}

#[test]
fn names_are_the_serde_tags_and_all_distinct() {
    let events = every_event();
    assert_eq!(events.len(), NAMES.len(), "one fixture per variant");
    for (e, name) in events.iter().zip(NAMES) {
        let tagged: Value = serde_json::to_value(e).unwrap();
        assert_eq!((e.name(), tagged["event"].as_str().unwrap()), (name, name));
    }
    let unique: std::collections::HashSet<_> = NAMES.iter().collect();
    assert_eq!(unique.len(), NAMES.len());
}

#[test]
fn topics_follow_v1() {
    let topics: Vec<String> = every_event().iter().map(|e| e.topic().to_string()).collect();
    let player = format!("player:{P}");
    assert_eq!(
        topics,
        [
            player.as_str(),
            &player,
            &player,
            &player,
            "patch",
            "ladder",
            "ladder",
            "ladder",
            "metrics"
        ]
    );
    let crawl_archive = Event::MatchArchived {
        puuid: None,
        match_id: "KR_1".into(),
        patch: None,
        participants: vec![],
    };
    assert_eq!(
        crawl_archive.topic().as_str(),
        "firehose",
        "no player to publish to"
    );
    assert_eq!(
        crawl_archive.frame(AT),
        r#"{"op":"event","event":"match.archived","topic":"firehose","at":1790247623902,"data":{"matchId":"KR_1"}}"#,
        "absent fields are left out, as v1 did"
    );
}

#[tokio::test]
async fn publish_reaches_the_topic_and_the_firehose() {
    let hub = Hub::new();
    let started = every_event().remove(0);
    let mut player = hub.subscribe(&started.topic());
    let mut fire = hub.subscribe(&Topic::named(FIREHOSE));
    assert_eq!(publish_at(&hub, &started, AT), 2);
    let expected = started.frame(AT);
    assert_eq!(player.recv().await.unwrap().as_str(), expected);
    assert_eq!(fire.recv().await.unwrap().as_str(), expected);
    assert_eq!(hub.event_counts()["game.started"], 1);
    assert_eq!(
        publish(&Hub::new(), &started),
        0,
        "nobody listening is not an error"
    );
}

#![allow(clippy::unwrap_used, clippy::expect_used)]
//! THR-02's stamps, on the real ranked solo match the archive tests use.

use bytes::Bytes;

use super::*;
use crate::archive::{matches, ranks};
use crate::db::{Db, DbError};
use crate::jobs::ladder::store;

/// A real ranked solo match (tests/fixtures/replay, recorded in P3-06): kr,
/// queue 420, ten participants, ended at [`END`].
const MATCH: &[u8] = include_bytes!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";
const END: i64 = 1_790_247_623_902;
const SCOPE: &str = "s";
const SOLO: &str = "RANKED_SOLO_5x5";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    (dir, db)
}

/// The match's participants, in Riot's order.
fn puuids() -> Vec<String> {
    let body: serde_json::Value = serde_json::from_slice(MATCH).unwrap();
    body["metadata"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect()
}

async fn on_ladder(db: &Db, platform: &str, queue: &str, puuid: &str, tier: &str, at: i64) {
    let (platform, queue, puuid, tier) = (
        platform.to_string(),
        queue.to_string(),
        puuid.to_string(),
        tier.to_string(),
    );
    db.write(move |c| {
        c.execute(
            "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points, wins,
               losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
             VALUES ('s', ?1, ?2, ?3, ?4, 'I', 0, 1, 1, 'c', 'c', ?5)
             ON CONFLICT (key_scope, platform, queue, puuid) DO UPDATE SET tier = excluded.tier,
               updated_at = excluded.updated_at",
            rusqlite::params![platform, queue, puuid, tier, at],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

async fn archive(db: &Db, body: &'static [u8], now: i64) {
    matches::put(db, MATCH_ID, "asia", SCOPE, Bytes::from_static(body), now)
        .await
        .unwrap();
}

/// `puuid → (tier, stamped_at)` for the match, with platform and queue checked.
async fn stamps(db: &Db) -> std::collections::BTreeMap<String, (String, i64)> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT puuid, tier, stamped_at, platform, queue, key_scope FROM match_tiers WHERE match_id = ?1",
        )?;
        let rows = stmt
            .query_map([MATCH_ID], |r| {
                assert_eq!(r.get::<_, String>(3)?, "kr");
                assert_eq!(r.get::<_, String>(4)?, SOLO);
                assert_eq!(r.get::<_, String>(5)?, SCOPE);
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .unwrap()
}

fn tier_of<'a>(s: &'a std::collections::BTreeMap<String, (String, i64)>, puuid: &str) -> &'a str {
    &s[puuid].0
}

#[tokio::test]
async fn archiving_stamps_every_participant_once() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    on_ladder(&db, "kr", SOLO, &p[1], "GRANDMASTER", 1).await;
    // A league lookup newer than the ladder entry wins (ADR-105).
    on_ladder(&db, "kr", SOLO, &p[2], "MASTER", 1).await;
    ranks::record(
        &db,
        SCOPE,
        "kr",
        &p[2],
        br#"[{"queueType":"RANKED_SOLO_5x5","tier":"DIAMOND","rank":"I","leaguePoints":1}]"#,
        5,
        i64::MAX,
    )
    .await
    .unwrap();
    // Flex and another platform don't place a solo game on kr.
    on_ladder(&db, "kr", "RANKED_FLEX_SR", &p[3], "GOLD", 1).await;
    on_ladder(&db, "euw1", SOLO, &p[4], "IRON", 1).await;
    archive(&db, MATCH, 100).await;

    let s = stamps(&db).await;
    assert_eq!(s.len(), 10);
    assert_eq!(tier_of(&s, &p[0]), "MASTER");
    assert_eq!(tier_of(&s, &p[1]), "GRANDMASTER");
    assert_eq!(tier_of(&s, &p[2]), "DIAMOND");
    for q in &p[3..] {
        assert_eq!(tier_of(&s, q), "UNKNOWN", "{q}");
    }
    assert!(s.values().all(|(_, at)| *at == 100));

    // Promoted, then archived again: the first stamp stays.
    on_ladder(&db, "kr", SOLO, &p[0], "CHALLENGER", 200).await;
    archive(&db, MATCH, 300).await;
    let again = stamps(&db).await;
    assert_eq!(tier_of(&again, &p[0]), "MASTER");
    assert_eq!(again, s);
}

#[tokio::test]
async fn a_match_outside_solo_and_flex_is_not_stamped() {
    let (_d, db) = db();
    let aram = String::from_utf8(MATCH.to_vec())
        .unwrap()
        .replacen("\"queueId\":420", "\"queueId\":450", 1);
    let aram: &'static [u8] = Box::leak(aram.into_bytes().into_boxed_slice());
    archive(&db, aram, 100).await;
    assert!(stamps(&db).await.is_empty());
    assert!(!db.read(|c| Ok::<_, DbError>(any_unstamped(c)?)).await.unwrap());
}

#[tokio::test]
async fn reextracting_facts_leaves_the_stamps_alone() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    let before = stamps(&db).await;
    on_ladder(&db, "kr", SOLO, &p[0], "CHALLENGER", 200).await;
    // What `facts:reextract` does for each match.
    let derived = matches::derive(MATCH).unwrap();
    db.write(move |c| {
        let tx = c.transaction()?;
        matches::write_derived(&tx, MATCH_ID, SCOPE, &derived)?;
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    assert_eq!(stamps(&db).await, before);
}

/// A rank seen later places a player's `UNKNOWN` stamps on matches inside
/// the window, never older ones, and never changes a known tier.
#[tokio::test]
async fn a_later_rank_places_recent_unknown_stamps_only() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    let solo = |tier: &str| {
        format!(r#"[{{"queueType":"RANKED_SOLO_5x5","tier":"{tier}","rank":"I","leaguePoints":1}}]"#)
    };

    // The match ended before the window: p[1] stays UNKNOWN.
    ranks::record(&db, SCOPE, "kr", &p[1], solo("EMERALD").as_bytes(), 500, END + 1)
        .await
        .unwrap();
    assert_eq!(tier_of(&stamps(&db).await, &p[1]), "UNKNOWN");
    // Inside it: placed and restamped.
    ranks::record(&db, SCOPE, "kr", &p[1], solo("EMERALD").as_bytes(), 600, END)
        .await
        .unwrap();
    let s = stamps(&db).await;
    assert_eq!(s[&p[1]], ("EMERALD".to_string(), 600));
    // A known tier, whether stamped on archive or placed late, stays.
    for q in [&p[0], &p[1]] {
        ranks::record(&db, SCOPE, "kr", q, solo("IRON").as_bytes(), 700, 0)
            .await
            .unwrap();
    }
    let after = stamps(&db).await;
    assert_eq!(tier_of(&after, &p[0]), "MASTER");
    assert_eq!(after[&p[1]], ("EMERALD".to_string(), 600));
    // Another queue's or platform's rank doesn't apply.
    ranks::record(
        &db,
        SCOPE,
        "kr",
        &p[2],
        br#"[{"queueType":"RANKED_FLEX_SR","tier":"GOLD","rank":"I","leaguePoints":1}]"#,
        800,
        0,
    )
    .await
    .unwrap();
    ranks::record(&db, SCOPE, "euw1", &p[3], solo("GOLD").as_bytes(), 800, 0)
        .await
        .unwrap();
    let after = stamps(&db).await;
    assert_eq!(tier_of(&after, &p[2]), "UNKNOWN");
    assert_eq!(tier_of(&after, &p[3]), "UNKNOWN");
}

/// A ladder page places its players' recent `UNKNOWN` stamps too.
#[tokio::test]
async fn a_ladder_page_places_recent_unknown_stamps() {
    let (_d, db) = db();
    let p = puuids();
    archive(&db, MATCH, 100).await;
    let entry = |puuid: &str, tier: &str| store::Entry {
        puuid: puuid.to_string(),
        tier: tier.to_string(),
        division: "I".into(),
        league_points: 0,
        wins: 1,
        losses: 1,
        veteran: false,
        inactive: false,
        fresh_blood: false,
        hot_streak: false,
    };
    let page = |entries: &[store::Entry], late_since: i64| {
        let db = db.clone();
        let entries = entries.to_vec();
        async move {
            store::write_page(
                &db,
                store::Page {
                    key_scope: SCOPE,
                    crawl_id: "c",
                    platform: "kr",
                    queue: SOLO,
                    entries: &entries,
                    cursor: None,
                    apex_tier: None,
                    now: 900,
                    late_since,
                },
            )
            .await
            .unwrap();
        }
    };
    page(&[entry(&p[0], "MASTER")], END + 1).await;
    assert_eq!(tier_of(&stamps(&db).await, &p[0]), "UNKNOWN");
    page(&[entry(&p[0], "MASTER"), entry(&p[1], "DIAMOND")], END).await;
    let s = stamps(&db).await;
    assert_eq!(s[&p[0]], ("MASTER".to_string(), 900));
    assert_eq!(tier_of(&s, &p[1]), "DIAMOND");
    assert_eq!(tier_of(&s, &p[2]), "UNKNOWN");
}

#[test]
fn the_late_stamp_window() {
    assert_eq!(late_since(100 * DAY_MS, 14), 86 * DAY_MS);
    assert_eq!(late_since(100, 0), i64::MAX, "0 days places nothing");
    assert_eq!(late_since(0, 14), -14 * DAY_MS);
    assert_eq!(ranked_queue(420), Some(SOLO));
    assert_eq!(ranked_queue(440), Some("RANKED_FLEX_SR"));
    assert_eq!(ranked_queue(450), None);
}

/// Matches archived before V0016 are found and stamped at today's tiers.
#[tokio::test]
async fn unstamped_matches_are_found_and_stamped() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    db.write(|c| {
        c.execute("DELETE FROM match_tiers", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let found = db
        .read(|c| {
            Ok::<_, DbError>((
                any_unstamped(c)?,
                unstamped(c, "", 10)?,
                unstamped(c, MATCH_ID, 10)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(found, (true, vec![(MATCH_ID.to_string(), 420)], vec![]));
    let n = db
        .write(|c| Ok::<_, DbError>(stamp(c, MATCH_ID, 420, 5)?))
        .await
        .unwrap();
    assert_eq!(n, 10);
    assert_eq!(tier_of(&stamps(&db).await, &p[0]), "MASTER");
    assert!(!db.read(|c| Ok::<_, DbError>(any_unstamped(c)?)).await.unwrap());
}

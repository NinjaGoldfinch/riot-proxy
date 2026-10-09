#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use serde_json::Value;

use super::*;
use crate::archive::matches;

/// A real ranked solo match (tests/fixtures/replay, recorded in P3-06).
const MATCH: &[u8] = include_bytes!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.body");
const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const SCOPE: &str = "abcd1234";
const END: i64 = 1_790_247_623_902;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

/// The fixture as another game: a new id, queue and end time; optionally a remake.
fn variant(id: &str, queue: i64, end: i64, remake: bool) -> Bytes {
    let mut m: Value = serde_json::from_slice(MATCH).unwrap();
    m["metadata"]["matchId"] = id.into();
    m["info"]["queueId"] = queue.into();
    m["info"]["gameEndTimestamp"] = end.into();
    if remake {
        for p in m["info"]["participants"].as_array_mut().unwrap() {
            p["gameEndedInEarlySurrender"] = true.into();
        }
    }
    Bytes::from(serde_json::to_vec(&m).unwrap())
}

async fn archive(db: &Db, scope: &str, games: &[(&str, i64, i64, bool)]) {
    for (id, queue, end, remake) in games {
        matches::put(db, id, "asia", scope, variant(id, *queue, *end, *remake), 1)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_unknown_player_has_nothing() {
    let (_d, db) = db();
    assert_eq!(summary(&db, SCOPE, PUUID).await.unwrap(), Summary::default());
    assert_eq!(lines(&db, SCOPE, PUUID, None, 0, 25).await.unwrap(), (0, vec![]));
}

#[tokio::test]
async fn summary_counts_every_archived_match_by_queue() {
    let (_d, db) = db();
    archive(
        &db,
        SCOPE,
        &[
            ("KR_1", 420, END - 3_000, false),
            ("KR_2", 420, END - 2_000, false),
            ("KR_3", 440, END - 1_000, true),
            ("KR_4", 420, END, false),
        ],
    )
    .await;
    // Another key's PUUIDs are another player: not counted.
    archive(&db, "otherkey", &[("KR_9", 420, END, false)]).await;
    let s = summary(&db, SCOPE, PUUID).await.unwrap();
    assert_eq!(
        s,
        Summary {
            matches: 4,
            remakes: 1,
            wins: 4, // the fixture's player won; the variants keep that
            timelines: 0,
            oldest_game_end_ms: Some(END - 3_000),
            newest_game_end_ms: Some(END),
            by_queue: vec![(420, 3), (440, 1)],
        }
    );
}

#[tokio::test]
async fn lines_page_newest_first_with_the_total_for_the_filter() {
    let (_d, db) = db();
    let games: Vec<(String, i64)> = (0..30).map(|i| (format!("KR_{i:02}"), END - i * 1_000)).collect();
    for (id, end) in &games {
        let queue = if id.ends_with('7') { 440 } else { 420 };
        archive(&db, SCOPE, &[(id, queue, *end, false)]).await;
    }
    let (total, page) = lines(&db, SCOPE, PUUID, None, 0, 25).await.unwrap();
    assert_eq!((total, page.len()), (30, 25));
    assert_eq!(page[0].match_id, "KR_00");
    let (total, rest) = lines(&db, SCOPE, PUUID, None, 25, 25).await.unwrap();
    assert_eq!((total, rest.len()), (30, 5));
    assert_eq!(rest.last().unwrap().match_id, "KR_29");

    let (total, flex) = lines(&db, SCOPE, PUUID, Some(440), 0, 25).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(
        flex.iter().map(|l| l.match_id.as_str()).collect::<Vec<_>>(),
        ["KR_07", "KR_17", "KR_27"]
    );

    // The player's own line, from match_facts.
    let l = &page[0];
    assert_eq!(
        (
            l.queue_id,
            l.game_end_ms,
            l.game_duration,
            l.remake,
            l.champion_id,
            l.position.as_deref()
        ),
        (420, END, Some(1682), Some(false), 134, Some("MIDDLE"))
    );
    assert_eq!(
        (l.win, l.kills, l.deaths, l.assists, l.cs),
        (true, Some(8), Some(6), Some(14), Some(206))
    );
}

/// The fixture as a game on `champion`, for the champion filter.
async fn archive_on(db: &Db, region: &str, id: &str, queue: i64, end: i64, champion: i64) {
    let mut m: Value = serde_json::from_slice(&variant(id, queue, end, false)).unwrap();
    for p in m["info"]["participants"].as_array_mut().unwrap() {
        if p["puuid"] == PUUID {
            p["championId"] = champion.into();
        }
    }
    let body = Bytes::from(serde_json::to_vec(&m).unwrap());
    matches::put(db, id, region, SCOPE, body, 1).await.unwrap();
}

#[tokio::test]
async fn champion_match_ids_page_one_champion_newest_first() {
    let (_d, db) = db();
    // Ahri (103) on every third game, Taliyah (163) otherwise; KR_05 is flex.
    for i in 0..12 {
        let champion = if i % 3 == 0 { 103 } else { 163 };
        let queue = if i == 6 { 440 } else { 420 };
        archive_on(
            &db,
            "asia",
            &format!("KR_{i:02}"),
            queue,
            END - i * 1_000,
            champion,
        )
        .await;
    }
    // Another region's game on Ahri is not on an asia page.
    archive_on(&db, "europe", "EUW1_1", 420, END + 1_000, 103).await;

    let ids = |start, count, queue| champion_match_ids(&db, SCOPE, PUUID, 103, "asia", queue, start, count);
    assert_eq!(
        ids(0, 10, None).await.unwrap(),
        ["KR_00", "KR_03", "KR_06", "KR_09"]
    );
    assert_eq!(ids(1, 2, None).await.unwrap(), ["KR_03", "KR_06"]);
    assert_eq!(ids(4, 2, None).await.unwrap(), Vec::<String>::new());
    assert_eq!(ids(0, 10, Some(440)).await.unwrap(), ["KR_06"]);
    assert_eq!(
        champion_match_ids(&db, "other", PUUID, 103, "asia", None, 0, 10)
            .await
            .unwrap(),
        Vec::<String>::new(),
        "another key's archive"
    );
}

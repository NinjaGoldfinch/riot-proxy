#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A real ranked solo match (tests/fixtures/replay, recorded in P3-06).
const MATCH: &[u8] = include_bytes!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";
const SCOPE: &str = "abcd1234";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

#[test]
fn extracts_patch_queue_and_end_from_a_real_match() {
    assert_eq!(
        extract(MATCH).unwrap(),
        MatchMeta {
            patch: "16.19".into(),
            queue_id: 420,
            game_end_ms: 1_790_247_623_902,
        }
    );
}

#[test]
fn refuses_bodies_missing_an_indexed_column() {
    for body in [
        &b"not json"[..],
        br#"{"metadata":{}}"#,
        br#"{"info":{"queueId":420,"gameEndTimestamp":1}}"#,
        br#"{"info":{"gameVersion":"14","queueId":420,"gameEndTimestamp":1}}"#,
        br#"{"info":{"gameVersion":"14.18.1","gameEndTimestamp":1}}"#,
        br#"{"info":{"gameVersion":"14.18.1","queueId":420}}"#,
    ] {
        assert!(
            matches!(extract(body), Err(ArchiveError::NotAMatch(_))),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[tokio::test]
async fn round_trip_is_byte_identical_and_compressed() {
    let (_dir, db) = db();
    assert_eq!(get(&db, MATCH_ID).await.unwrap(), None);
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 1_000)
        .await
        .unwrap();
    assert_eq!(get(&db, MATCH_ID).await.unwrap().unwrap().as_ref(), MATCH);

    let (region, patch, queue, end, size, stored, at): (String, String, i64, i64, i64, i64, i64) = db
        .read(|c| {
            c.query_row(
                "SELECT region, patch, queue_id, game_end_ms, body_size, length(body_zstd), archived_at FROM matches",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .map_err(DbError::from)
        })
        .await
        .unwrap();
    assert_eq!(
        (region.as_str(), patch.as_str(), queue, end, at),
        ("asia", "16.19", 420, 1_790_247_623_902, 1_000)
    );
    assert_eq!(size, MATCH.len() as i64);
    assert!(
        stored * 4 < MATCH.len() as i64,
        "zstd shrinks a match well: {stored} of {}",
        MATCH.len()
    );
}

#[tokio::test]
async fn archiving_twice_is_idempotent() {
    let (_dir, db) = db();
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 1_000)
        .await
        .unwrap();
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 2_000)
        .await
        .unwrap();
    let (n, at): (i64, i64) = db
        .read(|c| {
            c.query_row("SELECT count(*), max(archived_at) FROM matches", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(DbError::from)
        })
        .await
        .unwrap();
    assert_eq!((n, at), (1, 2_000));
    assert_eq!(get(&db, MATCH_ID).await.unwrap().unwrap().as_ref(), MATCH);
}

#[tokio::test]
async fn a_body_that_is_not_a_match_is_not_archived() {
    let (_dir, db) = db();
    assert!(
        put(&db, "EUW1_1", "europe", SCOPE, Bytes::from_static(b"{}"), 1)
            .await
            .is_err()
    );
    assert_eq!(get(&db, "EUW1_1").await.unwrap(), None);
}

#[tokio::test]
async fn filter_unarchived_keeps_order_and_spans_chunks() {
    let (_dir, db) = db();
    assert!(filter_unarchived(&db, &[]).await.unwrap().is_empty());
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 1)
        .await
        .unwrap();

    let mut ids: Vec<String> = (0..1_200).map(|i| format!("KR_{i}")).collect();
    ids.insert(700, MATCH_ID.to_string());
    ids.insert(3, MATCH_ID.to_string());
    let left = filter_unarchived(&db, &ids).await.unwrap();
    let expected: Vec<String> = (0..1_200).map(|i| format!("KR_{i}")).collect();
    assert_eq!(left, expected);
}

#[tokio::test]
async fn timelines_need_their_match_first() {
    let (_dir, db) = db();
    let timeline = Bytes::from_static(br#"{"info":{"frames":[]}}"#);
    assert!(
        !put_timeline(&db, MATCH_ID, timeline.clone()).await.unwrap(),
        "no parent yet"
    );
    assert_eq!(get_timeline(&db, MATCH_ID).await.unwrap(), None);

    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 1)
        .await
        .unwrap();
    assert!(put_timeline(&db, MATCH_ID, timeline.clone()).await.unwrap());
    assert!(
        put_timeline(&db, MATCH_ID, timeline.clone()).await.unwrap(),
        "idempotent"
    );
    assert_eq!(get_timeline(&db, MATCH_ID).await.unwrap(), Some(timeline));
}

async fn facts_rows(db: &Db) -> Vec<(String, String, i64)> {
    db.read(|c| {
        let mut s = c.prepare("SELECT key_scope, puuid, facts_version FROM match_facts ORDER BY rowid")?;
        let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn archiving_writes_the_facts_once_per_participant() {
    let (_dir, db) = db();
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 1)
        .await
        .unwrap();
    put(&db, MATCH_ID, "asia", SCOPE, Bytes::from_static(MATCH), 2)
        .await
        .unwrap();
    let rows = facts_rows(&db).await;
    let expected: Vec<_> = facts::extract(MATCH)
        .unwrap()
        .into_iter()
        .map(|f| (SCOPE.to_string(), f.puuid, facts::FACTS_VERSION))
        .collect();
    assert_eq!(rows, expected);
}

#[tokio::test]
async fn a_match_whose_facts_cannot_be_read_is_still_archived() {
    let (_dir, db) = db();
    let body = br#"{"info":{"gameVersion":"14.1.1","queueId":420,"gameEndTimestamp":1,"participants":"x"}}"#;
    put(&db, "EUW1_1", "europe", SCOPE, Bytes::from_static(body), 1)
        .await
        .unwrap();
    assert_eq!(get(&db, "EUW1_1").await.unwrap().unwrap().as_ref(), body);
    assert!(facts_rows(&db).await.is_empty());
}

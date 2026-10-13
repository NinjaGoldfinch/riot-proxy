use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt;

use super::*;
use crate::archive::matches::{self, BodyStates};
use crate::db::DbError;

/// A real ranked solo match (tests/fixtures/replay, recorded in P3-06).
const MATCH: &[u8] = include_bytes!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

/// A store attached to `db`, and a handle on it for the test.
fn attach(db: &Db) -> Objects {
    let objects = Objects::in_memory();
    assert!(db.attach_objects(objects.clone()));
    objects
}

fn body(id: &str) -> Vec<u8> {
    format!(
        "{{\"metadata\":{{\"matchId\":\"{id}\"}},\"info\":{{\"pad\":\"{}\"}}}}",
        "x".repeat(300)
    )
    .into_bytes()
}

/// Rows written straight into `kind`'s table: reads only decompress, so the
/// body need not be a real match.
async fn seed(db: &Db, kind: Kind, ids: &[&str]) {
    let rows: Vec<(String, Vec<u8>)> = ids
        .iter()
        .map(|id| (id.to_string(), zstd::encode_all(body(id).as_slice(), 3).unwrap()))
        .collect();
    db.write(move |c| {
        for (id, blob) in &rows {
            match kind {
                Kind::Match => c.execute(
                    "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                     VALUES (?1, 'asia', '16.19', 420, 1, ?2, 1, 1)",
                    rusqlite::params![id, blob],
                )?,
                Kind::Timeline => c.execute(
                    "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                    rusqlite::params![id, blob],
                )?,
            };
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

/// Pack `list` and free them: the state a body past the window is in.
async fn offload(db: &Db, objects: &Objects, kind: Kind, list: &[&str], at: i64) -> Written {
    let written = write_pack(db, objects, kind, &ids(list), at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(free(db, kind, &ids(list), at + 1).await.unwrap(), list.len());
    written
}

async fn one(db: &Db, sql: &'static str) -> String {
    db.read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get::<_, String>(0))?))
        .await
        .unwrap()
}

/// Warnings logged while `f` runs on this thread.
async fn warnings<F: std::future::Future>(f: F) -> (F::Output, String) {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::WARN),
    );
    let out = {
        let _guard = tracing::subscriber::set_default(subscriber);
        f.await
    };
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, text)
}

use tracing_subscriber::Layer;

#[tokio::test]
async fn an_offloaded_match_reads_from_its_range_byte_identical() {
    let (_dir, db) = db();
    let objects = attach(&db);
    matches::put(&db, MATCH_ID, "asia", "s", Bytes::from_static(MATCH), 1)
        .await
        .unwrap();
    let written = offload(&db, &objects, Kind::Match, &[MATCH_ID], 10).await;
    assert_eq!(written.bodies, 1);
    assert!(
        written.key.starts_with("packs/matches/1970/01/"),
        "{}",
        written.key
    );
    assert_eq!(
        one(
            &db,
            "SELECT length(body_zstd) || ' ' || packed_at || ' ' || offloaded_at FROM matches"
        )
        .await,
        "0 10 11",
        "freed: the empty blob, both stamps"
    );
    assert_eq!(
        matches::get(&db, MATCH_ID).await.unwrap().unwrap().as_ref(),
        MATCH
    );
    let many = matches::get_many(&db, &ids(&[MATCH_ID])).await.unwrap();
    assert_eq!(many[MATCH_ID].as_ref(), MATCH);
}

#[tokio::test]
async fn a_packed_body_keeps_its_local_copy_and_still_reads_without_the_store() {
    let (_dir, db) = db();
    let objects = Objects::in_memory(); // not attached
    seed(&db, Kind::Match, &["KR_1"]).await;
    write_pack(&db, &objects, Kind::Match, &ids(&["KR_1"]), 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        one(
            &db,
            "SELECT coalesce(offloaded_at, '-') || ' ' || (length(body_zstd) > 0) FROM matches"
        )
        .await,
        "- 1"
    );
    assert_eq!(
        matches::get(&db, "KR_1").await.unwrap().unwrap().as_ref(),
        body("KR_1")
    );
}

#[tokio::test]
async fn get_many_mixes_local_and_offloaded_ids_across_packs() {
    let (_dir, db) = db();
    let objects = attach(&db);
    let all = ["KR_1", "KR_2", "KR_3", "KR_4", "KR_5"];
    seed(&db, Kind::Match, &all).await;
    let a = offload(&db, &objects, Kind::Match, &["KR_1", "KR_3"], 10).await;
    let b = offload(&db, &objects, Kind::Match, &["KR_4"], 20).await;
    assert_ne!(a.key, b.key);
    // KR_2 local, KR_5 packed but not freed.
    write_pack(&db, &objects, Kind::Match, &ids(&["KR_5"]), 30)
        .await
        .unwrap()
        .unwrap();

    let mut wanted = ids(&all);
    wanted.push("KR_404".into());
    let found = matches::get_many(&db, &wanted).await.unwrap();
    assert_eq!(found.len(), 5, "{:?}", found.keys());
    for id in all {
        assert_eq!(found[id].as_ref(), body(id), "{id}");
    }
    assert_eq!(objects.list("packs/").await.unwrap().len(), 3);
}

#[tokio::test]
async fn timelines_offload_and_read_the_same_way() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Timeline, &["KR_1", "KR_2"]).await;
    seed(&db, Kind::Match, &["KR_1"]).await;
    let w = offload(&db, &objects, Kind::Timeline, &["KR_1", "KR_2"], 10).await;
    assert!(w.key.starts_with("packs/timelines/"), "{}", w.key);
    assert_eq!(
        matches::get_timeline(&db, "KR_1")
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        body("KR_1")
    );
    let found = matches::get_timelines(&db, &ids(&["KR_1", "KR_2"]))
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    // The match of the same id is untouched.
    assert_eq!(
        matches::get(&db, "KR_1").await.unwrap().unwrap().as_ref(),
        body("KR_1")
    );
    assert_eq!(
        one(&db, "SELECT coalesce(packed_at, '-') FROM matches").await,
        "-"
    );
}

#[tokio::test]
async fn a_missing_location_returns_none_and_warns() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1"]).await;
    offload(&db, &objects, Kind::Match, &["KR_1"], 10).await;
    db.write(|c| Ok::<_, DbError>(c.execute("DELETE FROM body_locations", []).map(|_| ())?))
        .await
        .unwrap();
    let (got, log) = warnings(matches::get(&db, "KR_1")).await;
    assert_eq!(got.unwrap(), None);
    assert!(log.contains("offloaded body has no location"), "{log}");
}

#[tokio::test]
async fn a_store_error_returns_none_and_warns() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    let w = offload(&db, &objects, Kind::Match, &["KR_1", "KR_2"], 10).await;
    objects.delete(&w.key).await.unwrap();
    let (got, log) = warnings(matches::get_many(&db, &ids(&["KR_1", "KR_2"]))).await;
    assert!(got.unwrap().is_empty());
    assert_eq!(
        log.matches("pack read failed").count(),
        1,
        "one warning per pack: {log}"
    );
}

#[tokio::test]
async fn a_digest_mismatch_returns_none_and_warns() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    let w = offload(&db, &objects, Kind::Match, &["KR_1", "KR_2"], 10).await;
    // Same length, one byte changed inside KR_2's body.
    let mut bytes = objects.get_range(&w.key, 0..w.bytes).await.unwrap().to_vec();
    let at: i64 = db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT offset FROM body_locations WHERE match_id = 'KR_2'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    bytes[at as usize + 4] ^= 0xff;
    objects.put(&w.key, bytes.into(), &[]).await.unwrap();

    let (got, log) = warnings(matches::get_many(&db, &ids(&["KR_1", "KR_2"]))).await;
    let got = got.unwrap();
    assert_eq!(got.keys().collect::<Vec<_>>(), ["KR_1"]);
    assert!(log.contains("failed its sha256") && log.contains("KR_2"), "{log}");
}

#[tokio::test]
async fn without_a_store_an_offloaded_body_is_absent_never_empty() {
    let (_dir, db) = db();
    let objects = Objects::in_memory(); // never attached
    seed(&db, Kind::Match, &["KR_1"]).await;
    offload(&db, &objects, Kind::Match, &["KR_1"], 10).await;
    let (got, log) = warnings(matches::get(&db, "KR_1")).await;
    assert_eq!(got.unwrap(), None, "the empty blob is never returned as a body");
    assert!(log.contains("no ARCHIVE_OBJECT_URL"), "{log}");
    assert!(matches::get_many(&db, &ids(&["KR_1"])).await.unwrap().is_empty());
}

#[tokio::test]
async fn only_packed_bodies_are_ever_freed() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    write_pack(&db, &objects, Kind::Match, &ids(&["KR_1"]), 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        free(&db, Kind::Match, &ids(&["KR_1", "KR_2", "KR_404"]), 11)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        free(&db, Kind::Match, &ids(&["KR_1"]), 12).await.unwrap(),
        0,
        "already freed"
    );
    assert_eq!(
        one(
            &db,
            "SELECT group_concat(match_id || ':' || length(body_zstd) || ':' || coalesce(offloaded_at, '-'), ' ')
               FROM (SELECT * FROM matches ORDER BY match_id)"
        )
        .await,
        format!("KR_1:0:11 KR_2:{}:-", zstd::encode_all(body("KR_2").as_slice(), 3).unwrap().len())
    );
    // A packed row whose location went missing is not freed either.
    seed(&db, Kind::Match, &["KR_3"]).await;
    write_pack(&db, &objects, Kind::Match, &ids(&["KR_3"]), 20)
        .await
        .unwrap()
        .unwrap();
    db.write(|c| {
        Ok::<_, DbError>(
            c.execute("DELETE FROM body_locations WHERE match_id = 'KR_3'", [])
                .map(|_| ())?,
        )
    })
    .await
    .unwrap();
    assert_eq!(free(&db, Kind::Match, &ids(&["KR_3"]), 21).await.unwrap(), 0);
}

#[tokio::test]
async fn write_pack_skips_freed_bodies_and_writes_nothing_without_local_ones() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    offload(&db, &objects, Kind::Match, &["KR_1"], 10).await;
    let w = write_pack(&db, &objects, Kind::Match, &ids(&["KR_1", "KR_2"]), 20)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(w.bodies, 1, "only KR_2 still had a local body");
    assert_eq!(
        write_pack(&db, &objects, Kind::Match, &ids(&["KR_1", "KR_404"]), 30)
            .await
            .unwrap(),
        None
    );
    assert_eq!(objects.list("packs/").await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_uploaded_pack_carries_its_digest_and_count() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    let w = write_pack(&db, &objects, Kind::Match, &ids(&["KR_2", "KR_1"]), 10)
        .await
        .unwrap()
        .unwrap();
    let head = objects.head(&w.key).await.unwrap().unwrap();
    assert_eq!(head.size, w.bytes);
    assert_eq!(head.metadata["bodies"], "2");
    let stored = objects.get_range(&w.key, 0..w.bytes).await.unwrap();
    assert_eq!(head.metadata["sha256"], hex::encode(pack::sha256(&stored)));
    let entries = pack::read(Kind::Match, &stored).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.match_id.as_str()).collect::<Vec<_>>(),
        ["KR_1", "KR_2"]
    );
    assert!(pack::verify(&stored, &entries));
}

/// A store without metadata (a local directory) is verified by reading back.
#[tokio::test]
async fn a_local_directory_store_round_trips() {
    let (dir, db) = db();
    let objects = Objects::from_store(
        Arc::new(object_store::local::LocalFileSystem::new_with_prefix(dir.path()).unwrap()),
        false,
        "file://",
    );
    assert!(db.attach_objects(objects.clone()));
    seed(&db, Kind::Match, &["KR_1", "KR_2"]).await;
    offload(&db, &objects, Kind::Match, &["KR_1", "KR_2"], 10).await;
    assert_eq!(
        matches::get_many(&db, &ids(&["KR_1", "KR_2"]))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn archiving_an_offloaded_match_again_makes_it_local() {
    let (_dir, db) = db();
    let objects = attach(&db);
    matches::put(&db, MATCH_ID, "asia", "s", Bytes::from_static(MATCH), 1)
        .await
        .unwrap();
    offload(&db, &objects, Kind::Match, &[MATCH_ID], 10).await;
    matches::put(&db, MATCH_ID, "asia", "s", Bytes::from_static(MATCH), 20)
        .await
        .unwrap();
    assert_eq!(
        one(&db, "SELECT packed_at || ' ' || coalesce(offloaded_at, '-') || ' ' || (length(body_zstd) > 0) FROM matches").await,
        "10 - 1",
        "packed (its location is still good), no longer offloaded"
    );
    seed(&db, Kind::Timeline, &["KR_9"]).await;
    offload(&db, &objects, Kind::Timeline, &["KR_9"], 30).await;
    matches::put_timeline(&db, "KR_9", Bytes::from(body("KR_9")))
        .await
        .unwrap();
    assert_eq!(
        one(&db, "SELECT coalesce(offloaded_at, '-') FROM timelines").await,
        "-"
    );
    // Read from the local copy: no pack is needed any more.
    for o in objects.list("packs/").await.unwrap() {
        objects.delete(&o.key).await.unwrap();
    }
    assert_eq!(
        matches::get(&db, MATCH_ID).await.unwrap().unwrap().as_ref(),
        MATCH
    );
    assert_eq!(
        matches::get_timeline(&db, "KR_9")
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        body("KR_9")
    );
}

#[tokio::test]
async fn stats_count_each_state_and_the_packs() {
    let (_dir, db) = db();
    let objects = attach(&db);
    seed(&db, Kind::Match, &["KR_1", "KR_2", "KR_3"]).await;
    seed(&db, Kind::Timeline, &["KR_1", "KR_2"]).await;
    let w = offload(&db, &objects, Kind::Match, &["KR_1"], 10).await;
    let p = write_pack(&db, &objects, Kind::Match, &ids(&["KR_2"]), 20)
        .await
        .unwrap()
        .unwrap();
    let t = write_pack(&db, &objects, Kind::Timeline, &ids(&["KR_1"]), 30)
        .await
        .unwrap()
        .unwrap();
    let s = matches::stats(&db).await.unwrap();
    assert_eq!(
        s.match_bodies,
        BodyStates {
            local: 1,
            packed: 1,
            offloaded: 1
        }
    );
    assert_eq!(
        s.timeline_bodies,
        BodyStates {
            local: 1,
            packed: 1,
            offloaded: 0
        }
    );
    assert_eq!((s.matches, s.timelines, s.packs), (3, 2, 3));
    assert_eq!(s.pack_bytes as u64, w.bytes + p.bytes + t.bytes);
    assert!(
        s.pack_live_bytes > 0 && s.pack_live_bytes < s.pack_bytes,
        "bodies, not the index"
    );
    let local = zstd::encode_all(body("KR_2").as_slice(), 3).unwrap().len() * 2;
    assert_eq!(
        s.stored_bytes as usize, local,
        "a freed body takes no bytes in SQLite"
    );
}

/// The state columns sit after the body in each row; counting them must read
/// the partial indexes, never the table.
#[tokio::test]
async fn state_counts_read_only_the_partial_indexes() {
    let (_dir, db) = db();
    let plans = db
        .read(|c| {
            let mut out = Vec::new();
            for table in ["matches", "timelines"] {
                for cond in [
                    "packed_at IS NULL",
                    "packed_at IS NOT NULL AND offloaded_at IS NULL",
                ] {
                    let sql = format!("EXPLAIN QUERY PLAN SELECT count(*) FROM {table} WHERE {cond}");
                    let plan: String = c.query_row(&sql, [], |r| r.get(3))?;
                    out.push(plan);
                }
            }
            Ok::<_, DbError>(out)
        })
        .await
        .unwrap();
    assert_eq!(
        plans,
        [
            "SCAN matches USING COVERING INDEX matches_to_pack",
            "SCAN matches USING COVERING INDEX matches_to_free",
            "SCAN timelines USING COVERING INDEX timelines_to_pack",
            "SCAN timelines USING COVERING INDEX timelines_to_free",
        ]
    );
}

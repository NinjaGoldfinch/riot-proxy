//! `archive:offload` end to end (OBJ-03, design/12 §Offload job): bodies
//! archived through the fetcher's archive queue the job once a pack is full;
//! a run packs them into a local-directory "bucket", frees what left the
//! window, and every body still reads back byte for byte.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use riot_proxy::archive::objects::Objects;
use riot_proxy::archive::pack::Kind;
use riot_proxy::archive::{SqliteArchive, matches};
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::db::{Db, DbError};
use riot_proxy::fetcher::Archive;
use riot_proxy::jobs::Queue;
use riot_proxy::jobs::offload::{OffloadContext, Pending};
use riot_proxy::riot::client::RiotRequest;
use riot_proxy::riot::endpoints::Endpoint;
use riot_proxy::riot::routing::Region;

const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";
const DAY: i64 = 86_400_000;

fn request(endpoint: &str, id: &str) -> RiotRequest {
    let e = Endpoint::by_id(endpoint).unwrap();
    RiotRequest::new(e, e.target_for_region(Region::Asia).unwrap(), &[id]).unwrap()
}

/// A database with a local-directory bucket attached.
fn setup() -> (tempfile::TempDir, Db, Objects) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 4).unwrap();
    let bucket = dir.path().join("bucket");
    std::fs::create_dir_all(&bucket).unwrap();
    let objects = Objects::from_store(
        Arc::new(object_store::local::LocalFileSystem::new_with_prefix(&bucket).unwrap()),
        false,
        "file://bucket",
    );
    assert!(db.attach_objects(objects.clone()));
    (dir, db, objects)
}

fn context(
    db: &Db,
    objects: &Objects,
    pending: Arc<Pending>,
    target: u64,
    local_days: u32,
) -> OffloadContext {
    OffloadContext {
        db: db.clone(),
        objects: objects.clone(),
        pending,
        local_days,
        target_bytes: target,
        pace: Duration::ZERO,
    }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// A match and its timeline stored through the archive count towards a pack;
/// the first full one queues the job, and a run packs both kinds.
#[tokio::test]
async fn archiving_through_the_fetchers_archive_queues_the_offload() {
    let (_dir, db, objects) = setup();
    let queue = Queue::new(db.clone());
    let pending = Arc::new(Pending::new(queue.clone(), 4_096));
    let archive = SqliteArchive::new(
        db.clone(),
        KeyScope::from_key(&riot_proxy::config::Secret::new("RGAPI-test-key")),
    )
    .count_unpacked(Arc::clone(&pending));

    archive
        .put(&request("match.byId", MATCH_ID), Bytes::from_static(MATCH))
        .await;
    assert!(
        pending.bytes(Kind::Match) > 4_096,
        "a real match is over 4 KB of zstd"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM jobs WHERE kind = 'archive:offload' AND state = 'pending'"
        )
        .await,
        1
    );
    let timeline = Bytes::from(format!(
        r#"{{"metadata":{{"matchId":"{MATCH_ID}"}},"info":{{"frames":[]}}}}"#
    ));
    archive
        .put(&request("match.timeline", MATCH_ID), timeline.clone())
        .await;
    assert!(pending.bytes(Kind::Timeline) > 0);

    let now = riot_proxy::clock::Clock::now().unix_ms;
    let report = context(&db, &objects, Arc::clone(&pending), 4_096, 14)
        .run(now)
        .await
        .unwrap();
    assert_eq!(
        report.packs, 1,
        "the match is a full pack; the timeline waits for more"
    );
    assert_eq!(pending.bytes(Kind::Match), 0);
    assert_eq!(
        matches::get(&db, MATCH_ID).await.unwrap().unwrap().as_ref(),
        MATCH
    );
    assert_eq!(
        matches::get_timeline(&db, MATCH_ID).await.unwrap().unwrap(),
        timeline
    );
}

/// design/12 §Tasks: 500 matches with timelines offload into several packs
/// and every body reads back.
#[tokio::test]
async fn five_hundred_matches_with_timelines_offload_end_to_end() {
    let (_dir, db, objects) = setup();
    let ids: Vec<String> = (0..500)
        .map(|i| format!("OC1_{:09}", 700_000_000 + i * 37))
        .collect();
    let body = |id: &str, kind: &str| {
        format!(
            r#"{{"metadata":{{"matchId":"{id}","kind":"{kind}"}},"info":{{"pad":"{}"}}}}"#,
            id.repeat(40)
        )
        .into_bytes()
    };
    let seed = ids.clone();
    db.write(move |c| {
        let tx = c.transaction()?;
        for (n, id) in seed.iter().enumerate() {
            let at = 100 * DAY - i64::try_from(n).unwrap() * 60_000 - 20 * DAY;
            tx.execute(
                "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                 VALUES (?1, 'sea', '16.19', 420, 1, ?2, 1, ?3)",
                rusqlite::params![id, zstd::encode_all(body(id, "m").as_slice(), 3).unwrap(), at],
            )?;
            tx.execute(
                "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                rusqlite::params![id, zstd::encode_all(body(id, "t").as_slice(), 3).unwrap()],
            )?;
        }
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();

    let pending = Arc::new(Pending::new(Queue::new(db.clone()), 0));
    let report = context(&db, &objects, pending, 16 * 1024, 14)
        .run(100 * DAY)
        .await
        .unwrap();
    assert_eq!((report.packed_bodies, report.freed), (1_000, 1_000), "{report:?}");
    assert!(report.packs > 4, "several packs per kind: {report:?}");
    let listed = objects.list("packs/").await.unwrap();
    assert_eq!(listed.len(), report.packs);
    assert_eq!(
        count(&db, "SELECT count(*) FROM body_packs").await,
        i64::try_from(report.packs).unwrap()
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM matches WHERE length(body_zstd) = 0 AND offloaded_at IS NOT NULL"
        )
        .await
            + count(
                &db,
                "SELECT count(*) FROM timelines WHERE length(body_zstd) = 0 AND offloaded_at IS NOT NULL"
            )
            .await,
        1_000,
        "every body freed"
    );

    let got = matches::get_many(&db, &ids).await.unwrap();
    let tls = matches::get_timelines(&db, &ids).await.unwrap();
    assert_eq!((got.len(), tls.len()), (500, 500));
    for id in &ids {
        assert_eq!(got[id].as_ref(), body(id, "m"), "{id}");
        assert_eq!(tls[id].as_ref(), body(id, "t"), "{id}");
    }
    let s = matches::stats(&db).await.unwrap();
    assert_eq!(
        (
            s.match_bodies.offloaded,
            s.timeline_bodies.offloaded,
            s.stored_bytes
        ),
        (500, 500, 0)
    );
}

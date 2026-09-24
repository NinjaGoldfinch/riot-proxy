//! L2 against a real SQLite file: write-behind, restart, warm, sweep.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use bytes::Bytes;
use riot_proxy::cache::ResponseCache;
use riot_proxy::cache::l1::{L1, Lookup};
use riot_proxy::cache::l2::{self, L2Writer, Row};
use riot_proxy::clock::Clock;
use riot_proxy::db::Db;
use riot_proxy::riot::endpoints::{Endpoint, Ttls};

fn ep(id: &str) -> &'static Endpoint {
    Endpoint::by_id(id).unwrap()
}

fn ttls(soft: u64, hard: u64) -> Ttls {
    Ttls {
        soft: Some(Duration::from_secs(soft)),
        hard: Some(Duration::from_secs(hard)),
        negative: None,
    }
}

fn l1() -> L1 {
    L1::new(8 * 1024 * 1024)
}

async fn rows(db: &Db) -> i64 {
    db.read(|c| {
        Ok::<i64, riot_proxy::db::DbError>(c.query_row("SELECT COUNT(*) FROM cache", [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

/// Plan P3-03: put → restart Db → warm → HIT.
#[tokio::test]
async fn put_restart_warm_hit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("riot-proxy.db");
    let body = Bytes::from_static(b"{\"puuid\":\"P\",\"gameName\":\"x\"}");

    let content_at;
    {
        let db = Db::open(&path, 1).unwrap();
        let cache = ResponseCache::new(l1(), Some(L2Writer::spawn(db.clone())));
        let e = cache
            .put(
                "s:account.byPuuid:h:P",
                ep("account.byPuuid"),
                body.clone(),
                &ttls(3600, 14_400),
            )
            .await
            .unwrap();
        content_at = e.content_at;
        cache
            .put_negative(
                "s:summoner.byPuuid:h:GONE",
                ep("summoner.byPuuid"),
                Duration::from_secs(300),
            )
            .await;
        // Not an L2 tier: stays in memory only.
        cache
            .put(
                "s:spectator.activeGame:h:P",
                ep("spectator.activeGame"),
                Bytes::from_static(b"{}"),
                &ttls(30, 120),
            )
            .await;
        cache.shutdown().await;
        assert_eq!(rows(&db).await, 2);
    }

    let db = Db::open(&path, 1).unwrap();
    let warmed = l1();
    assert_eq!(l2::warm(&db, &warmed).await.unwrap(), 2);
    match warmed.get("s:account.byPuuid:h:P").await {
        Lookup::Fresh(e) => {
            assert_eq!(e.body, body, "byte-identical after the round trip");
            assert_eq!(e.status, 200);
            // Stored as unix ms: within a millisecond of the original instant.
            let drift = if e.content_at > content_at {
                e.content_at - content_at
            } else {
                content_at - e.content_at
            };
            assert!(drift <= Duration::from_millis(2), "{drift:?}");
        }
        other => panic!("{other:?}"),
    }
    match warmed.get("s:summoner.byPuuid:h:GONE").await {
        Lookup::Fresh(e) => assert!(e.is_negative()),
        other => panic!("{other:?}"),
    }
    assert_eq!(warmed.get("s:spectator.activeGame:h:P").await, Lookup::Miss);
}

#[tokio::test]
async fn warm_skips_and_sweep_deletes_expired_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let now = Clock::now().unix_ms;
    let row = |key: &str, hard: i64| Row {
        key: key.into(),
        body: Bytes::from_static(b"{}"),
        status: 200,
        content_at: now - 10_000,
        soft_expires: now - 5_000,
        hard_expires: hard,
    };
    l2::put_rows(&db, vec![row("live-stale", now + 60_000), row("dead", now - 1)])
        .await
        .unwrap();

    let warmed = l1();
    assert_eq!(l2::warm(&db, &warmed).await.unwrap(), 1);
    assert!(
        matches!(warmed.get("live-stale").await, Lookup::Stale(_)),
        "soft passed, hard not"
    );
    assert_eq!(warmed.get("dead").await, Lookup::Miss);

    assert_eq!(l2::sweep(&db).await.unwrap(), 1);
    assert_eq!(rows(&db).await, 1);
}

#[tokio::test]
async fn flushes_on_batch_size_without_waiting_for_the_timer() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let writer = L2Writer::spawn_with(db.clone(), 10, Duration::from_secs(3600));
    let cache = ResponseCache::new(l1(), Some(writer));
    for i in 0..10 {
        cache
            .put(
                &format!("k{i}"),
                ep("summoner.byPuuid"),
                Bytes::from_static(b"{}"),
                &ttls(60, 60),
            )
            .await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rows(&db).await < 10 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "batch of 10 never flushed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn flushes_on_the_timer_with_a_partial_batch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let writer = L2Writer::spawn_with(db.clone(), 500, Duration::from_millis(150));
    let cache = ResponseCache::new(l1(), Some(writer));
    cache
        .put(
            "one",
            ep("mastery.byPuuid"),
            Bytes::from_static(b"[]"),
            &ttls(60, 60),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rows(&db).await, 0, "still inside the batch window");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rows(&db).await, 1);
}

#[tokio::test]
async fn upserts_replace_and_delete_where_purges() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let cache = ResponseCache::new(l1(), Some(L2Writer::spawn(db.clone())));
    for (k, b) in [("s:a:h:P", "1"), ("s:a:h:P", "2"), ("s:a:h:Q", "3")] {
        cache
            .put(
                k,
                ep("summoner.byPuuid"),
                Bytes::from(b.to_string()),
                &ttls(60, 60),
            )
            .await;
    }
    cache.shutdown().await;
    cache.shutdown().await; // idempotent
    assert_eq!(rows(&db).await, 2);
    let body: Vec<u8> = db
        .read(|c| {
            Ok::<_, riot_proxy::db::DbError>(c.query_row(
                "SELECT body FROM cache WHERE key = 's:a:h:P'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(body, b"2");
    assert_eq!(l2::delete_where(&db, |k| k.ends_with(":Q")).await.unwrap(), 1);
    assert_eq!(rows(&db).await, 1);
}

#[tokio::test]
async fn without_l2_nothing_is_persisted() {
    let cache = ResponseCache::new(l1(), None);
    cache
        .put(
            "k",
            ep("account.byPuuid"),
            Bytes::from_static(b"{}"),
            &ttls(60, 60),
        )
        .await
        .unwrap();
    cache.shutdown().await;
    assert!(matches!(cache.get("k").await, Lookup::Fresh(_)));
}

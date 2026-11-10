//! L2 against a real SQLite file: write-behind, restart, warm, sweep.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use bytes::Bytes;
use riot_proxy::cache::ResponseCache;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::cache::l1::{EXPIRED_KEEP, L1, Lookup};
use riot_proxy::cache::l2::{self, Kept, L2Writer, Row};
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

/// The key scope the kept rows of these tests are under.
fn scope() -> KeyScope {
    KeyScope::from_key(&riot_proxy::config::Secret::new("RGAPI-test-key"))
}

fn kept() -> Kept {
    Kept::for_scope(&scope())
}

/// A key as `cache_key` makes one for `endpoint` under [`scope`].
fn key(endpoint: &str, param: &str) -> String {
    format!("{}:{endpoint}:h:{param}", scope())
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
    assert_eq!(l2::warm(&db, &warmed, &kept()).await.unwrap().loaded, 2);
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

/// AVL-03 (ADR-212), AVL-08: a 200 of an endpoint a player's page reads is
/// kept `EXPIRED_KEEP` past its hard TTL, as L1 keeps it, and warms as
/// `Expired`. Every other row is as it was before AVL-03: loaded only inside
/// its hard TTL and swept at it, like a negative one.
#[tokio::test]
async fn warm_keeps_a_players_expired_rows_for_a_day_and_sweep_deletes_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let now = Clock::now().unix_ms;
    let keep = i64::try_from(EXPIRED_KEEP.as_millis()).unwrap();
    let row = |key: String, status: u16, hard: i64| Row {
        key,
        body: Bytes::from_static(b"{}"),
        status,
        content_at: now - 10_000,
        fetched_at: now - 5_000,
        soft_expires: now - 5_000,
        hard_expires: hard,
    };
    let (live_stale, expired, dead, neg_dead) = (
        key("league.entriesByTier", "live-stale"),
        key("league.entriesByPuuid", "expired"),
        key("league.entriesByPuuid", "dead"),
        key("summoner.byPuuid", "neg-dead"),
    );
    // Not a player page's endpoint: a ladder page, a full mastery list.
    let (ladder, mastery) = (
        key("league.entriesByTier", "expired"),
        key("mastery.byPuuid", "expired"),
    );
    // A player's endpoint, but another key's ids.
    let foreign = "other:league.entriesByPuuid:h:expired".to_string();
    l2::put_rows(
        &db,
        vec![
            row(live_stale.clone(), 200, now + 60_000),
            row(expired.clone(), 200, now - 1),
            row(dead.clone(), 200, now - keep - 1),
            row(neg_dead.clone(), 404, now - 1),
            row(ladder.clone(), 200, now - 1),
            row(mastery.clone(), 200, now - 1),
            row(foreign.clone(), 200, now - 1),
        ],
    )
    .await
    .unwrap();

    let warmed = l1();
    let w = l2::warm(&db, &warmed, &kept()).await.unwrap();
    assert_eq!((w.loaded, w.full), (2, false));
    assert!(
        matches!(warmed.get(&live_stale).await, Lookup::Stale(_)),
        "soft passed, hard not: any endpoint"
    );
    match warmed.get(&expired).await {
        Lookup::Expired(e) => {
            let at = tokio::time::Instant::now();
            let near =
                |d: Duration, ms: u64| d.abs_diff(Duration::from_millis(ms)) < Duration::from_millis(500);
            assert!(near(e.age(at), 10_000), "its real ages: {:?}", e.age(at));
            assert!(near(e.fetch_age(at), 5_000), "{:?}", e.fetch_age(at));
        }
        other => panic!("{other:?}"),
    }
    for gone in [&dead, &neg_dead, &ladder, &mastery, &foreign] {
        assert_eq!(warmed.get(gone).await, Lookup::Miss, "{gone}");
    }

    assert_eq!(
        l2::sweep(&db, &kept()).await.unwrap(),
        5,
        "everything past its hard TTL but the player's row inside the day"
    );
    assert_eq!(rows(&db).await, 2);
    let left: Vec<String> = db
        .read(|c| {
            let mut stmt = c.prepare("SELECT key FROM cache ORDER BY key")?;
            let keys = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok::<_, riot_proxy::db::DbError>(keys)
        })
        .await
        .unwrap();
    let mut want = vec![live_stale, expired.clone()];
    want.sort();
    assert_eq!(left, want);

    // With nothing kept, the sweep is the one from before AVL-03.
    assert_eq!(l2::sweep(&db, &Kept::none()).await.unwrap(), 1);
    let cold = l1();
    assert_eq!(l2::warm(&db, &cold, &Kept::none()).await.unwrap().loaded, 1);
    assert_eq!(cold.get(&expired).await, Lookup::Miss);
}

/// A 200 stored through the cache is kept past its hard TTL in L1 only for an
/// endpoint with `keep_expired`; a live row of a kept endpoint warms as one
/// that will be.
#[tokio::test(start_paused = true)]
async fn the_cache_keeps_only_a_players_endpoints_past_their_hard_ttl() {
    let cache = ResponseCache::new(l1(), None);
    for (endpoint, flagged) in [
        ("league.entriesByPuuid", true),
        ("match.idsByPuuid", true),
        ("league.entriesByTier", false),
        ("mastery.byPuuid", false),
        ("spectator.activeGame", false),
    ] {
        assert_eq!(ep(endpoint).keep_expired, flagged, "{endpoint}");
        let k = key(endpoint, "P");
        cache
            .put(&k, ep(endpoint), Bytes::from_static(b"{}"), &ttls(30, 120))
            .await
            .unwrap();
    }
    tokio::time::advance(Duration::from_secs(120)).await;
    for (endpoint, flagged) in [
        ("league.entriesByPuuid", true),
        ("match.idsByPuuid", true),
        ("league.entriesByTier", false),
        ("mastery.byPuuid", false),
        ("spectator.activeGame", false),
    ] {
        let got = cache.get(&key(endpoint, "P")).await;
        assert_eq!(matches!(got, Lookup::Expired(_)), flagged, "{endpoint}: {got:?}");
        assert_eq!(got == Lookup::Miss, !flagged, "{endpoint}");
    }
}

/// More in L2 than L1's budget holds: the warm loads the latest-expiring rows,
/// live ones before a player's expired ones, stops at the first that does not
/// fit and leaves the rest unread.
#[tokio::test]
async fn warm_over_the_budget_keeps_the_newest_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let now = Clock::now().unix_ms;
    let body = Bytes::from(vec![b'x'; 1_000]);
    let row = |key: String, hard: i64| Row {
        key,
        body: body.clone(),
        status: 200,
        content_at: now - 600_000,
        fetched_at: now - 600_000,
        soft_expires: now - 500_000,
        hard_expires: hard,
    };
    // 40 live ladder pages, then 40 of a player's rows past their hard TTL;
    // in each group a higher number expires later.
    let live = |i: i64| key("league.entriesByTier", &format!("{i:02}"));
    let player = |i: i64| key("league.entriesByPuuid", &format!("{i:02}"));
    let mut rows_in = Vec::new();
    for i in 0..40 {
        rows_in.push(row(live(i), now + 60_000 + i * 1_000));
        rows_in.push(row(player(i), now - 60_000 + i * 1_000));
    }
    l2::put_rows(&db, rows_in).await.unwrap();

    // Room for 50 of the 80: each weighs its key, 1 000 bytes and L1's overhead.
    let each = riot_proxy::cache::l1::entry_weight(&live(0), 1_000);
    assert_eq!(each, riot_proxy::cache::l1::entry_weight(&player(0), 1_000) - 1);
    let small = L1::new(40 * each + 10 * (each + 1) + each / 2);
    let reads_before = db.reads();
    let w = l2::warm(&db, &small, &kept()).await.unwrap();
    assert_eq!((w.loaded, w.full), (50, true));
    assert_eq!(w.bytes, 40 * each + 10 * (each + 1));
    assert_eq!(db.reads() - reads_before, 1, "one streamed read");
    small.sync().await;
    for i in 0..40 {
        assert!(
            matches!(small.get(&live(i)).await, Lookup::Stale(_)),
            "every live row: {i}"
        );
        let got = small.get(&player(i)).await;
        if i >= 30 {
            assert!(matches!(got, Lookup::Expired(_)), "the 10 newest: {i} {got:?}");
        } else {
            assert_eq!(got, Lookup::Miss, "left in L2: {i}");
        }
    }
    assert_eq!(rows(&db).await, 80, "the warm deletes nothing");

    // With room for everything it loads everything and is not full.
    let roomy = l1();
    let w = l2::warm(&db, &roomy, &kept()).await.unwrap();
    assert_eq!((w.loaded, w.full), (80, false));
}

/// AVL-08: what boot reads is bounded by what it loads. The live rows come
/// off `cache_hard` backwards with no sort, a kept endpoint's expired rows
/// off its own range of the primary key, and the sweep off `cache_hard`'s
/// range below now.
#[tokio::test]
async fn warm_and_sweep_read_their_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let plan = |sql: String| {
        let db = db.clone();
        async move {
            db.read(move |c| {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, riot_proxy::db::DbError>(rows.join(" / "))
            })
            .await
            .unwrap()
        }
    };
    let live = plan(l2::WARM_LIVE_SQL.to_string()).await;
    assert_eq!(live, "SEARCH cache USING INDEX cache_hard (hard_expires>?)");
    let kept_ids = plan(l2::WARM_KEPT_SQL.to_string()).await;
    assert_eq!(
        kept_ids,
        "SEARCH cache USING INDEX sqlite_autoindex_cache_1 (key>? AND key<?)"
    );
    let by_id = plan(l2::WARM_ROW_SQL.to_string()).await;
    assert_eq!(by_id, "SEARCH cache USING INTEGER PRIMARY KEY (rowid=?)");
    for ranges in [0, 1, 6] {
        let sweep = plan(l2::sweep_sql(ranges)).await;
        assert_eq!(
            sweep, "SEARCH cache USING INDEX cache_hard (hard_expires<?)",
            "{ranges} kept ranges"
        );
    }
    // Six of the seven kept endpoints are persisted; the match id list is not.
    assert_eq!(
        riot_proxy::riot::endpoints::ENDPOINTS
            .iter()
            .filter(|e| e.keep_expired && e.persist_l2)
            .count(),
        6
    );
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
async fn upserts_replace_and_delete_keys_purges() {
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
    let selected = l2::keys_where(&db, |k| k.ends_with(":Q")).await.unwrap();
    assert_eq!(selected, ["s:a:h:Q"]);
    // A key that is not there (purged meanwhile) is not reported as deleted.
    let gone = l2::delete_keys(&db, vec!["s:a:h:Q".into(), "s:a:h:gone".into()])
        .await
        .unwrap();
    assert_eq!(gone, ["s:a:h:Q"]);
    assert_eq!(rows(&db).await, 1);
    assert!(l2::delete_keys(&db, Vec::new()).await.unwrap().is_empty());
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

/// SITE-01: the fetch time survives a restart apart from the content time, and
/// a row written before V0009 (no fetch time) warms with its content time.
#[tokio::test]
async fn fetched_at_round_trips_and_defaults_to_content_at() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let now = Clock::now().unix_ms;
    let row = Row {
        key: "refetched".into(),
        body: Bytes::from_static(b"{}"),
        status: 200,
        content_at: now - 60_000,
        fetched_at: now - 1_000,
        soft_expires: now + 60_000,
        hard_expires: now + 120_000,
    };
    l2::put_rows(&db, vec![row]).await.unwrap();
    db.write(move |c| {
        Ok::<_, riot_proxy::db::DbError>(c.execute(
            "INSERT INTO cache (key, body, status, content_at, soft_expires, hard_expires)
             VALUES ('old', x'7b7d', 200, ?1, ?2, ?3)",
            [now - 30_000, now + 60_000, now + 120_000],
        )?)
    })
    .await
    .unwrap();

    let warmed = l1();
    assert_eq!(l2::warm(&db, &warmed, &kept()).await.unwrap().loaded, 2);
    let near = |d: Duration, secs: u64| d.abs_diff(Duration::from_secs(secs)) < Duration::from_millis(500);
    let at = tokio::time::Instant::now();
    match warmed.get("refetched").await {
        Lookup::Fresh(e) => {
            assert!(near(e.age(at), 60), "content age {:?}", e.age(at));
            assert!(near(e.fetch_age(at), 1), "fetch age {:?}", e.fetch_age(at));
        }
        other => panic!("{other:?}"),
    }
    match warmed.get("old").await {
        Lookup::Fresh(e) => assert!(near(e.fetch_age(at), 30), "{:?}", e.fetch_age(at)),
        other => panic!("{other:?}"),
    }
}

/// ADM-04: a full write-behind queue drops the entry, never waits, and counts
/// it (`cache_l2_dropped_total`, the dashboard's `cacheStore.l2Dropped`).
#[tokio::test]
async fn a_full_queue_drops_and_counts_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let writer = L2Writer::spawn_queued(db.clone(), 500, Duration::from_secs(3600), 2);
    let entry = l1()
        .put("k", Bytes::from_static(b"{}"), &ttls(60, 60))
        .await
        .unwrap();
    // No await between them: the writer task cannot drain the queue.
    for i in 0..5 {
        writer.enqueue(&format!("k{i}"), &entry);
    }
    assert_eq!(writer.dropped(), 3);
    let cache = ResponseCache::new(l1(), Some(writer));
    assert_eq!(cache.l2_dropped(), 3);
    cache.shutdown().await;
    assert_eq!(rows(&db).await, 2, "the two queued writes still land");
    assert_eq!(ResponseCache::new(l1(), None).l2_dropped(), 0);
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::db::Db;
use crate::jobs::scheduler::Registry;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

async fn track(db: &Db, scope: &str, puuid: &str, tracked: bool) {
    let row = players::Upsert {
        puuid,
        platform: "kr",
        tracked: Some(tracked),
        ..players::Upsert::default()
    };
    players::upsert(db, scope, row, 1).await.unwrap();
}

async fn rows(db: &Db) -> Vec<(String, String, i64, String, String)> {
    db.read(|c| {
        let mut s = c.prepare("SELECT kind, coalesce(dedupe_key, ''), priority, payload, state FROM jobs ORDER BY kind, dedupe_key")?;
        let r = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        r.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    })
    .await
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_tick_fires_now_then_every_period_until_stopped() {
    let n = Arc::new(AtomicUsize::new(0));
    let (stop, stopped) = watch::channel(false);
    let counter = Arc::clone(&n);
    let task = tokio::spawn(every(Duration::from_secs(60), stopped, move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }));
    tokio::task::yield_now().await;
    assert_eq!(
        n.load(Ordering::SeqCst),
        1,
        "immediately, so a tick missed while down fires on boot"
    );
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 1);
    for expected in 2..=4 {
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(n.load(Ordering::SeqCst), expected);
    }
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_stalled_tick_is_skipped_not_burst() {
    let n = Arc::new(AtomicUsize::new(0));
    let (_stop, stopped) = watch::channel(false);
    let counter = Arc::clone(&n);
    tokio::spawn(every(Duration::from_secs(10), stopped, move || {
        let counter = Arc::clone(&counter);
        async move {
            // The first run takes three and a half periods.
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_secs(35)).await;
            }
        }
    }));
    tokio::time::sleep(Duration::from_secs(36)).await;
    assert_eq!(
        n.load(Ordering::SeqCst),
        2,
        "the three missed ticks collapse into one"
    );
}

#[tokio::test]
async fn polls_fan_out_one_job_per_tracked_player_of_this_scope() {
    let (_d, db) = db();
    track(&db, "s1", "p1", true).await;
    track(&db, "s1", "p2", true).await;
    track(&db, "s1", "p3", false).await;
    track(&db, "s2", "p4", true).await;
    let s = Scheduler::new(db.clone(), Registry::new());

    assert_eq!(fan_out(&s, "s1", kinds::POLL_LIVE).await.unwrap(), 2);
    assert_eq!(
        rows(&db).await,
        [
            (
                "poll:live".into(),
                "p1".into(),
                10_000,
                r#"{"platform":"kr","puuid":"p1"}"#.into(),
                "pending".into()
            ),
            (
                "poll:live".into(),
                "p2".into(),
                10_000,
                r#"{"platform":"kr","puuid":"p2"}"#.into(),
                "pending".into()
            ),
        ]
    );
    assert_eq!(
        fan_out(&s, "s1", kinds::POLL_LIVE).await.unwrap(),
        0,
        "a slow poll is not queued twice"
    );
    assert_eq!(
        fan_out(&s, "s1", kinds::POLL_RANK).await.unwrap(),
        2,
        "kinds are independent"
    );

    // Once p1's live poll has run, the next tick queues it again.
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&job, &Ok(()), 1).await.unwrap();
    assert_eq!(fan_out(&s, "s1", kinds::POLL_LIVE).await.unwrap(), 1);
    assert_eq!(
        fan_out(&s, "s3", kinds::POLL_LIVE).await.unwrap(),
        0,
        "nobody tracked"
    );
}

#[tokio::test]
async fn singletons_exist_once_at_a_time() {
    let (_d, db) = db();
    let s = Scheduler::new(db.clone(), Registry::new());
    assert!(
        singleton(&s, kinds::DDRAGON_SYNC, priority::MAINTENANCE)
            .await
            .unwrap()
    );
    assert!(
        !singleton(&s, kinds::DDRAGON_SYNC, priority::MAINTENANCE)
            .await
            .unwrap()
    );
    assert!(
        singleton(&s, kinds::MAINTENANCE, priority::MAINTENANCE)
            .await
            .unwrap()
    );
    assert_eq!(rows(&db).await.len(), 2);
}

#[test]
fn periods_come_from_config() {
    let config = Config::from_sources(crate::config::Sources {
        env: vec![("RIOT_API_KEY".into(), "RGAPI-test-key-not-real".into())],
        ..crate::config::Sources::default()
    })
    .unwrap();
    let got: Vec<(&str, u64)> = schedule(&config)
        .into_iter()
        .map(|(k, d)| (k, d.as_secs()))
        .collect();
    assert_eq!(
        got,
        [
            ("poll:live", 60),
            ("poll:rank", 600),
            ("poll:matches", 300),
            ("ddragon:sync", 3600),
            ("maintenance", 86_400)
        ]
    );
}

#[tokio::test]
async fn running_ticks_enqueue_until_shut_down() {
    let (_d, db) = db();
    track(&db, "s1", "p1", true).await;
    let s = Scheduler::new(db.clone(), Registry::new());
    let ticks = Ticks::start(
        &s,
        "s1",
        vec![
            (kinds::POLL_LIVE, Duration::from_millis(20)),
            (kinds::MAINTENANCE, Duration::from_millis(20)),
        ],
    );
    for _ in 0..100 {
        if rows(&db).await.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ticks.shutdown().await;
    let kinds: Vec<String> = rows(&db).await.into_iter().map(|r| r.0).collect();
    assert_eq!(kinds, ["maintenance", "poll:live"]);
}

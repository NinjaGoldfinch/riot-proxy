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
    let task = tokio::spawn(every(
        Duration::from_secs(60),
        Duration::ZERO,
        stopped,
        move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        },
    ));
    tokio::task::yield_now().await;
    assert_eq!(
        n.load(Ordering::SeqCst),
        1,
        "immediately: a first delay of zero is an overdue tick (ADR-214)"
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

/// AVL-04: a tick whose enqueue waits behind a long job write does not hold
/// shutdown until that write ends.
#[tokio::test(start_paused = true)]
async fn a_stop_ends_a_tick_still_running() {
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(every(
        Duration::from_secs(60),
        Duration::ZERO,
        stopped,
        || async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        },
    ));
    tokio::task::yield_now().await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("the stop ends the running tick")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_stalled_tick_is_skipped_not_burst() {
    let n = Arc::new(AtomicUsize::new(0));
    let (_stop, stopped) = watch::channel(false);
    let counter = Arc::clone(&n);
    tokio::spawn(every(
        Duration::from_secs(10),
        Duration::ZERO,
        stopped,
        move || {
            let counter = Arc::clone(&counter);
            async move {
                // The first run takes three and a half periods.
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_secs(35)).await;
                }
            }
        },
    ));
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

    // Once a player's poll has run, the next tick of that kind queues it again.
    // (Equal priorities tie on the ULID, so either kind may be claimed.)
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&job, &Ok(()), 1).await.unwrap();
    let kind = if job.kind == kinds::POLL_LIVE {
        kinds::POLL_LIVE
    } else {
        kinds::POLL_RANK
    };
    assert_eq!(fan_out(&s, "s1", kind).await.unwrap(), 1);
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
            ("maintenance", 86_400),
            ("names:backfill", 86_400),
            ("aggregate:delta", 60)
        ]
    );
}

#[test]
fn the_timeline_backfill_ticks_hourly_only_while_it_is_on() {
    let with = |patches: &str| {
        Config::from_sources(crate::config::Sources {
            env: vec![
                ("RIOT_API_KEY".into(), "RGAPI-test-key-not-real".into()),
                ("TIMELINE_BACKFILL_PATCHES".into(), patches.into()),
            ],
            ..crate::config::Sources::default()
        })
        .unwrap()
    };
    let tick = |c: &Config| {
        schedule(c)
            .into_iter()
            .find(|(k, _)| *k == kinds::TIMELINES_BACKFILL)
            .map(|(_, d)| d.as_secs())
    };
    assert_eq!(tick(&with("0")), None, "off by default (TL-02)");
    assert_eq!(tick(&with("6")), Some(3600));
}

#[test]
fn serve_ticks_every_kind_whose_handler_exists() {
    let config = Config::from_sources(crate::config::Sources {
        env: vec![("RIOT_API_KEY".into(), "RGAPI-test-key-not-real".into())],
        ..crate::config::Sources::default()
    })
    .unwrap();
    let kinds: Vec<&str> = running_schedule(&config).into_iter().map(|(k, _)| k).collect();
    // Every tick has its handler since P7-05; the crawl and rebuild ticks
    // are off by default (v1); the delta's is on (THR-03).
    assert_eq!(
        kinds,
        [
            "poll:live",
            "poll:rank",
            "poll:matches",
            "ddragon:sync",
            "maintenance",
            "names:backfill",
            "aggregate:delta"
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

#[tokio::test]
async fn the_crawl_tick_is_opt_in_and_starts_every_configured_ladder_once() {
    let config = |extra: &[(&str, &str)]| {
        let mut env = vec![("RIOT_API_KEY".to_string(), "RGAPI-test-key-not-real".to_string())];
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Config::from_sources(crate::config::Sources {
            env,
            ..crate::config::Sources::default()
        })
        .unwrap()
    };
    // v1: off by default; the cost of a crawl is opt-in.
    assert!(
        !schedule(&config(&[]))
            .iter()
            .any(|(k, _)| *k == kinds::LADDER_CRAWL)
    );
    let on = config(&[
        ("LADDER_CRAWL_S", "86400"),
        ("LADDER_PLATFORMS", "kr,euw1"),
        ("LADDER_QUEUES", "RANKED_SOLO_5x5,RANKED_FLEX_SR"),
    ]);
    assert!(running_schedule(&on).contains(&(kinds::LADDER_CRAWL, Duration::from_secs(86_400))));
    let ladders = ladders(&on);
    assert_eq!(ladders.len(), 4);
    assert_eq!(ladders[0], ("kr".to_string(), "RANKED_SOLO_5x5".to_string()));

    let (_d, db) = db();
    let s = Scheduler::new(db.clone(), Registry::new());
    let ticks = loops(&running_schedule(&on), &ladders);
    let crawls: Vec<&Tick> = ticks.iter().filter(|t| t.kind == kinds::LADDER_CRAWL).collect();
    assert_eq!(crawls.len(), 4, "one loop per ladder");
    let mut created = 0;
    for t in &crawls {
        created += fire(&s, "s1", t).await.unwrap();
    }
    assert_eq!(created, 4);
    for t in &crawls {
        assert_eq!(fire(&s, "s1", t).await.unwrap(), 0, "deduped per ladder");
    }
    assert_eq!(rows(&db).await.len(), 4);
}

// ── AVL-07 (ADR-214): a tick remembers its last run ─────────────────────────

/// Insert a `done` row of `kind`, finished `ago_ms` before now.
async fn done(db: &Db, kind: &str, dedupe: &str, ago_ms: i64) {
    let (kind, dedupe) = (kind.to_string(), dedupe.to_string());
    let at = Clock::now().unix_ms - ago_ms;
    db.write(move |c| {
        c.execute(
            "INSERT INTO jobs (id, kind, dedupe_key, priority, payload, state, run_after, finished_at)
             VALUES (?1, ?2, ?3, 1, '{}', 'done', 0, ?4)",
            (format!("{kind}-{dedupe}-{at}"), &kind, &dedupe, at),
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

/// `kind:key` of every pending row, sorted.
async fn pending(db: &Db) -> Vec<String> {
    db.read(|c| {
        let mut s = c.prepare(
            "SELECT kind || coalesce(':' || dedupe_key, '') FROM jobs WHERE state = 'pending' ORDER BY 1",
        )?;
        let r = s.query_map([], |r| r.get(0))?;
        r.collect::<Result<Vec<String>, _>>().map_err(DbError::from)
    })
    .await
    .unwrap()
}

/// Let the tick loops run what the clock has reached, and return what is
/// pending then. Time stays paused: a read goes through `spawn_blocking`,
/// which holds the clock still, and nothing here sleeps, so nothing moves the
/// clock behind the test's back.
async fn settle(db: &Db) -> Vec<String> {
    let mut last = pending(db).await;
    let mut same = 0;
    while same < 20 {
        tokio::task::yield_now().await;
        let now = pending(db).await;
        same = if now == last { same + 1 } else { 0 };
        last = now;
    }
    last
}

/// What a worker does with every pending row: finish it now. Through a
/// connection of its own in `spawn_blocking`, which holds the paused clock
/// still: awaiting the writer thread would let tokio advance it to the next
/// tick.
async fn finish_all(dir: &tempfile::TempDir) {
    let path = dir.path().join("riot-proxy.db");
    let now = Clock::now().unix_ms;
    tokio::task::spawn_blocking(move || {
        let c = rusqlite::Connection::open(path).unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c.execute(
            "UPDATE jobs SET state = 'done', finished_at = ?1 WHERE state = 'pending'",
            [now],
        )
        .unwrap();
    })
    .await
    .unwrap();
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
}

#[test]
fn the_first_fire_is_the_last_run_plus_the_period() {
    let period = Duration::from_secs(100);
    let now = 1_000_000_000;
    assert_eq!(
        first_delay(None, period, now),
        Duration::ZERO,
        "never ran: at once"
    );
    assert_eq!(
        first_delay(Some(now - 50_000), period, now),
        Duration::from_secs(50),
        "half a period ago: in half a period"
    );
    assert_eq!(
        first_delay(Some(now - 100_000), period, now),
        Duration::ZERO,
        "exactly due"
    );
    assert_eq!(
        first_delay(Some(now - 150_000), period, now),
        Duration::ZERO,
        "overdue: at once"
    );
    assert_eq!(
        first_delay(Some(now + 3_600_000), period, now),
        period,
        "stamped in the future (the clock stepped back): one period, not until then"
    );
    assert_eq!(first_delay(Some(i64::MIN), period, now), Duration::ZERO);
}

#[test]
fn only_the_polls_and_the_delta_fire_on_every_boot() {
    for kind in [
        kinds::POLL_LIVE,
        kinds::POLL_RANK,
        kinds::POLL_MATCHES,
        kinds::AGGREGATE_DELTA,
    ] {
        assert!(!remembers(kind), "{kind}");
    }
    for kind in [
        kinds::MAINTENANCE,
        kinds::NAMES_BACKFILL,
        kinds::DDRAGON_SYNC,
        kinds::LADDER_CRAWL,
        kinds::AGGREGATE_ANALYTICS,
        kinds::TIMELINES_BACKFILL,
    ] {
        assert!(remembers(kind), "{kind}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_first_delay_fires_then_and_every_period_after() {
    let n = Arc::new(AtomicUsize::new(0));
    let (_stop, stopped) = watch::channel(false);
    let counter = Arc::clone(&n);
    let run = every(
        Duration::from_secs(60),
        Duration::from_secs(25),
        stopped,
        move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    // The clock started when `every` was called, not when it is first polled.
    advance(5).await;
    tokio::spawn(run);
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 0);
    advance(19).await;
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 0, "24 s in");
    advance(1).await;
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 1, "25 s in");
    advance(59).await;
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 1);
    advance(1).await;
    tokio::task::yield_now().await;
    assert_eq!(n.load(Ordering::SeqCst), 2, "then every period");
}

#[tokio::test(start_paused = true)]
async fn a_kind_that_ran_half_a_period_ago_first_fires_half_a_period_later() {
    let (d, db) = db();
    done(&db, kinds::MAINTENANCE, kinds::MAINTENANCE, 50_000).await;
    let s = Scheduler::new(db.clone(), Registry::new());
    let ticks = Ticks::start(&s, "s1", vec![(kinds::MAINTENANCE, Duration::from_secs(100))]);
    assert!(settle(&db).await.is_empty(), "not at boot");
    advance(49).await;
    assert!(settle(&db).await.is_empty(), "49 s in");
    advance(1).await;
    assert_eq!(settle(&db).await, ["maintenance:maintenance"], "50 s in");
    finish_all(&d).await;
    advance(98).await;
    assert!(settle(&db).await.is_empty(), "148 s in");
    advance(2).await;
    assert_eq!(
        settle(&db).await,
        ["maintenance:maintenance"],
        "then every period"
    );
    ticks.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn none_or_an_old_run_fires_at_once_and_the_polls_and_delta_always_do() {
    let (d, db) = db();
    track(&db, "s1", "p1", true).await;
    // Overdue: a run older than its period.
    done(&db, kinds::MAINTENANCE, kinds::MAINTENANCE, 150_000).await;
    // Recent, so deferred.
    done(&db, kinds::DDRAGON_SYNC, kinds::DDRAGON_SYNC, 10_000).await;
    // Recent too, but exempt.
    done(&db, kinds::POLL_LIVE, "p1", 10_000).await;
    done(&db, kinds::POLL_RANK, "p1", 10_000).await;
    done(&db, kinds::POLL_MATCHES, "p1", 10_000).await;
    done(&db, kinds::AGGREGATE_DELTA, kinds::AGGREGATE_DELTA, 10_000).await;
    // names:backfill never ran.
    let s = Scheduler::new(db.clone(), Registry::new());
    let period = Duration::from_secs(100);
    let schedule = [
        kinds::POLL_LIVE,
        kinds::POLL_RANK,
        kinds::POLL_MATCHES,
        kinds::DDRAGON_SYNC,
        kinds::MAINTENANCE,
        kinds::NAMES_BACKFILL,
        kinds::AGGREGATE_DELTA,
    ]
    .map(|k| (k, period))
    .to_vec();
    let ticks = Ticks::start(&s, "s1", schedule);
    assert_eq!(
        settle(&db).await,
        [
            "aggregate:delta:aggregate:delta",
            "maintenance:maintenance",
            "names:backfill:names:backfill",
            "poll:live:p1",
            "poll:matches:p1",
            "poll:rank:p1",
        ],
        "everything but the recent ddragon:sync"
    );
    finish_all(&d).await;
    advance(85).await;
    assert!(settle(&db).await.is_empty(), "85 s in");
    advance(5).await;
    assert_eq!(
        settle(&db).await,
        ["ddragon:sync:ddragon:sync"],
        "ddragon:sync at its own time"
    );
    ticks.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn each_ladder_and_region_remembers_its_own_last_run() {
    let (d, db) = db();
    let now = Clock::now().unix_ms;
    db.write(move |c| {
        c.execute_batch(&format!(
            "INSERT INTO ladder_crawls (id, key_scope, platform, queue, tier_floor, status, started_at)
               VALUES ('c1', 's1', 'kr', 'RANKED_SOLO_5x5', 'IRON', 'completed', {ago}),
                      ('c2', 's2', 'euw1', 'RANKED_SOLO_5x5', 'IRON', 'completed', {ago});
             INSERT INTO analytics_runs (key_scope, platform, queue, kind, at, status, ms, steps, rows, games)
               VALUES ('s1', 'kr', 'RANKED_SOLO_5x5', 'rebuild', {ago}, 'completed', 1, '{{}}', '{{}}', 1),
                      ('s1', 'euw1', 'RANKED_SOLO_5x5', 'rebuild', {ago}, 'failed', 1, '{{}}', '{{}}', 0),
                      ('s1', 'euw1', 'RANKED_SOLO_5x5', 'delta', {ago}, 'completed', 1, '{{}}', '{{}}', 1);",
            ago = now - 30_000,
        ))?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    done(&db, kinds::TIMELINES_BACKFILL, "europe", 30_000).await;
    let s = Scheduler::new(db.clone(), Registry::new());
    let period = Duration::from_secs(100);
    let ladders = vec![
        ("kr".to_string(), "RANKED_SOLO_5x5".to_string()),
        ("euw1".to_string(), "RANKED_SOLO_5x5".to_string()),
    ];
    let ticks = Ticks::start_with(
        &s,
        "s1",
        vec![
            (kinds::LADDER_CRAWL, period),
            (kinds::AGGREGATE_ANALYTICS, period),
            (kinds::TIMELINES_BACKFILL, period),
        ],
        ladders,
    );
    assert_eq!(
        settle(&db).await,
        [
            // Only a completed rebuild counts, not a failed one or a delta.
            "aggregate:analytics:euw1:RANKED_SOLO_5x5",
            // euw1's crawl was another key scope's.
            "ladder:crawl:euw1:RANKED_SOLO_5x5",
            "timelines:backfill:americas",
            "timelines:backfill:asia",
            "timelines:backfill:sea",
        ],
        "kr and europe ran 30 s ago"
    );
    finish_all(&d).await;
    advance(65).await;
    assert!(settle(&db).await.is_empty(), "65 s in");
    advance(5).await;
    assert_eq!(
        settle(&db).await,
        [
            "aggregate:analytics:kr:RANKED_SOLO_5x5",
            "ladder:crawl:kr:RANKED_SOLO_5x5",
            "timelines:backfill:europe",
        ],
        "70 s in, their turn"
    );
    ticks.shutdown().await;
}

/// The reads at boot seek an index (AVL-06's rule for a boot check): the
/// `jobs` reads take `jobs_state_kind`'s `(state, kind)` range, which the
/// 7-day trim bounds; the crawl and rebuild reads take one ladder's entries.
#[tokio::test]
async fn the_last_run_reads_seek_their_indexes() {
    let (_d, db) = db();
    for (sql, index) in [
        (
            LAST_DONE_SQL,
            "COVERING INDEX jobs_state_kind (state=? AND kind=?)",
        ),
        (LAST_DONE_KEYED_SQL, "INDEX jobs_state_kind (state=? AND kind=?)"),
        (
            LAST_CRAWL_SQL,
            "COVERING INDEX ladder_crawls_recent (key_scope=? AND platform=? AND queue=?)",
        ),
        (
            LAST_REBUILD_SQL,
            "INDEX sqlite_autoindex_analytics_runs_1 (key_scope=? AND platform=? AND queue=? AND kind=?)",
        ),
    ] {
        let plan: Vec<String> = db
            .read(move |c| {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap();
        assert!(plan.iter().any(|d| d.contains(index)), "{sql}: {plan:?}");
    }
}

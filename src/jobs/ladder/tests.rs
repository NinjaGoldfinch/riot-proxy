#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::store::{self, Ended, Stage};
use super::*;
use crate::jobs::scheduler::Queue;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    (dir, db)
}

fn req(platform: &str, queue: &str, floor: Option<&str>) -> CrawlRequest {
    CrawlRequest {
        platform: platform.into(),
        queue: queue.into(),
        tier_floor: floor.map(str::to_string),
    }
}

async fn jobs(db: &Db) -> Vec<(String, i64, String)> {
    db.read(|c| {
        let mut s = c.prepare("SELECT kind, priority, dedupe_key FROM jobs ORDER BY priority, dedupe_key")?;
        let rows = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .unwrap()
}

async fn legs(db: &Db, crawl: &str) -> Vec<String> {
    let crawl = crawl.to_string();
    db.read(move |c| {
        let mut s = c.prepare("SELECT leg FROM crawl_legs WHERE crawl_id = ?1 ORDER BY leg")?;
        let rows = s
            .query_map([crawl], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .unwrap()
}

/// End a leg as `LadderContext::end_leg` does, with `next_stage`.
async fn end(db: &Db, crawl: &str, leg: &str, failed: bool, limit: u32) -> Ended {
    let (crawl, leg) = (crawl.to_string(), leg.to_string());
    db.write(move |c| {
        let tx = c.transaction()?;
        let ended = store::end_leg(&tx, &crawl, &leg, failed, 99, &mut |_tx, crawl| {
            Ok(next_stage(crawl, limit))
        })?;
        tx.commit()?;
        Ok::<_, DbError>(ended)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_crawl_fans_out_apex_leagues_then_every_division_down_to_the_floor() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let s = start_crawl(&q, "s1", "MASTER", &req("kr", "RANKED_SOLO_5x5", Some("emerald")))
        .await
        .unwrap();
    // 3 apex leagues + (EMERALD, DIAMOND) × 4 divisions.
    assert_eq!(
        (s.created, s.platform.as_str(), s.queue.as_str(), s.legs),
        (true, "kr", "RANKED_SOLO_5x5", 11)
    );
    let crawl = store::get(&db, "s1", &s.crawl_id).await.unwrap().unwrap();
    assert_eq!(
        (
            crawl.status.as_str(),
            crawl.phase.as_str(),
            crawl.tier_floor.as_str()
        ),
        ("running", "enumerate", "EMERALD")
    );
    let queued = jobs(&db).await;
    assert_eq!(queued.len(), 11);
    assert_eq!(
        queued[0],
        (
            "ladder:apex".into(),
            order::APEX,
            format!("{}:ladder:apex:CHALLENGER", s.crawl_id)
        )
    );
    assert!(
        queued[3..]
            .iter()
            .all(|(k, p, _)| k == "ladder:walk" && *p == order::WALK)
    );
    assert_eq!(legs(&db, &s.crawl_id).await.len(), 11);
    assert!(
        legs(&db, &s.crawl_id)
            .await
            .contains(&"ladder:walk:EMERALD:IV".to_string())
    );

    // One live crawl per ladder: a second start is told the first's id.
    let again = start_crawl(&q, "s1", "MASTER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    assert_eq!(
        (again.created, again.crawl_id.as_str(), again.legs),
        (false, s.crawl_id.as_str(), 0)
    );
    assert_eq!(jobs(&db).await.len(), 11, "nothing new queued");
    // Another ladder, or another key scope, is its own crawl.
    let flex = start_crawl(&q, "s1", "CHALLENGER", &req("kr", "RANKED_FLEX_SR", None))
        .await
        .unwrap();
    assert_eq!((flex.created, flex.legs), (true, 1));
    assert!(
        start_crawl(&q, "s2", "CHALLENGER", &req("kr", "RANKED_SOLO_5x5", None))
            .await
            .unwrap()
            .created
    );
}

#[tokio::test]
async fn a_crawl_request_is_checked_with_v1s_messages() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let message = |r: CrawlRequest| {
        let q = q.clone();
        async move {
            match start_crawl(&q, "s", "MASTER", &r).await {
                Err(StartError::Invalid(e)) => e.message,
                other => panic!("expected a validation error, got {other:?}"),
            }
        }
    };
    assert_eq!(
        message(req("kr", "ARAM", None)).await,
        "Unknown ranked queue 'ARAM'. Expected one of: RANKED_SOLO_5x5, RANKED_FLEX_SR"
    );
    assert_eq!(
        message(req("kr", "RANKED_SOLO_5x5", Some("Masters"))).await,
        "Unknown tier 'Masters'. Expected one of: IRON, BRONZE, SILVER, GOLD, PLATINUM, EMERALD, \
         DIAMOND, MASTER, GRANDMASTER, CHALLENGER"
    );
    assert!(message(req("xx1", "RANKED_SOLO_5x5", None)).await.contains("xx1"));
    assert!(jobs(&db).await.is_empty());
}

#[tokio::test]
async fn the_last_leg_moves_the_stage_once_and_a_rerun_changes_nothing() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let s = start_crawl(&q, "s", "MASTER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    let id = s.crawl_id.as_str();
    assert_eq!(
        end(&db, id, "ladder:apex:MASTER", false, 100).await,
        Ended::Nothing
    );
    // A re-run of an ended leg (crash before its job was marked done).
    assert_eq!(
        end(&db, id, "ladder:apex:MASTER", false, 100).await,
        Ended::Nothing
    );
    assert_eq!(
        end(&db, id, "ladder:apex:GRANDMASTER", false, 100).await,
        Ended::Nothing
    );
    let Ended::Phase(crawl) = end(&db, id, "ladder:apex:CHALLENGER", false, 100).await else {
        panic!("the last leg moves the crawl on");
    };
    assert_eq!(
        (crawl.status.as_str(), crawl.phase.as_str()),
        ("running", "collect")
    );
    assert_eq!(
        end(&db, id, "ladder:apex:CHALLENGER", false, 100).await,
        Ended::Nothing
    );
    assert_eq!(store::get(&db, "s", id).await.unwrap().unwrap().phase, "collect");
}

#[tokio::test]
async fn no_backfill_ends_the_crawl_at_enumeration_and_a_failed_leg_fails_it() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let a = start_crawl(&q, "s", "CHALLENGER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    // LADDER_BACKFILL_LIMIT=0 walks nobody: nothing to collect (v1).
    let Ended::Finished(done) = end(&db, &a.crawl_id, "ladder:apex:CHALLENGER", false, 0).await else {
        panic!("finished");
    };
    assert_eq!((done.status.as_str(), done.finished_at), ("completed", Some(99)));

    let b = start_crawl(&q, "s", "GRANDMASTER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    assert!(b.created, "the finished crawl no longer blocks its ladder");
    assert_eq!(
        end(&db, &b.crawl_id, "ladder:apex:GRANDMASTER", true, 100).await,
        Ended::Nothing
    );
    let Ended::Finished(failed) = end(&db, &b.crawl_id, "ladder:apex:CHALLENGER", false, 100).await else {
        panic!("finished");
    };
    // Part of a ladder must not be spent on as though it were the whole (v1).
    assert_eq!((failed.status.as_str(), failed.legs_failed), ("failed", 1));
    assert!(legs(&db, &b.crawl_id).await.is_empty());
}

#[tokio::test]
async fn a_cancelled_crawl_is_never_moved_on() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let s = start_crawl(&q, "s", "CHALLENGER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    let id = s.crawl_id.clone();
    let cancelled = db
        .write(move |c| {
            let tx = c.transaction()?;
            let r = store::finish(&tx, &id, "cancelled", 5)?;
            tx.commit()?;
            Ok::<_, DbError>(r)
        })
        .await
        .unwrap();
    assert_eq!(cancelled.unwrap().status, "cancelled");
    assert!(legs(&db, &s.crawl_id).await.is_empty(), "working state dropped");
    assert_eq!(
        end(&db, &s.crawl_id, "ladder:apex:CHALLENGER", false, 100).await,
        Ended::Nothing
    );
    let row = store::get(&db, "s", &s.crawl_id).await.unwrap().unwrap();
    assert_eq!((row.status.as_str(), row.finished_at), ("cancelled", Some(5)));
}

#[test]
fn stages_follow_v1s_order() {
    let crawl = |phase: &str| store::Crawl {
        id: "c".into(),
        platform: "kr".into(),
        queue: "RANKED_SOLO_5x5".into(),
        tier_floor: "MASTER".into(),
        status: "running".into(),
        phase: phase.into(),
        started_at: 0,
        finished_at: None,
        counters: store::Counters::default(),
        legs_failed: 0,
    };
    let name = |s: Stage| match s {
        Stage::Phase(p) => p,
        Stage::Complete => "complete",
    };
    assert_eq!(name(next_stage(&crawl("enumerate"), 100)), "collect");
    assert_eq!(name(next_stage(&crawl("enumerate"), 0)), "complete");
    assert_eq!(name(next_stage(&crawl("collect"), 100)), "archive");
    assert_eq!(name(next_stage(&crawl("archive"), 100)), "complete");
}

#[test]
fn entries_take_the_legs_tier_and_default_to_division_one() {
    let raw: RiotEntry = serde_json::from_value(serde_json::json!({
        "puuid": "P", "leaguePoints": 812, "wins": 300, "losses": 250, "hotStreak": true
    }))
    .unwrap();
    let e = to_entry(raw, "CHALLENGER").unwrap();
    assert_eq!(
        (
            e.tier.as_str(),
            e.division.as_str(),
            e.league_points,
            e.hot_streak,
            e.veteran
        ),
        ("CHALLENGER", "I", 812, true, false)
    );
    let no_puuid: RiotEntry = serde_json::from_value(serde_json::json!({"leaguePoints": 1})).unwrap();
    assert!(to_entry(no_puuid, "MASTER").is_none());
}

#[tokio::test]
async fn collect_skips_players_walked_since_the_crawl_started_and_goes_best_first() {
    let (_d, db) = db();
    let q = Queue::new(db.clone());
    let s = start_crawl(&q, "s", "CHALLENGER", &req("kr", "RANKED_SOLO_5x5", None))
        .await
        .unwrap();
    let crawl = store::get(&db, "s", &s.crawl_id).await.unwrap().unwrap();
    let entries: Vec<store::Entry> = [("low", 10), ("high", 900), ("walked", 500), ("old", 300)]
        .iter()
        .map(|(p, lp)| store::Entry {
            puuid: (*p).into(),
            tier: "CHALLENGER".into(),
            division: "I".into(),
            league_points: *lp,
            wins: 1,
            losses: 1,
            veteran: false,
            inactive: false,
            fresh_blood: false,
            hot_streak: false,
        })
        .collect();
    store::write_page(
        &db,
        store::Page {
            key_scope: "s",
            crawl_id: &crawl.id,
            platform: "kr",
            queue: "RANKED_SOLO_5x5",
            entries: &entries,
            cursor: None,
            now: crawl.started_at,
        },
    )
    .await
    .unwrap();
    // "walked" started a walk after this crawl began; "old" long before.
    store::mark_walk_started(&db, "s", "walked", crawl.started_at + 1)
        .await
        .unwrap();
    store::mark_walk_started(&db, "s", "old", crawl.started_at - 1)
        .await
        .unwrap();
    let candidates = db
        .write(move |c| {
            let tx = c.transaction()?;
            store::collect_candidates(&tx, "s", &crawl)
        })
        .await
        .unwrap();
    assert_eq!(candidates, ["high", "old", "low"]);
}

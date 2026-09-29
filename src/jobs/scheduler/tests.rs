#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::*;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

fn sched(db: &Db) -> Scheduler {
    Scheduler::new(db.clone(), Registry::new())
}

async fn state(db: &Db, id: &str) -> (String, u32, i64, Option<String>) {
    let id = id.to_string();
    db.read(move |c| {
        c.query_row(
            "SELECT state, attempts, run_after, error FROM jobs WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(DbError::from)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn enqueue_dedupes_while_pending_or_running_only() {
    let (_d, db) = db();
    let s = sched(&db);
    let job = || NewJob::new("archive:match", 100, json!({"matchId": "KR_1"})).dedupe("KR_1");
    let first = s.enqueue(job()).await.unwrap();
    let again = s.enqueue(job()).await.unwrap();
    assert!(first.created && !again.created);
    assert_eq!(
        (again.id.as_str(), again.status()),
        (first.id.as_str(), "already-queued")
    );

    // Running still dedupes.
    let claimed = s.claim(i64::MAX).await.unwrap().unwrap();
    assert_eq!(claimed.id, first.id);
    assert!(!s.enqueue(job()).await.unwrap().created);

    // Once done, the same work may be queued again (design/06).
    s.finish(&claimed, &Ok(()), 1).await.unwrap();
    let later = s.enqueue(job()).await.unwrap();
    assert!(later.created && later.id != first.id);

    // Another kind with the same key is different work; no key never dedupes.
    assert!(
        s.enqueue(NewJob::new("poll:live", 10_000, json!({})).dedupe("KR_1"))
            .await
            .unwrap()
            .created
    );
    assert!(
        s.enqueue(NewJob::new("maintenance", 30_000, json!({})))
            .await
            .unwrap()
            .created
    );
    assert!(
        s.enqueue(NewJob::new("maintenance", 30_000, json!({})))
            .await
            .unwrap()
            .created
    );
}

#[tokio::test]
async fn claims_follow_priority_then_age_and_skip_future_rows() {
    let (_d, db) = db();
    let s = sched(&db);
    for (priority, run_after, name) in [
        (30_000, 1, "aggregate"),
        (10_000, 1, "poll"),
        (10, 5, "interactive-late"),
        (10, 2, "interactive-early"),
        (100, 1, "depth"),
        (0, 50, "not-yet"),
    ] {
        let mut j = NewJob::new("k", priority, json!({"name": name}));
        j.run_after = Some(run_after);
        s.enqueue(j).await.unwrap();
    }
    let mut order = Vec::new();
    while let Some(job) = s.claim(10).await.unwrap() {
        assert_eq!(job.attempts, 1);
        order.push(
            job.payload::<serde_json::Value>().unwrap()["name"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(
        order,
        [
            "interactive-early",
            "interactive-late",
            "depth",
            "poll",
            "aggregate"
        ]
    );
    assert_eq!(
        s.claim(100).await.unwrap().unwrap().payload,
        r#"{"name":"not-yet"}"#
    );
}

#[test]
fn backoff_doubles_from_thirty_seconds_with_twenty_percent_jitter() {
    let s = |attempts, jitter| backoff(attempts, jitter).as_secs_f64();
    assert_eq!((s(1, 1.0), s(2, 1.0), s(4, 1.0)), (60.0, 120.0, 480.0));
    assert_eq!((s(1, 0.8), s(1, 1.2)), (48.0, 72.0));
    assert_eq!(s(1, 5.0), 72.0, "jitter is clamped");
    for _ in 0..100 {
        let j = random_jitter();
        assert!((0.8..=1.2).contains(&j));
    }
}

#[tokio::test]
async fn outcomes_mark_done_retry_with_backoff_or_fail() {
    let (_d, db) = db();
    let s = sched(&db);
    let id = s.enqueue(NewJob::new("k", 1, json!({}))).await.unwrap().id;
    // After the enqueue's own clock, so the row is ready.
    let now = Clock::now().unix_ms + 1_000;

    let job = s.claim(now).await.unwrap().unwrap();
    s.finish(&job, &Err(JobError::Retry("riot down".into())), now)
        .await
        .unwrap();
    let (st, attempts, run_after, error) = state(&db, &id).await;
    assert_eq!(
        (st.as_str(), attempts, error.as_deref()),
        ("pending", 1, Some("riot down"))
    );
    assert!((now + 48_000..=now + 72_000).contains(&run_after), "{run_after}");
    assert!(s.claim(now).await.unwrap().is_none(), "backing off");

    // Attempts 2..=4 retry; the fifth failure is final.
    for n in 2..=MAX_ATTEMPTS {
        let job = s.claim(i64::MAX).await.unwrap().unwrap();
        assert_eq!(job.attempts, n);
        s.finish(&job, &Err(JobError::Retry(format!("try {n}"))), now)
            .await
            .unwrap();
    }
    let (st, attempts, _, error) = state(&db, &id).await;
    assert_eq!(
        (st.as_str(), attempts, error.as_deref()),
        ("failed", 5, Some("try 5"))
    );

    let bad = s.enqueue(NewJob::new("k", 1, json!({}))).await.unwrap().id;
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&job, &Err(JobError::Fail("bad payload".into())), now)
        .await
        .unwrap();
    assert_eq!(
        state(&db, &bad).await.0,
        "failed",
        "no retry for a permanent failure"
    );

    let good = s.enqueue(NewJob::new("k", 1, json!({}))).await.unwrap().id;
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&job, &Ok(()), now).await.unwrap();
    let (st, attempts, _, error) = state(&db, &good).await;
    assert_eq!((st.as_str(), attempts, error), ("done", 1, None));
}

#[tokio::test]
async fn fan_out_can_enqueue_inside_the_callers_transaction() {
    let (_d, db) = db();
    let made = db
        .write(|c| {
            let tx = c.transaction()?;
            let a = enqueue_on(
                &tx,
                &NewJob::new("ladder:walk", 20_000, json!({"tier": "MASTER"})).dedupe("c1:MASTER"),
                1,
            )?;
            let b = enqueue_on(
                &tx,
                &NewJob::new("ladder:walk", 20_000, json!({"tier": "MASTER"})).dedupe("c1:MASTER"),
                1,
            )?;
            tx.commit()?;
            Ok::<_, DbError>((a.created, b.created))
        })
        .await
        .unwrap();
    assert_eq!(made, (true, false));
}

#[tokio::test]
async fn recover_requeues_rows_a_dead_process_left_running() {
    let (_d, db) = db();
    let s = sched(&db);
    let id = s.enqueue(NewJob::new("k", 1, json!({}))).await.unwrap().id;
    s.claim(i64::MAX).await.unwrap().unwrap();
    assert_eq!(state(&db, &id).await.0, "running");
    assert_eq!(s.recover().await.unwrap(), 1);
    assert_eq!(state(&db, &id).await.0, "pending");
    let again = s.claim(i64::MAX).await.unwrap().unwrap();
    assert_eq!(again.attempts, 2, "the interrupted attempt still counts");
}

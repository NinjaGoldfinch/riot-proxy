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

#[tokio::test]
async fn for_puuid_counts_a_players_jobs_by_kind_and_state_uncapped() {
    let (_d, db) = db();
    let s = sched(&db);
    let me = "PUUID_A";
    for i in 0..600 {
        let job = NewJob::new(
            "archive:match",
            100,
            json!({"matchId": format!("KR_{i}"), "puuid": me}),
        )
        .dedupe(format!("KR_{i}"));
        s.enqueue(job).await.unwrap();
    }
    s.enqueue(NewJob::new(
        "archive:match",
        100,
        json!({"matchId": "KR_x", "puuid": "PUUID_B"}),
    ))
    .await
    .unwrap();
    s.enqueue(NewJob::new("archive:match", 100, json!({"matchId": "KR_y"})))
        .await
        .unwrap();
    let walk = s
        .enqueue(NewJob::new(
            "backfill:player",
            20_000,
            json!({"puuid": me, "platform": "kr", "limit": 500}),
        ))
        .await
        .unwrap();
    s.enqueue(NewJob::new("names:backfill", 30_000, json!({"puuid": me})))
        .await
        .unwrap();
    db.write(|c| {
        c.execute(
            "UPDATE jobs SET state = 'failed', error = 'boom'
              WHERE id IN (SELECT id FROM jobs WHERE kind = 'archive:match' AND payload LIKE '%PUUID_A%' LIMIT 7)",
            [],
        )
        .map_err(DbError::from)
    })
    .await
    .unwrap();

    let (counts, latest) = s
        .queue()
        .for_puuid(me, &["archive:match", "backfill:player"])
        .await
        .unwrap();
    assert_eq!(
        counts,
        vec![
            ("archive:match".to_string(), "failed".to_string(), 7),
            ("archive:match".to_string(), "pending".to_string(), 593),
            ("backfill:player".to_string(), "pending".to_string(), 1),
        ]
    );
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[1].id, walk.id);
    assert_eq!(latest[1].kind, "backfill:player");
}

#[tokio::test]
async fn enqueue_or_promote_lifts_a_pending_duplicate_and_leaves_a_running_one() {
    let (_d, db) = db();
    let s = sched(&db);
    let q = s.queue().clone();
    let later = Clock::now().unix_ms + 3_600_000;
    s.enqueue(NewJob::new("ladder:walk", 20_000, json!({})).dedupe("w"))
        .await
        .unwrap();
    let mut queued = NewJob::new("aggregate:analytics", 30_000, json!({})).dedupe("kr:Q");
    queued.run_after = Some(later);
    let slow = s.enqueue(queued).await.unwrap();

    // The queued rebuild moves up and becomes ready now: no second row.
    let fast = q
        .enqueue_or_promote(NewJob::new("aggregate:analytics", 0, json!({})).dedupe("kr:Q"))
        .await
        .unwrap();
    assert!(!fast.created);
    assert_eq!(fast.id, slow.id);
    let claimed = s.claim(Clock::now().unix_ms).await.unwrap().unwrap();
    assert_eq!((claimed.id.as_str(), claimed.priority), (slow.id.as_str(), 0));

    // Running: untouched; a looser request never lowers a pending row.
    assert!(
        !q.enqueue_or_promote(NewJob::new("aggregate:analytics", 0, json!({})).dedupe("kr:Q"))
            .await
            .unwrap()
            .created
    );
    let walk = q
        .enqueue_or_promote(NewJob::new("ladder:walk", 30_000, json!({})).dedupe("w"))
        .await
        .unwrap();
    assert!(!walk.created);
    let rows = db
        .read(|c| {
            let mut st = c.prepare("SELECT kind, state, priority FROM jobs ORDER BY kind")?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("aggregate:analytics".into(), "running".into(), 0),
            ("ladder:walk".into(), "pending".into(), 20_000),
        ]
    );

    // No duplicate: queued fresh.
    assert!(
        q.enqueue_or_promote(NewJob::new("aggregate:analytics", 0, json!({})).dedupe("na1:Q"))
            .await
            .unwrap()
            .created
    );
}

// ── SCH-01: rate-limit-aware claims ─────────────────────────────────────────

mod lanes {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::riot::limiter::Limiter;
    use crate::riot::limiter::headers::LimitWindow;

    fn limited(db: &Db) -> (Scheduler, Arc<Limiter>) {
        let limiter = Arc::new(Limiter::new(0.8));
        (sched(db).with_limiter(Arc::clone(&limiter)), limiter)
    }

    /// `scope`'s app limit used up for two minutes.
    async fn fill(limiter: &Limiter, scope: &str) {
        limiter.configure_app(
            scope,
            &[LimitWindow {
                limit: 1,
                seconds: 120,
            }],
        );
        limiter
            .acquire(
                scope,
                "x",
                crate::riot::limiter::Priority::Interactive,
                Duration::ZERO,
            )
            .await
            .unwrap();
    }

    fn walk(platform: &str, division: &str) -> NewJob {
        NewJob::new(
            "ladder:walk",
            20_002,
            json!({"crawlId": "c", "platform": platform, "queue": "RANKED_SOLO_5x5", "tier": "DIAMOND", "division": division}),
        )
    }

    fn apex(platform: &str) -> NewJob {
        NewJob::new(
            "ladder:apex",
            20_002,
            json!({"crawlId": "c", "platform": platform, "queue": "RANKED_SOLO_5x5", "tier": "MASTER"}),
        )
    }

    fn rank_poll(platform: &str) -> NewJob {
        NewJob::new("poll:rank", 10_000, json!({"puuid": "p", "platform": platform}))
    }

    fn lane_of(job: &Job) -> String {
        let payload: serde_json::Value = serde_json::from_str(&job.payload).unwrap();
        payload["platform"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn only_our_own_limiter_running_out_yields() {
        use crate::fetcher::FetchError;
        use crate::http::ApiError;
        use crate::riot::limiter::RateLimited;

        let at = tokio::time::Instant::now() + Duration::from_secs(30);
        let ours = FetchError::from(RateLimited { retry_at: at });
        assert_eq!(ours.api.code, crate::http::ErrorCode::RateLimited);
        match JobError::from_fetch(&ours) {
            JobError::Yield { retry_at, payload } => {
                let expect = Clock::now().unix_ms + 30_000;
                assert!((retry_at - expect).abs() < 1_000, "{retry_at} vs {expect}");
                assert_eq!(payload, None);
            }
            other => panic!("{other:?}"),
        }
        // Riot's own 429 (a service limit with no type) is a real failure:
        // retried with backoff, and it counts.
        let riot = FetchError::from(ApiError::rate_limited(5));
        assert!(matches!(JobError::from_fetch(&riot), JobError::Retry(_)));
    }

    #[tokio::test]
    async fn enqueue_stamps_the_lane_and_method() {
        let (_d, db) = db();
        let s = sched(&db);
        let id = s.enqueue(walk("na1", "I")).await.unwrap().id;
        let none = s
            .enqueue(NewJob::new("maintenance", 30_000, json!({})))
            .await
            .unwrap()
            .id;
        let read = |id: String| {
            db.read(move |c| {
                c.query_row("SELECT lane, method FROM jobs WHERE id = ?1", [id], |r| {
                    Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
                })
                .map_err(DbError::from)
            })
        };
        assert_eq!(
            read(id).await.unwrap(),
            (Some("na1".into()), Some("league.entriesByTier".into()))
        );
        assert_eq!(read(none).await.unwrap(), (None, None));
    }

    #[tokio::test]
    async fn a_blocked_lane_is_skipped_for_a_free_one_even_if_older() {
        let (_d, db) = db();
        let (s, limiter) = limited(&db);
        s.enqueue(walk("na1", "I")).await.unwrap();
        s.enqueue(walk("na1", "II")).await.unwrap();
        s.enqueue(walk("euw1", "I")).await.unwrap();
        fill(&limiter, "na1").await;

        let job = s.claim(i64::MAX).await.unwrap().unwrap();
        assert_eq!(lane_of(&job), "euw1");
        assert!(
            s.claim(i64::MAX).await.unwrap().is_none(),
            "na1's rows wait for its limit"
        );
    }

    #[tokio::test]
    async fn a_capped_method_blocks_only_its_own_jobs_on_the_lane() {
        let (_d, db) = db();
        let (s, limiter) = limited(&db);
        let capped = s.enqueue(walk("na1", "I")).await.unwrap().id;
        let other = s.enqueue(apex("na1")).await.unwrap().id;
        let elsewhere = s.enqueue(walk("euw1", "I")).await.unwrap().id;
        limiter.configure_method(
            "na1",
            "league.entriesByTier",
            &[LimitWindow {
                limit: 1,
                seconds: 120,
            }],
        );
        limiter
            .acquire(
                "na1",
                "league.entriesByTier",
                crate::riot::limiter::Priority::Interactive,
                Duration::ZERO,
            )
            .await
            .unwrap();

        // na1's app limit is free: its apex (another method) runs first,
        // though the capped walk is older and euw1 is just as idle.
        assert_eq!(s.claim(i64::MAX).await.unwrap().unwrap().id, other);
        assert_eq!(s.claim(i64::MAX).await.unwrap().unwrap().id, elsewhere);
        assert!(s.claim(i64::MAX).await.unwrap().is_none());
        assert_eq!(state(&db, &capped).await.0, "pending");
    }

    #[tokio::test]
    async fn a_free_lower_band_beats_a_blocked_higher_one_and_bands_still_rank() {
        let (_d, db) = db();
        let (s, limiter) = limited(&db);
        s.enqueue(rank_poll("kr")).await.unwrap();
        let walk_na1 = s.enqueue(walk("na1", "I")).await.unwrap().id;
        let poll_na1 = s.enqueue(rank_poll("na1")).await.unwrap().id;
        fill(&limiter, "kr").await;

        // The free 10 000 beats the free 20 000.
        assert_eq!(s.claim(i64::MAX).await.unwrap().unwrap().id, poll_na1);
        // Then the free 20 000 beats the blocked 10 000: a worker never idles
        // while there is work it can do.
        assert_eq!(s.claim(i64::MAX).await.unwrap().unwrap().id, walk_na1);
        assert!(s.claim(i64::MAX).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_first_claims_of_three_crawls_cover_all_three_platforms() {
        let (_d, db) = db();
        let s = sched(&db);
        let q = s.queue().clone();
        for platform in ["na1", "euw1", "kr"] {
            let req = crate::jobs::ladder::CrawlRequest {
                platform: platform.into(),
                queue: "RANKED_SOLO_5x5".into(),
                tier_floor: Some("DIAMOND".into()),
            };
            crate::jobs::ladder::start_crawl(&q, "s", "MASTER", &req)
                .await
                .unwrap();
        }
        let mut first: Vec<String> = Vec::new();
        for _ in 0..3 {
            first.push(lane_of(&s.claim(i64::MAX).await.unwrap().unwrap()));
        }
        first.sort();
        assert_eq!(first, ["euw1", "kr", "na1"], "na1's 7 legs were queued first");
        // Then each lane's second job, before any third.
        let mut next: Vec<String> = Vec::new();
        for _ in 0..3 {
            next.push(lane_of(&s.claim(i64::MAX).await.unwrap().unwrap()));
        }
        next.sort();
        assert_eq!(next, ["euw1", "kr", "na1"]);
    }

    #[tokio::test]
    async fn a_yield_returns_the_attempt_keeps_the_error_and_can_resume() {
        let (_d, db) = db();
        let s = sched(&db);
        let id = s.enqueue(walk("na1", "I")).await.unwrap().id;
        let job = s.claim(i64::MAX).await.unwrap().unwrap();
        s.finish(&job, &Err(JobError::Retry("boom".into())), 1)
            .await
            .unwrap();
        let job = s.claim(i64::MAX).await.unwrap().unwrap();
        assert_eq!(job.attempts, 2);

        let yielded = Err(JobError::Yield {
            retry_at: 5_000,
            payload: Some(r#"{"resume":true}"#.into()),
        });
        s.finish(&job, &yielded, 2).await.unwrap();
        assert_eq!(
            state(&db, &id).await,
            ("pending".into(), 1, 5_000, Some("boom".into()))
        );
        assert!(s.claim(4_999).await.unwrap().is_none(), "not before retry_at");
        let again = s.claim(5_000).await.unwrap().unwrap();
        assert_eq!(
            (again.attempts, again.payload.as_str()),
            (2, r#"{"resume":true}"#)
        );

        // Without a payload the job keeps its own; yields never use up attempts.
        for _ in 0..(MAX_ATTEMPTS * 2) {
            let yielded = Err(JobError::Yield {
                retry_at: 5_000,
                payload: None,
            });
            s.finish(&again, &yielded, 5_000).await.unwrap();
            s.claim(5_000).await.unwrap().unwrap();
        }
        assert_eq!(state(&db, &id).await.1, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_worker_sleeps_until_a_lane_frees_or_a_delayed_row_is_due() {
        let (_d, db) = db();
        let (s, limiter) = limited(&db);
        assert_eq!(s.idle_for().await, IDLE_POLL, "nothing to wait for");

        fill(&limiter, "na1").await;
        let blocked = s.idle_for().await;
        assert!(
            blocked > Duration::from_secs(119) && blocked <= Duration::from_secs(120),
            "{blocked:?}"
        );

        let mut soon = rank_poll("euw1");
        soon.run_after = Some(Clock::now().unix_ms + 2_000);
        s.enqueue(soon).await.unwrap();
        let due = s.idle_for().await;
        assert!(
            due > Duration::from_millis(1_000) && due <= Duration::from_millis(2_000),
            "{due:?}"
        );
    }

    #[tokio::test]
    async fn rows_queued_before_lanes_get_theirs_at_boot() {
        let (_d, db) = db();
        let s = sched(&db);
        let walk_id = s.enqueue(walk("kr", "I")).await.unwrap().id;
        let done_id = s.enqueue(apex("kr")).await.unwrap().id;
        s.enqueue(NewJob::new("maintenance", 30_000, json!({})))
            .await
            .unwrap();
        let ids = (walk_id.clone(), done_id.clone());
        db.write(move |c| {
            c.execute("UPDATE jobs SET lane = NULL, method = NULL", [])?;
            c.execute("UPDATE jobs SET state = 'done' WHERE id = ?1", [ids.1])?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
        assert_eq!(s.assign_lanes().await.unwrap(), 1, "pending riot work only");
        assert_eq!(s.assign_lanes().await.unwrap(), 0, "idempotent");
        let lane: Option<String> = db
            .read(move |c| {
                c.query_row("SELECT lane FROM jobs WHERE id = ?1", [walk_id], |r| r.get(0))
                    .map_err(DbError::from)
            })
            .await
            .unwrap();
        assert_eq!(lane.as_deref(), Some("kr"));
    }
}

#[test]
fn a_trace_says_how_each_run_ended() {
    let job = |attempts| Job {
        id: "J".into(),
        kind: "archive:match".into(),
        dedupe_key: None,
        priority: 100,
        payload: "{}".into(),
        attempts,
        run_after: 0,
    };
    assert_eq!(outcome_text(&job(1), &Ok(())), "done");
    let retry = Err(JobError::Retry("503".into()));
    assert_eq!(outcome_text(&job(1), &retry), "retry later: 503");
    assert_eq!(outcome_text(&job(MAX_ATTEMPTS), &retry), "failed: 503");
    let yielded = Err(JobError::Yield {
        retry_at: 1_760_000_000_000,
        payload: None,
    });
    assert_eq!(
        outcome_text(&job(1), &yielded),
        "yielded: no rate-limit room until 2025-10-09T08:53:20.000Z"
    );
}

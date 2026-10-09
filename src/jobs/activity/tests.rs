#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn texts(view: &TraceView) -> Vec<(&'static str, &str)> {
    view.events.iter().map(|e| (e.kind, e.text.as_str())).collect()
}

#[tokio::test]
async fn a_job_logs_its_steps_and_the_worker_shows_the_latest() {
    let a = Activity::new();
    a.set_workers(2);
    a.claimed(1, "J1", "ladder:walk", 2);
    within(Some(Current::new(a.clone(), "J1")), async {
        step("GOLD II page 3 on na1");
        event("riot", "league.entriesByTier → MISS", Some(120));
    })
    .await;

    let workers = a.workers();
    assert_eq!(workers.len(), 2);
    assert_eq!(workers[0].job_id, None);
    assert_eq!(
        (
            workers[1].worker,
            workers[1].job_id.as_deref(),
            workers[1].kind.as_deref(),
            workers[1].now.as_deref(),
            workers[1].events
        ),
        (
            2,
            Some("J1"),
            Some("ladder:walk"),
            Some("GOLD II page 3 on na1"),
            3
        )
    );

    let t = a.trace("J1", 0).unwrap();
    assert_eq!(
        texts(&t),
        vec![
            ("job", "claimed by worker 2 (attempt 2)"),
            ("step", "GOLD II page 3 on na1"),
            ("riot", "league.entriesByTier → MISS"),
        ]
    );
    assert_eq!(t.events[2].ms, Some(120));
    // `after` reads only what is new.
    let tail = a.trace("J1", 2).unwrap();
    assert_eq!(tail.events.len(), 1);
    assert_eq!(tail.next_seq, 3);
}

#[tokio::test]
async fn outside_a_job_nothing_is_recorded() {
    let a = Activity::new();
    a.set_workers(1);
    a.claimed(0, "J1", "archive:match", 1);
    assert!(!tracing_job());
    step("not mine");
    event("riot", "not mine", None);
    assert_eq!(a.trace("J1", 0).unwrap().events.len(), 1);
    // A spawned task is traced only when handed the current job.
    within(Some(Current::new(a.clone(), "J1")), async {
        let carried = current();
        tokio::spawn(async { step("lost") }).await.unwrap();
        tokio::spawn(within(carried, async { step("carried") }))
            .await
            .unwrap();
    })
    .await;
    assert_eq!(texts(&a.trace("J1", 1).unwrap()), vec![("step", "carried")]);
}

#[tokio::test]
async fn finishing_frees_the_worker_and_keeps_the_trace() {
    let a = Activity::new();
    a.set_workers(1);
    a.claimed(0, "J1", "archive:match", 1);
    a.finished("J1", "retry later: RIOT_UNAVAILABLE");
    assert_eq!(a.workers()[0].job_id, None);
    let t = a.trace("J1", 0).unwrap();
    assert_eq!(t.outcome.as_deref(), Some("retry later: RIOT_UNAVAILABLE"));
    assert!(t.finished_at.is_some() && t.now.is_none());
    // Steps after the end are ignored.
    within(Some(Current::new(a.clone(), "J1")), async { step("late") }).await;
    assert_eq!(a.trace("J1", 0).unwrap().events.len(), 2);
    let f = a.finished_jobs(10);
    assert_eq!((f.len(), f[0].job_id.as_str()), (1, "J1"));

    // The next attempt starts a fresh trace and leaves the finished list.
    a.claimed(0, "J1", "archive:match", 2);
    assert!(a.finished_jobs(10).is_empty());
    assert_eq!(a.trace("J1", 0).unwrap().attempt, 2);
}

#[tokio::test]
async fn traces_are_bounded() {
    let a = Activity::new();
    a.set_workers(1);
    a.claimed(0, "J", "ladder:walk", 1);
    within(Some(Current::new(a.clone(), "J")), async {
        for i in 0..EVENTS_PER_JOB + 5 {
            event("step", format!("{i}"), None);
        }
    })
    .await;
    let t = a.trace("J", 0).unwrap();
    assert_eq!((t.events.len(), t.dropped), (EVENTS_PER_JOB, 6));
    assert_eq!(t.events[0].seq, 6);

    for i in 0..FINISHED_KEPT + 3 {
        let id = format!("F{i}");
        a.claimed(0, &id, "archive:match", 1);
        a.finished(&id, "done");
    }
    assert_eq!(a.finished_jobs(usize::MAX).len(), FINISHED_KEPT);
    assert!(a.trace("F0", 0).is_none());
    assert_eq!(a.finished_jobs(1)[0].job_id, format!("F{}", FINISHED_KEPT + 2));
}

//! 60 s soak (plan P2-07, P2 exit check): 50 concurrent tasks hammer one scope on
//! real time; no window is ever over-committed. Ignored by default:
//!     cargo test --release --test limiter_soak -- --ignored --nocapture
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use riot_proxy::riot::limiter::headers::LimitWindow;
use riot_proxy::riot::limiter::{Limiter, Priority};
use tokio::time::Instant;

const SCOPE: &str = "soak";
const TASKS: usize = 50;
const RUN: Duration = Duration::from_secs(60);

fn worst_burst(sorted: &[Instant], span: Duration) -> usize {
    // Two pointers over sorted stamps.
    let mut worst = 0;
    let mut start = 0;
    for end in 0..sorted.len() {
        while sorted[end] >= sorted[start] + span {
            start += 1;
        }
        worst = worst.max(end - start + 1);
    }
    worst
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "60 s soak; run for the P2 exit check"]
async fn fifty_tasks_for_sixty_seconds_never_over_commit() {
    // Dev-key app limits plus a tight method window, so every window binds at some point.
    let app = [
        LimitWindow {
            limit: 20,
            seconds: 1,
        },
        LimitWindow {
            limit: 100,
            seconds: 120,
        },
    ];
    let method = [LimitWindow { limit: 7, seconds: 2 }];
    let limiter = Arc::new(Limiter::new(0.8));
    limiter.configure_app(SCOPE, &app);
    limiter.configure_method(SCOPE, "tight", &method);

    let admitted: Arc<Mutex<Vec<(Instant, bool)>>> = Arc::default();
    let deadline = Instant::now() + RUN;
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..TASKS {
        let (limiter, admitted) = (Arc::clone(&limiter), Arc::clone(&admitted));
        tasks.spawn(async move {
            let tight = i % 5 == 0;
            let method = if tight { "tight" } else { "loose" };
            let prio = if i % 3 == 0 {
                Priority::Bulk
            } else {
                Priority::Interactive
            };
            let budget = if prio == Priority::Bulk {
                RUN
            } else {
                Duration::from_secs(2)
            };
            while Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match limiter.acquire(SCOPE, method, prio, budget.min(remaining)).await {
                    Ok(_) => admitted.lock().unwrap().push((Instant::now(), tight)),
                    Err(e) => tokio::time::sleep_until(e.retry_at.min(deadline)).await,
                }
            }
        });
    }
    tasks.join_all().await;

    let mut all = admitted.lock().unwrap().clone();
    all.sort_by_key(|(t, _)| *t);
    let stamps: Vec<Instant> = all.iter().map(|(t, _)| *t).collect();
    let tight: Vec<Instant> = all.iter().filter(|(_, m)| *m).map(|(t, _)| *t).collect();

    let worst_1s = worst_burst(&stamps, Duration::from_secs(1));
    let worst_120s = worst_burst(&stamps, Duration::from_secs(120));
    let worst_tight = worst_burst(&tight, Duration::from_secs(2));
    println!(
        "admitted {} in {:?}; worst 1 s = {worst_1s}/20, worst 120 s = {worst_120s}/100, worst tight 2 s = {worst_tight}/7",
        stamps.len(),
        RUN
    );
    assert!(worst_1s <= 20);
    assert!(worst_120s <= 100);
    assert!(worst_tight <= 7);
    assert_eq!(
        stamps.len(),
        100,
        "the 120 s window caps a 60 s run at exactly 100"
    );
}

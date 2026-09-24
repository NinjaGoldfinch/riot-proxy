//! Kill-and-restart against a real SQLite file: a restored limiter never admits
//! more than a window allows across the restart (plan P2-06 acceptance).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use riot_proxy::db::Db;
use riot_proxy::riot::limiter::headers::LimitWindow;
use riot_proxy::riot::limiter::persist::{self, Clock};
use riot_proxy::riot::limiter::{Limiter, Priority};

const SCOPE: &str = "euw1";

fn limiter() -> Limiter {
    let l = Limiter::new(0.8);
    // Long windows so nothing expires while the test runs on real time.
    l.configure_app(
        SCOPE,
        &[
            LimitWindow {
                limit: 10,
                seconds: 60,
            },
            LimitWindow {
                limit: 30,
                seconds: 600,
            },
        ],
    );
    l.configure_method(
        SCOPE,
        "match.byId",
        &[LimitWindow {
            limit: 8,
            seconds: 60,
        }],
    );
    l
}

async fn admit_all(l: &Limiter) -> usize {
    let mut n = 0;
    while l
        .acquire(SCOPE, "match.byId", Priority::Interactive, Duration::ZERO)
        .await
        .is_ok()
    {
        n += 1;
    }
    n
}

#[tokio::test]
async fn restart_never_over_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("riot-proxy.db");

    // First life: spend 6 of the 8 method tokens, checkpoint, then "crash".
    {
        let db = Db::open(&path, 1).unwrap();
        let a = limiter();
        for _ in 0..6 {
            a.acquire(SCOPE, "match.byId", Priority::Interactive, Duration::ZERO)
                .await
                .unwrap();
        }
        persist::checkpoint_now(&a, &db).await;
    }

    // Second life: a fresh limiter restored from the file.
    let db = Db::open(&path, 1).unwrap();
    let b = Limiter::new(0.8);
    assert!(persist::restore_from(&b, &db).await.unwrap() >= 2);
    assert_eq!(admit_all(&b).await, 2, "only what the first life left unspent");

    // And a third life after that, with nothing left at all.
    persist::checkpoint_now(&b, &db).await;
    let c = Limiter::new(0.8);
    persist::restore_from(&c, &db).await.unwrap();
    assert_eq!(admit_all(&c).await, 0);
    assert_eq!(c.method_usage(SCOPE, &["match.byId"])[0].windows[0].used, 8);
}

#[tokio::test]
async fn an_old_checkpoint_on_disk_restores_full_windows() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let a = limiter();
    a.acquire(SCOPE, "m", Priority::Interactive, Duration::ZERO)
        .await
        .unwrap();
    // Written "three minutes ago".
    let now = Clock::now();
    let old = Clock {
        instant: now.instant,
        unix_ms: now.unix_ms - 180_000,
    };
    persist::save(&db, a.checkpoint(old)).await.unwrap();

    let b = Limiter::new(0.8);
    persist::restore_from(&b, &db).await.unwrap();
    assert!(
        b.acquire(SCOPE, "m", Priority::Interactive, Duration::ZERO)
            .await
            .is_err()
    );
    let usage = b.usage(SCOPE);
    assert!(usage.iter().all(|u| u.used == u.limit), "{usage:?}");
}

#[tokio::test]
async fn checkpoints_replace_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    let a = limiter();
    persist::checkpoint_now(&a, &db).await;
    persist::checkpoint_now(&a, &db).await;
    let rows = persist::load(&db).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.scope.as_str()).collect::<Vec<_>>(),
        ["app:euw1", "method:euw1:match.byId"]
    );
}

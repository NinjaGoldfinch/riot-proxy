//! Single-flight: concurrent misses for one key share one upstream call
//! (docs/design/03, v1 §8.4).
//!
//! `DashMap<key, Shared<BoxFuture>>`. The first caller (the leader) starts the
//! work; later callers join the same future. The work runs on its own spawned
//! task, so it finishes, and its result is cached by the caller's code, even if
//! every waiting request is cancelled; the rate-limit token it spent is not wasted.
//! The slot is freed as soon as the work completes, success or failure, so errors
//! are never cached here (plan P3-04).
//!
//! v1 also coordinated *across* processes through a Redis lock. v2 is one process,
//! so that half has no equivalent (ADR-030).

use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;

use dashmap::DashMap;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};

/// The shared work panicked or was aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("single-flight work did not complete")]
pub struct WorkFailed;

type Slot<T, E> = Shared<BoxFuture<'static, Result<T, E>>>;

/// What one caller got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flight<T, E> {
    pub value: Result<T, E>,
    /// True for exactly one caller per call: the one whose `work` ran.
    pub did_work: bool,
}

pub struct SingleFlight<K, T, E> {
    inflight: Arc<DashMap<K, Slot<T, E>>>,
}

impl<K, T, E> Default for SingleFlight<K, T, E>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self {
            inflight: Arc::new(DashMap::new()),
        }
    }
}

impl<K, T, E> std::fmt::Debug for SingleFlight<K, T, E>
where
    K: Eq + Hash,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleFlight")
            .field("inflight", &self.inflight.len())
            .finish()
    }
}

impl<K, T, E> SingleFlight<K, T, E>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + From<WorkFailed> + 'static,
{
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `work` for `key`, or join the call already in flight for it. `work` is
    /// only called by the leader.
    pub async fn run<F, Fut>(&self, key: K, work: F) -> Flight<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
    {
        let mut did_work = false;
        let slot = self
            .inflight
            .entry(key.clone())
            .or_insert_with(|| {
                did_work = true;
                let inflight = Arc::clone(&self.inflight);
                let key = key.clone();
                let task = tokio::spawn(work());
                async move {
                    let out = task.await.unwrap_or_else(|_| Err(E::from(WorkFailed)));
                    // Free the slot from inside the work, so it is freed even if
                    // no caller is still waiting. Errors are therefore never shared
                    // with later callers.
                    inflight.remove(&key);
                    out
                }
                .boxed()
                .shared()
            })
            .clone();
        Flight {
            value: slot.await,
            did_work,
        }
    }

    /// Calls currently in flight.
    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Error {
        Upstream(&'static str),
        Failed,
    }

    impl From<WorkFailed> for Error {
        fn from(_: WorkFailed) -> Self {
            Self::Failed
        }
    }

    type Sf = SingleFlight<String, String, Error>;

    /// v1: "coalesces 100 concurrent identical misses into one call" (Phase 3 acceptance).
    #[tokio::test(start_paused = true)]
    async fn coalesces_100_concurrent_misses_into_one_call() {
        let sf = Arc::new(Sf::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let (sf, calls) = (Arc::clone(&sf), Arc::clone(&calls));
            set.spawn(async move {
                sf.run("key".into(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Ok("payload".to_string())
                })
                .await
            });
        }
        let results = set.join_all().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 100);
        assert!(results.iter().all(|r| r.value == Ok("payload".to_string())));
        // Exactly one caller did the work; the other 99 shared it.
        assert_eq!(results.iter().filter(|r| r.did_work).count(), 1);
    }

    /// v1: "releases the in-flight slot so a later request fetches again".
    #[tokio::test]
    async fn releases_the_slot_so_a_later_request_fetches_again() {
        let sf = Sf::new();
        let calls = Arc::new(AtomicUsize::new(0));
        for expected in 1..=2 {
            let c = Arc::clone(&calls);
            let r = sf
                .run("key".into(), || async move {
                    Ok(c.fetch_add(1, Ordering::SeqCst).to_string())
                })
                .await;
            assert!(r.did_work);
            assert_eq!(sf.inflight_count(), 0);
            assert_eq!(calls.load(Ordering::SeqCst), expected);
        }
    }

    /// v1: "propagates failure to every waiter without leaving the slot occupied".
    #[tokio::test(start_paused = true)]
    async fn propagates_failure_to_every_waiter_and_frees_the_slot() {
        let sf = Arc::new(Sf::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let (sf, calls) = (Arc::clone(&sf), Arc::clone(&calls));
            set.spawn(async move {
                sf.run("key".into(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Err::<String, _>(Error::Upstream("exploded"))
                })
                .await
            });
        }
        let results = set.join_all().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            results
                .iter()
                .all(|r| r.value == Err(Error::Upstream("exploded")))
        );
        assert_eq!(sf.inflight_count(), 0, "errors are not cached");
    }

    /// In place of v1's cross-instance cases: a cancelled leader must not strand
    /// the work. It completes on its own task and later joiners still share it.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_leader_does_not_abandon_the_work() {
        let sf = Arc::new(Sf::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let leader = {
            let (sf, calls) = (Arc::clone(&sf), Arc::clone(&calls));
            tokio::spawn(async move {
                sf.run("key".into(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok("done".to_string())
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        leader.abort();
        let _ = leader.await;
        assert_eq!(sf.inflight_count(), 1, "still running");

        let joiner = sf
            .run("key".into(), || async { Ok("second call".to_string()) })
            .await;
        assert!(!joiner.did_work);
        assert_eq!(joiner.value, Ok("done".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sf.inflight_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn different_keys_do_not_share() {
        let sf = Arc::new(Sf::new());
        let (a, b) = tokio::join!(
            sf.run("a".into(), || async { Ok("A".to_string()) }),
            sf.run("b".into(), || async { Ok("B".to_string()) }),
        );
        assert_eq!((a.value, b.value), (Ok("A".into()), Ok("B".into())));
        assert!(a.did_work && b.did_work);
    }

    #[tokio::test]
    async fn a_panicking_work_is_reported_and_frees_the_slot() {
        let sf = Sf::new();
        #[allow(clippy::panic)]
        let r = sf.run("key".into(), || async { panic!("boom") }).await;
        assert_eq!(r.value, Err(Error::Failed));
        assert_eq!(sf.inflight_count(), 0);
    }
}

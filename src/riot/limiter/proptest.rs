//! Property test (plan P2-07): under any interleaving of acquires (both
//! priorities), clock advances, Riot count syncs and freezes, the limiter never
//! over-commits a window. Checked against an independent log of what it admitted.

use std::time::Duration;

use ::proptest::prelude::*;
use tokio::time::Instant;

use super::headers::{CountWindow, LimitWindow, RateLimitHeaders, RateLimitType};
use super::{Limiter, Priority};

const SCOPE: &str = "prop";
const METHODS: [&str; 2] = ["a", "b"];
const APP: [LimitWindow; 2] = [
    LimitWindow { limit: 5, seconds: 1 },
    LimitWindow {
        limit: 12,
        seconds: 10,
    },
];
const METHOD_A: [LimitWindow; 1] = [LimitWindow { limit: 3, seconds: 2 }];

#[derive(Debug, Clone)]
enum Op {
    Acquire { method: usize, bulk: bool },
    Advance { ms: u64 },
    Sync { app_short: u32, app_long: u32 },
    Freeze { ms: u64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..METHODS.len(), any::<bool>()).prop_map(|(method, bulk)| Op::Acquire { method, bulk }),
        3 => (0u64..1500).prop_map(|ms| Op::Advance { ms }),
        1 => (0u32..8, 0u32..16).prop_map(|(app_short, app_long)| Op::Sync { app_short, app_long }),
        1 => (1u64..3000).prop_map(|ms| Op::Freeze { ms }),
    ]
}

/// Most of `stamps` inside any `span`-long interval.
fn worst_burst(stamps: &[Instant], span: Duration) -> usize {
    stamps
        .iter()
        .map(|&s| stamps.iter().filter(|&&t| t >= s && t < s + span).count())
        .max()
        .unwrap_or(0)
}

fn run(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()?;
    rt.block_on(async move {
        let l = Limiter::new(0.8);
        l.configure_app(SCOPE, &APP);
        l.configure_method(SCOPE, METHODS[0], &METHOD_A);

        let mut all: Vec<Instant> = Vec::new();
        let mut method_a: Vec<Instant> = Vec::new();
        let mut frozen_until: Option<Instant> = None;

        for op in ops {
            let now = Instant::now();
            match op {
                Op::Acquire { method, bulk } => {
                    let prio = if bulk {
                        Priority::Bulk
                    } else {
                        Priority::Interactive
                    };
                    if l.acquire(SCOPE, METHODS[method], prio, Duration::ZERO)
                        .await
                        .is_ok()
                    {
                        prop_assert!(frozen_until.is_none_or(|t| now >= t), "admitted while frozen");
                        all.push(now);
                        if method == 0 {
                            method_a.push(now);
                        }
                    }
                }
                Op::Advance { ms } => tokio::time::advance(Duration::from_millis(ms)).await,
                Op::Sync { app_short, app_long } => {
                    let h = RateLimitHeaders {
                        app_counts: Some(vec![
                            CountWindow {
                                count: app_short,
                                seconds: 1,
                            },
                            CountWindow {
                                count: app_long,
                                seconds: 10,
                            },
                        ]),
                        ..RateLimitHeaders::default()
                    };
                    l.observe(SCOPE, METHODS[0], &h);
                }
                Op::Freeze { ms } => {
                    l.freeze(SCOPE, Duration::from_millis(ms), RateLimitType::Application);
                    let until = now + Duration::from_millis(ms);
                    frozen_until = Some(frozen_until.map_or(until, |t| t.max(until)));
                }
            }

            // Invariant: our own admissions never exceed any window's limit inside
            // any rolling interval of that window's length.
            for w in APP {
                let worst = worst_burst(&all, Duration::from_secs(w.seconds.into()));
                prop_assert!(
                    worst <= w.limit as usize,
                    "app {}:{} admitted {worst}",
                    w.limit,
                    w.seconds
                );
            }
            for w in METHOD_A {
                let worst = worst_burst(&method_a, Duration::from_secs(w.seconds.into()));
                prop_assert!(
                    worst <= w.limit as usize,
                    "method {}:{} admitted {worst}",
                    w.limit,
                    w.seconds
                );
            }
            // Reported usage never exceeds the limit plus what Riot told us to pad.
            for u in l.usage(SCOPE) {
                prop_assert!(u.used <= u.limit.max(16), "{u:?}");
            }
        }
        Ok(())
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, ..ProptestConfig::default() })]

    #[test]
    fn never_over_commits_a_window(ops in proptest::collection::vec(op(), 1..200)) {
        run(ops)?;
    }
}

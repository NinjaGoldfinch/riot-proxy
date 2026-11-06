use super::*;

const T0: i64 = 1_794_000_000_000;
const HOUR: i64 = 3_600_000;

fn verdict(status: u16) -> KeyVerdict {
    KeyVerdict {
        accepted: (200..300).contains(&status),
        status,
        method: "summoner.byPuuid",
        scope: "oc1",
        at_ms: T0,
    }
}

fn result(probe: Probe) -> Platformed {
    Platformed {
        last: Some(ProbeResult {
            probe,
            latency_ms: 85,
            at_ms: T0,
        }),
        skipped: false,
    }
}

fn up() -> Platformed {
    result(Probe::Answered { status: 200 })
}

fn failed() -> Platformed {
    result(Probe::Failed {
        detail: "Riot answered 503".into(),
    })
}

#[test]
fn riot_key_follows_the_last_verdict() {
    let pending = riot_key(None, None, T0, T0 - 5);
    assert_eq!(
        (pending.status, pending.detail.as_str()),
        (StatusLevel::Ok, "pending")
    );
    assert_eq!(pending.checked_at, rfc3339(T0 - 5));
    assert_eq!(pending.latency_ms, None, "it makes no call of its own");

    let ok = riot_key(Some(&verdict(200)), None, T0, 0);
    assert_eq!((ok.status, ok.detail.as_str()), (StatusLevel::Ok, ""));
    assert_eq!(ok.checked_at, rfc3339(T0));

    for status in [401, 403] {
        let down = riot_key(Some(&verdict(status)), None, T0, 0);
        assert_eq!(down.status, StatusLevel::Down);
        assert_eq!(
            down.detail,
            format!("Riot answered {status} to summoner.byPuuid on oc1")
        );
    }
}

/// design/14: `degraded` within 48 h of `RIOT_API_KEY_EXPIRES_AT`; a
/// rejection still wins.
#[test]
fn riot_key_warns_before_the_key_expires() {
    let ok = Some(verdict(200));
    let at = |expires: i64| riot_key(ok.as_ref(), Some(expires), T0, 0);
    assert_eq!(at(T0 + 48 * HOUR).status, StatusLevel::Ok, "exactly 48 h away");
    let soon = at(T0 + 48 * HOUR - 1);
    assert_eq!(soon.status, StatusLevel::Degraded);
    assert_eq!(
        soon.detail,
        format!("the key expires at {}", rfc3339(T0 + 48 * HOUR - 1))
    );
    let gone = at(T0 - HOUR);
    assert_eq!(gone.status, StatusLevel::Degraded);
    assert!(gone.detail.starts_with("the key expired at "), "{}", gone.detail);

    let pending = riot_key(None, Some(T0 + HOUR), T0, 0);
    assert_eq!(pending.status, StatusLevel::Degraded);
    assert!(
        pending.detail.starts_with("the key expires at "),
        "pending is dropped: {}",
        pending.detail
    );

    let rejected = riot_key(Some(&verdict(403)), Some(T0 + HOUR), T0, 0);
    assert_eq!(rejected.status, StatusLevel::Down);
    assert!(
        rejected.detail.contains("; the key expires at "),
        "{}",
        rejected.detail
    );
}

#[test]
fn riot_api_is_down_only_on_the_first_platform() {
    let none = Platformed::default();
    let p = riot_api(&[(Platform::Oc1, &none)], T0 - 1);
    assert_eq!((p.status, p.detail.as_str()), (StatusLevel::Ok, "pending"));
    assert_eq!((p.latency_ms, p.checked_at.clone()), (None, rfc3339(T0 - 1)));

    let (up, failed) = (up(), failed());
    let ok = riot_api(&[(Platform::Oc1, &up), (Platform::Kr, &up)], 0);
    assert_eq!((ok.status, ok.detail.as_str()), (StatusLevel::Ok, ""));
    assert_eq!((ok.latency_ms, ok.checked_at), (Some(85), rfc3339(T0)));

    let down = riot_api(&[(Platform::Oc1, &failed), (Platform::Kr, &up)], 0);
    assert_eq!(
        (down.status, down.detail.as_str()),
        (StatusLevel::Down, "oc1: Riot answered 503")
    );

    let degraded = riot_api(&[(Platform::Oc1, &up), (Platform::Kr, &failed)], 0);
    assert_eq!(
        (degraded.status, degraded.detail.as_str()),
        (StatusLevel::Degraded, "kr: Riot answered 503")
    );
}

#[test]
fn riot_api_degrades_on_a_429_and_shrugs_at_other_answers() {
    let limited = result(Probe::RateLimited);
    for list in [[Platform::Oc1, Platform::Kr], [Platform::Kr, Platform::Oc1]] {
        let up = up();
        let order: Vec<(Platform, &Platformed)> = list
            .iter()
            .map(|&p| (p, if p == Platform::Kr { &limited } else { &up }))
            .collect();
        let c = riot_api(&order, 0);
        assert_eq!(
            (c.status, c.detail.as_str()),
            (StatusLevel::Degraded, "kr: Riot answered 429")
        );
    }
    // A rejected key is Riot answering: riot_key shows it, riot_api stays ok.
    for status in [401, 403, 404] {
        let answered = result(Probe::Answered { status });
        let c = riot_api(&[(Platform::Oc1, &answered)], 0);
        assert_eq!(c.status, StatusLevel::Ok, "{status}");
        assert_eq!(c.detail, format!("oc1: Riot answered {status}"));
    }
}

/// design/14: a skipped probe leaves the last result standing and says so.
#[test]
fn a_skipped_probe_keeps_the_last_result() {
    let mut failed = failed();
    failed.skipped = true;
    let c = riot_api(&[(Platform::Oc1, &failed)], 0);
    assert_eq!(c.status, StatusLevel::Down);
    assert_eq!(
        c.detail,
        "oc1: Riot answered 503; oc1: probe skipped, no room in the rate limiter"
    );
    let never = Platformed {
        last: None,
        skipped: true,
    };
    let c = riot_api(&[(Platform::Oc1, &never)], 0);
    assert_eq!(
        (c.status, c.detail.as_str()),
        (StatusLevel::Ok, "oc1: probe skipped, no room in the rate limiter")
    );
}

#[test]
fn a_freeze_degrades_riot_api_when_read() {
    let ok = riot_api(&[(Platform::Oc1, &up())], 0);
    assert_eq!(frozen(ok.clone(), 0), ok);
    let f = frozen(ok, 12);
    assert_eq!(
        (f.status, f.detail.as_str()),
        (StatusLevel::Degraded, "rate limited by Riot for 12 s more")
    );
    let down = frozen(riot_api(&[(Platform::Oc1, &failed())], 0), 3);
    assert_eq!(down.status, StatusLevel::Down, "never better than it was");
    assert_eq!(ceil_secs(Duration::from_millis(1_001)), 2);
    assert_eq!(ceil_secs(Duration::from_secs(2)), 2);
}

/// The owner's cadence (design/14): the first platform every 30 s idle and
/// every 60 s active, the others every 60 s, with ticks 30 s apart.
#[tokio::test(start_paused = true)]
async fn the_probe_cadence() {
    let start = Instant::now();
    let mut last: Vec<Option<Instant>> = vec![None, None];
    let mut run = |active: bool| {
        let now = Instant::now();
        let due = due(&last, active, now);
        for &i in &due {
            last[i] = Some(now);
        }
        due
    };
    assert_eq!(run(false), [0, 1], "everything at boot");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(run(false), [0], "idle: the first again, the other waits");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(run(false), [0, 1]);
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(run(true), Vec::<usize>::new(), "active: 30 s is too soon");
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(run(true), [0, 1], "active: 60 s after the last");
    assert_eq!(Instant::now() - start, Duration::from_secs(120));
}

/// A round with nothing to probe still stores `cache`; with a bucket,
/// `storage` too.
#[tokio::test]
async fn cache_and_storage_answer() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    let c = cache(&db).await;
    assert_eq!(
        (c.name.as_str(), c.status, c.detail.as_str()),
        (CACHE, StatusLevel::Ok, "")
    );
    assert!(c.latency_ms.is_some());
    assert_eq!(storage(None).await, None, "no bucket, no check");
    let s = storage(Some(&Objects::in_memory())).await.unwrap();
    assert_eq!((s.name.as_str(), s.status), (STORAGE, StatusLevel::Ok));
}

#[test]
fn a_failed_check_takes_its_level() {
    let c = timed(STORAGE, Instant::now(), Some("x".into()), StatusLevel::Degraded);
    assert_eq!((c.status, c.detail.as_str()), (StatusLevel::Degraded, "x"));
    let c = timed(CACHE, Instant::now(), None, StatusLevel::Down);
    assert_eq!((c.status, c.detail.as_str()), (StatusLevel::Ok, ""));
}

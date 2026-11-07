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
        skips: 0,
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
    failed.skips = 1;
    let c = riot_api(&[(Platform::Oc1, &failed)], 0);
    assert_eq!(c.status, StatusLevel::Down);
    assert_eq!(
        c.detail,
        "oc1: Riot answered 503; oc1: probe skipped, no room in the rate limiter"
    );
    let never = Platformed { last: None, skips: 1 };
    let c = riot_api(&[(Platform::Oc1, &never)], 0);
    assert_eq!(
        (c.status, c.detail.as_str()),
        (StatusLevel::Ok, "oc1: probe skipped, no room in the rate limiter")
    );
}

/// The owner (ADR-206): `STALE_AFTER_SKIPS` skipped attempts in a row on a
/// platform degrade `riot_api`; it is never `down` for it.
#[test]
fn a_platform_skipped_four_times_in_a_row_degrades_riot_api() {
    let skipped = |skips: u32| {
        let mut p = up();
        p.skips = skips;
        p
    };
    let three = skipped(STALE_AFTER_SKIPS - 1);
    let c = riot_api(&[(Platform::Oc1, &three)], 0);
    assert_eq!(
        (c.status, c.detail.as_str()),
        (StatusLevel::Ok, "oc1: probe skipped, no room in the rate limiter")
    );
    let four = skipped(STALE_AFTER_SKIPS);
    let up = up();
    for order in [
        [(Platform::Oc1, &four), (Platform::Kr, &up)],
        [(Platform::Kr, &up), (Platform::Oc1, &four)],
    ] {
        let c = riot_api(&order, 0);
        assert_eq!(
            (c.status, c.detail.as_str()),
            (
                StatusLevel::Degraded,
                "oc1: probe skipped 4 times in a row, no room in the rate limiter"
            ),
            "first or not, it only degrades"
        );
    }
    let never = Platformed {
        last: None,
        skips: STALE_AFTER_SKIPS + 1,
    };
    let c = riot_api(&[(Platform::Oc1, &never)], 0);
    assert_eq!(c.status, StatusLevel::Degraded, "never probed at all");
    assert_eq!(
        c.detail,
        "oc1: probe skipped 5 times in a row, no room in the rate limiter"
    );
    let mut down = failed();
    down.skips = STALE_AFTER_SKIPS;
    assert_eq!(
        riot_api(&[(Platform::Oc1, &down)], 0).status,
        StatusLevel::Down,
        "never better than the last result"
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
    let (c, failed) = cache(&db, CACHE_WITHIN, true).await;
    assert_eq!(
        (c.name.as_str(), c.status, c.detail.as_str(), failed),
        (CACHE, StatusLevel::Ok, "", false)
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

/// A read SQLite refuses is `down` at once, with SQLite's reason.
#[tokio::test]
async fn a_failing_sqlite_read_makes_cache_down() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    db.write(|c| c.execute_batch("DROP TABLE cache").map_err(DbError::from))
        .await
        .unwrap();
    let (c, failed) = cache(&db, CACHE_WITHIN, false).await;
    assert!(failed);
    assert_eq!(c.status, StatusLevel::Down, "an error never waits a round");
    assert!(
        c.detail.starts_with("SQLite read failed: ") && c.detail.contains("no such table: cache"),
        "{}",
        c.detail
    );
}

/// The owner (ADR-206): with every reader held, the read times out. The
/// first timeout is `degraded`; a second in a row is `down`; an answer
/// clears it.
#[tokio::test]
async fn a_held_reader_pool_is_degraded_before_it_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (holding, started) = tokio::sync::oneshot::channel();
    let holder = {
        let db = db.clone();
        tokio::spawn(async move {
            db.read(move |_| {
                let _ = holding.send(());
                let _ = held.recv();
                Ok::<_, DbError>(())
            })
            .await
        })
    };
    started.await.unwrap();
    let within = Duration::from_millis(50);

    let (first, failed) = cache(&db, within, false).await;
    assert!(failed);
    assert_eq!(first.status, StatusLevel::Degraded, "one slow round: {first:?}");
    assert!(
        first.detail.starts_with("SQLite did not answer a read within "),
        "{}",
        first.detail
    );
    let (second, failed) = cache(&db, within, failed).await;
    assert!(failed);
    assert_eq!(second.status, StatusLevel::Down, "two in a row: {second:?}");
    assert!(
        second.detail.ends_with(", after a failure the round before"),
        "{}",
        second.detail
    );

    release.send(()).unwrap();
    holder.await.unwrap().unwrap();
    let (back, failed) = cache(&db, CACHE_WITHIN, failed).await;
    assert_eq!((back.status, failed), (StatusLevel::Ok, false));
}

/// A bucket whose listing fails, or never answers, degrades `storage`.
mod a_bucket_that {
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use object_store::memory::InMemory;
    use object_store::path::Path;
    use object_store::{
        CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
        PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions,
    };

    use super::*;

    /// An in-memory bucket whose listing errors, or hangs.
    #[derive(Debug)]
    struct Listing {
        inner: InMemory,
        hangs: bool,
    }

    impl std::fmt::Display for Listing {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Listing")
        }
    }

    #[async_trait]
    impl ObjectStore for Listing {
        async fn put_opts(
            &self,
            location: &Path,
            payload: PutPayload,
            opts: PutOptions,
        ) -> object_store::Result<PutResult> {
            self.inner.put_opts(location, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            location: &Path,
            opts: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(location, opts).await
        }

        async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
            self.inner.get_opts(location, options).await
        }

        fn delete_stream(
            &self,
            locations: BoxStream<'static, object_store::Result<Path>>,
        ) -> BoxStream<'static, object_store::Result<Path>> {
            self.inner.delete_stream(locations)
        }

        fn list(&self, _prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
            use futures_util::StreamExt;
            if self.hangs {
                return futures_util::stream::pending().boxed();
            }
            futures_util::stream::once(async {
                Err(object_store::Error::Generic {
                    store: "Listing",
                    source: "access denied".into(),
                })
            })
            .boxed()
        }

        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> object_store::Result<()> {
            self.inner.copy_opts(from, to, options).await
        }

        async fn rename_opts(
            &self,
            from: &Path,
            to: &Path,
            options: RenameOptions,
        ) -> object_store::Result<()> {
            self.inner.rename_opts(from, to, options).await
        }
    }

    fn objects(hangs: bool) -> Objects {
        let store = Listing {
            inner: InMemory::new(),
            hangs,
        };
        Objects::from_store(Arc::new(store), true, "memory://")
    }

    #[tokio::test]
    async fn fails_its_listing_is_degraded() {
        let s = storage(Some(&objects(false))).await.unwrap();
        assert_eq!(s.status, StatusLevel::Degraded);
        assert!(
            s.detail.starts_with("the bucket's listing failed: ") && s.detail.contains("access denied"),
            "{}",
            s.detail
        );
    }

    #[tokio::test(start_paused = true)]
    async fn never_answers_is_degraded_after_5_s() {
        let started = Instant::now();
        let s = storage(Some(&objects(true))).await.unwrap();
        assert_eq!(
            (s.status, s.detail.as_str()),
            (
                StatusLevel::Degraded,
                "the bucket did not answer a listing within 5 s"
            )
        );
        assert_eq!(started.elapsed(), STORAGE_WITHIN);
    }
}

/// The probe's request builds on every platform, so `Checker::new` never
/// refuses a boot and a round never builds one.
#[test]
fn the_probe_request_builds_on_every_platform() {
    let endpoint = Endpoint::by_id(PROBE_METHOD).unwrap();
    for platform in Platform::ALL {
        let req = RiotRequest::new(endpoint, Target::Platform(platform), &[]).unwrap();
        assert_eq!(req.path_and_query(), "/lol/status/v4/platform-data");
        assert_eq!(req.target.scope(), platform.as_str());
    }
}

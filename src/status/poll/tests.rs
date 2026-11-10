use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio::sync::mpsc;
use tokio::time::Instant;

use super::*;
use crate::config::{Config, Sources};
use crate::http::ApiError;
use crate::riot::limiter::Limiter;
use crate::riot::limiter::headers::RateLimitType;

const KEY: &str = "RGAPI-test-key-not-real";

/// A body with the portal's `PlatformDataDto` fields (ADR-217) and the
/// given incident ids. Test data, not a recording.
fn body(platform: &str, incidents: &[i64], note: &str) -> Bytes {
    let incidents: Vec<serde_json::Value> = incidents
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "incident_severity": "warning",
                "titles": [{"locale": "en_US", "content": note}],
                "updates": [],
            })
        })
        .collect();
    Bytes::from(
        serde_json::json!({
            "id": platform.to_ascii_uppercase(),
            "name": platform,
            "locales": ["en_US"],
            "maintenances": [],
            "incidents": incidents,
        })
        .to_string(),
    )
}

fn ok(platform: Platform) -> Read {
    Read::Body(body(platform.as_str(), &[], "none"))
}

/// A scripted Riot: each platform's reads in order (a 200 with no incident
/// once its script runs out), and every read reported on `calls`.
struct Script {
    reads: Mutex<HashMap<Platform, VecDeque<Read>>>,
    calls: mpsc::UnboundedSender<Platform>,
}

impl Script {
    fn new(reads: Vec<(Platform, Vec<Read>)>) -> (Arc<Self>, mpsc::UnboundedReceiver<Platform>) {
        let (calls, rx) = mpsc::unbounded_channel();
        let reads = reads.into_iter().map(|(p, r)| (p, r.into())).collect();
        (
            Arc::new(Self {
                reads: Mutex::new(reads),
                calls,
            }),
            rx,
        )
    }
}

impl Reader for Script {
    fn read(&self, req: RiotRequest) -> BoxFuture<'_, Read> {
        let Target::Platform(platform) = req.target else {
            panic!("status is read per platform")
        };
        assert_eq!(req.endpoint.id, METHOD);
        let _ = self.calls.send(platform);
        let next = self
            .reads
            .lock()
            .unwrap()
            .get_mut(&platform)
            .and_then(VecDeque::pop_front);
        Box::pin(async move { next.unwrap_or_else(|| ok(platform)) })
    }
}

fn temp_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    (dir, db)
}

fn poller(
    reader: Arc<dyn Reader>,
    db: &Db,
    platforms: &[Platform],
    recordings: Option<Recordings>,
) -> Poller {
    Poller::with_reader(
        reader,
        db.clone(),
        platforms,
        Duration::from_secs(300),
        recordings,
    )
    .unwrap()
}

fn row(rows: &[PlatformStatus], platform: Platform) -> &PlatformStatus {
    rows.iter()
        .find(|r| r.platform == platform)
        .unwrap_or_else(|| panic!("no row for {platform}"))
}

/// Each round reads every listed platform once, at boot and then every
/// interval; aborting the task (shutdown) stops it.
#[tokio::test(start_paused = true)]
async fn a_round_reads_each_listed_platform_once_every_interval() {
    let (_dir, db) = temp_db();
    let platforms = [Platform::Oc1, Platform::Kr, Platform::Euw1];
    let (script, mut calls) = Script::new(vec![]);
    let started = Instant::now();
    let task = poller(script, &db, &platforms, None).spawn();
    let mut sorted = platforms.to_vec();
    sorted.sort();
    for round in 0..3u32 {
        let mut read = Vec::new();
        for _ in 0..platforms.len() {
            read.push(calls.recv().await.unwrap());
        }
        read.sort();
        assert_eq!(read, sorted, "round {round}");
        assert_eq!(started.elapsed(), Duration::from_secs(300) * round);
        assert!(calls.try_recv().is_err(), "one read per platform per round");
    }
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_secs(900)).await;
    assert!(calls.try_recv().is_err(), "an aborted poller reads nothing");
    let rows = stored(&db).await.unwrap();
    let order: Vec<Platform> = rows.iter().map(|r| r.platform).collect();
    assert_eq!(order, sorted, "Platform::ALL order");
    assert!(rows.iter().all(|r| r.body.is_some() && r.fetched_at_ms.is_some()));
}

/// A platform the limiter had no room for is not stored, and is read again
/// the next round.
#[tokio::test(start_paused = true)]
async fn a_skipped_platform_is_read_next_round() {
    let (_dir, db) = temp_db();
    let (script, mut calls) = Script::new(vec![(Platform::Kr, vec![Read::Skipped])]);
    let p = poller(script, &db, &[Platform::Oc1, Platform::Kr], None);

    let first = p.round().await;
    assert_eq!(
        first,
        [(Platform::Oc1, Polled::Ok), (Platform::Kr, Polled::Skipped)]
    );
    let rows = stored(&db).await.unwrap();
    assert_eq!(rows.len(), 1, "a skip writes nothing: {rows:?}");
    assert_eq!(rows[0].platform, Platform::Oc1);

    let second = p.round().await;
    assert_eq!(second, [(Platform::Oc1, Polled::Ok), (Platform::Kr, Polled::Ok)]);
    let kr = stored(&db).await.unwrap();
    let kr = row(&kr, Platform::Kr);
    assert_eq!(kr.body.as_deref().map(str::len), Some(ok(Platform::Kr).len()));
    assert_eq!((kr.failed_at_ms, kr.error.as_deref()), (None, None));
    let mut read = Vec::new();
    while let Ok(p) = calls.try_recv() {
        read.push(p);
    }
    assert_eq!(read.iter().filter(|p| **p == Platform::Kr).count(), 2);
}

impl Read {
    fn len(&self) -> usize {
        match self {
            Self::Body(b) => b.len(),
            _ => 0,
        }
    }
}

/// A failure keeps the last 200's body and `fetched_at` and stamps
/// `failed_at` and the error; the next 200 keeps the failure's stamp.
#[tokio::test(start_paused = true)]
async fn a_failure_keeps_the_last_body_and_sets_failed_at() {
    let (_dir, db) = temp_db();
    let first = body("oc1", &[], "first");
    let last = body("oc1", &[], "last");
    let (script, _calls) = Script::new(vec![(
        Platform::Oc1,
        vec![
            Read::Body(first.clone()),
            Read::Failed("UPSTREAM_ERROR: Upstream request failed".into()),
            Read::Body(Bytes::from_static(b"<html>not json</html>")),
            Read::Body(last.clone()),
        ],
    )]);
    let p = poller(script, &db, &[Platform::Oc1], None);
    let text = |b: &Bytes| Some(std::str::from_utf8(b).unwrap().to_string());

    assert_eq!(p.round().await, [(Platform::Oc1, Polled::Ok)]);
    let after_ok = stored(&db).await.unwrap().remove(0);
    assert_eq!(after_ok.body, text(&first));
    assert!(after_ok.fetched_at_ms.is_some());
    assert_eq!((after_ok.failed_at_ms, after_ok.error.as_deref()), (None, None));

    assert_eq!(p.round().await, [(Platform::Oc1, Polled::Failed)]);
    let failed = stored(&db).await.unwrap().remove(0);
    assert_eq!(failed.body, text(&first), "the body stays");
    assert_eq!(failed.fetched_at_ms, after_ok.fetched_at_ms);
    assert!(failed.failed_at_ms >= failed.fetched_at_ms && failed.failed_at_ms.is_some());
    assert_eq!(
        failed.error.as_deref(),
        Some("UPSTREAM_ERROR: Upstream request failed")
    );

    assert_eq!(
        p.round().await,
        [(Platform::Oc1, Polled::Failed)],
        "a 200 that is not JSON"
    );
    let not_json = stored(&db).await.unwrap().remove(0);
    assert_eq!(not_json.body, text(&first));
    assert!(
        not_json
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Riot's 200 is not JSON")),
        "{not_json:?}"
    );

    assert_eq!(p.round().await, [(Platform::Oc1, Polled::Ok)]);
    let again = stored(&db).await.unwrap().remove(0);
    assert_eq!(again.body, text(&last));
    assert!(again.fetched_at_ms >= not_json.failed_at_ms);
    assert_eq!(
        (again.failed_at_ms, again.error.as_deref()),
        (not_json.failed_at_ms, not_json.error.as_deref()),
        "the last failure is kept for a reader to compare"
    );
}

fn files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A 200 with an incident is recorded once per platform and incident id,
/// with the key redacted; one without, or one holding another `RGAPI-`
/// string, is not.
#[tokio::test(start_paused = true)]
async fn a_recording_is_written_only_with_an_incident_once_per_id_and_has_no_key() {
    let (dir, db) = temp_db();
    let out = dir.path().join("recordings");
    let recordings = Recordings {
        dir: out.clone(),
        key: KEY.into(),
    };
    let leaked = format!("a title quoting {KEY}");
    let (script, _calls) = Script::new(vec![
        (
            Platform::Oc1,
            vec![
                Read::Body(body("oc1", &[], "quiet")),
                Read::Body(body("oc1", &[7, 7], &leaked)),
                Read::Body(body("oc1", &[7, 9], "later")),
                Read::Body(body("oc1", &[11], "RGAPI-00000000-0000-0000-0000-000000000000")),
            ],
        ),
        (
            Platform::Kr,
            vec![
                Read::Body(body("kr", &[], "quiet")),
                Read::Body(body("kr", &[7], "kr")),
            ],
        ),
    ]);
    let p = poller(
        script,
        &db,
        &[Platform::Oc1, Platform::Kr],
        Some(recordings.clone()),
    );

    p.round().await;
    assert!(files(&out).is_empty(), "no incident, no recording");

    p.round().await;
    assert_eq!(files(&out), ["kr-incident-7.json", "oc1-incident-7.json"]);
    let oc1_7 = std::fs::read_to_string(recordings.path(Platform::Oc1, 7)).unwrap();
    assert!(!oc1_7.contains("RGAPI-"), "{oc1_7}");
    assert!(oc1_7.contains(&format!("a title quoting {REDACTED}")), "{oc1_7}");
    let v: serde_json::Value = serde_json::from_str(&oc1_7).unwrap();
    assert_eq!(
        v["incidents"][0]["id"], 7,
        "the body as Riot sent it, but the key"
    );
    let rows = stored(&db).await.unwrap();
    let stored_body = row(&rows, Platform::Oc1).body.clone().unwrap();
    assert!(stored_body.contains(KEY), "only the recording is redacted");

    p.round().await;
    assert_eq!(
        files(&out),
        ["kr-incident-7.json", "oc1-incident-7.json", "oc1-incident-9.json"]
    );
    assert_eq!(
        std::fs::read_to_string(recordings.path(Platform::Oc1, 7)).unwrap(),
        oc1_7,
        "written once: the first body that listed it"
    );
    assert!(
        std::fs::read_to_string(recordings.path(Platform::Oc1, 9))
            .unwrap()
            .contains("later")
    );

    p.round().await;
    assert!(
        !recordings.path(Platform::Oc1, 11).exists(),
        "an RGAPI- string that is not our key is never written"
    );
    for name in files(&out) {
        let text = std::fs::read_to_string(out.join(&name)).unwrap();
        assert!(!text.contains("RGAPI-"), "{name}");
        assert!(!name.ends_with(".tmp"), "{name}");
    }
}

#[test]
fn incident_ids_and_redaction() {
    let b = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
    assert!(incident_ids(&b(r#"{"incidents":[]}"#)).is_empty());
    assert!(incident_ids(&b(r#"{"maintenances":[{"id":3}]}"#)).is_empty());
    assert_eq!(
        incident_ids(&b(r#"{"incidents":[{"id":5},{"id":"x"},{},{"id":2},{"id":5}]}"#))
            .into_iter()
            .collect::<Vec<_>>(),
        [2, 5]
    );
    assert_eq!(
        redact("a KEY b KEY", "KEY").as_deref(),
        Some("a [REDACTED] b [REDACTED]")
    );
    assert_eq!(redact("RGAPI-other", "KEY"), None);
    assert_eq!(redact(&format!("x{KEY}y"), KEY).as_deref(), Some("x[REDACTED]y"));
}

/// How a [`READ`] fetch's outcome reads: our limiter's refusal is a skip; a
/// `STALE` copy, any error and Riot's other answers are failures.
#[test]
fn a_fetch_reads_as_a_body_a_skip_or_a_failure() {
    let result = |x_cache| {
        Ok(FetchResult {
            body: Bytes::from_static(b"{}"),
            x_cache,
            cache_age: Duration::ZERO,
            fetched_age: Some(Duration::ZERO),
        })
    };
    for x in [XCache::Bypass, XCache::Miss, XCache::Hit] {
        assert_eq!(read_of(result(x)), Read::Body(Bytes::from_static(b"{}")), "{x:?}");
    }
    assert!(matches!(read_of(result(XCache::Stale)), Read::Failed(_)));
    let limited = FetchError {
        limited_until: Some(Instant::now()),
        ..ApiError::from(crate::riot::limiter::RateLimited {
            retry_at: Instant::now(),
        })
        .into()
    };
    assert_eq!(read_of(Err(limited)), Read::Skipped);
    assert_eq!(
        read_of(Err(ApiError::not_found("Resource not found upstream").into())),
        Read::Failed("NOT_FOUND: Resource not found upstream".into())
    );
    let riot_429 = ApiError::new(crate::http::ErrorCode::RateLimited, "Upstream rate limited");
    assert!(
        matches!(read_of(Err(riot_429.into())), Read::Failed(e) if e.starts_with("RATE_LIMITED")),
        "Riot's own 429, retries spent, is a failure"
    );
}

fn config() -> Config {
    Config::from_sources(Sources {
        env: vec![("RIOT_API_KEY".into(), KEY.into())],
        ..Sources::default()
    })
    .unwrap()
}

/// The fetcher as `serve` builds it, pointed at a port nothing answers on.
fn fetcher(config: &Config, limiter: &Arc<Limiter>) -> Fetcher {
    Fetcher::new(crate::fetcher::FetcherParts {
        client: crate::riot::client::RiotClient::with_base_url(config, "http://127.0.0.1:9").unwrap(),
        limiter: Arc::clone(limiter),
        cache: Arc::new(crate::cache::ResponseCache::new(
            crate::cache::l1::L1::new(1024 * 1024),
            None,
        )),
        archive: Arc::new(crate::fetcher::NoArchive),
        scope: crate::cache::keys::KeyScope::for_config(config),
        policy: crate::riot::endpoints::TtlPolicy::from_config(config),
        interactive_budget: Duration::from_millis(config.client_wait_budget_ms),
        job_budget: Duration::from_millis(config.job_yield_budget_ms),
        swr: config.stale_while_revalidate,
    })
}

fn request(platform: Platform) -> RiotRequest {
    RiotRequest::new(Endpoint::by_id(METHOD).unwrap(), Target::Platform(platform), &[]).unwrap()
}

/// Through the real fetcher: no limiter room within a second is a skip,
/// even with a copy in L1 (it is never served in place of Riot's answer);
/// room inside the second is a read, here one nothing answers.
#[tokio::test(start_paused = true)]
async fn the_fetcher_skips_a_platform_without_room_within_a_second() {
    let config = config();
    let limiter = Arc::new(Limiter::new(0.8));
    let f = fetcher(&config, &limiter);
    let req = request(Platform::Oc1);
    let key = crate::cache::keys::cache_key(f.key_scope(), &req);
    let ttls = crate::riot::endpoints::TtlPolicy::from_config(&config).ttls(req.endpoint);
    f.cache()
        .put(&key, req.endpoint, body("oc1", &[], "cached"), &ttls)
        .await
        .unwrap();

    limiter.freeze("oc1", Duration::from_secs(30), RateLimitType::Service);
    let started = Instant::now();
    assert_eq!(f.read(req.clone()).await, Read::Skipped);
    assert!(started.elapsed() <= ROOM, "{:?}", started.elapsed());

    let kr = request(Platform::Kr);
    limiter.freeze("kr", Duration::from_millis(500), RateLimitType::Service);
    let started = Instant::now();
    let read = f.read(kr).await;
    assert!(matches!(read, Read::Failed(_)), "{read:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "it waited for room"
    );
}

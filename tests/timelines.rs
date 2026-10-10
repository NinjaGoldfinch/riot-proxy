//! `timelines:backfill` end to end (TL-02): archived matches without a
//! timeline, Riot mocked with wiremock, the app's own fetcher, limiter and
//! archive, and a tempfile SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use riot_proxy::archive::matches;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::DbError;
use riot_proxy::jobs::analytics::AnalyticsContext;
use riot_proxy::jobs::timelines::{self, TimelinesContext};
use riot_proxy::jobs::{Job, JobError, Queue, kinds};
use riot_proxy::riot::limiter::Priority;
use riot_proxy::riot::limiter::headers::LimitWindow;
use riot_proxy::riot::routing::Region;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Patch 16.19, ranked solo: its timeline gives every player a build.
const OLDER: (&str, &[u8], &[u8]) = (
    "OC1_711969250",
    include_bytes!("fixtures/builds/OC1_711969250.match.json"),
    include_bytes!("fixtures/builds/OC1_711969250.timeline.json"),
);
/// Patch 16.20, the newest.
const NEWEST: (&str, &[u8], &[u8]) = (
    "OC1_712417978",
    include_bytes!("fixtures/builds/OC1_712417978.match.json"),
    include_bytes!("fixtures/builds/OC1_712417978.timeline.json"),
);
/// Patch 16.14, the oldest.
const OLDEST: (&str, &[u8], &[u8]) = (
    "OC1_706102889",
    include_bytes!("fixtures/builds/OC1_706102889.match.json"),
    include_bytes!("fixtures/builds/OC1_706102889.timeline.json"),
);
const ITEMS: &[u8] = include_bytes!("fixtures/builds/item-16.19.1.json");

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    scope: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let scope = state.fetcher.key_scope().as_str().to_string();
    Env {
        _dir: dir,
        server,
        state,
        router,
        scope,
    }
}

fn job(region: Region) -> Job {
    let j = timelines::job(region);
    Job {
        id: "j".into(),
        kind: kinds::TIMELINES_BACKFILL.into(),
        dedupe_key: j.dedupe_key,
        priority: j.priority,
        payload: j.payload.to_string(),
        attempts: 1,
        run_after: 0,
    }
}

fn path_of(id: &str) -> String {
    format!("/lol/match/v5/matches/{id}/timeline")
}

impl Env {
    fn ctx(&self, patches: u32) -> TimelinesContext {
        TimelinesContext {
            fetcher: self.state.fetcher.clone(),
            queue: Queue::new(self.state.db.clone()),
            patches,
        }
    }

    /// Archive these matches without their timelines.
    async fn archive(&self, games: &[(&str, &[u8], &[u8])]) {
        for (id, body, _) in games {
            matches::put(&self.state.db, id, "sea", &self.scope, body.to_vec().into(), 1)
                .await
                .unwrap();
        }
    }

    async fn serve_timelines(&self, games: &[(&str, &[u8], &[u8])]) {
        for (id, _, timeline) in games {
            Mock::given(method("GET"))
                .and(path(path_of(id)))
                .respond_with(ResponseTemplate::new(200).set_body_raw(timeline.to_vec(), "application/json"))
                .mount(&self.server)
                .await;
        }
    }

    /// The timelines Riot was asked for, in order.
    async fn asked(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| {
                r.url
                    .path()
                    .strip_prefix("/lol/match/v5/matches/")
                    .and_then(|p| p.strip_suffix("/timeline"))
                    .map(str::to_string)
            })
            .collect()
    }

    async fn count(&self, sql: &'static str) -> i64 {
        self.state
            .db
            .read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    /// Run the region's job until it stops taking turns: `Ok` when nothing is
    /// left, or the first yield that isn't a turn (no room) or error.
    async fn run(&self, patches: u32) -> Result<(), JobError> {
        for _ in 0..20 {
            match self.ctx(patches).backfill(&job(Region::Sea)).await {
                Err(JobError::Yield {
                    retry_at,
                    payload: None,
                }) if retry_at <= now_ms() => {}
                other => return other,
            }
        }
        panic!("the backfill never finished");
    }
}

fn now_ms() -> i64 {
    riot_proxy::clock::Clock::now().unix_ms
}

#[tokio::test]
async fn newest_patch_first_within_the_bound_and_nothing_twice() {
    let e = env().await;
    e.archive(&[OLDER, NEWEST, OLDEST]).await;
    e.serve_timelines(&[OLDER, NEWEST, OLDEST]).await;

    // One turn: both timelines in the newest two patches, newest first, then
    // the worker goes back.
    let turn = e.ctx(2).backfill(&job(Region::Sea)).await;
    assert!(
        matches!(turn, Err(JobError::Yield { payload: None, .. })),
        "{turn:?}"
    );
    assert_eq!(e.asked().await, [NEWEST.0, OLDER.0]);
    assert_eq!(e.count("SELECT COUNT(*) FROM timelines").await, 2);
    // 16.14 is outside the bound.
    assert!(!e.asked().await.contains(&OLDEST.0.to_string()));
    // The new timelines' builds are queued, once.
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE kind = 'builds:extract' AND state = 'pending'")
            .await,
        1
    );

    // Run again: nothing left, nothing asked twice.
    assert!(e.run(2).await.is_ok());
    assert_eq!(e.asked().await.len(), 2);

    // Widen the bound and the older patch follows.
    assert!(e.run(3).await.is_ok());
    assert_eq!(e.asked().await, [NEWEST.0, OLDER.0, OLDEST.0]);
}

#[tokio::test]
async fn off_does_nothing() {
    let e = env().await;
    e.archive(&[NEWEST]).await;
    e.serve_timelines(&[NEWEST]).await;
    assert!(e.ctx(0).backfill(&job(Region::Sea)).await.is_ok());
    assert!(e.asked().await.is_empty());
    // And with no tick, no job is queued: the tick is only scheduled while it is on.
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE kind = 'timelines:backfill'")
            .await,
        0
    );
}

#[tokio::test]
async fn a_404_is_marked_and_never_asked_for_again() {
    let e = env().await;
    e.archive(&[OLDER, NEWEST]).await;
    Mock::given(method("GET"))
        .and(path(path_of(NEWEST.0)))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"status": {"status_code": 404}})))
        .mount(&e.server)
        .await;
    e.serve_timelines(&[OLDER]).await;

    assert!(e.run(5).await.is_ok());
    assert_eq!(
        e.asked().await,
        [NEWEST.0, OLDER.0],
        "the 404 doesn't stop the turn"
    );
    assert_eq!(
        e.count(
            "SELECT COUNT(*) FROM timeline_gaps WHERE match_id = 'OC1_712417978' AND reason = 'not_found'"
        )
        .await,
        1
    );
    // A second run asks for nothing; the progress route counts it as not found.
    assert!(e.run(5).await.is_ok());
    assert_eq!(e.asked().await.len(), 2);
    let p = timelines::progress(&e.state.db, 5, 5).await.unwrap();
    assert_eq!(
        (p.totals.not_found, p.totals.left, p.totals.with_timeline),
        (1, 0, 1)
    );
}

#[tokio::test]
async fn a_429_backs_off_until_its_retry_after() {
    let e = env().await;
    e.archive(&[OLDER, NEWEST]).await;
    Mock::given(method("GET"))
        .and(path_regex(r"/timeline$"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-rate-limit-type", "method")
                .insert_header("retry-after", "30"),
        )
        .mount(&e.server)
        .await;

    let before = now_ms();
    let out = e.ctx(5).backfill(&job(Region::Sea)).await;
    let Err(JobError::Yield { retry_at, .. }) = out else {
        panic!("expected a yield, got {out:?}");
    };
    assert!(
        retry_at - before >= 25_000,
        "yields until Retry-After, not {} ms",
        retry_at - before
    );
    // One request: the scope stays frozen, so the second match isn't tried.
    assert_eq!(e.asked().await.len(), 1);
    assert_eq!(e.count("SELECT COUNT(*) FROM timelines").await, 0);
}

#[tokio::test]
async fn a_stopped_backfill_resumes_where_it_left_off() {
    let e = env().await;
    e.archive(&[OLDER, NEWEST, OLDEST]).await;
    e.serve_timelines(&[OLDER, NEWEST, OLDEST]).await;
    // Room for one timeline: the job fetches it and yields for want of room.
    e.state.limiter.configure_method(
        "sea",
        "match.timeline",
        &[LimitWindow {
            limit: 1,
            seconds: 120,
        }],
    );
    let out = e.ctx(5).backfill(&job(Region::Sea)).await;
    let Err(JobError::Yield { retry_at, .. }) = out else {
        panic!("expected a yield, got {out:?}");
    };
    assert!(retry_at > now_ms(), "waits for room");
    assert_eq!(e.asked().await, [NEWEST.0]);

    // A new process (the job's state is the archive itself) with room again.
    e.state.limiter.configure_method(
        "sea",
        "match.timeline",
        &[LimitWindow {
            limit: 100,
            seconds: 120,
        }],
    );
    assert!(e.run(5).await.is_ok());
    assert_eq!(
        e.asked().await,
        [NEWEST.0, OLDER.0, OLDEST.0],
        "each asked once, in order"
    );
}

#[tokio::test]
async fn live_requests_keep_their_share() {
    let e = env().await;
    // Ten games to fetch, on a region allowed 10 calls in two minutes.
    let ids: Vec<String> = (0..10).map(|k| format!("OC1_9000000{k}")).collect();
    let rows = ids.clone();
    e.state
        .db
        .write(move |c| {
            for (k, id) in rows.iter().enumerate() {
                c.execute(
                    "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                     VALUES (?1, 'sea', '16.20', 420, ?2, x'00', 1, 0)",
                    rusqlite::params![id, i64::try_from(k).unwrap()],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    Mock::given(method("GET"))
        .and(path_regex(r"/timeline$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"info": {"frames": []}})))
        .mount(&e.server)
        .await;
    e.state.limiter.configure_app(
        "sea",
        &[LimitWindow {
            limit: 10,
            seconds: 120,
        }],
    );

    let out = e.ctx(5).backfill(&job(Region::Sea)).await;
    assert!(
        matches!(out, Err(JobError::Yield { retry_at, .. }) if retry_at > now_ms()),
        "the backfill stops at the bulk ceiling: {out:?}"
    );
    // BULK_USAGE_CEILING 0.80: 8 of the 10, and the rest is a visitor's.
    assert_eq!(e.asked().await.len(), 8);
    for _ in 0..2 {
        e.state
            .limiter
            .acquire("sea", "match.timeline", Priority::Interactive, Duration::ZERO)
            .await
            .expect("a visitor's request still has room");
    }
}

/// After a backfill turn and `builds:extract`, the archive has build rows, and
/// the builds route counts the game after a recompute.
#[tokio::test]
async fn backfilled_timelines_become_builds() {
    let e = env().await;
    // The match's players on the OC1 solo ladder, so its facts count (THR-02).
    let players: Vec<String> = serde_json::from_slice::<Value>(OLDER.1).unwrap()["metadata"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    let scope = e.scope.clone();
    e.state
        .db
        .write(move |c| {
            for p in players {
                c.execute(
                    "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division,
                       league_points, wins, losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
                     VALUES (?1, 'oc1', 'RANKED_SOLO_5x5', ?2, 'MASTER', 'I', 100, 1, 1, 'c', 'c', 1)",
                    rusqlite::params![scope, p],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    e.archive(&[OLDER]).await;
    e.serve_timelines(&[OLDER]).await;
    let dir = e.state.ddragon.dir().join("16.19.1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("item.json"), ITEMS).unwrap();
    std::fs::write(dir.join("versions.json"), r#"["16.19.1"]"#).unwrap();

    let analytics = AnalyticsContext {
        queue: Queue::new(e.state.db.clone()),
        hub: e.state.hub.clone(),
        key_scope: e.scope.clone(),
        patch_limit: 0,
        reextract_batch: 500,
        mirror: Arc::clone(&e.state.ddragon),
    };
    let recompute = || async {
        analytics
            .aggregate(&Job {
                id: "a".into(),
                kind: "aggregate:analytics".into(),
                dedupe_key: None,
                priority: 30_000,
                payload: json!({"platform": "oc1", "queue": "RANKED_SOLO_5x5"}).to_string(),
                attempts: 1,
                run_after: 0,
            })
            .await
            .unwrap();
    };
    // Trinity Force first (bld-fixture-puuid-01's champion): no timeline, no build.
    let champion = serde_json::from_slice::<Value>(OLDER.1).unwrap()["info"]["participants"][0]["championId"]
        .as_i64()
        .unwrap();
    let reader = consumers::create(
        &e.state.db,
        NewConsumer {
            name: "web".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 10_000,
            key: None,
        },
    )
    .await
    .unwrap();
    let builds = || async {
        let req = Request::builder()
            .uri(format!(
                "/v1/lol/analytics/champions/{champion}/builds?platform=oc1"
            ))
            .header("authorization", format!("Bearer {}", reader.key.expose()))
            .body(Body::empty())
            .unwrap();
        common::send(e.router.clone(), req).await.json()
    };
    recompute().await;
    assert_eq!(builds().await["totalGames"], 0);
    assert_eq!(e.count("SELECT COUNT(*) FROM match_builds").await, 0);

    assert!(e.run(5).await.is_ok());
    // The turn queued builds:extract; run it as the worker would.
    assert_eq!(analytics.extract_builds().await.unwrap(), 1);
    assert_eq!(e.count("SELECT COUNT(*) FROM match_builds").await, 10);
    recompute().await;
    assert_eq!(builds().await["totalGames"], 1);
}

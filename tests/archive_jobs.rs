//! `archive:match` and `backfill:player` against a wiremock Riot (plan P6-06):
//! a 250-id backfill across three pages, a walk that resumes from its cursor
//! after a failure with no duplicate archive jobs, and `match.archived` only
//! for a match that is new.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use riot_proxy::archive::SqliteArchive;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::archive::{ArchiveContext, BackfillState};
use riot_proxy::jobs::{Job, JobError, Queue};
use riot_proxy::players;
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::ws::protocol::FIREHOSE;
use riot_proxy::ws::{Hub, Topic};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const IDS: &str = "/lol/match/v5/matches/by-puuid/NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw/ids";
const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    db: Db,
    hub: Hub,
    scope: String,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let config = common::config(&[]);
    let scope = KeyScope::from_key(&config.riot_api_key).as_str().to_string();
    Env {
        _dir: dir,
        server: MockServer::start().await,
        db,
        hub: Hub::new(),
        scope,
    }
}

impl Env {
    /// A fresh context, as a new process would build it (empty caches).
    fn ctx(&self) -> ArchiveContext {
        let config = common::config(&[]);
        let key = KeyScope::from_key(&config.riot_api_key);
        let archive = Arc::new(SqliteArchive::new(self.db.clone(), key, false));
        ArchiveContext {
            fetcher: common::fetcher(
                &config,
                &self.server.uri(),
                Arc::new(Limiter::new(0.8)),
                Some(archive),
            ),
            queue: Queue::new(self.db.clone()),
            hub: self.hub.clone(),
            key_scope: self.scope.clone(),
            archive_timelines: false,
            lookup_backfill_limit: 10_000,
        }
    }

    async fn page(&self, start: u32, count: u32, ids: std::ops::Range<u32>) {
        let body: Vec<String> = ids.map(|n| format!("KR_{}", 10_000 - n)).collect();
        Mock::given(method("GET"))
            .and(path(IDS))
            .and(query_param("start", start.to_string()))
            .and(query_param("count", count.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn requests_for_start(&self, start: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.query_pairs().any(|(k, v)| k == "start" && v == start))
            .count()
    }

    async fn archive_jobs(&self) -> Vec<(String, i64)> {
        self.db
            .read(|c| {
                let mut s = c.prepare("SELECT dedupe_key, priority FROM jobs WHERE kind = 'archive:match' ORDER BY priority, rowid")?;
                let r = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                r.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
            })
            .await
            .unwrap()
    }

    async fn walk(&self) -> BackfillState {
        let p = players::get(&self.db, &self.scope, P).await.unwrap().unwrap();
        BackfillState::parse(p.backfill_state.as_deref()).unwrap()
    }
}

fn backfill(limit: u32) -> Job {
    Job {
        id: "b".into(),
        kind: "backfill:player".into(),
        dedupe_key: Some(P.into()),
        priority: 20_000,
        payload: json!({"puuid": P, "platform": "kr", "limit": limit, "reason": "lookup"}).to_string(),
        attempts: 1,
        run_after: 0,
    }
}

#[tokio::test]
async fn a_backfill_walks_250_ids_across_three_pages() {
    let e = env().await;
    e.page(0, 100, 0..100).await;
    e.page(100, 100, 100..200).await;
    e.page(200, 100, 200..250).await; // the end of their history
    e.ctx().backfill_player(&backfill(10_000)).await.unwrap();

    let jobs = e.archive_jobs().await;
    assert_eq!(jobs.len(), 250);
    assert_eq!(
        jobs.first().unwrap(),
        &("KR_10000".to_string(), 100),
        "newest first, depth block 0"
    );
    assert_eq!(
        jobs.last().unwrap(),
        &("KR_9751".to_string(), 124),
        "depth 249 → block 24"
    );
    let walk = e.walk().await;
    assert_eq!(
        (walk.depth, walk.cursor, walk.done_at.is_some()),
        (250, None, true),
        "ran out: complete"
    );
    assert_eq!(e.server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn the_default_uncapped_walk_ends_where_the_history_does() {
    // ADR-081: LOOKUP_BACKFILL_LIMIT defaults to u32::MAX, so a lookup walk
    // pages 100 at a time until Riot runs out, and that counts as complete.
    let e = env().await;
    e.page(0, 100, 0..100).await;
    e.page(100, 100, 100..200).await;
    e.page(200, 100, 200..250).await;
    let mut ctx = e.ctx();
    ctx.lookup_backfill_limit = u32::MAX;
    ctx.backfill_player(&backfill(u32::MAX)).await.unwrap();

    assert_eq!(e.archive_jobs().await.len(), 250);
    let walk = e.walk().await;
    assert_eq!(
        (walk.depth, walk.cursor, walk.done_at.is_some(), walk.limit),
        (250, None, true, u32::MAX)
    );
    assert_eq!(
        e.server.received_requests().await.unwrap().len(),
        3,
        "no page past the end"
    );
}

#[tokio::test]
async fn a_walk_that_fails_part_way_resumes_from_its_cursor() {
    let e = env().await;
    e.page(0, 100, 0..100).await;
    // Page two fails the first time only.
    Mock::given(method("GET"))
        .and(path(IDS))
        .and(query_param("start", "100"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(3) // the fetcher's own retries
        .with_priority(1)
        .mount(&e.server)
        .await;
    e.page(100, 100, 100..200).await;
    e.page(200, 100, 200..250).await;

    let err = e.ctx().backfill_player(&backfill(10_000)).await.unwrap_err();
    assert!(matches!(err, JobError::Retry(_)), "{err:?}");
    let walk = e.walk().await;
    assert_eq!(
        (walk.cursor, walk.depth, walk.done_at),
        (Some(100), 100, None),
        "stopped after page one"
    );
    assert_eq!(e.archive_jobs().await.len(), 100);

    // The retry, as a restarted process would run it.
    e.ctx().backfill_player(&backfill(10_000)).await.unwrap();
    assert_eq!(e.requests_for_start("0").await, 1, "page one was not read again");
    let jobs = e.archive_jobs().await;
    let unique: std::collections::HashSet<_> = jobs.iter().map(|j| &j.0).collect();
    assert_eq!(
        (jobs.len(), unique.len()),
        (250, 250),
        "no duplicate archive jobs"
    );
    assert!(e.walk().await.done_at.is_some());
}

#[tokio::test]
async fn a_shallow_walk_that_hits_its_limit_is_not_complete() {
    let e = env().await;
    e.page(0, 100, 0..100).await;
    e.page(100, 50, 100..150).await;
    e.ctx().backfill_player(&backfill(150)).await.unwrap();
    let walk = e.walk().await;
    assert_eq!(
        (walk.depth, walk.cursor, walk.done_at),
        (150, None, None),
        "v1 walkIsComplete"
    );
    assert_eq!(e.archive_jobs().await.len(), 150);
}

#[tokio::test]
async fn already_archived_matches_are_not_queued_again() {
    let e = env().await;
    e.db.write(|c| {
        c.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at) VALUES ('KR_9999', 'asia', '16.19', 420, 1, x'00', 1, 1)",
            [],
        )
        .map_err(DbError::from)
    })
    .await
    .unwrap();
    e.page(0, 100, 0..3).await;
    e.ctx().backfill_player(&backfill(10_000)).await.unwrap();
    let ids: Vec<String> = e.archive_jobs().await.into_iter().map(|j| j.0).collect();
    assert_eq!(ids, ["KR_10000", "KR_9998"]);
}

fn archive(match_id: &str) -> Job {
    Job {
        id: "a".into(),
        kind: "archive:match".into(),
        dedupe_key: Some(match_id.into()),
        priority: 0,
        payload: json!({"matchId": match_id, "puuid": P, "fetchTimeline": false}).to_string(),
        attempts: 1,
        run_after: 0,
    }
}

#[tokio::test]
async fn archiving_announces_a_new_match_once() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/lol/match/v5/matches/KR_8393343196"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MATCH, "application/json"))
        .mount(&e.server)
        .await;
    let mut events = e.hub.subscribe(&Topic::named(FIREHOSE));
    let ctx = e.ctx();
    ctx.archive_match(&archive("KR_8393343196")).await.unwrap();

    let frame: Value = serde_json::from_str(events.try_recv().unwrap().as_str()).unwrap();
    assert_eq!(
        (frame["event"].as_str(), frame["topic"].as_str()),
        (Some("match.archived"), Some(format!("player:{P}").as_str()))
    );
    let data = &frame["data"];
    assert_eq!(
        (data["matchId"].as_str(), data["patch"].as_str()),
        (Some("KR_8393343196"), Some("16.19"))
    );
    assert_eq!(data["participants"].as_array().unwrap().len(), 10);
    let facts: i64 =
        e.db.read(|c| {
            c.query_row("SELECT count(*) FROM match_facts", [], |r| r.get(0))
                .map_err(DbError::from)
        })
        .await
        .unwrap();
    assert_eq!(facts, 10, "archived with its facts");

    // Again: served from the archive, no Riot call, no second announcement.
    ctx.archive_match(&archive("KR_8393343196")).await.unwrap();
    assert!(events.try_recv().is_err());
    assert_eq!(e.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_match_riot_does_not_know_fails_without_retrying() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/lol/match/v5/matches/KR_1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&e.server)
        .await;
    let err = e.ctx().archive_match(&archive("KR_1")).await.unwrap_err();
    assert_eq!(err, JobError::Fail("match KR_1 not found".into()));
    let bad = e.ctx().archive_match(&archive("nonsense")).await.unwrap_err();
    assert!(matches!(bad, JobError::Fail(_)));
}

#[tokio::test]
async fn a_completed_walk_stops_further_lookup_backfills() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(IDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let (_dir, state, router) = common::app_with(&[("AUTH_DISABLED", "true")], &server.uri());
    let uri = format!("/v1/players/{P}/matches?platform=kr");
    let first = common::get(router.clone(), &uri).await.json();
    assert_eq!(first["backfill"]["status"], "queued");
    let again = common::get(router.clone(), &uri).await.json();
    assert_eq!(
        again["backfill"]["status"], "already-queued",
        "one walk at a time"
    );

    // Run the walk (the history is empty, so it completes).
    let scope = state.fetcher.key_scope().as_str().to_string();
    let ctx = ArchiveContext {
        fetcher: state.fetcher.clone(),
        queue: state.jobs.clone(),
        hub: state.hub.clone(),
        key_scope: scope,
        archive_timelines: false,
        lookup_backfill_limit: 10_000,
    };
    let s = riot_proxy::jobs::Scheduler::with_queue(state.jobs.clone(), riot_proxy::jobs::Registry::new());
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    ctx.backfill_player(&job).await.unwrap();
    s.finish(&job, &Ok(()), 1).await.unwrap();

    let later = common::get(router, &uri).await.json();
    assert_eq!(later["backfill"], Value::Null, "the history is accounted for");
}

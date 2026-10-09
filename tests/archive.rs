//! The match archive through the full app (plan P5-02): a match is fetched from
//! Riot once, then answered from SQLite with `X-Cache: ARCHIVE`, byte-identical.
//! A match and its timeline end up archived together, whichever is asked for
//! (TL-01).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::jobs::archive::ArchiveContext;
use riot_proxy::jobs::{Job, NewJob};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";
const TIMELINE: &str = r#"{"metadata":{"matchId":"KR_8393343196"},"info":{"frames":[]}}"#;

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    key: String,
}

async fn env(vars: &[(&str, &str)]) -> Env {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/lol/match/v5/matches/{MATCH_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MATCH, "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/lol/match/v5/matches/{MATCH_ID}/timeline")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TIMELINE, "application/json"))
        .mount(&server)
        .await;
    let (dir, state, router) = common::app_with(vars, &server.uri());
    let key = consumers::create(
        &state.db,
        NewConsumer {
            name: "web".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 10_000,
            key: None,
        },
    )
    .await
    .unwrap()
    .key
    .expose()
    .to_string();
    Env {
        _dir: dir,
        server,
        state,
        router,
        key,
    }
}

impl Env {
    async fn get(&self, uri: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {}", self.key))
            .body(Body::empty())
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn upstream_calls(&self) -> usize {
        self.server.received_requests().await.unwrap().len()
    }

    async fn archived(&self, table: &'static str) -> i64 {
        self.state
            .db
            .read(move |c| {
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(riot_proxy::db::DbError::from)
            })
            .await
            .unwrap()
    }
}

impl Env {
    /// Pending `archive:match` jobs: (match id, priority, payload).
    async fn queued(&self) -> Vec<(String, i64, Value)> {
        self.state
            .db
            .read(|c| {
                let mut s = c.prepare(
                    "SELECT dedupe_key, priority, payload FROM jobs
                      WHERE kind = 'archive:match' AND state = 'pending' ORDER BY rowid",
                )?;
                let rows = s.query_map([], |r| {
                    let payload: String = r.get(2)?;
                    Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&payload).unwrap()))
                })?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(riot_proxy::db::DbError::from)
            })
            .await
            .unwrap()
    }

    /// Run the queued `archive:match` for `match_id` as a worker would.
    async fn run_queued(&self, match_id: &str) {
        let (id, priority, payload) = self
            .queued()
            .await
            .into_iter()
            .find(|(id, ..)| id == match_id)
            .expect("queued");
        let ctx = ArchiveContext {
            fetcher: self.state.fetcher.clone(),
            queue: self.state.jobs.clone(),
            hub: self.state.hub.clone(),
            key_scope: self.state.fetcher.key_scope().as_str().to_string(),
            archive_timelines: self.state.config.archive_timelines,
            lookup_backfill_limit: 10_000,
        };
        let job = Job {
            id: "j".into(),
            kind: "archive:match".into(),
            dedupe_key: Some(id),
            priority,
            payload: payload.to_string(),
            attempts: 1,
            run_after: 0,
        };
        ctx.archive_match(&job).await.unwrap();
    }

    async fn paths(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect()
    }
}

fn x_cache(r: &common::Reply) -> &str {
    r.headers["x-cache"].to_str().unwrap()
}

#[tokio::test]
async fn a_match_is_fetched_once_then_served_from_the_archive() {
    let e = env(&[]).await;
    let uri = format!("/v1/lol/matches/asia/{MATCH_ID}");

    let first = e.get(&uri).await;
    assert_eq!((first.status, x_cache(&first)), (StatusCode::OK, "MISS"));
    assert_eq!(first.body, MATCH);
    assert_eq!(e.archived("matches").await, 1);
    assert_eq!(e.archived("match_facts").await, 10, "one fact row per player");

    let second = e.get(&uri).await;
    assert_eq!((second.status, x_cache(&second)), (StatusCode::OK, "ARCHIVE"));
    assert_eq!(second.body, MATCH, "byte-identical out of the archive");
    assert_eq!(e.upstream_calls().await, 1, "Riot asked once");
}

#[tokio::test]
async fn a_read_keys_refresh_is_still_served_from_the_archive() {
    let e = env(&[]).await;
    let uri = format!("/v1/lol/matches/asia/{MATCH_ID}");
    e.get(&uri).await;
    // `?refresh=true` is admin-only; a read consumer's is ignored (v1).
    let again = e.get(&format!("{uri}?refresh=true")).await;
    assert_eq!(x_cache(&again), "ARCHIVE");
    assert_eq!(e.upstream_calls().await, 1);
}

/// Timelines are immutable like matches: a fetched one is archived whatever
/// `ARCHIVE_TIMELINES` says, which only decides whether archive jobs fetch them (ADR-085).
#[tokio::test]
async fn a_fetched_timeline_is_archived_whatever_archive_timelines_says() {
    let tl = format!("/v1/lol/matches/asia/{MATCH_ID}/timeline");
    for vars in [
        &[][..],
        &[("ARCHIVE_TIMELINES", "false")],
        &[("ARCHIVE_TIMELINES", "true")],
    ] {
        let e = env(vars).await;
        e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
        assert_eq!(x_cache(&e.get(&tl).await), "MISS", "{vars:?}");
        assert_eq!(e.archived("timelines").await, 1, "{vars:?}");
        let r = e.get(&tl).await;
        assert_eq!(
            (r.status, x_cache(&r), r.body.as_slice()),
            (StatusCode::OK, "ARCHIVE", TIMELINE.as_bytes()),
            "{vars:?}"
        );
        assert_eq!(e.upstream_calls().await, 2, "one match, one timeline: {vars:?}");
    }
}

/// TL-01: a match stored by a read queues its timeline at the top priority, and
/// the job fetches only the timeline. The caller's own request costs one call.
#[tokio::test]
async fn a_stored_match_queues_its_timeline_and_the_job_fetches_only_that() {
    let e = env(&[]).await;
    let r = e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    assert_eq!((r.status, x_cache(&r)), (StatusCode::OK, "MISS"));
    assert_eq!(
        e.queued().await,
        [(
            MATCH_ID.to_string(),
            0,
            json!({"matchId": MATCH_ID, "fetchTimeline": true})
        )]
    );
    assert_eq!(
        e.upstream_calls().await,
        1,
        "the timeline isn't fetched on the caller's time"
    );

    e.run_queued(MATCH_ID).await;
    let m = format!("/lol/match/v5/matches/{MATCH_ID}");
    assert_eq!(e.paths().await, [m.clone(), format!("{m}/timeline")]);
    assert_eq!(e.archived("timelines").await, 1);
    let r = e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}/timeline")).await;
    assert_eq!((x_cache(&r), r.body.as_slice()), ("ARCHIVE", TIMELINE.as_bytes()));
    assert_eq!(e.upstream_calls().await, 2);
}

/// TL-01: a timeline asked for before its match (a client loading both at
/// once) is stored, and its match is queued; the job fetches only the match.
#[tokio::test]
async fn a_timeline_before_its_match_is_archived_and_queues_the_match() {
    for vars in [&[][..], &[("ARCHIVE_TIMELINES", "false")]] {
        let e = env(vars).await;
        let tl = format!("/v1/lol/matches/asia/{MATCH_ID}/timeline");
        let r = e.get(&tl).await;
        assert_eq!((r.status, x_cache(&r)), (StatusCode::OK, "MISS"), "{vars:?}");
        assert_eq!(
            (e.archived("timelines").await, e.archived("matches").await),
            (1, 0),
            "{vars:?}"
        );
        assert_eq!(x_cache(&e.get(&tl).await), "ARCHIVE", "{vars:?}");
        let queued = e.queued().await;
        assert_eq!(
            queued
                .iter()
                .map(|(id, p, _)| (id.as_str(), *p))
                .collect::<Vec<_>>(),
            [(MATCH_ID, 0)],
            "{vars:?}"
        );

        e.run_queued(MATCH_ID).await;
        let m = format!("/lol/match/v5/matches/{MATCH_ID}");
        assert_eq!(e.paths().await, [format!("{m}/timeline"), m], "{vars:?}");
        assert_eq!(e.archived("matches").await, 1, "{vars:?}");
    }
}

/// `ARCHIVE_TIMELINES=false` turns timelines off: a stored match queues nothing.
#[tokio::test]
async fn with_archive_timelines_off_a_stored_match_queues_nothing() {
    let e = env(&[("ARCHIVE_TIMELINES", "false")]).await;
    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    assert_eq!(e.queued().await, []);
}

/// A match a walk already queued (low priority, no timeline) is lifted to the
/// top and made to fetch its timeline, not queued twice.
#[tokio::test]
async fn a_walks_queued_job_is_lifted_and_asks_for_the_timeline() {
    let e = env(&[]).await;
    let walk = json!({"matchId": MATCH_ID, "puuid": "P", "fetchTimeline": false});
    e.state
        .jobs
        .enqueue(NewJob::new("archive:match", 128, walk).dedupe(MATCH_ID))
        .await
        .unwrap();
    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    assert_eq!(
        e.queued().await,
        [(
            MATCH_ID.to_string(),
            0,
            json!({"matchId": MATCH_ID, "puuid": "P", "fetchTimeline": true})
        )]
    );
}

/// A match whose timeline is already archived queues nothing.
#[tokio::test]
async fn a_match_whose_timeline_is_archived_queues_nothing() {
    let e = env(&[]).await;
    riot_proxy::archive::matches::put_timeline(&e.state.db, MATCH_ID, TIMELINE.as_bytes().to_vec().into())
        .await
        .unwrap();
    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    assert_eq!(e.queued().await, []);
}

//! The match archive through the full app (plan P5-02): a match is fetched from
//! Riot once, then answered from SQLite with `X-Cache: ARCHIVE`, byte-identical.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
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

#[tokio::test]
async fn timelines_are_archived_only_with_archive_timelines() {
    let tl = format!("/v1/lol/matches/asia/{MATCH_ID}/timeline");

    let off = env(&[]).await;
    off.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    for _ in 0..2 {
        let r = off.get(&tl).await;
        assert_eq!((r.status, x_cache(&r)), (StatusCode::OK, "MISS"));
    }
    assert_eq!(off.archived("timelines").await, 0);

    let on = env(&[("ARCHIVE_TIMELINES", "true")]).await;
    on.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    assert_eq!(x_cache(&on.get(&tl).await), "MISS");
    let r = on.get(&tl).await;
    assert_eq!((x_cache(&r), r.body.as_slice()), ("ARCHIVE", TIMELINE.as_bytes()));
    assert_eq!(on.upstream_calls().await, 2, "one match, one timeline");
}

#[tokio::test]
async fn a_timeline_before_its_match_is_not_archived() {
    let e = env(&[("ARCHIVE_TIMELINES", "true")]).await;
    let r = e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}/timeline")).await;
    assert_eq!((r.status, x_cache(&r)), (StatusCode::OK, "MISS"));
    assert_eq!(e.archived("timelines").await, 0);
}

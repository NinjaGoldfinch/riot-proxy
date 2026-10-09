//! `POST /v1/admin/ladder/probe` (LAD-03) against a wiremock league-v4 shaped
//! like kr on 2026-10-09: `masterleagues` lists exactly 10,000, league-exp
//! pages the same list (48 pages of 205, then 160, then empty) and the paged
//! route answers 400 for MASTER.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SOLO: &str = "RANKED_SOLO_5x5";

fn player(i: usize) -> Value {
    json!({"puuid": format!("p{i}"), "leaguePoints": 2000 - i64::try_from(i).unwrap() / 10,
           "rank": "I", "wins": 10, "losses": 5})
}

async fn list(server: &MockServer, tier: &str, n: usize) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/lol/league/v4/{}leagues/by-queue/{SOLO}",
            tier.to_ascii_lowercase()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"tier": tier, "queue": SOLO, "entries": (0..n).map(player).collect::<Vec<_>>()}),
        ))
        .mount(server)
        .await;
}

/// league-exp's MASTER/I: `n` players, 205 a page, then empty pages.
async fn exp(server: &MockServer, n: usize) {
    let pages = n.div_ceil(205);
    for page in 1..=pages + 1 {
        let body: Vec<Value> = ((page - 1) * 205..(page * 205).min(n)).map(player).collect();
        Mock::given(method("GET"))
            .and(path(format!("/lol/league-exp/v4/entries/{SOLO}/MASTER/I")))
            .and(query_param("page", page.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }
}

async fn paged_refuses_master(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/lol/league/v4/entries/{SOLO}/MASTER/I")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"status": {"status_code": 400}})))
        .mount(server)
        .await;
}

struct App {
    _dir: tempfile::TempDir,
    server: MockServer,
    router: axum::Router,
    admin: String,
    reader: String,
}

async fn app() -> App {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let mint = |name: &str, scopes| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 10_000,
        key: None,
    };
    let admin = consumers::create(&state.db, mint("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap();
    let reader = consumers::create(&state.db, mint("web", vec![Scope::Read]))
        .await
        .unwrap();
    App {
        _dir: dir,
        server,
        router,
        admin: admin.key.expose().to_string(),
        reader: reader.key.expose().to_string(),
    }
}

impl App {
    async fn probe(&self, key: &str, body: Value) -> common::Reply {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/admin/ladder/probe")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn calls_to(&self, part: &str) -> usize {
        let got = self.server.received_requests().await.unwrap();
        got.iter().filter(|r| r.url.path().contains(part)).count()
    }
}

fn verdicts(body: &Value) -> Vec<(String, String)> {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["verdict"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_capped_master_list_confirms_every_check() {
    let a = app().await;
    list(&a.server, "MASTER", 10_000).await;
    list(&a.server, "GRANDMASTER", 700).await;
    list(&a.server, "CHALLENGER", 300).await;
    exp(&a.server, 10_000).await;
    paged_refuses_master(&a.server).await;

    let r = a.probe(&a.admin, json!({"platform": "kr", "queue": SOLO})).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(
        verdicts(&body),
        [
            ("master-capped".into(), "confirmed".into()),
            ("exp-same-list".into(), "confirmed".into()),
            ("paged-refuses-apex".into(), "confirmed".into()),
        ]
    );
    assert_eq!(
        (&body["platform"], &body["queue"], &body["cap"]),
        (&json!("kr"), &json!(SOLO), &json!(10_000))
    );
    assert_eq!(
        (
            &body["exp"]["pages"],
            &body["exp"]["lastPageSize"],
            &body["exp"]["inBoth"],
            &body["exp"]["onlyInExp"]
        ),
        (&json!(49), &json!(160), &json!(10_000), &json!(0)),
        "48 pages of 205 and 160 on page 49, as on kr"
    );
    assert_eq!(body["lists"][0]["tier"], "MASTER");
    assert_eq!(body["lists"][0]["capped"], true);
    assert_eq!(body["lists"][0]["lowestLp"], 1001);
    assert_eq!(body["pagedMaster"]["status"], "refused");
    // Pages go out ten at a time and stop with the batch holding the first
    // empty page (page 50 here).
    assert_eq!(a.calls_to("/league-exp/").await, 50);

    // Fresh every time: a second probe asks Riot again.
    a.probe(&a.admin, json!({"platform": "kr"})).await;
    assert_eq!(a.calls_to("/masterleagues/").await, 2);
}

#[tokio::test]
async fn a_short_master_list_is_not_capped() {
    let a = app().await;
    list(&a.server, "MASTER", 450).await;
    list(&a.server, "GRANDMASTER", 200).await;
    list(&a.server, "CHALLENGER", 50).await;
    exp(&a.server, 450).await;
    paged_refuses_master(&a.server).await;

    let body = a.probe(&a.admin, json!({"platform": "oc1"})).await.json();
    assert_eq!(verdicts(&body)[0], ("master-capped".into(), "not-seen".into()));
    assert!(body["summary"].as_str().unwrap().starts_with("Not capped"));
}

#[tokio::test]
async fn the_probe_is_admin_only_and_validates_its_body() {
    let a = app().await;
    assert_eq!(
        a.probe(&a.reader, json!({"platform": "kr"})).await.status,
        StatusCode::FORBIDDEN
    );
    for (body, message) in [
        (json!({}), "body must have required property 'platform'"),
        (
            json!({"platform": "kr", "queue": "ARAM"}),
            "body/queue must be equal to one of the allowed values",
        ),
        (
            json!({"platform": "xx1"}),
            "body/platform must be equal to one of the allowed values",
        ),
    ] {
        let r = a.probe(&a.admin, body.clone()).await;
        assert_eq!(
            (r.status, r.json()["error"]["message"].clone()),
            (StatusCode::BAD_REQUEST, json!(message)),
            "{body}"
        );
    }
    assert!(a.server.received_requests().await.unwrap().is_empty());
}

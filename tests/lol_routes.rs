//! `/v1/lol/*` through the full app against a wiremock Riot: every route's exact
//! upstream request (v1 defaults included), v1 validation, and contract coverage.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    router: axum::Router,
    key: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"from":"riot"}"#))
        .mount(&server)
        .await;
    let (dir, state, router) = common::app_with(&[], &server.uri());
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
        router,
        key,
    }
}

async fn get(e: &Env, uri: &str) -> common::Reply {
    let req = Request::get(uri)
        .header("authorization", format!("Bearer {}", e.key))
        .body(Body::empty())
        .unwrap();
    common::send(e.router.clone(), req).await
}

/// What the last upstream request looked like.
async fn last_upstream(e: &Env) -> String {
    let reqs = e.server.received_requests().await.unwrap();
    let r = reqs.last().expect("an upstream request");
    match r.url.query() {
        Some(q) => format!("{}?{q}", r.url.path()),
        None => r.url.path().to_string(),
    }
}

/// Every route once: the exact upstream call and the proxy's answer (snapshot).
#[tokio::test]
async fn every_route_reaches_the_right_upstream_endpoint() {
    let e = env().await;
    let routes = [
        format!("/v1/lol/summoners/by-puuid/euw1/{P}"),
        format!("/v1/lol/league/entries/by-puuid/kr/{P}"),
        "/v1/lol/league/apex/euw1/CHALLENGER/RANKED_SOLO_5x5".to_string(),
        "/v1/lol/league/apex/na1/GRANDMASTER/RANKED_FLEX_SR".to_string(),
        "/v1/lol/league/entries/euw1/RANKED_SOLO_5x5/DIAMOND/I".to_string(),
        "/v1/lol/league/entries/euw1/RANKED_SOLO_5x5/IRON/IV?page=42".to_string(),
        format!("/v1/lol/matches/ids/europe/{P}"),
        format!(
            "/v1/lol/matches/ids/europe/{P}?count=100&start=5&queue=420&type=ranked&startTime=1&endTime=2&junk=x"
        ),
        "/v1/lol/matches/europe/EUW1_7381937461".to_string(),
        "/v1/lol/matches/sea/OC1_123456789/timeline".to_string(),
        format!("/v1/lol/spectator/active/oc1/{P}"),
        format!("/v1/lol/mastery/by-puuid/euw1/{P}"),
        format!("/v1/lol/mastery/by-puuid/euw1/{P}?top=3"),
        "/v1/lol/rotations/br1".to_string(),
        "/v1/lol/status/jp1".to_string(),
    ];
    let mut table = Vec::new();
    for uri in &routes {
        let r = get(&e, uri).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        assert_eq!(r.body, br#"{"from":"riot"}"#, "{uri}: passthrough");
        table.push(serde_json::json!({
            "route": uri.replace(P, "{puuid}"),
            "upstream": last_upstream(&e).await.replace(P, "{puuid}"),
            "status": r.status.as_u16(),
            "x_cache": r.headers["x-cache"].to_str().unwrap(),
        }));
    }
    insta::assert_json_snapshot!("lol_routes_upstream_calls", table);
}

#[tokio::test]
async fn validation_follows_v1_and_never_reaches_upstream() {
    let e = env().await;
    let cases = [
        // (uri, code, message)
        (
            "/v1/lol/league/entries/euw1/RANKED_SOLO_5x5/MASTER/I",
            "VALIDATION",
            "params/tier must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/league/apex/euw1/DIAMOND/RANKED_SOLO_5x5",
            "VALIDATION",
            "params/tier must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/league/apex/euw1/MASTER/RANKED_TFT",
            "VALIDATION",
            "params/queue must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/league/entries/euw1/RANKED_SOLO_5x5/GOLD/V",
            "VALIDATION",
            "params/division must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/league/entries/euw1/RANKED_SOLO_5x5/GOLD/I?page=0",
            "VALIDATION",
            "querystring/page must be >= 1",
        ),
        (
            "/v1/lol/matches/americas/EUW1_7381937461",
            "BAD_REGION",
            "Match EUW1_7381937461 belongs to region 'europe', not 'americas'",
        ),
        (
            "/v1/lol/matches/europe/not-a-match",
            "VALIDATION",
            "params/matchId must match pattern \"^[A-Za-z0-9]+_[0-9]+$\"",
        ),
        (
            "/v1/lol/rotations/EUW1",
            "BAD_REGION",
            "params/platform must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/status/xx1",
            "BAD_REGION",
            "params/platform must be equal to one of the allowed values",
        ),
    ];
    for (uri, code, message) in cases {
        let r = get(&e, uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        let j = r.json();
        assert_eq!(
            (
                j["error"]["code"].as_str().unwrap(),
                j["error"]["message"].as_str().unwrap()
            ),
            (code, message),
            "{uri}"
        );
    }
    for (query, message) in [
        ("count=101", "querystring/count must be <= 100"),
        ("count=0", "querystring/count must be >= 1"),
        ("start=10001", "querystring/start must be <= 10000"),
        ("queue=5001", "querystring/queue must be <= 5000"),
        (
            "type=arena",
            "querystring/type must be equal to one of the allowed values",
        ),
        ("startTime=-1", "querystring/startTime must be >= 0"),
    ] {
        let r = get(&e, &format!("/v1/lol/matches/ids/europe/{P}?{query}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(r.json()["error"]["message"], message, "{query}");
    }
    let r = get(&e, &format!("/v1/lol/mastery/by-puuid/euw1/{P}?top=201")).await;
    assert_eq!(r.json()["error"]["message"], "querystring/top must be <= 200");
    assert!(e.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn equivalent_match_id_queries_share_one_cache_entry() {
    let e = env().await;
    let a = get(&e, &format!("/v1/lol/matches/ids/europe/{P}")).await;
    // v1 defaults made these the same request.
    let b = get(&e, &format!("/v1/lol/matches/ids/europe/{P}?count=20&start=0")).await;
    assert_eq!(
        (
            a.headers["x-cache"].to_str().unwrap(),
            b.headers["x-cache"].to_str().unwrap()
        ),
        ("MISS", "HIT")
    );
    assert_eq!(e.server.received_requests().await.unwrap().len(), 1);
}

/// Plan P4-05, as amended by the owner (ADR-037): every v1 `/v1/lol` operation is
/// documented except the three analytics routes, which P7-04 owns.
#[test]
fn every_v1_lol_operation_except_analytics_is_documented() {
    let v1: serde_json::Value =
        serde_json::from_str(include_str!("../docs/contract/v1-openapi.json")).unwrap();
    let ours: serde_json::Value =
        serde_json::from_str(&riot_proxy::routes::docs::spec().to_json().unwrap()).unwrap();
    let ops = |d: &serde_json::Value| -> std::collections::BTreeSet<String> {
        d["paths"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with("/v1/lol/"))
            .flat_map(|(p, item)| {
                item.as_object()
                    .unwrap()
                    .keys()
                    .map(move |m| format!("{} {p}", m.to_uppercase()))
            })
            .collect()
    };
    let missing: Vec<String> = ops(&v1).difference(&ops(&ours)).cloned().collect();
    assert_eq!(
        missing,
        [
            "GET /v1/lol/analytics/champions",
            "GET /v1/lol/analytics/champions/{championId}",
            "GET /v1/lol/analytics/champions/{championId}/matchups",
        ]
    );
    assert!(
        ops(&ours).difference(&ops(&v1)).next().is_none(),
        "nothing undocumented in v1"
    );
}

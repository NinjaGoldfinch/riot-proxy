//! The Riot client against a wiremock upstream: headers sent, bytes passed through,
//! each status classified, no retries, metrics recorded.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use riot_proxy::riot::client::{RiotClient, RiotError, RiotErrorKind, RiotRequest};
use riot_proxy::riot::endpoints::Endpoint;
use riot_proxy::riot::routing::{Platform, Region};
use riot_proxy::telemetry;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "riot-proxy-test/1.0";

fn client(server: &MockServer) -> RiotClient {
    let config = common::config(&[("RIOT_USER_AGENT", UA)]);
    RiotClient::with_base_url(&config, &server.uri()).unwrap()
}

fn summoner(puuid: &str) -> RiotRequest {
    let e = Endpoint::by_id("summoner.byPuuid").unwrap();
    RiotRequest::new(e, e.target_for_platform(Platform::Euw1), &[puuid]).unwrap()
}

async fn status_error(status: u16, headers: &[(&str, &str)]) -> RiotError {
    let server = MockServer::start().await;
    let mut tpl = ResponseTemplate::new(status).set_body_string("{\"status\":{}}");
    for (k, v) in headers {
        tpl = tpl.insert_header(*k, *v);
    }
    Mock::given(method("GET"))
        .respond_with(tpl)
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .send(&summoner("P"))
        .await
        .expect_err("non-2xx is an error")
}

#[tokio::test]
async fn sends_token_user_agent_and_accept_and_passes_bytes_through() {
    let server = MockServer::start().await;
    // Deliberately odd formatting and a non-ASCII byte sequence: must survive untouched.
    let body: &[u8] = b"{ \"puuid\" :\"P\",\n  \"name\":\"\xc3\x9cml\xc3\xa4ut\", \"n\": 1.50 }";
    Mock::given(method("GET"))
        .and(path("/lol/summoner/v4/summoners/by-puuid/P%20Q"))
        .and(header("x-riot-token", common::TEST_KEY))
        .and(header("user-agent", UA))
        .and(header("accept", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body.to_vec(), "application/json;charset=utf-8")
                .insert_header("x-app-rate-limit", "20:1,100:120")
                .insert_header("x-app-rate-limit-count", "1:1,1:120"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let res = client(&server).send(&summoner("P Q")).await.expect("200");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_ref(), body, "byte-identical passthrough");
    assert_eq!(res.headers["x-app-rate-limit"], "20:1,100:120");
    assert_eq!(res.headers["x-app-rate-limit-count"], "1:1,1:120");
}

#[tokio::test]
async fn query_strings_reach_upstream() {
    let server = MockServer::start().await;
    Mock::given(path("/lol/match/v5/matches/by-puuid/P/ids"))
        .and(query_param("start", "0"))
        .and(query_param("count", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;
    let e = Endpoint::by_id("match.idsByPuuid").unwrap();
    let req = RiotRequest::new(e, e.target_for_region(Region::Europe).unwrap(), &["P"])
        .unwrap()
        .query("start", Some(0))
        .unwrap()
        .query("count", Some(100))
        .unwrap();
    assert_eq!(client(&server).send(&req).await.unwrap().body.as_ref(), b"[]");
}

#[tokio::test]
async fn each_status_maps_to_its_kind_without_retrying() {
    assert_eq!(status_error(404, &[]).await.kind, RiotErrorKind::NotFound);
    assert_eq!(status_error(401, &[]).await.kind, RiotErrorKind::UpstreamAuth);
    assert_eq!(status_error(403, &[]).await.kind, RiotErrorKind::UpstreamAuth);
    for s in [500, 502, 503, 504] {
        let e = status_error(s, &[]).await;
        assert_eq!(e.kind, RiotErrorKind::UpstreamUnavailable, "{s}");
        assert_eq!(e.status, Some(s));
    }
    assert_eq!(status_error(400, &[]).await.kind, RiotErrorKind::UnexpectedStatus);
    // `.expect(1)` on every mock asserts exactly one upstream call: no retries here.
}

#[tokio::test]
async fn typed_429_carries_type_retry_after_and_headers() {
    let e = status_error(
        429,
        &[
            ("x-rate-limit-type", "application"),
            ("retry-after", "7"),
            ("x-app-rate-limit-count", "21:1,40:120"),
        ],
    )
    .await;
    assert_eq!(
        e.kind,
        RiotErrorKind::RateLimited {
            limit_type: Some("application".into()),
            retry_after: Some(7)
        }
    );
    assert_eq!(
        e.headers["x-app-rate-limit-count"], "21:1,40:120",
        "limiter can observe errors"
    );
    assert_eq!(e.status, Some(429));
}

#[tokio::test]
async fn untyped_429_is_service_level() {
    let e = status_error(429, &[]).await;
    assert_eq!(
        e.kind,
        RiotErrorKind::RateLimited {
            limit_type: None,
            retry_after: None
        }
    );
    let e = status_error(429, &[("retry-after", "soon")]).await;
    assert_eq!(
        e.kind,
        RiotErrorKind::RateLimited {
            limit_type: None,
            retry_after: None
        }
    );
}

#[tokio::test]
async fn network_failure_is_upstream_unavailable() {
    // Bind then drop: nothing listens on this port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = common::config(&[]);
    let c = RiotClient::with_base_url(&config, &format!("http://127.0.0.1:{port}")).unwrap();
    let e = c.send(&summoner("P")).await.expect_err("connect refused");
    assert_eq!(e.kind, RiotErrorKind::UpstreamUnavailable);
    assert_eq!(e.status, None);
    assert!(e.headers.is_empty());
}

#[tokio::test]
async fn upstream_metrics_are_recorded() {
    let handle = telemetry::metrics_handle().unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/lol/status/v4/platform-data"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-rate-limit-type", "method")
                .insert_header("retry-after", "2"),
        )
        .mount(&server)
        .await;
    let e = Endpoint::by_id("status.platformData").unwrap();
    let req = RiotRequest::new(e, e.target_for_platform(Platform::Kr), &[]).unwrap();
    let _ = client(&server).send(&req).await;

    let text = handle.render();
    assert!(
        text.contains(
            r#"proxy_upstream_requests_total{region="kr",method="status.platformData",status="429"} 1"#
        ),
        "{text}"
    );
    assert!(
        text.contains(r#"proxy_rl_429_total{region="kr",type="method"} 1"#),
        "{text}"
    );
    assert!(
        text.contains(r#"proxy_upstream_latency_seconds_count{region="kr",method="status.platformData"}"#),
        "{text}"
    );
}

#[tokio::test]
async fn the_key_never_appears_in_debug_output() {
    let server = MockServer::start().await;
    let c = client(&server);
    assert!(!format!("{c:?}").contains(common::TEST_KEY));
    let e = status_error(403, &[]).await;
    assert!(!format!("{e:?}").contains(common::TEST_KEY));
    assert!(!e.to_string().contains(common::TEST_KEY));
}

#[tokio::test]
async fn error_bodies_snapshot() {
    let cases = [
        ("404", status_error(404, &[]).await),
        ("401", status_error(401, &[]).await),
        (
            "429_typed",
            status_error(429, &[("x-rate-limit-type", "method"), ("retry-after", "3")]).await,
        ),
        ("429_service", status_error(429, &[]).await),
        ("503", status_error(503, &[]).await),
        ("418", status_error(418, &[]).await),
    ];
    let bodies: serde_json::Map<String, serde_json::Value> = cases
        .iter()
        .map(|(name, e)| {
            let api = e.to_api_error();
            let body = serde_json::to_value(api.envelope()).unwrap();
            (
                name.to_string(),
                serde_json::json!({"status": api.status.as_u16(), "body": body}),
            )
        })
        .collect();
    insta::assert_json_snapshot!("riot_error_bodies", bodies);
}

//! `/metrics` exposition and the request-id middleware, driven through a Router.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use riot_proxy::http::request_id::{RequestId, X_REQUEST_ID, request_id};
use riot_proxy::metrics::{RL_WAIT_SECONDS, UPSTREAM_LATENCY_SECONDS};
use riot_proxy::telemetry::{METRICS_CONTENT_TYPE, metrics_handle, metrics_router};
use tower::ServiceExt;

fn app() -> Router {
    let handle = metrics_handle().expect("recorder");
    Router::new()
        .route(
            "/echo",
            get(|axum::Extension(id): axum::Extension<RequestId>| async move { id.0 }),
        )
        .merge(metrics_router(handle))
        .layer(axum::middleware::from_fn(request_id))
}

async fn get_body(app: Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.oneshot(req).await.expect("response");
    let status = res.status();
    let headers = res.headers().clone();
    let body = to_bytes(res.into_body(), usize::MAX).await.expect("body");
    (status, headers, String::from_utf8(body.to_vec()).expect("utf8"))
}

#[tokio::test]
async fn metrics_is_prometheus_text_with_registered_counters() {
    let (status, headers, body) =
        get_body(app(), Request::get("/metrics").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], METRICS_CONTENT_TYPE);
    assert!(
        body.contains("# TYPE proxy_archived_matches_total counter"),
        "{body}"
    );
    assert!(body.contains("proxy_archived_matches_total 0"), "{body}");
    assert!(
        body.contains("# HELP proxy_archived_matches_total Matches upserted into the archive"),
        "{body}"
    );
    assert!(body.contains("# TYPE proxy_ws_connections gauge"), "{body}");
}

#[tokio::test]
async fn histograms_render_with_v1_buckets() {
    let handle = metrics_handle().expect("recorder");
    metrics::histogram!(UPSTREAM_LATENCY_SECONDS, "region" => "europe", "method" => "account").record(0.2);
    metrics::histogram!(RL_WAIT_SECONDS, "region" => "euw1", "priority" => "interactive").record(0.0);
    handle.run_upkeep();

    let (_, _, body) = get_body(app(), Request::get("/metrics").body(Body::empty()).unwrap()).await;
    assert!(
        body.contains("# TYPE proxy_upstream_latency_seconds histogram"),
        "{body}"
    );
    assert!(
        body.contains(
            r#"proxy_upstream_latency_seconds_bucket{region="europe",method="account",le="0.25"} 1"#
        ),
        "{body}"
    );
    assert!(
        body.contains(
            r#"proxy_upstream_latency_seconds_bucket{region="europe",method="account",le="0.1"} 0"#
        ),
        "{body}"
    );
    assert!(
        body.contains(r#"proxy_rl_wait_seconds_bucket{region="euw1",priority="interactive",le="0.001"} 1"#),
        "{body}"
    );
}

#[tokio::test]
async fn generates_a_ulid_request_id_and_echoes_it() {
    let (status, headers, body) = get_body(app(), Request::get("/echo").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let id = headers[&X_REQUEST_ID].to_str().unwrap();
    assert_eq!(id.len(), 26, "ULID is 26 chars: {id}");
    assert!(id.parse::<ulid::Ulid>().is_ok(), "{id}");
    assert_eq!(body, id, "handler sees the same id the response carries");
}

#[tokio::test]
async fn honours_a_well_formed_inbound_request_id() {
    let req = Request::get("/echo")
        .header("x-request-id", "client-abc_123.4:5")
        .body(Body::empty())
        .unwrap();
    let (_, headers, body) = get_body(app(), req).await;
    assert_eq!(headers[&X_REQUEST_ID], "client-abc_123.4:5");
    assert_eq!(body, "client-abc_123.4:5");
}

#[tokio::test]
async fn replaces_a_malformed_or_oversized_inbound_request_id() {
    for bad in ["has space", "semi;colon", "", &"x".repeat(129)] {
        let req = Request::get("/echo")
            .header("x-request-id", bad)
            .body(Body::empty())
            .unwrap();
        let (_, headers, _) = get_body(app(), req).await;
        let id = headers[&X_REQUEST_ID].to_str().unwrap();
        assert_ne!(id, bad);
        assert!(id.parse::<ulid::Ulid>().is_ok(), "{bad:?} → {id}");
    }
}

#[tokio::test]
async fn unrouted_requests_still_get_a_request_id() {
    let (status, headers, _) = get_body(app(), Request::get("/nope").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.contains_key(&X_REQUEST_ID));
}

#[tokio::test]
async fn published_events_are_counted_by_name() {
    let handle = metrics_handle().expect("recorder");
    let event = riot_proxy::events::Event::PatchNew {
        version: "16.19.1".into(),
    };
    riot_proxy::events::publish(&riot_proxy::ws::Hub::new(), &event);
    handle.run_upkeep();
    let (_, _, body) = get_body(app(), Request::get("/metrics").body(Body::empty()).unwrap()).await;
    assert!(body.contains("# TYPE events_published_total counter"), "{body}");
    assert!(
        body.contains(r#"events_published_total{name="patch.new"}"#),
        "{body}"
    );
}

#[tokio::test]
async fn sqlite_writes_are_timed_and_the_queue_is_a_gauge() {
    let handle = metrics_handle().expect("recorder");
    let dir = tempfile::tempdir().unwrap();
    let db = riot_proxy::db::Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    db.write(|c| {
        c.execute_batch("CREATE TABLE t (a INTEGER)")
            .map_err(riot_proxy::db::DbError::from)
    })
    .await
    .unwrap();
    handle.run_upkeep();
    let (_, _, body) = get_body(app(), Request::get("/metrics").body(Body::empty()).unwrap()).await;
    for name in ["sqlite_write_seconds", "sqlite_write_wait_seconds"] {
        assert!(body.contains(&format!("# TYPE {name} histogram")), "{body}");
        let les: Vec<&str> = body
            .lines()
            .filter_map(|l| l.strip_prefix(&format!("{name}_bucket{{le=\"")))
            .filter_map(|l| l.split('"').next())
            .collect();
        assert_eq!(
            les,
            [
                "0.001", "0.01", "0.05", "0.1", "0.25", "0.5", "1", "2", "5", "10", "30", "60", "+Inf"
            ],
            "{body}"
        );
        assert!(body.contains(&format!("{name}_count ")), "{body}");
    }
    assert!(body.contains("# TYPE sqlite_write_queue gauge"), "{body}");
    assert!(body.contains("sqlite_write_queue 0"), "{body}");
}

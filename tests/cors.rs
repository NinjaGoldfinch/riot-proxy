//! ADM-07: opt-in CORS for the admin API. Unset, no response carries an
//! `Access-Control-*` header; set, the listed origins may call the admin
//! routes, `/v1/ws` and the health routes from a browser, a preflight needs
//! no key, and the read routes stay without CORS.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};

const ORIGIN: &str = "https://admin.example.com";
const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

async fn admin_key(state: &riot_proxy::app::AppState) -> String {
    let c = consumers::create(
        &state.db,
        NewConsumer {
            name: "ops".into(),
            scopes: vec![Scope::Read, Scope::Admin],
            quota_per_min: 10_000,
            key: None,
        },
    )
    .await
    .unwrap();
    c.key.expose().to_string()
}

fn preflight(uri: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method(Method::OPTIONS)
        .uri(uri)
        .header("origin", origin)
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "authorization")
        .body(Body::empty())
        .unwrap()
}

fn get(uri: &str, origin: &str, key: Option<&str>) -> Request<Body> {
    let mut r = Request::get(uri).header("origin", origin);
    if let Some(k) = key {
        r = r.header("authorization", format!("Bearer {k}"));
    }
    r.body(Body::empty()).unwrap()
}

fn cors_headers(r: &common::Reply) -> Vec<String> {
    r.headers
        .keys()
        .map(|k| k.as_str().to_string())
        .filter(|k| k.starts_with("access-control-"))
        .collect()
}

fn header(r: &common::Reply, name: &str) -> Option<String> {
    r.headers.get(name).map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test]
async fn unset_sends_no_cors_header_anywhere() {
    let (_dir, state, router) = common::app();
    let key = admin_key(&state).await;
    for req in [
        preflight("/v1/admin/info", ORIGIN),
        get("/v1/admin/info", ORIGIN, Some(&key)),
        get("/v1/admin/info", ORIGIN, None),
        get("/readyz", ORIGIN, None),
        preflight("/v1/ws", ORIGIN),
    ] {
        let uri = req.uri().to_string();
        let r = common::send(router.clone(), req).await;
        assert_eq!(cors_headers(&r), Vec::<String>::new(), "{uri}");
    }
}

#[tokio::test]
async fn a_listed_origin_preflights_without_a_key_and_reads_with_one() {
    let (_dir, state, router) = common::app_with(
        &[("ADMIN_CORS_ORIGINS", &format!("{ORIGIN},http://localhost:5173"))],
        "http://127.0.0.1:9",
    );
    let key = admin_key(&state).await;

    for uri in ["/v1/admin/info", "/v1/admin/jobs/stats", "/v1/ws", "/readyz"] {
        let r = common::send(router.clone(), preflight(uri, ORIGIN)).await;
        assert!(r.status.is_success(), "{uri}: {}", r.status);
        assert_eq!(
            header(&r, "access-control-allow-origin").as_deref(),
            Some(ORIGIN),
            "{uri}"
        );
        let methods = header(&r, "access-control-allow-methods").unwrap();
        for m in ["GET", "POST", "DELETE"] {
            assert!(methods.contains(m), "{uri}: {methods}");
        }
        assert!(
            header(&r, "access-control-allow-headers")
                .unwrap()
                .to_lowercase()
                .contains("authorization"),
            "{uri}"
        );
        assert_eq!(header(&r, "access-control-max-age").as_deref(), Some("600"));
    }

    // The request itself still needs the key, and the page can read the 401.
    let denied = common::send(router.clone(), get("/v1/admin/info", ORIGIN, None)).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        header(&denied, "access-control-allow-origin").as_deref(),
        Some(ORIGIN)
    );
    let ok = common::send(router.clone(), get("/v1/admin/info", ORIGIN, Some(&key))).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(
        header(&ok, "access-control-allow-origin").as_deref(),
        Some(ORIGIN)
    );
    assert!(
        header(&ok, "access-control-expose-headers")
            .unwrap()
            .to_lowercase()
            .contains("x-request-id")
    );
    // The second listed origin works too.
    let other = common::send(router.clone(), get("/readyz", "http://localhost:5173", None)).await;
    assert_eq!(
        header(&other, "access-control-allow-origin").as_deref(),
        Some("http://localhost:5173")
    );
}

#[tokio::test]
async fn an_unlisted_origin_and_the_read_routes_get_no_cors() {
    let (_dir, state, router) = common::app_with(&[("ADMIN_CORS_ORIGINS", ORIGIN)], "http://127.0.0.1:9");
    let key = admin_key(&state).await;
    let evil = "https://evil.example";
    for req in [
        preflight("/v1/admin/info", evil),
        get("/v1/admin/info", evil, Some(&key)),
    ] {
        let r = common::send(router.clone(), req).await;
        assert_eq!(header(&r, "access-control-allow-origin"), None);
    }
    // /v1/lol, /v1/riot and the other read routes are untouched, listed origin or not.
    for uri in [
        format!("/v1/lol/summoners/by-puuid/kr/{PUUID}"),
        "/v1/riot/account/v1/accounts/by-riot-id/asia/a/b".to_string(),
    ] {
        let r = common::send(router.clone(), preflight(&uri, ORIGIN)).await;
        assert_eq!(cors_headers(&r), Vec::<String>::new(), "{uri}");
        let r = common::send(router.clone(), get(&uri, ORIGIN, Some(&key))).await;
        assert_eq!(cors_headers(&r), Vec::<String>::new(), "{uri}");
    }
}

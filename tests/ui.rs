//! `/dev` and `/dashboard` (plan P4-06, DEV-01): served without a key when
//! enabled, 404 when not, and their config documents.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;

fn app(env: &[(&str, &str)]) -> (tempfile::TempDir, axum::Router) {
    let (dir, _, router) = common::app_with(env, "http://127.0.0.1:9");
    (dir, router)
}

/// The dev explorer is served without a key (design/10).
#[tokio::test]
async fn dev_ui_is_served_without_a_key() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/dev").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = String::from_utf8(r.body).unwrap();
    assert_eq!(body, riot_proxy::routes::ui::DEV_UI_HTML);
}

/// One file, nothing fetched from elsewhere: no CDN, no external script.
#[test]
fn dev_ui_is_self_contained() {
    let html = riot_proxy::routes::ui::DEV_UI_HTML;
    for needle in ["http://", "https://", "<script src", "<link "] {
        assert!(!html.contains(needle), "dev-ui.html contains {needle:?}");
    }
    // It reads the two documents the router serves beside it.
    assert!(html.contains("/dev/config.json"));
    assert!(html.contains("/dev/openapi.json"));
}

/// The page keeps its state in the hash, so there are no client-side paths (ADR-071).
#[tokio::test]
async fn dev_ui_has_no_catch_all() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/dev/NinjaGoldfinch-OCENZ").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dev_config_publishes_what_the_page_needs() {
    let (_d, router) = app(&[("DOCS_UI", "false")]);
    let r = common::get(router, "/dev/config.json").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = r.json();
    assert_eq!(body["authDisabled"], false);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["env"], "development");
    assert_eq!(body["docsUi"], false);
    assert_eq!(body["dashboardUi"], true);
    assert!(
        body.get("defaultPlatform").is_none(),
        "no default platform (ADR-065)"
    );
    assert_eq!(
        body["regions"],
        serde_json::json!(["americas", "europe", "asia", "sea"])
    );
    let platforms = body["platforms"].as_array().unwrap();
    assert_eq!(platforms.len(), 16);
    assert!(platforms.contains(&serde_json::json!({"value": "oc1", "label": "Oceania", "region": "sea"})));
}

/// The explorer's forms come from the spec, so it is served even with `DOCS_UI=false`.
#[tokio::test]
async fn dev_openapi_is_the_published_document_even_without_docs_ui() {
    let (_d, router) = app(&[("DOCS_UI", "false")]);
    assert_eq!(
        common::get(router.clone(), "/openapi.json").await.status,
        StatusCode::NOT_FOUND
    );
    let r = common::get(router, "/dev/openapi.json").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "application/json");
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(
        r.json(),
        serde_json::to_value(riot_proxy::routes::docs::spec()).unwrap()
    );
}

/// The page is public; the API behind it is not.
#[tokio::test]
async fn the_api_behind_the_page_still_needs_a_key() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/v1/lol/status/euw1").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

/// ADR-071: never in production, not even with `DEV_UI=true`; off by flag elsewhere.
#[tokio::test]
async fn dev_ui_is_off_in_production_and_by_flag() {
    for env in [
        vec![("ENV", "production")],
        vec![("ENV", "production"), ("DEV_UI", "true")],
        vec![("DEV_UI", "false")],
    ] {
        let (_d, router) = app(&env);
        for p in ["/dev", "/dev/config.json", "/dev/openapi.json"] {
            assert_eq!(
                common::get(router.clone(), p).await.status,
                StatusCode::NOT_FOUND,
                "{env:?} {p}"
            );
        }
    }
}

#[tokio::test]
async fn dashboard_is_on_by_default_including_production() {
    let (_d, router) = app(&[("ENV", "production")]);
    let r = common::get(router.clone(), "/dashboard").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        String::from_utf8(r.body).unwrap(),
        riot_proxy::routes::ui::DASHBOARD_HTML
    );
    let cfg = common::get(router, "/dashboard/config.json").await;
    assert_eq!(cfg.json(), serde_json::json!({"authDisabled": false}));
}

#[tokio::test]
async fn dashboard_ui_false_removes_it() {
    let (_d, router) = app(&[("DASHBOARD_UI", "false")]);
    for p in ["/dashboard", "/dashboard/config.json"] {
        assert_eq!(
            common::get(router.clone(), p).await.status,
            StatusCode::NOT_FOUND,
            "{p}"
        );
    }
}

#[tokio::test]
async fn config_reports_auth_disabled() {
    let (_d, router) = app(&[("AUTH_DISABLED", "true")]);
    assert_eq!(
        common::get(router.clone(), "/dev/config.json").await.json()["authDisabled"],
        true
    );
    assert_eq!(
        common::get(router, "/dashboard/config.json").await.json()["authDisabled"],
        true
    );
}

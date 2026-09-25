//! `/dev` and `/dashboard` (plan P4-06): served without a key when enabled, 404
//! when not, v1's config documents.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;

fn app(env: &[(&str, &str)]) -> (tempfile::TempDir, axum::Router) {
    let (dir, _, router) = common::app_with(env, "http://127.0.0.1:9");
    (dir, router)
}

/// v1 dev UI: "serves the page without a key".
#[tokio::test]
async fn dev_ui_is_served_without_a_key() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/dev").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("riot-proxy"));
    assert_eq!(body, riot_proxy::routes::ui::DEV_UI_HTML, "v1's page, verbatim");
}

/// v1 dev UI: "serves the same document for a client-side profile route".
#[tokio::test]
async fn dev_ui_client_routes_get_the_same_document() {
    let (_d, router) = app(&[]);
    let root = common::get(router.clone(), "/dev").await;
    let deep = common::get(router, "/dev/NinjaGoldfinch-OCENZ?platform=oc1").await;
    assert_eq!(deep.status, StatusCode::OK);
    assert_eq!(deep.body, root.body);
}

/// v1 dev UI: "publishes the platform table the page builds its selector from".
#[tokio::test]
async fn dev_config_publishes_the_platform_table() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/dev/config.json").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = r.json();
    assert_eq!(body["authDisabled"], false);
    assert_eq!(body["defaultPlatform"], "euw1");
    assert_eq!(
        body["regions"],
        serde_json::json!(["americas", "europe", "asia", "sea"])
    );
    let platforms = body["platforms"].as_array().unwrap();
    assert_eq!(platforms.len(), 16);
    assert!(platforms.contains(&serde_json::json!({"value": "oc1", "label": "Oceania", "region": "sea"})));
}

/// v1 dev UI: "still requires a key for the data the page fetches".
#[tokio::test]
async fn the_api_behind_the_page_still_needs_a_key() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/v1/lol/status/euw1").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dev_ui_is_off_in_production_by_default_and_by_flag() {
    for env in [vec![("ENV", "production")], vec![("DEV_UI", "false")]] {
        let (_d, router) = app(&env);
        for p in ["/dev", "/dev/x", "/dev/config.json"] {
            assert_eq!(
                common::get(router.clone(), p).await.status,
                StatusCode::NOT_FOUND,
                "{env:?} {p}"
            );
        }
    }
    // Explicitly on in production (v1: an explicit DEV_UI wins).
    let (_d, router) = app(&[("ENV", "production"), ("DEV_UI", "true")]);
    assert_eq!(common::get(router, "/dev").await.status, StatusCode::OK);
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

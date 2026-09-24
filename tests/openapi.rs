//! The OpenAPI document and the docs routes (plan P4-03), and the compare script.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use riot_proxy::routes::docs;

#[test]
fn document_snapshot() {
    let doc: serde_json::Value = serde_json::from_str(&docs::spec().to_json().unwrap()).unwrap();
    insta::assert_json_snapshot!("openapi_document", doc);
}

#[test]
fn document_carries_v1_metadata() {
    let doc: serde_json::Value = serde_json::from_str(&docs::spec().to_json().unwrap()).unwrap();
    let v1: serde_json::Value =
        serde_json::from_str(include_str!("../docs/contract/v1-openapi.json")).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["title"], "riot-proxy");
    assert_eq!(doc["info"]["version"], env!("CARGO_PKG_VERSION"));
    let names = |d: &serde_json::Value, key: &str| -> Vec<String> {
        d[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&doc, "tags"), names(&v1, "tags"));
    assert_eq!(doc["x-tagGroups"], v1["x-tagGroups"]);
    assert_eq!(doc["security"], v1["security"]);
    for scheme in ["bearerAuth", "tokenQuery"] {
        assert_eq!(
            doc["components"]["securitySchemes"][scheme]["type"],
            v1["components"]["securitySchemes"][scheme]["type"]
        );
    }
    assert_eq!(
        doc["components"]["securitySchemes"]["tokenQuery"]["name"],
        "token"
    );
    assert_eq!(doc["servers"][1]["url"], v1["servers"][1]["url"]);
    // The ops routes are documented and public.
    for p in ["/healthz", "/readyz", "/metrics"] {
        assert_eq!(doc["paths"][p]["get"]["security"], serde_json::json!([{}]), "{p}");
        assert_eq!(doc["paths"][p]["get"]["tags"], serde_json::json!(["ops"]), "{p}");
    }
}

#[tokio::test]
async fn docs_routes_are_served_without_a_key() {
    let (_dir, _state, router) = common::app();
    let json = common::get(router.clone(), "/openapi.json").await;
    assert_eq!(json.status, StatusCode::OK);
    assert_eq!(json.headers["content-type"], "application/json");
    let served: serde_json::Value = json.json();
    let built: serde_json::Value = serde_json::from_str(&docs::spec().to_json().unwrap()).unwrap();
    assert_eq!(served, built, "/openapi.json is the same document `spec` prints");

    let yaml = common::get(router.clone(), "/openapi.yaml").await;
    assert_eq!(yaml.status, StatusCode::OK);
    assert!(
        String::from_utf8(yaml.body)
            .unwrap()
            .starts_with("openapi: 3.1.0")
    );

    let page = common::get(router, "/docs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(
        String::from_utf8(page.body)
            .unwrap()
            .contains("@scalar/api-reference")
    );
}

#[tokio::test]
async fn docs_ui_false_removes_all_three() {
    let (_dir, mut state, _) = common::app();
    state.config = common::config(&[("DOCS_UI", "false")]).into();
    let router = riot_proxy::app::router(state, riot_proxy::telemetry::metrics_handle().unwrap());
    for p in ["/openapi.json", "/openapi.yaml", "/docs"] {
        assert_eq!(
            common::get(router.clone(), p).await.status,
            StatusCode::NOT_FOUND,
            "{p}"
        );
    }
}

fn python() -> Option<&'static str> {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()
        .map(|_| "python3")
}

fn compare(old: &serde_json::Value, new: &serde_json::Value, prefixes: &[&str]) -> (i32, String) {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("old.json"), dir.path().join("new.json"));
    std::fs::write(&a, old.to_string()).unwrap();
    std::fs::write(&b, new.to_string()).unwrap();
    let mut cmd = std::process::Command::new(python().unwrap());
    cmd.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/compare-openapi.py"))
        .arg(&a)
        .arg(&b);
    for p in prefixes {
        cmd.args(["--prefix", p]);
    }
    let out = cmd.output().unwrap();
    (out.status.code().unwrap(), String::from_utf8(out.stdout).unwrap())
}

#[test]
fn compare_script_reports_missing_operations_by_method_and_path() {
    if python().is_none() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let old = serde_json::json!({"paths": {
        "/v1/riot/a/{x}": {"get": {}},
        "/v1/lol/b": {"get": {}, "post": {}},
        "/v1/admin/c": {"delete": {}},
    }});
    let new = serde_json::json!({"paths": {
        "/v1/riot/a/{x}": {"get": {}},
        "/v1/lol/b": {"get": {}},
        "/v1/lol/extra": {"get": {}},
    }});
    let (code, out) = compare(&old, &old, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("0 missing, 0 added"), "{out}");

    let (code, out) = compare(&old, &new, &["/v1/riot/", "/v1/lol/"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("missing  POST /v1/lol/b"), "{out}");
    assert!(out.contains("added    GET /v1/lol/extra"), "{out}");
    assert!(!out.contains("/v1/admin/c"), "prefix filter applies: {out}");
    assert!(out.contains("1 missing, 1 added"), "{out}");
}

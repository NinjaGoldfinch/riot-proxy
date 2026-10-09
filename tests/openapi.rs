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

/// SITE-04: every response declares the headers it sends, by reference to one
/// definition each, so generated clients can type them.
#[test]
fn responses_declare_the_headers_they_send() {
    let doc: serde_json::Value = serde_json::from_str(&docs::spec().to_json().unwrap()).unwrap();
    let defined = doc["components"]["headers"].as_object().unwrap();
    for name in [
        "X-Request-Id",
        "X-Cache",
        "X-Cache-Age",
        "X-Cache-Fetched-Age",
        "X-RateLimit-Limit",
        "X-RateLimit-Remaining",
        "X-RateLimit-Reset",
        "Retry-After",
    ] {
        assert!(defined[name]["description"].is_string(), "{name}");
    }
    let mut checked = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let tags: Vec<&str> = op["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap())
                .collect();
            // The image mirror is outside the keyed API: no quota, no cache tier.
            let images = path.starts_with("/ddragon/");
            let read = !images
                && tags
                    .iter()
                    .any(|t| ["players", "riot", "lol", "static"].contains(t));
            for (status, res) in op["responses"].as_object().unwrap() {
                let at = format!("{method} {path} {status}");
                let has = |h: &str| res["headers"].get(h).is_some();
                assert!(has("X-Request-Id"), "{at}");
                for (h, r) in res["headers"].as_object().unwrap() {
                    assert_eq!(r["$ref"], format!("#/components/headers/{h}"), "{at}");
                }
                if matches!(status.as_str(), "429" | "503") {
                    assert!(has("Retry-After"), "{at}");
                }
                if read && status.as_str() != "401" && status.as_str() != "403" {
                    assert!(has("X-RateLimit-Remaining"), "{at}");
                }
                if images {
                    assert!(!has("X-RateLimit-Remaining") && !has("X-Cache"), "{at}");
                    if status == "200" {
                        assert!(has("Cache-Control") && has("Last-Modified"), "{at}");
                    }
                }
                if read && status.starts_with('2') && !path.starts_with("/v1/lol/analytics") {
                    assert!(has("X-Cache") && has("X-Cache-Age"), "{at}");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 20, "only {checked} cached responses found");
    let pool = &doc["paths"]["/v1/players/{puuid}/champions"]["get"]["responses"]["200"]["headers"];
    assert!(
        pool.get("X-Cache-Fetched-Age").is_none(),
        "the pool is never read from Riot"
    );
}

/// Whether `v` fits `schema`: required fields present, property types right,
/// `$ref`s and arrays followed. Extra properties are allowed, as Riot adds them.
fn conforms(doc: &serde_json::Value, schema: &serde_json::Value, v: &serde_json::Value, at: &str) {
    if let Some(r) = schema["$ref"].as_str() {
        let name = r.trim_start_matches("#/components/schemas/");
        return conforms(doc, &doc["components"]["schemas"][name], v, at);
    }
    let types: Vec<&str> = match &schema["type"] {
        serde_json::Value::String(t) => vec![t.as_str()],
        serde_json::Value::Array(ts) => ts.iter().filter_map(|t| t.as_str()).collect(),
        _ => vec![],
    };
    let fits = |t: &str| match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        _ => true,
    };
    assert!(
        types.is_empty() || types.iter().any(|t| fits(t)),
        "{at}: {v} is not {types:?}"
    );
    if let Some(items) = v.as_array() {
        for (i, item) in items.iter().enumerate() {
            conforms(doc, &schema["items"], item, &format!("{at}[{i}]"));
        }
    }
    if let Some(obj) = v.as_object() {
        for req in schema["required"].as_array().into_iter().flatten() {
            assert!(obj.contains_key(req.as_str().unwrap()), "{at}: missing {req}");
        }
        for (k, val) in obj {
            if let Some(prop) = schema["properties"].get(k) {
                conforms(doc, prop, val, &format!("{at}.{k}"));
            }
        }
    }
}

/// SITE-05: the recorded Riot bodies fit the schemas the document gives them,
/// on the passthrough routes and as the profile's parts.
#[test]
fn recorded_riot_bodies_fit_their_schemas() {
    let doc: serde_json::Value = serde_json::from_str(&docs::spec().to_json().unwrap()).unwrap();
    let fixture = |name: &str| -> serde_json::Value {
        let path = format!(
            "{}/tests/fixtures/replay/cold-lookup/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };
    let ok = |path: &str| {
        doc["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"].clone()
    };
    for (route, part, body) in [
        (
            "/v1/riot/accounts/by-puuid/{puuid}",
            "account",
            "01-account.byRiotId.body",
        ),
        (
            "/v1/lol/summoners/by-puuid/{platform}/{puuid}",
            "summoner",
            "02-summoner.byPuuid.body",
        ),
        (
            "/v1/lol/league/entries/by-puuid/{platform}/{puuid}",
            "league",
            "03-league.entriesByPuuid.body",
        ),
        (
            "/v1/lol/mastery/by-puuid/{platform}/{puuid}",
            "mastery",
            "04-mastery.topByPuuid.body",
        ),
    ] {
        let body = fixture(body);
        let schema = ok(route);
        assert!(!schema.is_null(), "{route} has no body schema");
        conforms(&doc, &schema, &body, route);
        conforms(
            &doc,
            &doc["components"]["schemas"]["ProfileBody"]["properties"][part],
            &body,
            part,
        );
    }
    // Every other account and league route shares a schema with one checked above.
    assert_eq!(
        ok("/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}")["$ref"],
        "#/components/schemas/AccountDto"
    );
    assert_eq!(
        ok("/v1/lol/league/entries/{platform}/{queue}/{tier}/{division}")["items"]["$ref"],
        "#/components/schemas/LeagueEntryDTO"
    );
}

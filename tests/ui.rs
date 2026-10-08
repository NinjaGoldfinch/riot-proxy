//! `/dev`, `/dev/showcase` and `/dashboard` (plan P4-06, DEV-01, DEV-06): served
//! without a key when enabled, 404 when not, and their config documents.
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
    let config = common::config(&[]);
    assert_eq!(
        body,
        riot_proxy::routes::ui::render(riot_proxy::routes::ui::DEV_UI_HTML, &config, "/dev")
    );
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

/// Runs `node --test <file>` from the crate root. Needs `node`, which GitHub's
/// runners have; a local run without it skips, CI fails.
fn node_test(file: &str) {
    let dir = env!("CARGO_MANIFEST_DIR");
    let out = match std::process::Command::new("node")
        .args(["--test", file])
        .current_dir(dir)
        .output()
    {
        Ok(out) => out,
        Err(e) if std::env::var_os("CI").is_none() => {
            eprintln!("skipping: node not runnable ({e})");
            return;
        }
        Err(e) => panic!("node is needed in CI: {e}"),
    };
    assert!(
        out.status.success(),
        "node --test {file} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The page's pure helpers (paging, summary, scoreboard, walk status) pass their
/// unit tests in `tests/dev_ui.mjs` (DEV-02).
#[test]
fn dev_ui_helpers_pass_their_node_tests() {
    node_test("tests/dev_ui.mjs");
}

/// Every response window can be closed, and the match list pages 10, 25 or 50 (DEV-02).
#[test]
fn dev_ui_windows_close_and_matches_page() {
    let html = riot_proxy::routes::ui::DEV_UI_HTML;
    assert!(
        html.contains("data-close title=\"close (Esc)\""),
        "the viewer has a close button"
    );
    assert!(html.contains("const PAGE_SIZES = [10, 25, 50];"));
    assert!(
        html.contains("data-close-match"),
        "an open scoreboard has a close button"
    );
    // Queue names come from Riot's queues.json through the proxy, not a list in the page.
    assert!(html.contains("'/v1/static/queues'"));
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
        for p in ["/dev", "/dev/showcase", "/dev/config.json", "/dev/openapi.json"] {
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
    let body = String::from_utf8(r.body).unwrap();
    assert!(
        body.contains("<div class=\"wrap\">"),
        "the dashboard itself is served"
    );
    // In production the bar never offers the dev explorer, which does not exist there.
    assert!(body.contains(r#"<a href="/dashboard" aria-current="page">Dashboard</a>"#));
    assert!(!body.contains(r#"href="/dev""#));
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

fn bar(html: &str) -> &str {
    let start = html.find(r#"<div class="rp-bar""#).expect("page bar");
    &html[start..start + html[start..].find("</div>").unwrap()]
}

/// Both pages carry one bar linking every page this config serves, the
/// current one marked, and no leftover marker (DEV-10).
#[tokio::test]
async fn pages_share_a_bar_with_the_pages_that_exist() {
    let (_d, router) = app(&[("DOCS_UI", "true")]);
    for (path, here) in [
        ("/dev", "Dev explorer"),
        ("/dev/showcase", "Showcase"),
        ("/dashboard", "Dashboard"),
    ] {
        let html = String::from_utf8(common::get(router.clone(), path).await.body).unwrap();
        assert!(
            !html.contains(riot_proxy::routes::ui::PAGEBAR_MARK),
            "{path}: marker replaced"
        );
        assert_eq!(html.matches(r#"class="rp-bar""#).count(), 1, "{path}: one bar");
        let bar = bar(&html);
        let hrefs: Vec<&str> = bar
            .split(r#"href=""#)
            .skip(1)
            .map(|s| &s[..s.find('"').unwrap()])
            .collect();
        assert_eq!(
            hrefs,
            ["/dashboard", "/dev", "/dev/showcase", "/docs", "/metrics"],
            "{path}"
        );
        assert_eq!(bar.matches("aria-current").count(), 1, "{path}");
        assert!(
            bar.contains(&format!(r#"aria-current="page">{here}</a>"#)),
            "{path}"
        );
        assert!(bar.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
    }
}

#[tokio::test]
async fn the_bar_leaves_out_pages_that_are_off() {
    let (_d, router) = app(&[("DOCS_UI", "false"), ("DASHBOARD_UI", "false")]);
    let html = String::from_utf8(common::get(router, "/dev").await.body).unwrap();
    let bar = bar(&html);
    assert!(!bar.contains("/dashboard") && !bar.contains("/docs"), "{bar}");
    assert!(bar.contains(r#"href="/metrics""#));
}

/// Every page the bar links to is really served.
#[tokio::test]
async fn every_bar_link_resolves() {
    let (_d, router) = app(&[("DOCS_UI", "true")]);
    let config = common::config(&[("DOCS_UI", "true")]);
    for (href, _) in riot_proxy::routes::ui::pages(&config) {
        assert_eq!(
            common::get(router.clone(), href).await.status,
            StatusCode::OK,
            "{href}"
        );
    }
}

// ---------- the showcase (DEV-06, design/11)

#[tokio::test]
async fn showcase_is_served_without_a_key() {
    let (_d, router) = app(&[]);
    let r = common::get(router, "/dev/showcase").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = String::from_utf8(r.body).unwrap();
    let config = common::config(&[]);
    assert_eq!(
        body,
        riot_proxy::routes::ui::render(riot_proxy::routes::ui::SHOWCASE_HTML, &config, "/dev/showcase")
    );
}

/// One file; images come from the local Data Dragon mirror (ADR-076), never Riot's CDN.
#[test]
fn showcase_is_self_contained() {
    let html = riot_proxy::routes::ui::SHOWCASE_HTML;
    for needle in ["http://", "https://", "<script src", "<link "] {
        assert!(!html.contains(needle), "showcase.html contains {needle:?}");
    }
    assert!(html.contains("/dev/config.json"));
    assert!(html.contains("/ddragon/"));
}

#[test]
fn showcase_helpers_pass_their_node_tests() {
    node_test("tests/showcase.mjs");
}

/// The page's coverage block: `{showcased: {op: where}, notShowcased: {op: why}}`.
fn coverage() -> (
    serde_json::Map<String, serde_json::Value>,
    serde_json::Map<String, serde_json::Value>,
) {
    let html = riot_proxy::routes::ui::SHOWCASE_HTML;
    let open = r#"<script type="application/json" id="coverage">"#;
    let start = html.find(open).expect("coverage block") + open.len();
    let end = start + html[start..].find("</script>").unwrap();
    let doc: serde_json::Value = serde_json::from_str(&html[start..end]).expect("coverage is JSON");
    let list = |k: &str| {
        doc[k]
            .as_object()
            .unwrap_or_else(|| panic!("{k} is an object"))
            .clone()
    };
    (list("showcased"), list("notShowcased"))
}

/// The "change it when the proxy changes" rule (design/11): every `GET` read
/// route a consumer frontend could call is shown on the page or left out with a
/// reason, and neither list names a route that no longer exists.
#[test]
fn showcase_covers_every_read_route() {
    const READ_TAGS: [&str; 4] = ["players", "riot", "lol", "static"];
    let spec = serde_json::to_value(riot_proxy::routes::docs::spec()).unwrap();
    let mut read = std::collections::BTreeSet::new();
    let mut all_gets = std::collections::BTreeSet::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        let Some(op) = item.get("get") else { continue };
        let id = format!("GET {path}");
        all_gets.insert(id.clone());
        let tags: Vec<&str> = op["tags"]
            .as_array()
            .map(|t| t.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        if tags.iter().any(|t| READ_TAGS.contains(t)) {
            read.insert(id);
        }
    }
    let (shown, left_out) = coverage();
    let mut problems = vec![];
    for id in &read {
        match (shown.contains_key(id), left_out.contains_key(id)) {
            (false, false) => problems.push(format!(
                "{id} is a read route the showcase neither uses nor lists in notShowcased (src/ui/showcase.html)"
            )),
            (true, true) => problems.push(format!("{id} is in both showcased and notShowcased")),
            _ => {}
        }
    }
    for id in shown.keys().chain(left_out.keys()) {
        if !all_gets.contains(id) {
            problems.push(format!("{id} is listed in the showcase but is not in the spec"));
        } else if !read.contains(id) {
            problems.push(format!("{id} is not a read route (tags {READ_TAGS:?})"));
        }
    }
    for (id, why) in &left_out {
        if why.as_str().is_none_or(|w| w.trim().is_empty()) {
            problems.push(format!("{id} is left out without a reason"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A route marked as shown is really called by the page: its path up to the
/// first parameter appears in the script.
#[test]
fn showcased_routes_are_called_by_the_page() {
    let html = riot_proxy::routes::ui::SHOWCASE_HTML;
    let script = &html[html.find(r#"<script type="module">"#).unwrap()..];
    let (shown, _) = coverage();
    for id in shown.keys() {
        let path = id.trim_start_matches("GET ");
        let stem = &path[..path.find('{').unwrap_or(path.len())];
        assert!(
            script.contains(&format!("`{stem}")) || script.contains(&format!("'{stem}")),
            "{id}: the page never calls {stem}"
        );
    }
}

/// A card's chip names the operation it used and opens it in the explorer.
#[test]
fn showcase_chips_link_to_the_explorer() {
    let html = riot_proxy::routes::ui::SHOWCASE_HTML;
    assert!(html.contains("/dev#explorer?op="));
}

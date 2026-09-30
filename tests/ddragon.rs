//! Data Dragon (plan P7-01): `ddragon:sync` against a wiremock CDN (ported from
//! v1 `test/ddragon-mirror.test.ts`), the `/v1/static/*` routes, the raw files
//! at `/ddragon/*`, and `POST /v1/admin/ddragon/sync`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::jobs::JobError;
use riot_proxy::jobs::ddragon::{self, DdragonSync, SyncResult};
use riot_proxy::r#static::DATA_FILES;
use riot_proxy::ws::Topic;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LOCALE: &str = "en_US";
const OLD: &str = "16.17.1";
const NEW: &str = "16.18.1";
const VERSIONS: &str = "/api/versions.json";
const QUEUES: &str = "/docs/lol/queues.json";

/// A cut of Riot's real queues.json.
fn queue_table() -> Value {
    json!([
        {"queueId": 420, "map": "Summoner's Rift", "description": "5v5 Ranked Solo games"},
        {"queueId": 450, "map": "Howling Abyss", "description": "ARAM games"}
    ])
}

fn data_path(version: &str, file: &str) -> String {
    format!("/cdn/{version}/data/{LOCALE}/{file}.json")
}

/// What the mock serves for a data file: enough to tell files and patches apart.
fn data_body(version: &str, file: &str) -> Value {
    if file == "champion" {
        return json!({"type": "champion", "version": version, "data": {
            "Ahri": {"key": "103", "name": "Ahri"}, "Garen": {"key": "86", "name": "Garen"}}});
    }
    json!({"file": file, "version": version})
}

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    reader: String,
    admin: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let mint = |name: &str, scopes| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 10_000,
        key: None,
    };
    let reader = consumers::create(&state.db, mint("web", vec![Scope::Read]))
        .await
        .unwrap();
    let admin = consumers::create(&state.db, mint("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap();
    Env {
        _dir: dir,
        server,
        state,
        router,
        reader: reader.key.expose().to_string(),
        admin: admin.key.expose().to_string(),
    }
}

impl Env {
    /// Riot publishes `version` (newest) over `OLD`, with `files` of it.
    async fn publish(&self, version: &str, files: &[&str]) {
        Mock::given(method("GET"))
            .and(path(VERSIONS))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([version, OLD])))
            .mount(&self.server)
            .await;
        for file in files {
            Mock::given(method("GET"))
                .and(path(data_path(version, file)))
                .respond_with(ResponseTemplate::new(200).set_body_json(data_body(version, file)))
                .mount(&self.server)
                .await;
        }
    }

    async fn queues(&self) {
        Mock::given(method("GET"))
            .and(path(QUEUES))
            .respond_with(ResponseTemplate::new(200).set_body_json(queue_table()))
            .mount(&self.server)
            .await;
    }

    async fn requested(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect()
    }

    async fn sync(&self, force: bool) -> SyncResult {
        ddragon::sync(&self.state.ddragon, force).await.unwrap()
    }

    fn on_disk(&self, version: &str, file: &str) -> Option<Value> {
        let p = self
            .state
            .ddragon
            .dir()
            .join(version)
            .join(format!("{file}.json"));
        std::fs::read(p).ok().map(|b| serde_json::from_slice(&b).unwrap())
    }

    async fn call(&self, verb: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> common::Reply {
        let mut req = Request::builder().method(verb).uri(uri);
        if let Some(k) = key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        let req = req
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn get(&self, uri: &str) -> common::Reply {
        self.call("GET", uri, Some(&self.reader), None).await
    }
}

fn all_files() -> Vec<String> {
    DATA_FILES.iter().map(|f| (*f).to_string()).collect()
}

fn job(payload: Value) -> riot_proxy::jobs::Job {
    riot_proxy::jobs::Job {
        id: "01J9ZZZZZZZZZZZZZZZZZZZZZZ".into(),
        kind: "ddragon:sync".into(),
        dedupe_key: None,
        priority: 30_000,
        payload: payload.to_string(),
        attempts: 1,
        run_after: 0,
    }
}

fn error(r: &common::Reply) -> (StatusCode, String) {
    (
        r.status,
        r.json()["error"]["message"].as_str().unwrap().to_string(),
    )
}

// ── The sync ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_new_patch_is_mirrored_once_and_the_next_run_is_a_no_op() {
    let e = env().await;
    e.publish(NEW, &DATA_FILES).await;
    e.queues().await;

    let first = e.sync(false).await;
    assert_eq!(
        first,
        SyncResult {
            version: NEW.into(),
            changed: true,
            files: all_files(),
            meta: vec!["queues".into()],
        }
    );
    for file in DATA_FILES {
        assert_eq!(e.on_disk(NEW, file), Some(data_body(NEW, file)), "{file}");
    }
    // `/v1/static/versions` serves this, so it is part of the patch.
    assert_eq!(e.on_disk(NEW, "versions"), Some(json!([NEW, OLD])));
    assert_eq!(e.on_disk("meta", "queues"), Some(queue_table()));
    assert_eq!(e.state.ddragon.current_version().await.as_deref(), Some(NEW));
    let fetched = e.requested().await.len();
    assert_eq!(fetched, 1 + 1 + DATA_FILES.len(), "versions, queues, six files");

    // The tick that finds the patch mirrored: the version list and the queue
    // table (it has no version, so it is refreshed anyway), nothing else.
    let second = e.sync(false).await;
    assert_eq!(
        second,
        SyncResult {
            version: NEW.into(),
            changed: false,
            files: vec![],
            meta: vec!["queues".into()],
        }
    );
    assert_eq!(e.requested().await[fetched..], [VERSIONS, QUEUES]);

    // `force` downloads it again.
    let forced = e.sync(true).await;
    assert!(forced.changed);
    assert!(e.requested().await[fetched + 2..].contains(&data_path(NEW, "champion")));
}

#[tokio::test]
async fn the_job_publishes_patch_new_only_when_a_patch_lands() {
    let e = env().await;
    e.publish(NEW, &DATA_FILES).await;
    e.queues().await;
    let mut patch = e.state.hub.subscribe(&Topic::named("patch"));
    let handler = DdragonSync {
        mirror: e.state.ddragon.clone(),
        hub: e.state.hub.clone(),
    };

    handler.run(&job(json!({}))).await.unwrap();
    let frame: Value = serde_json::from_str(patch.try_recv().unwrap().as_str()).unwrap();
    assert_eq!(
        (&frame["op"], &frame["event"], &frame["topic"], &frame["data"]),
        (
            &json!("event"),
            &json!("patch.new"),
            &json!("patch"),
            &json!({"version": NEW})
        )
    );

    handler.run(&job(json!({}))).await.unwrap();
    assert!(patch.try_recv().is_err(), "no new patch, no event");
    handler.run(&job(json!({"force": true}))).await.unwrap();
    assert!(
        patch.try_recv().is_ok(),
        "a forced re-download announces again (v1)"
    );
}

#[tokio::test]
async fn a_file_riot_does_not_publish_is_skipped_not_fatal() {
    let e = env().await;
    // Riot 403s (here: 404s) files it has dropped; the patch still lands.
    e.publish(NEW, &["champion", "item"]).await;
    e.queues().await;
    let r = e.sync(false).await;
    assert_eq!(
        (r.changed, r.files),
        (true, vec!["champion".into(), "item".into()])
    );
    assert_eq!(e.state.ddragon.current_version().await.as_deref(), Some(NEW));
    assert!(e.state.ddragon.read("map", Some(NEW)).await.is_none());
}

#[tokio::test]
async fn the_queue_table_survives_riot_not_answering() {
    let e = env().await;
    e.publish(NEW, &DATA_FILES).await;
    let queues = Mock::given(method("GET"))
        .and(path(QUEUES))
        .respond_with(ResponseTemplate::new(200).set_body_json(queue_table()))
        .mount_as_scoped(&e.server)
        .await;
    e.sync(false).await;
    drop(queues);

    // The queue table fails; the patch data does not care, and the old copy stays.
    let r = e.sync(true).await;
    assert_eq!((r.meta, r.files), (vec![], all_files()));
    assert_eq!(e.on_disk("meta", "queues"), Some(queue_table()));
}

#[tokio::test]
async fn a_sync_that_died_half_way_is_finished_by_the_next() {
    let e = env().await;
    // champion.json written, versions.json never: not mirrored.
    let dir = e.state.ddragon.dir().join(NEW);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("champion.json"), "{}").unwrap();
    assert_eq!(e.state.ddragon.current_version().await, None);

    e.publish(NEW, &DATA_FILES).await;
    e.queues().await;
    assert!(e.sync(false).await.changed);
    assert_eq!(e.on_disk(NEW, "champion"), Some(data_body(NEW, "champion")));
}

#[tokio::test]
async fn an_unreachable_version_list_is_retried() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path(VERSIONS))
        .respond_with(ResponseTemplate::new(503))
        .mount(&e.server)
        .await;
    let handler = DdragonSync {
        mirror: e.state.ddragon.clone(),
        hub: e.state.hub.clone(),
    };
    match handler.run(&job(json!({}))).await {
        Err(JobError::Retry(m)) => assert!(m.contains("503"), "{m}"),
        other => panic!("expected a retry, got {other:?}"),
    }
    assert!(matches!(
        handler.run(&job(json!({"force": "yes"}))).await,
        Err(JobError::Fail(_))
    ));
}

// ── /v1/static/* ────────────────────────────────────────────────────────────

#[tokio::test]
async fn before_the_first_sync_versions_are_fetched_live_and_files_are_404() {
    let e = env().await;
    e.publish(NEW, &[]).await;
    let r = e.get("/v1/static/versions").await;
    assert_eq!(
        (r.status, r.headers["x-cache"].to_str().unwrap()),
        (StatusCode::OK, "MISS")
    );
    assert_eq!(r.json(), json!({"current": null, "versions": [NEW, OLD]}));

    assert_eq!(
        error(&e.get("/v1/static/champions").await),
        (
            StatusCode::NOT_FOUND,
            "Static data 'champions' has not been synced yet. Run the ddragon:sync job.".into()
        )
    );
    assert_eq!(
        error(&e.get("/v1/static/queues").await),
        (
            StatusCode::NOT_FOUND,
            "The queue table has not been synced yet. Run the ddragon:sync job.".into()
        )
    );
}

#[tokio::test]
async fn after_a_sync_the_mirror_answers_every_static_route() {
    let e = env().await;
    e.publish(NEW, &DATA_FILES).await;
    e.queues().await;
    e.sync(false).await;
    let before = e.requested().await.len();

    let versions = e.get("/v1/static/versions").await;
    assert_eq!(
        (
            versions.headers["x-cache"].to_str().unwrap(),
            versions.headers["x-cache-age"].to_str().unwrap()
        ),
        ("HIT", "0")
    );
    assert_eq!(versions.json(), json!({"current": NEW, "versions": [NEW, OLD]}));
    assert_eq!(e.get("/v1/static/queues").await.json(), queue_table());
    for (name, file) in [
        ("champion", "champion"),
        ("champions", "champion"),
        ("runes", "runesReforged"),
        ("summoner-spells", "summoner"),
        ("profile-icons", "profileicon"),
    ] {
        let r = e.get(&format!("/v1/static/{name}")).await;
        assert_eq!(r.status, StatusCode::OK, "{name}");
        assert_eq!(r.json(), data_body(NEW, file), "{name}");
    }
    assert_eq!(
        e.get(&format!("/v1/static/items?version={NEW}")).await.json(),
        data_body(NEW, "item")
    );
    assert_eq!(
        error(&e.get(&format!("/v1/static/items?version={OLD}")).await).0,
        StatusCode::NOT_FOUND,
        "a patch that was never mirrored"
    );
    assert_eq!(e.requested().await.len(), before, "never calls Riot");
}

#[tokio::test]
async fn static_routes_validate_as_v1_did() {
    let e = env().await;
    for (uri, message) in [
        (
            "/v1/static/queue",
            "params/file must be equal to one of the allowed values",
        ),
        (
            "/v1/static/champions?version=..",
            "querystring/version must match pattern \"^[0-9]+(\\.[0-9]+)*$\"",
        ),
        (
            "/v1/static/champions?version=16.17.1%2F..",
            "querystring/version must match pattern \"^[0-9]+(\\.[0-9]+)*$\"",
        ),
        (
            "/v1/static/champions?version=1.2.3.4.5.6.7.8.9.10.11",
            "querystring/version must NOT have more than 20 characters",
        ),
    ] {
        assert_eq!(
            error(&e.get(uri).await),
            (StatusCode::BAD_REQUEST, message.into()),
            "{uri}"
        );
    }
    for uri in ["/v1/static/versions", "/v1/static/queues", "/v1/static/champions"] {
        assert_eq!(
            e.call("GET", uri, None, None).await.status,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn the_static_routes_are_documented_under_the_static_tag() {
    let doc = serde_json::to_value(riot_proxy::routes::docs::spec()).unwrap();
    for p in ["/v1/static/versions", "/v1/static/queues", "/v1/static/{file}"] {
        assert_eq!(doc["paths"][p]["get"]["tags"], json!(["static"]), "{p}");
    }
    let params = doc["paths"]["/v1/static/{file}"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let version = params.iter().find(|p| p["name"] == "version").unwrap();
    assert_eq!(
        (&version["schema"]["pattern"], &version["schema"]["maxLength"]),
        (&json!("^[0-9]+(\\.[0-9]+)*$"), &json!(20))
    );
    assert!(
        doc["paths"]["/v1/admin/ddragon/sync"]["post"].is_object(),
        "the admin route too"
    );
}

// ── /ddragon/* ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_files_are_served_immutable_without_a_key() {
    let e = env().await;
    e.publish(NEW, &DATA_FILES).await;
    e.queues().await;
    e.sync(false).await;

    let r = e
        .call("GET", &format!("/ddragon/{NEW}/champion.json"), None, None)
        .await;
    let headers = |r: &common::Reply| {
        let h = |n: &str| r.headers.get(n).map(|v| v.to_str().unwrap().to_string());
        json!({
            "status": r.status.as_u16(),
            "content-type": h("content-type"),
            "cache-control": h("cache-control"),
        })
    };
    insta::assert_json_snapshot!("ddragon_file_headers", headers(&r));
    assert_eq!(
        serde_json::from_slice::<Value>(&r.body).unwrap(),
        data_body(NEW, "champion")
    );
    // A patch not synced yet must not be cached for a week.
    let missing = e.call("GET", "/ddragon/99.1.1/champion.json", None, None).await;
    insta::assert_json_snapshot!("ddragon_missing_headers", headers(&missing));
    // ServeDir resolves inside the mirror only.
    let out = e.call("GET", "/ddragon/../riot-proxy.db", None, None).await;
    assert_ne!(out.status, StatusCode::OK);
}

// ── POST /v1/admin/ddragon/sync ─────────────────────────────────────────────

#[tokio::test]
async fn the_admin_route_queues_a_sync() {
    let e = env().await;
    let post = |key: String, body: Option<Value>| {
        let e = &e;
        async move { e.call("POST", "/v1/admin/ddragon/sync", Some(&key), body).await }
    };
    assert_eq!(post(e.reader.clone(), None).await.status, StatusCode::FORBIDDEN);

    let plain = post(e.admin.clone(), None).await.json();
    assert_eq!(plain["ok"], json!(true));
    let again = post(e.admin.clone(), Some(json!({"force": false}))).await.json();
    assert_eq!(again["jobId"], plain["jobId"], "joins the queued sync");
    let forced = post(e.admin.clone(), Some(json!({"force": true}))).await.json();
    assert_ne!(forced["jobId"], plain["jobId"]);

    let row = e
        .state
        .jobs
        .get(forced["jobId"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.kind.as_str(), row.priority), ("ddragon:sync", 30_000));
    assert_eq!(
        serde_json::from_str::<Value>(&row.payload).unwrap(),
        json!({"force": true})
    );
    assert_eq!(
        error(&post(e.admin.clone(), Some(json!({"force": {}}))).await),
        (StatusCode::BAD_REQUEST, "body/force must be boolean".into())
    );
}

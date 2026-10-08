//! `/dev/reset` (DEV-05, design/10 §Reset tab, ADR-075): admin only, mounted only
//! with the dev explorer, needs `{"confirm":"reset"}`, empties every fetched
//! table and the L1 cache, and keeps consumers and the limiter checkpoint.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use serde_json::{Value, json};

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    router: axum::Router,
    admin: String,
    reader: String,
}

async fn env(extra: &[(&str, &str)]) -> Env {
    let (dir, state, router) = common::app_with(extra, "http://127.0.0.1:9");
    let mint = |name: &str, scopes: Vec<Scope>| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 10_000,
        key: None,
    };
    let key = |c: consumers::Created| c.key.expose().to_string();
    let admin = key(
        consumers::create(&state.db, mint("ops", vec![Scope::Read, Scope::Admin]))
            .await
            .unwrap(),
    );
    let reader = key(consumers::create(&state.db, mint("web", vec![Scope::Read]))
        .await
        .unwrap());
    Env {
        _dir: dir,
        state,
        router,
        admin,
        reader,
    }
}

impl Env {
    async fn call(&self, verb: &str, key: Option<&str>, body: Option<Value>) -> common::Reply {
        let mut req = Request::builder().method(verb).uri("/dev/reset");
        if let Some(k) = key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(serde_json::to_vec(&b).unwrap())
            }
            None => Body::empty(),
        };
        common::send(self.router.clone(), req.body(body).unwrap()).await
    }

    /// One row in each kind of fetched table, a limiter checkpoint, and an L1 entry.
    async fn seed(&self) {
        self.state
            .db
            .write(|c| {
                c.execute_batch(
                    "INSERT INTO players (key_scope, puuid, platform, tracked, updated_at)
                       VALUES ('k', 'p1', 'oc1', 1, 1), ('k', 'p2', 'oc1', 0, 1);
                     INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                       VALUES ('OC1_1', 'sea', '16.20', 420, 1, x'00', 1, 1);
                     INSERT INTO timelines (match_id, body_zstd) VALUES ('OC1_1', x'00');
                     INSERT INTO cache (key, body, status, content_at, soft_expires, hard_expires)
                       VALUES ('k:summoner.byPuuid:oc1:p1', x'00', 200, 1, 9e15, 9e15);
                     INSERT INTO jobs (id, kind, priority, payload, state, run_after)
                       VALUES ('j1', 'archive:match', 100, '{}', 'running', 1),
                              ('j2', 'backfill:player', 20000, '{}', 'pending', 1),
                              ('j3', 'maintenance', 30000, '{}', 'done', 1);
                     INSERT INTO metrics_history (at, point) VALUES (1, '{}');
                     INSERT INTO limiter_state (scope, windows, updated_at) VALUES ('app:oc1', '[]', 1);",
                )?;
                Ok::<_, riot_proxy::db::DbError>(())
            })
            .await
            .unwrap();
        self.state
            .fetcher
            .cache()
            .l1
            .put_negative("k:account.byRiotId:asia:x:y", Duration::from_secs(60))
            .await;
    }

    async fn rows(&self, table: &'static str) -> i64 {
        self.state
            .db
            .read(move |c| {
                Ok::<_, riot_proxy::db::DbError>(c.query_row(
                    &format!("SELECT count(*) FROM {table}"),
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    }
}

fn rows_of(body: &Value, table: &str) -> i64 {
    body["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == table)
        .unwrap_or_else(|| panic!("{table} missing from {body}"))["rows"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn needs_an_admin_key() {
    let e = env(&[]).await;
    let none = e.call("GET", None, None).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(none.json()["error"]["code"], "UNAUTHORIZED");
    let reader = e
        .call("POST", Some(&e.reader), Some(json!({"confirm": "reset"})))
        .await;
    assert_eq!(reader.status, StatusCode::FORBIDDEN);
    assert_eq!(reader.json()["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn preview_counts_what_would_go() {
    let e = env(&[]).await;
    e.seed().await;
    let r = e.call("GET", Some(&e.admin), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "no-store");
    let body = r.json();
    assert_eq!(rows_of(&body, "players"), 2);
    assert_eq!(rows_of(&body, "matches"), 1);
    assert_eq!(rows_of(&body, "jobs"), 3);
    assert_eq!(body["runningJobs"], 1);
    assert_eq!(body["l1Entries"], 1);
    assert_eq!(
        body["kept"],
        json!(["consumers", "limiter_state", "refinery_schema_history"])
    );
    // A preview deletes nothing.
    assert_eq!(e.rows("players").await, 2);
}

#[tokio::test]
async fn post_needs_the_confirm_word() {
    let e = env(&[]).await;
    e.seed().await;
    for body in [None, Some(json!({})), Some(json!({"confirm": "yes"}))] {
        let r = e.call("POST", Some(&e.admin), body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "body {body:?}");
        assert_eq!(r.json()["error"]["code"], "VALIDATION");
    }
    assert_eq!(e.rows("matches").await, 1, "a refused reset deletes nothing");
}

#[tokio::test]
async fn reset_wipes_fetched_data_and_keeps_keys_and_limits() {
    let e = env(&[]).await;
    e.seed().await;
    let r = e
        .call("POST", Some(&e.admin), Some(json!({"confirm": "reset"})))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", String::from_utf8_lossy(&r.body));
    let body = r.json();
    assert_eq!(body["ok"], true);
    assert_eq!(rows_of(&body, "players"), 2);
    assert_eq!(rows_of(&body, "timelines"), 1);
    assert_eq!(rows_of(&body, "jobs"), 3);
    assert_eq!(body["runningJobs"], 1);
    assert_eq!(body["l1Entries"], 1);

    for table in riot_proxy::routes::dev::WIPED {
        assert_eq!(e.rows(table).await, 0, "{table} still has rows");
    }
    assert_eq!(e.rows("consumers").await, 2);
    assert_eq!(e.rows("limiter_state").await, 1);
    e.state.fetcher.cache().l1.sync().await;
    assert_eq!(e.state.fetcher.cache().l1.entry_count(), 0);

    // The same key still works, and a second reset finds nothing.
    let again = e
        .call("POST", Some(&e.admin), Some(json!({"confirm": "reset"})))
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(rows_of(&again.json(), "players"), 0);
}

/// Only where the explorer is: production and `DEV_UI=false` have no such route.
#[tokio::test]
async fn absent_without_the_dev_explorer() {
    for extra in [vec![("ENV", "production")], vec![("DEV_UI", "false")]] {
        let e = env(&extra).await;
        let r = e
            .call("POST", Some(&e.admin), Some(json!({"confirm": "reset"})))
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{extra:?}");
        assert_eq!(r.json()["error"]["code"], "NOT_FOUND");
    }
}

/// Not in the OpenAPI document, so the explorer never builds a form for it.
#[tokio::test]
async fn not_in_the_openapi_document() {
    let e = env(&[]).await;
    let doc = common::get(e.router.clone(), "/dev/openapi.json").await;
    assert_eq!(doc.status, StatusCode::OK);
    assert!(doc.json()["paths"].get("/dev/reset").is_none());
}

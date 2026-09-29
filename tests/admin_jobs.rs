//! `/v1/admin/jobs*` and `/v1/admin/backfill` (plan P6-08): list with filters,
//! stats by kind and state, retry of failed rows only, cancel of pending rows
//! only, and the admin scope on each.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::jobs::{JobError, NewJob, Registry, Scheduler};
use serde_json::{Value, json};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

struct Env {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    admin: String,
    reader: String,
}

async fn env() -> Env {
    let (dir, state, router) = common::app_with(&[], "http://127.0.0.1:9");
    let mint = |name: &str, scopes| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 10_000,
        key: None,
    };
    let admin = consumers::create(&state.db, mint("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap();
    let reader = consumers::create(&state.db, mint("web", vec![Scope::Read]))
        .await
        .unwrap();
    Env {
        _dir: dir,
        state,
        router,
        admin: admin.key.expose().to_string(),
        reader: reader.key.expose().to_string(),
    }
}

impl Env {
    async fn call(&self, verb: &str, uri: &str, key: &str, body: Option<Value>) -> common::Reply {
        let req = Request::builder()
            .method(verb)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"))
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn get(&self, uri: &str) -> common::Reply {
        self.call("GET", uri, &self.admin, None).await
    }

    async fn job(&self, kind: &'static str, key: &str) -> String {
        let job = NewJob::new(kind, 10_000, json!({"puuid": key})).dedupe(key);
        self.state.jobs.enqueue(job).await.unwrap().id
    }

    fn scheduler(&self) -> Scheduler {
        Scheduler::with_queue(self.state.jobs.clone(), Registry::new())
    }
}

fn error(r: &common::Reply) -> (StatusCode, String) {
    (
        r.status,
        r.json()["error"]["message"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn every_job_route_needs_the_admin_scope() {
    let e = env().await;
    const ID: &str = "01J9ZZZZZZZZZZZZZZZZZZZZZZ";
    for (verb, uri) in [
        ("GET", "/v1/admin/jobs".to_string()),
        ("GET", "/v1/admin/jobs/stats".to_string()),
        ("POST", format!("/v1/admin/jobs/{ID}/retry")),
        ("DELETE", format!("/v1/admin/jobs/{ID}")),
        ("POST", "/v1/admin/backfill".to_string()),
    ] {
        let r = e.call(verb, &uri, &e.reader, Some(json!({}))).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{verb} {uri}");
    }
}

#[tokio::test]
async fn jobs_are_listed_newest_first_and_filtered() {
    let e = env().await;
    let a = e.job("poll:live", "a").await;
    let b = e.job("poll:rank", "b").await;
    let c = e.job("poll:live", "c").await;
    // One of them runs and fails for good.
    let s = e.scheduler();
    let claimed = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&claimed, &Err(JobError::Fail("bad payload".into())), 1)
        .await
        .unwrap();

    let all = e.get("/v1/admin/jobs").await.json();
    let ids: Vec<&str> = all["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [c.as_str(), b.as_str(), a.as_str()], "newest first");

    let failed = e.get("/v1/admin/jobs?state=failed").await.json();
    let row = &failed["jobs"][0];
    assert_eq!(failed["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            row["id"].as_str(),
            row["state"].as_str(),
            row["error"].as_str(),
            row["attempts"].clone()
        ),
        (
            Some(claimed.id.as_str()),
            Some("failed"),
            Some("bad payload"),
            json!(1)
        )
    );
    assert!(row["finishedAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(row["payload"]["puuid"], claimed.dedupe_key.clone().unwrap());

    let live = e.get("/v1/admin/jobs?kind=poll:live&limit=1").await.json();
    assert_eq!(live["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(live["jobs"][0]["kind"], "poll:live");

    assert_eq!(
        error(&e.get("/v1/admin/jobs?state=stuck").await),
        (
            StatusCode::BAD_REQUEST,
            "querystring/state must be equal to one of the allowed values".into()
        )
    );
    assert_eq!(
        error(&e.get("/v1/admin/jobs?limit=0").await).1,
        "querystring/limit must be >= 1"
    );
}

#[tokio::test]
async fn stats_count_rows_by_kind_and_state() {
    let e = env().await;
    e.job("poll:live", "a").await;
    e.job("poll:live", "b").await;
    e.job("archive:match", "KR_1").await;
    let s = e.scheduler();
    let j = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&j, &Ok(()), 1).await.unwrap();
    let stats = e.get("/v1/admin/jobs/stats").await.json();
    assert_eq!(
        stats["totals"],
        json!({"pending": 2, "running": 0, "done": 1, "failed": 0})
    );
    let kinds = stats["kinds"].as_object().unwrap();
    assert_eq!(kinds.len(), 2);
    let summed: i64 = kinds
        .values()
        .map(|k| k["pending"].as_i64().unwrap() + k["done"].as_i64().unwrap())
        .sum();
    assert_eq!(summed, 3);
}

/// Claim the only ready job and fail it for good.
async fn fail(e: &Env) -> String {
    let s = e.scheduler();
    let job = s.claim(i64::MAX).await.unwrap().unwrap();
    s.finish(&job, &Err(JobError::Fail("riot said no".into())), 1)
        .await
        .unwrap();
    job.id
}

#[tokio::test]
async fn only_failed_jobs_are_retried_and_never_into_a_duplicate() {
    let e = env().await;
    let id = e.job("archive:match", "KR_1").await;
    let retry = |id: String| async move { format!("/v1/admin/jobs/{id}/retry") };
    assert_eq!(
        error(&e.call("POST", &retry(id.clone()).await, &e.admin, None).await),
        (StatusCode::BAD_REQUEST, format!("Job {id} is pending"))
    );

    assert_eq!(fail(&e).await, id);
    let r = e.call("POST", &retry(id.clone()).await, &e.admin, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let row = r.json();
    assert_eq!(
        (
            row["state"].as_str(),
            row["attempts"].clone(),
            row["error"].clone()
        ),
        (Some("pending"), json!(0), Value::Null)
    );
    let claimed = e.scheduler().claim(i64::MAX).await.unwrap().unwrap();
    assert_eq!(
        (claimed.id.as_str(), claimed.attempts),
        (id.as_str(), 1),
        "ready now, attempts reset"
    );

    // Failed again, and meanwhile the same match was queued afresh: retrying
    // would run it twice.
    e.scheduler()
        .finish(&claimed, &Err(JobError::Fail("again".into())), 1)
        .await
        .unwrap();
    let twin = e.job("archive:match", "KR_1").await;
    assert_eq!(
        error(&e.call("POST", &retry(id.clone()).await, &e.admin, None).await),
        (
            StatusCode::BAD_REQUEST,
            format!("An identical job is already queued ({twin})")
        )
    );
}

#[tokio::test]
async fn only_pending_jobs_can_be_cancelled() {
    let e = env().await;
    let pending = e.job("poll:live", "a").await;
    let r = e
        .call("DELETE", &format!("/v1/admin/jobs/{pending}"), &e.admin, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        (r.json()["state"].as_str(), r.json()["error"].as_str()),
        (Some("failed"), Some("cancelled"))
    );
    assert!(
        e.scheduler().claim(i64::MAX).await.unwrap().is_none(),
        "never runs"
    );

    let running = e.job("poll:live", "b").await;
    e.scheduler().claim(i64::MAX).await.unwrap().unwrap();
    assert_eq!(
        error(
            &e.call("DELETE", &format!("/v1/admin/jobs/{running}"), &e.admin, None)
                .await
        ),
        (StatusCode::BAD_REQUEST, format!("Job {running} is running"))
    );
    assert_eq!(
        error(
            &e.call(
                "DELETE",
                "/v1/admin/jobs/01J9ZZZZZZZZZZZZZZZZZZZZZZ",
                &e.admin,
                None
            )
            .await
        ),
        (StatusCode::NOT_FOUND, "No such job".into())
    );
    assert_eq!(
        error(&e.call("DELETE", "/v1/admin/jobs/nope", &e.admin, None).await).1,
        "params/id must match format \"ulid\""
    );
}

#[tokio::test]
async fn the_admin_backfill_route_queues_one_walk_per_player() {
    let e = env().await;
    let body = json!({"puuid": P, "platform": "kr", "limit": 200});
    let first = e
        .call("POST", "/v1/admin/backfill", &e.admin, Some(body.clone()))
        .await
        .json();
    assert_eq!(
        (first["ok"].clone(), first["status"].as_str()),
        (json!(true), Some("queued"))
    );
    let again = e
        .call("POST", "/v1/admin/backfill", &e.admin, Some(body))
        .await
        .json();
    assert_eq!(
        (again["jobId"].clone(), again["status"].as_str()),
        (first["jobId"].clone(), Some("already-queued"))
    );

    let row = e
        .state
        .jobs
        .get(first["jobId"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.kind.as_str(), row.priority), ("backfill:player", 20_000));
    let payload: Value = serde_json::from_str(&row.payload).unwrap();
    assert_eq!(
        payload,
        json!({"puuid": P, "platform": "kr", "limit": 200, "fetchTimeline": false, "reason": "admin"})
    );

    for (body, message) in [
        (
            json!({"platform": "kr"}),
            "body must have required property 'puuid'",
        ),
        (
            json!({"puuid": P, "platform": "kr", "limit": 10_001}),
            "body/limit must be <= 10000",
        ),
        (
            json!({"puuid": "x", "platform": "kr"}),
            "body/puuid must NOT have fewer than 60 characters",
        ),
    ] {
        let r = e
            .call("POST", "/v1/admin/backfill", &e.admin, Some(body.clone()))
            .await;
        assert_eq!(error(&r), (StatusCode::BAD_REQUEST, message.into()), "{body}");
    }
}

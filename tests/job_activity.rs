//! What each worker is doing (DEV-19): a real worker runs a job that calls a
//! wiremock Riot, and `GET /v1/admin/jobs/activity` and
//! `GET /v1/admin/jobs/{id}/activity` show it while it runs and after.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use futures_util::future::BoxFuture;
use riot_proxy::fetcher::{FetchOptions, Fetcher};
use riot_proxy::jobs::{Handler, Job, JobError, NewJob, Registry, Scheduler, activity};
use riot_proxy::riot::endpoints::Endpoint;
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::riot::routing::Region;
use riot_proxy::routes::passthrough::request;
use serde_json::{Value, json};
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Fetches a match and one Riot does not know, then holds until released.
struct Probe {
    fetcher: Fetcher,
    gate: Arc<Notify>,
}

impl Handler for Probe {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            if job.payload.contains("fail") {
                return Err(JobError::Fail("nope".into()));
            }
            activity::step("looking at KR_1");
            let target = Endpoint::by_id("match.byId").and_then(|e| e.target_for_region(Region::Asia));
            let opts = FetchOptions::JOB;
            let req = request("match.byId", target, &["KR_1"], &[]).unwrap();
            self.fetcher
                .fetch(req, opts)
                .await
                .map_err(|e| JobError::Retry(e.api.message))?;
            // Riot does not know this one (wiremock answers 404).
            let req = request("match.byId", target, &["KR_2"], &[]).unwrap();
            assert!(self.fetcher.fetch(req, opts).await.is_err());
            activity::step("waiting for the test");
            self.gate.notified().await;
            Ok(())
        })
    }
}

async fn json_of(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let r = common::get(router.clone(), uri).await;
    let body = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    (r.status, body)
}

/// Polls `uri` until `done` holds, for up to five seconds.
async fn until(router: &axum::Router, uri: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..250 {
        let (_, v) = json_of(router, uri).await;
        if done(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{uri} never got there: {}", json_of(router, uri).await.1);
}

fn texts(trace: &Value) -> Vec<String> {
    trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| format!("{}: {}", e["kind"].as_str().unwrap(), e["text"].as_str().unwrap()))
        .collect()
}

#[tokio::test]
async fn the_routes_show_what_a_running_job_does_and_how_it_ended() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/lol/match/v5/matches/KR_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"metadata": {}})))
        .mount(&server)
        .await;
    let (_dir, state, router) = common::app_with(&[("AUTH_DISABLED", "true")], &server.uri());
    let config = common::config(&[]);
    let gate = Arc::new(Notify::new());
    let probe = Probe {
        fetcher: common::fetcher(&config, &server.uri(), Arc::new(Limiter::new(0.8)), None),
        gate: Arc::clone(&gate),
    };
    let scheduler = Scheduler::with_queue(state.jobs.clone(), Registry::new().with("test:probe", probe))
        .with_activity(state.activity.clone());

    // Before any worker starts, there are none to show.
    let (status, v) = json_of(&router, "/v1/admin/jobs/activity").await;
    assert_eq!(
        (status, v),
        (StatusCode::OK, json!({"workers": [], "finished": []}))
    );

    let id = scheduler
        .enqueue(NewJob::new("test:probe", 0, json!({})))
        .await
        .unwrap()
        .id;
    let workers = scheduler.start(2);

    // Running: its worker shows the job and its latest step.
    let v = until(&router, "/v1/admin/jobs/activity", |v| {
        v["workers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["now"] == "waiting for the test")
    })
    .await;
    let w = v["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["jobId"] == id.as_str())
        .unwrap();
    assert_eq!(w["kind"], "test:probe");
    assert_eq!(v["workers"].as_array().unwrap().len(), 2);

    let uri = format!("/v1/admin/jobs/{id}/activity");
    let (status, v) = json_of(&router, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["job"]["state"], "running");
    let trace = &v["trace"];
    assert_eq!(
        (trace["attempt"].as_u64(), trace["outcome"].is_null()),
        (Some(1), true)
    );
    let seen = texts(trace);
    assert!(seen[0].starts_with("job: claimed by worker "), "{seen:?}");
    assert_eq!(seen[1], "step: looking at KR_1");
    // The miss went to Riot; a success is the fetch's one line.
    assert!(
        seen.contains(&"riot: match.byId /lol/match/v5/matches/KR_1 → MISS".to_string()),
        "{seen:?}"
    );
    assert_eq!(seen.last().unwrap(), "step: waiting for the test");
    // Momentary lines (the limiter, a call in flight) set "now" but are not logged.
    assert_eq!(seen.len(), 6, "{seen:?}");
    assert_eq!(seen[3], "error: Riot answered 404 on asia");
    assert!(
        seen[4].starts_with("error: match.byId /lol/match/v5/matches/KR_2 → NOT_FOUND"),
        "{seen:?}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // `after` returns only what is new.
    let next = trace["nextSeq"].as_u64().unwrap();
    let (_, v) = json_of(&router, &format!("{uri}?after={next}")).await;
    assert_eq!(v["trace"]["events"], json!([]));

    // Released: done, the worker is idle, and the trace keeps how it ended.
    gate.notify_one();
    let v = until(&router, &uri, |v| v["job"]["state"] == "done").await;
    assert_eq!(v["trace"]["outcome"], "done");
    assert!(v["trace"]["now"].is_null());
    assert_eq!(texts(&v["trace"]).last().unwrap(), "job: done");
    let (_, v) = json_of(&router, "/v1/admin/jobs/activity").await;
    assert!(
        v["workers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["jobId"].is_null())
    );
    assert_eq!(v["finished"][0]["jobId"], id.as_str());
    assert_eq!(v["finished"][0]["outcome"], "done");

    // A failure says why.
    let failing = scheduler
        .enqueue(NewJob::new("test:probe", 0, json!({"fail": true})))
        .await
        .unwrap()
        .id;
    let v = until(&router, &format!("/v1/admin/jobs/{failing}/activity"), |v| {
        v["job"]["state"] == "failed"
    })
    .await;
    assert_eq!(v["trace"]["outcome"], "failed: nope");

    // A job this process never ran has its row and no trace; an unknown id is a 404.
    let queued = state
        .jobs
        .enqueue(NewJob::new("test:never", 0, json!({})).dedupe("x"))
        .await
        .unwrap()
        .id;
    workers.shutdown(Duration::from_secs(1)).await;
    let (status, v) = json_of(&router, &format!("/v1/admin/jobs/{queued}/activity")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["trace"].is_null() && v["job"]["id"] == queued.as_str());
    let (status, _) = json_of(&router, "/v1/admin/jobs/01K00000000000000000000000/activity").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

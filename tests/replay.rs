//! Replay (plan P3-06): recorded Riot exchanges served by wiremock, driven through
//! the real fetcher, cache and limiter. The event log (X-Cache per step, upstream
//! calls, limiter usage) is an insta snapshot; review changes deliberately.
//! Fixtures and how to re-record them: tests/fixtures/replay/README.md.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use riot_proxy::archive::SqliteArchive;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::db::Db;
use riot_proxy::fetcher::{FetchOptions, Fetcher};
use riot_proxy::riot::client::RiotRequest;
use riot_proxy::riot::endpoints::{Endpoint, Target};
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::riot::routing::{Platform, Region};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/replay");

struct Exchange {
    meta: Value,
    body: Vec<u8>,
}

fn load(scenario: &str) -> Vec<Exchange> {
    let dir = Path::new(FIXTURES).join(scenario);
    let mut metas: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    metas.sort();
    metas
        .into_iter()
        .map(|p| {
            let meta: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            let body = std::fs::read(dir.join(meta["body_file"].as_str().unwrap())).unwrap();
            Exchange { meta, body }
        })
        .collect()
}

/// Mount every exchange. Earlier exchanges win; an exchange whose request
/// repeats later in the scenario answers only once.
async fn mount(server: &MockServer, exchanges: &[Exchange]) {
    for (i, ex) in exchanges.iter().enumerate() {
        let pq = ex.meta["path_and_query"].as_str().unwrap();
        let p = pq.split('?').next().unwrap();
        let mut tpl = ResponseTemplate::new(u16::try_from(ex.meta["status"].as_u64().unwrap()).unwrap());
        let headers = ex.meta["headers"].as_object().unwrap();
        for (k, v) in headers {
            if k != "content-type" {
                tpl = tpl.insert_header(k.as_str(), v.as_str().unwrap());
            }
        }
        let ct = headers
            .get("content-type")
            .and_then(Value::as_str)
            .unwrap_or("application/json");
        tpl = tpl.set_body_raw(ex.body.clone(), ct);

        let mut mock = Mock::given(method("GET")).and(path(p));
        for q in ex.meta["query"].as_array().unwrap() {
            mock = mock.and(query_param(q[0].as_str().unwrap(), q[1].as_str().unwrap()));
        }
        let repeats = exchanges[i + 1..]
            .iter()
            .any(|later| later.meta["path_and_query"] == ex.meta["path_and_query"]);
        let mut mock = mock.respond_with(tpl).with_priority(u8::try_from(i + 1).unwrap());
        if repeats {
            mock = mock.up_to_n_times(1);
        }
        mock.mount(server).await;
    }
}

fn request(ex: &Exchange) -> RiotRequest {
    let e = Endpoint::by_id(ex.meta["endpoint"].as_str().unwrap()).unwrap();
    let routing = ex.meta["routing"].as_str().unwrap();
    let target: Target = match Platform::parse(routing) {
        Ok(p) => e.target_for_platform(p),
        Err(_) => e.target_for_region(Region::parse(routing).unwrap()).unwrap(),
    };
    let params: Vec<&str> = ex.meta["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let mut req = RiotRequest::new(e, target, &params).unwrap();
    for q in ex.meta["query"].as_array().unwrap() {
        req = req
            .query(q[0].as_str().unwrap(), Some(q[1].as_str().unwrap()))
            .unwrap();
    }
    req
}

struct Harness {
    _dir: tempfile::TempDir,
    server: MockServer,
    fetcher: Fetcher,
    limiter: Arc<Limiter>,
}

async fn harness(scenario: &str) -> (Harness, Vec<Exchange>) {
    let exchanges = load(scenario);
    let server = MockServer::start().await;
    mount(&server, &exchanges).await;
    let config = common::config(&[]);
    let limiter = Arc::new(Limiter::new(0.8));
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let archive = Arc::new(SqliteArchive::new(
        db,
        KeyScope::from_key(&config.riot_api_key),
        false,
    ));
    let fetcher = common::fetcher(&config, &server.uri(), Arc::clone(&limiter), Some(archive));
    (
        Harness {
            _dir: dir,
            server,
            fetcher,
            limiter,
        },
        exchanges,
    )
}

/// Limiter windows of at least 10 s, which the test's own run time cannot move.
fn usage(l: &Limiter, scope: &str, method: &str) -> Value {
    let long = |w: &riot_proxy::riot::limiter::WindowUsage| {
        w.window
            .split(':')
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
            .is_some_and(|s| s >= 10)
    };
    json!({
        "app": l.usage(scope).iter().filter(|w| long(w)).map(|w| format!("{} used {}", w.window, w.used)).collect::<Vec<_>>(),
        "method": l.method_usage(scope, &[method])[0].windows.iter().filter(|w| long(w)).map(|w| format!("{} used {}", w.window, w.used)).collect::<Vec<_>>(),
    })
}

async fn step(h: &Harness, pass: usize, n: usize, ex: &Exchange) -> Value {
    let req = request(ex);
    let (scope, method_id) = (req.target.scope(), req.endpoint.id);
    let out = h.fetcher.fetch(req, FetchOptions::default()).await;
    let x_cache = match &out {
        Ok(r) => r.x_cache.as_str().to_string(),
        Err(e) => format!("error {:?}", e.api.code),
    };
    if let Ok(r) = &out {
        assert_eq!(
            r.body.as_ref(),
            ex.body.as_slice(),
            "step {n}: bytes passed through untouched"
        );
    }
    json!({
        "pass": pass,
        "step": n,
        "endpoint": method_id,
        "scope": scope,
        "x_cache": x_cache,
        "upstream_calls": h.server.received_requests().await.map_or(0, |r| r.len()),
        "limiter": usage(&h.limiter, scope, method_id),
    })
}

/// A first lookup, then the same lookup again. Pass 2 is served from cache, and
/// the matches, which are immutable, from the archive.
#[tokio::test]
async fn cold_summoner_lookup() {
    let (h, exchanges) = harness("cold-lookup").await;
    let mut log = Vec::new();
    for pass in 1..=2 {
        for (i, ex) in exchanges.iter().enumerate() {
            log.push(step(&h, pass, i + 1, ex).await);
        }
    }
    insta::assert_json_snapshot!("replay_cold_summoner_lookup", log);
}

/// A typed application 429 (synthetic): the scope freezes for Retry-After, the
/// fetcher waits it out within the interactive budget and succeeds.
#[tokio::test]
async fn typed_application_429() {
    let (h, exchanges) = harness("429-typed-application").await;
    let started = Instant::now();
    let event = step(&h, 1, 1, &exchanges[1]).await;
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(950),
        "waited out Retry-After: {waited:?}"
    );
    assert_eq!(h.limiter.frozen_for("kr"), None, "the freeze has lifted");
    insta::assert_json_snapshot!(
        "replay_typed_application_429",
        json!({
            "event": event,
            "note": "one 429 (application, Retry-After 1) then the recorded 200",
        })
    );
}

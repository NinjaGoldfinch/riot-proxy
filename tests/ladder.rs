//! Ladder enumeration against a wiremock league-v4 (plan P7-02): the apex
//! leagues and every (tier, division) walked to its first empty page, each
//! page fetched once; the stage flips exactly once however many workers end
//! legs at the same time; walks resume from their cursor; a leg that gives up
//! fails the crawl. Plus `POST /v1/admin/ladder/crawl` and `/options`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::ladder::{
    self, CrawlRequest, LadderApexHandler, LadderContext, LadderCrawlHandler, LadderWalkHandler, LegJob,
    store,
};
use riot_proxy::jobs::{Job, JobError, Queue, Registry, Scheduler, kinds};
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::ws::protocol::LADDER;
use riot_proxy::ws::{Hub, Topic};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SOLO: &str = "RANKED_SOLO_5x5";

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    db: Db,
    hub: Hub,
    scope: String,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let config = common::config(&[]);
    Env {
        _dir: dir,
        server: MockServer::start().await,
        db,
        hub: Hub::new(),
        scope: KeyScope::from_key(&config.riot_api_key).as_str().to_string(),
    }
}

fn apex_path(tier: &str) -> String {
    format!(
        "/lol/league/v4/{}leagues/by-queue/{SOLO}",
        tier.to_ascii_lowercase()
    )
}

fn entries_path(tier: &str, division: &str) -> String {
    format!("/lol/league/v4/entries/{SOLO}/{tier}/{division}")
}

/// `n` entries named after where they sit, as league-v4 lists them.
fn entries(tier: &str, division: &str, page: u32, n: u32) -> Vec<Value> {
    (0..n)
        .map(|i| {
            json!({"puuid": format!("{tier}-{division}-{page}-{i}"), "rank": division,
                   "leaguePoints": 100 - i, "wins": 10, "losses": 5, "veteran": i == 0})
        })
        .collect()
}

impl Env {
    fn ctx(&self, backfill_limit: u32) -> Arc<LadderContext> {
        let config = common::config(&[]);
        Arc::new(LadderContext {
            fetcher: common::fetcher(&config, &self.server.uri(), Arc::new(Limiter::new(0.8)), None),
            queue: Queue::new(self.db.clone()),
            hub: self.hub.clone(),
            key_scope: self.scope.clone(),
            tier_floor: "MASTER".into(),
            backfill_limit,
            lookup_backfill_limit: 500,
            archive_timelines: false,
        })
    }

    /// An apex league with `n` entries (no tier on the entries, as Riot sends it).
    async fn apex(&self, tier: &str, n: u32) {
        let list: Vec<Value> = entries(tier, "I", 1, n)
            .into_iter()
            .map(|mut e| {
                e.as_object_mut().unwrap().remove("rank");
                e
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(apex_path(tier)))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"tier": tier, "queue": SOLO, "entries": list})),
            )
            .mount(&self.server)
            .await;
    }

    /// A division whose pages hold `sizes[0]`, `sizes[1]`, … entries, then an
    /// empty page.
    async fn division(&self, tier: &str, division: &str, sizes: &[u32]) {
        for (i, n) in sizes.iter().chain(&[0]).enumerate() {
            let page = u32::try_from(i).unwrap() + 1;
            Mock::given(method("GET"))
                .and(path(entries_path(tier, division)))
                .and(query_param("page", page.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(entries(tier, division, page, *n)))
                .mount(&self.server)
                .await;
        }
    }

    async fn requests(&self) -> Vec<String> {
        let mut r: Vec<String> = self
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| match r.url.query() {
                Some(q) => format!("{}?{q}", r.url.path()),
                None => r.url.path().to_string(),
            })
            .collect();
        r.sort();
        r
    }

    async fn start(&self, floor: &str) -> String {
        let s = ladder::start_crawl(
            &Queue::new(self.db.clone()),
            &self.scope,
            "MASTER",
            &CrawlRequest {
                platform: "kr".into(),
                queue: SOLO.into(),
                tier_floor: Some(floor.into()),
            },
        )
        .await
        .unwrap();
        s.crawl_id
    }

    async fn crawl(&self, id: &str) -> store::Crawl {
        store::get(&self.db, &self.scope, id).await.unwrap().unwrap()
    }

    /// Run every queued job with `workers` workers until the crawl leaves
    /// enumeration (or ends).
    async fn run(&self, ctx: &Arc<LadderContext>, id: &str, workers: usize) -> store::Crawl {
        let registry = Registry::new()
            .with(kinds::LADDER_CRAWL, LadderCrawlHandler(Arc::clone(ctx)))
            .with(kinds::LADDER_APEX, LadderApexHandler(Arc::clone(ctx)))
            .with(kinds::LADDER_WALK, LadderWalkHandler(Arc::clone(ctx)));
        let running = Scheduler::with_queue(Queue::new(self.db.clone()), registry).start(workers);
        let crawl = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let c = self.crawl(id).await;
                if c.phase != "enumerate" || c.status != "running" {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the crawl leaves enumeration");
        running.shutdown(Duration::from_secs(5)).await;
        crawl
    }

    async fn count(&self, sql: &'static str) -> i64 {
        self.db
            .read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get(0))?))
            .await
            .unwrap()
    }
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<axum::extract::ws::Utf8Bytes>) -> Vec<Value> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|f| serde_json::from_str(f.as_str()).unwrap())
        .collect()
}

#[tokio::test]
async fn enumeration_records_every_apex_league_and_division_page_once() {
    let e = env().await;
    for tier in ["CHALLENGER", "GRANDMASTER", "MASTER"] {
        e.apex(tier, 2).await;
    }
    e.division("DIAMOND", "I", &[3, 3]).await;
    for d in ["II", "III", "IV"] {
        e.division("DIAMOND", d, &[]).await;
    }
    let mut ladder = e.hub.subscribe(&Topic::named(LADDER));
    let id = e.start("DIAMOND").await;
    let crawl = e.run(&e.ctx(100), &id, 4).await;

    assert_eq!(
        (crawl.status.as_str(), crawl.phase.as_str()),
        ("running", "collect")
    );
    // Empty pages end a walk and are not counted (v1).
    assert_eq!(
        (
            crawl.counters.pages_fetched,
            crawl.counters.entries_seen,
            crawl.counters.players_discovered
        ),
        (5, 12, 12)
    );
    let mut expected = vec![
        apex_path("CHALLENGER"),
        apex_path("GRANDMASTER"),
        apex_path("MASTER"),
    ];
    for page in 1..=3 {
        expected.push(format!("{}?page={page}", entries_path("DIAMOND", "I")));
    }
    for d in ["II", "III", "IV"] {
        expected.push(format!("{}?page=1", entries_path("DIAMOND", d)));
    }
    expected.sort();
    assert_eq!(e.requests().await, expected, "every page exactly once");

    // The ladder, with the leg's tier on apex entries, and the players known
    // but never tracked.
    assert_eq!(e.count("SELECT COUNT(*) FROM ladder_entries").await, 12);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM ladder_entries WHERE tier = 'CHALLENGER' AND division = 'I'")
            .await,
        2
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM ladder_entries WHERE veteran = 1")
            .await,
        5
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM players WHERE tracked = 0 AND platform = 'kr'")
            .await,
        12
    );
    // The collect stage was queued in the same commit: 12 players, one batch.
    assert_eq!(
        e.count("SELECT COUNT(*) FROM crawl_legs WHERE leg = 'ladder:collect:0'")
            .await,
        1
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE kind = 'ladder:collect'")
            .await,
        1
    );

    let frames = drain(&mut ladder);
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(
        (
            &frames[0]["event"],
            &frames[0]["data"]["phase"],
            &frames[0]["data"]["stats"]["entriesSeen"]
        ),
        (&json!("crawl.phase"), &json!("collect"), &json!(12))
    );
}

#[tokio::test]
async fn the_stage_flips_exactly_once_under_concurrency() {
    let e = env().await;
    for tier in ["CHALLENGER", "GRANDMASTER", "MASTER"] {
        e.apex(tier, 0).await;
    }
    for tier in [
        "IRON", "BRONZE", "SILVER", "GOLD", "PLATINUM", "EMERALD", "DIAMOND",
    ] {
        for d in ["I", "II", "III", "IV"] {
            e.division(tier, d, &[]).await;
        }
    }
    let mut ladder = e.hub.subscribe(&Topic::named(LADDER));
    let id = e.start("IRON").await;
    // 31 legs, 16 workers, every leg one request: they finish together.
    let crawl = e.run(&e.ctx(100), &id, 16).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Nobody on the ladder: nothing to collect, straight on to archive (v1).
    assert_eq!(crawl.phase, "archive");
    let frames = drain(&mut ladder);
    assert_eq!(frames.len(), 1, "one transition, announced once: {frames:?}");
    assert_eq!(e.requests().await.len(), 31);
}

#[tokio::test]
async fn without_backfill_the_crawl_completes_at_enumeration() {
    let e = env().await;
    e.apex("CHALLENGER", 3).await;
    let mut ladder = e.hub.subscribe(&Topic::named(LADDER));
    let id = e.start("CHALLENGER").await;
    let crawl = e.run(&e.ctx(0), &id, 2).await;
    assert_eq!(
        (crawl.status.as_str(), crawl.phase.as_str()),
        ("completed", "enumerate")
    );
    assert!(crawl.finished_at.is_some());
    let frames = drain(&mut ladder);
    let events: Vec<&str> = frames.iter().map(|f| f["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["crawl.phase", "ladder.crawl.completed"]);
    assert_eq!(frames[0]["data"]["phase"], "completed");
    assert_eq!(
        (&frames[1]["data"]["entries"], &frames[1]["data"]["players"]),
        (&json!(3), &json!(3))
    );
}

fn leg_job(crawl: &str, tier: &str, division: Option<&str>, attempts: u32) -> Job {
    let leg = LegJob {
        crawl_id: crawl.into(),
        platform: "kr".into(),
        queue: SOLO.into(),
        tier: tier.into(),
        division: division.map(str::to_string),
    };
    Job {
        id: "j".into(),
        kind: if division.is_some() {
            "ladder:walk"
        } else {
            "ladder:apex"
        }
        .into(),
        dedupe_key: None,
        priority: 20_002,
        payload: serde_json::to_string(&leg).unwrap(),
        attempts,
        run_after: 0,
    }
}

#[tokio::test]
async fn a_walk_resumes_on_the_page_after_the_last_one_stored() {
    let e = env().await;
    let id = e.start("DIAMOND").await;
    let crawl = id.clone();
    e.db.write(move |c| {
        Ok::<_, DbError>(c.execute(
            "UPDATE crawl_legs SET cursor = 3 WHERE crawl_id = ?1 AND leg = 'ladder:walk:DIAMOND:I'",
            [crawl],
        )?)
    })
    .await
    .unwrap();
    // Only pages 3 and 4 exist: asking for 1 or 2 would fail the test.
    for (page, n) in [(3, 2), (4, 0)] {
        Mock::given(method("GET"))
            .and(path(entries_path("DIAMOND", "I")))
            .and(query_param("page", page.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(entries("DIAMOND", "I", page, n)))
            .mount(&e.server)
            .await;
    }
    e.ctx(100)
        .walk(&leg_job(&id, "DIAMOND", Some("I"), 1))
        .await
        .unwrap();
    let p = |n: u32| format!("{}?page={n}", entries_path("DIAMOND", "I"));
    assert_eq!(e.requests().await, [p(3), p(4)]);
    assert_eq!(e.crawl(&id).await.counters.entries_seen, 2);
    // Ended: a re-run (crash before the job was marked done) does nothing.
    e.ctx(100)
        .walk(&leg_job(&id, "DIAMOND", Some("I"), 2))
        .await
        .unwrap();
    assert_eq!(e.requests().await.len(), 2);
}

#[tokio::test]
async fn a_leg_that_gives_up_fails_the_crawl() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path(apex_path("CHALLENGER")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&e.server)
        .await;
    let mut ladder = e.hub.subscribe(&Topic::named(LADDER));
    let id = e.start("CHALLENGER").await;
    let ctx = e.ctx(100);

    // A retry keeps the leg outstanding.
    assert!(matches!(
        ctx.apex(&leg_job(&id, "CHALLENGER", None, 1)).await,
        Err(JobError::Retry(_))
    ));
    assert_eq!(e.crawl(&id).await.status, "running");
    // The last attempt ends it as failed, and the crawl with it (v1).
    assert!(ctx.apex(&leg_job(&id, "CHALLENGER", None, 5)).await.is_err());
    let crawl = e.crawl(&id).await;
    assert_eq!((crawl.status.as_str(), crawl.legs_failed), ("failed", 1));
    let frames = drain(&mut ladder);
    assert_eq!(frames.len(), 1, "no completed event for a failed run: {frames:?}");
    assert_eq!(frames[0]["data"]["phase"], "failed");
}

#[tokio::test]
async fn a_walk_of_a_crawl_no_longer_running_stops_without_a_request() {
    let e = env().await;
    let id = e.start("DIAMOND").await;
    let crawl = id.clone();
    e.db.write(move |c| {
        Ok::<_, DbError>(c.execute(
            "UPDATE ladder_crawls SET status = 'cancelled' WHERE id = ?1",
            [crawl],
        )?)
    })
    .await
    .unwrap();
    e.ctx(100)
        .walk(&leg_job(&id, "DIAMOND", Some("II"), 1))
        .await
        .unwrap();
    e.ctx(100).apex(&leg_job(&id, "MASTER", None, 1)).await.unwrap();
    assert!(e.requests().await.is_empty());
}

// ── Admin routes ────────────────────────────────────────────────────────────

struct App {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    admin: String,
    reader: String,
}

async fn app(vars: &[(&str, &str)]) -> App {
    let (dir, state, router) = common::app_with(vars, "http://127.0.0.1:9");
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
    App {
        _dir: dir,
        state,
        router,
        admin: admin.key.expose().to_string(),
        reader: reader.key.expose().to_string(),
    }
}

impl App {
    async fn call(&self, verb: &str, uri: &str, key: &str, body: Option<Value>) -> common::Reply {
        let req = Request::builder()
            .method(verb)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"))
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        common::send(self.router.clone(), req).await
    }
}

#[tokio::test]
async fn the_crawl_route_starts_one_crawl_per_ladder_and_answers_202() {
    let a = app(&[
        ("DEFAULT_PLATFORM", "kr"),
        ("LADDER_QUEUES", "RANKED_FLEX_SR,RANKED_SOLO_5x5"),
    ])
    .await;
    let first = a.call("POST", "/v1/admin/ladder/crawl", &a.admin, None).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    let body = first.json();
    assert_eq!(
        (&body["status"], &body["platform"], &body["queue"], &body["legs"]),
        (
            &json!("started"),
            &json!("kr"),
            &json!("RANKED_FLEX_SR"),
            &json!(3)
        ),
        "defaults: DEFAULT_PLATFORM, the first LADDER_QUEUES, LADDER_TIER_FLOOR"
    );
    let again = a
        .call(
            "POST",
            "/v1/admin/ladder/crawl",
            &a.admin,
            Some(json!({"queue": "RANKED_FLEX_SR", "tierFloor": "IRON"})),
        )
        .await;
    assert_eq!(again.status, StatusCode::ACCEPTED);
    assert_eq!(
        (
            &again.json()["status"],
            &again.json()["crawlId"],
            &again.json()["legs"]
        ),
        (&json!("already-running"), &body["crawlId"], &json!(0))
    );
    let solo = a
        .call(
            "POST",
            "/v1/admin/ladder/crawl",
            &a.admin,
            Some(json!({"platform": "euw1", "queue": SOLO, "tierFloor": "DIAMOND"})),
        )
        .await
        .json();
    assert_eq!((&solo["status"], &solo["legs"]), (&json!("started"), &json!(7)));
    let crawl = store::get(
        &a.state.db,
        a.state.fetcher.key_scope().as_str(),
        solo["crawlId"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (crawl.platform.as_str(), crawl.tier_floor.as_str()),
        ("euw1", "DIAMOND")
    );

    assert_eq!(
        a.call("POST", "/v1/admin/ladder/crawl", &a.reader, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    for (body, code, message) in [
        (
            json!({"tierFloor": "master"}),
            "VALIDATION",
            "body/tierFloor must be equal to one of the allowed values",
        ),
        (
            json!({"queue": "ARAM"}),
            "VALIDATION",
            "body/queue must be equal to one of the allowed values",
        ),
        (
            json!({"platform": "xx1"}),
            "BAD_REGION",
            "body/platform must be equal to one of the allowed values",
        ),
    ] {
        let r = a
            .call("POST", "/v1/admin/ladder/crawl", &a.admin, Some(body.clone()))
            .await;
        assert_eq!(
            (
                r.status,
                r.json()["error"]["code"].as_str().unwrap(),
                r.json()["error"]["message"].as_str().unwrap()
            ),
            (StatusCode::BAD_REQUEST, code, message),
            "{body}"
        );
    }
}

#[tokio::test]
async fn the_options_route_lists_what_a_crawl_can_be_asked_for() {
    let a = app(&[
        ("DEFAULT_PLATFORM", "kr"),
        ("LADDER_TIER_FLOOR", "emerald"),
        ("LADDER_BACKFILL_LIMIT", "50"),
    ])
    .await;
    let r = a.call("GET", "/v1/admin/ladder/options", &a.admin, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(body["platforms"].as_array().unwrap().len(), 16);
    assert_eq!(
        body["platforms"][0]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["id", "label"]
    );
    assert_eq!(body["queues"], json!(["RANKED_SOLO_5x5", "RANKED_FLEX_SR"]));
    assert_eq!(body["tiers"][0], "IRON");
    assert_eq!(body["tiers"][9], "CHALLENGER");
    assert_eq!(
        body["defaults"],
        json!({"platform": "kr", "queue": SOLO, "tierFloor": "EMERALD", "backfillLimit": 50})
    );
    assert_eq!(
        a.call("GET", "/v1/admin/ladder/options", &a.reader, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

// ── The whole crawl (P7-03) ─────────────────────────────────────────────────

const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");

fn puuid(i: usize) -> String {
    format!("P{i:0>77}")
}

/// Match `k`'s ten players: a sliding window over the ladder, so every
/// player is in four of the twelve matches and every match is reachable from
/// ten walks.
fn players_of(k: usize) -> Vec<usize> {
    (0..10).map(|j| (k * 5 + j) % 30).collect()
}

fn match_id(k: usize) -> String {
    format!("KR_{}", 1000 + k)
}

/// The fixture match, re-cast with match `k`'s id and players.
fn match_body(k: usize) -> Vec<u8> {
    let mut body: Value = serde_json::from_slice(MATCH).unwrap();
    let players = players_of(k);
    body["metadata"]["matchId"] = json!(match_id(k));
    body["metadata"]["participants"] = json!(players.iter().map(|p| puuid(*p)).collect::<Vec<_>>());
    body["info"]["gameId"] = json!(1000 + k);
    body["info"]["gameEndTimestamp"] = json!(1_700_000_000_000_i64 + i64::try_from(k).unwrap() * 3_600_000);
    for (slot, p) in players.iter().enumerate() {
        let part = &mut body["info"]["participants"][slot];
        part["puuid"] = json!(puuid(*p));
        part["riotIdGameName"] = json!(format!("Player{p}"));
        part["riotIdTagline"] = json!("KR1");
    }
    serde_json::to_vec(&body).unwrap()
}

impl Env {
    async fn ladder_of_thirty(&self) {
        let list: Vec<Value> = (0..30)
            .map(|i| json!({"puuid": puuid(i), "leaguePoints": 1000 - i, "wins": 50, "losses": 40}))
            .collect();
        Mock::given(method("GET"))
            .and(path(apex_path("CHALLENGER")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"tier": "CHALLENGER", "entries": list})),
            )
            .mount(&self.server)
            .await;
        for p in 0..30 {
            let mut ids: Vec<usize> = (0..12).filter(|k| players_of(*k).contains(&p)).collect();
            ids.sort_unstable_by(|a, b| b.cmp(a));
            Mock::given(method("GET"))
                .and(path(format!("/lol/match/v5/matches/by-puuid/{}/ids", puuid(p))))
                .and(query_param("queue", "420"))
                .and(query_param("start", "0"))
                .and(query_param("count", "100"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(ids.iter().map(|k| match_id(*k)).collect::<Vec<_>>()),
                )
                .mount(&self.server)
                .await;
        }
        for k in 0..12 {
            Mock::given(method("GET"))
                .and(path(format!("/lol/match/v5/matches/{}", match_id(k))))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(match_body(k)))
                .mount(&self.server)
                .await;
        }
    }

    /// Every handler a crawl reaches, over one archiving fetcher.
    fn all_handlers(&self) -> Registry {
        let config = common::config(&[]);
        let key = KeyScope::from_key(&config.riot_api_key);
        let archive = Arc::new(riot_proxy::archive::SqliteArchive::new(
            self.db.clone(),
            key,
            false,
        ));
        let fetcher = common::fetcher(
            &config,
            &self.server.uri(),
            Arc::new(Limiter::new(0.8)),
            Some(archive),
        );
        let queue = Queue::new(self.db.clone());
        let ladder = Arc::new(LadderContext {
            fetcher: fetcher.clone(),
            queue: queue.clone(),
            hub: self.hub.clone(),
            key_scope: self.scope.clone(),
            tier_floor: "CHALLENGER".into(),
            backfill_limit: 100,
            lookup_backfill_limit: 500,
            archive_timelines: false,
        });
        let archiving = Arc::new(riot_proxy::jobs::archive::ArchiveContext {
            fetcher,
            queue,
            hub: self.hub.clone(),
            key_scope: self.scope.clone(),
            archive_timelines: false,
            lookup_backfill_limit: 500,
        });
        let names = Arc::new(riot_proxy::jobs::names::NamesBackfill {
            db: self.db.clone(),
            key_scope: self.scope.clone(),
        });
        Registry::new()
            .with(kinds::LADDER_CRAWL, LadderCrawlHandler(Arc::clone(&ladder)))
            .with(kinds::LADDER_APEX, LadderApexHandler(Arc::clone(&ladder)))
            .with(kinds::LADDER_WALK, LadderWalkHandler(Arc::clone(&ladder)))
            .with(
                kinds::LADDER_COLLECT,
                ladder::LadderCollectHandler(Arc::clone(&ladder)),
            )
            .with(
                kinds::LADDER_ARCHIVE,
                ladder::LadderArchiveHandler(Arc::clone(&ladder)),
            )
            .with(
                kinds::ARCHIVE_MATCH,
                riot_proxy::jobs::archive::ArchiveMatchHandler(Arc::clone(&archiving)),
            )
            .with(
                kinds::NAMES_BACKFILL,
                riot_proxy::jobs::names::NamesBackfillHandler(names),
            )
            .with(
                kinds::AGGREGATE_ANALYTICS,
                riot_proxy::jobs::analytics::AggregateHandler(Arc::new(
                    riot_proxy::jobs::analytics::AnalyticsContext {
                        queue: Queue::new(self.db.clone()),
                        hub: self.hub.clone(),
                        key_scope: self.scope.clone(),
                        patch_limit: 4,
                        reextract_batch: 500,
                    },
                )),
            )
    }

    /// Run until no job is pending or running.
    async fn drain_jobs(&self, workers: usize) {
        let running = Scheduler::with_queue(Queue::new(self.db.clone()), self.all_handlers()).start(workers);
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let open = self
                    .count("SELECT COUNT(*) FROM jobs WHERE state IN ('pending', 'running')")
                    .await;
                if open == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the queue drains");
        running.shutdown(Duration::from_secs(5)).await;
    }

    async fn fetches_of(&self, path_part: &str) -> usize {
        self.requests()
            .await
            .iter()
            .filter(|r| r.ends_with(path_part))
            .count()
    }
}

#[tokio::test]
async fn a_crawl_runs_every_stage_and_fetches_each_match_once() {
    let e = env().await;
    e.ladder_of_thirty().await;
    // Two of the twelve matches are in the archive already.
    for k in [0, 7] {
        riot_proxy::archive::matches::put(&e.db, &match_id(k), "asia", &e.scope, match_body(k).into(), 1)
            .await
            .unwrap();
    }
    let mut ladder = e.hub.subscribe(&Topic::named(LADDER));
    let id = e.start("CHALLENGER").await;
    e.drain_jobs(6).await;

    let crawl = e.crawl(&id).await;
    assert_eq!(
        (crawl.status.as_str(), crawl.phase.as_str()),
        ("completed", "archive")
    );
    let c = &crawl.counters;
    assert_eq!(
        (
            c.entries_seen,
            c.backfills_enqueued,
            c.match_ids_seen,
            c.matches_queued
        ),
        (30, 30, 12, 10),
        "30 players, 12 distinct matches after de-duplication, 10 not archived"
    );

    // The point of the stages: each unarchived match fetched exactly once,
    // however many of its ten players were walked; archived ones never.
    for k in 0..12 {
        let expected = usize::from(k != 0 && k != 7);
        assert_eq!(
            e.fetches_of(&format!("/matches/{}", match_id(k))).await,
            expected,
            "{}",
            match_id(k)
        );
    }
    assert_eq!(e.count("SELECT COUNT(*) FROM matches").await, 12);
    assert_eq!(
        e.count("SELECT COUNT(*) FROM crawl_match_ids").await,
        0,
        "set drained"
    );
    assert_eq!(e.count("SELECT COUNT(*) FROM crawl_legs").await, 0);
    // Every player's walk is stamped started; a 100-deep ranked walk is not
    // a whole history, so none is stamped done (v1 `walkIsComplete`).
    assert_eq!(
        e.count("SELECT COUNT(*) FROM players WHERE json_extract(backfill_state, '$.startedAt') > 0")
            .await,
        30
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM players WHERE json_extract(backfill_state, '$.doneAt') IS NOT NULL")
            .await,
        0
    );

    let events: Vec<(String, String)> = drain(&mut ladder)
        .iter()
        .map(|f| {
            (
                f["event"].as_str().unwrap().to_string(),
                f["data"]["phase"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            ("crawl.phase".to_string(), "collect".to_string()),
            ("crawl.phase".to_string(), "archive".to_string()),
            ("crawl.phase".to_string(), "completed".to_string()),
            ("ladder.crawl.completed".to_string(), String::new()),
            // A clean crawl recomputes its ladder's analytics (v1).
            ("analytics.updated".to_string(), String::new()),
        ]
    );
    assert!(
        e.count("SELECT COUNT(*) FROM champion_stats WHERE platform = 'kr'")
            .await
            > 0
    );

    // The finished crawl queued names:backfill, which named every player from
    // the archived matches without a request.
    assert_eq!(
        e.count("SELECT COUNT(*) FROM jobs WHERE kind = 'names:backfill' AND state = 'done'")
            .await,
        1
    );
    assert_eq!(
        e.count("SELECT COUNT(*) FROM players WHERE game_name IS NULL")
            .await,
        0
    );
    let name: String =
        e.db.read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT game_name || '#' || tag_line FROM players WHERE puuid = ?1",
                [puuid(3)],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(name, "Player3#KR1");
}

#[tokio::test]
async fn a_player_match_v5_does_not_know_is_skipped() {
    let e = env().await;
    e.apex("CHALLENGER", 2).await;
    // CHALLENGER-I-1-0 has history; CHALLENGER-I-1-1 is gone (404).
    Mock::given(method("GET"))
        .and(path("/lol/match/v5/matches/by-puuid/CHALLENGER-I-1-0/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(["KR_1", "KR_2"])))
        .mount(&e.server)
        .await;
    let id = e.start("CHALLENGER").await;
    let ctx = e.ctx(100);
    // Enumerate by hand, then run the one collect job.
    ctx.apex(&leg_job(&id, "CHALLENGER", None, 1)).await.unwrap();
    let payload: String =
        e.db.read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT payload FROM jobs WHERE kind = 'ladder:collect'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let job = Job {
        id: "c".into(),
        kind: "ladder:collect".into(),
        dedupe_key: None,
        priority: 20_003,
        payload,
        attempts: 1,
        run_after: 0,
    };
    ctx.collect(&job).await.unwrap();
    let crawl = e.crawl(&id).await;
    assert_eq!(
        (crawl.phase.as_str(), crawl.counters.match_ids_seen),
        ("archive", 2)
    );
}

#[tokio::test]
async fn crawls_are_listed_and_a_running_one_can_be_cancelled() {
    let a = app(&[("DEFAULT_PLATFORM", "kr")]).await;
    let started = a
        .call(
            "POST",
            "/v1/admin/ladder/crawl",
            &a.admin,
            Some(json!({"tierFloor": "DIAMOND"})),
        )
        .await
        .json();
    let id = started["crawlId"].as_str().unwrap().to_string();

    let list = a
        .call("GET", "/v1/admin/ladder/crawls", &a.admin, None)
        .await
        .json();
    let row = &list["crawls"][0];
    assert_eq!(
        (
            &row["id"],
            &row["status"],
            &row["phase"],
            &row["tierFloor"],
            &row["pendingLegs"],
            &row["finishedAt"]
        ),
        (
            &json!(id),
            &json!("running"),
            &json!("enumerate"),
            &json!("DIAMOND"),
            &json!(7),
            &Value::Null
        )
    );
    assert!(row["startedAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        a.call("GET", "/v1/admin/ladder/crawls?platform=euw1", &a.admin, None)
            .await
            .json()["crawls"],
        json!([])
    );
    assert_eq!(
        a.call("GET", "/v1/admin/ladder/crawls?limit=0", &a.admin, None)
            .await
            .json()["error"]["message"],
        "querystring/limit must be >= 1"
    );

    let mut ladder = a.state.hub.subscribe(&Topic::named(LADDER));
    let cancel = a
        .call("DELETE", &format!("/v1/admin/ladder/crawls/{id}"), &a.admin, None)
        .await;
    assert_eq!(cancel.status, StatusCode::OK);
    assert_eq!(
        cancel.json(),
        json!({"ok": true, "crawlId": id, "status": "cancelled", "droppedJobs": 7})
    );
    assert_eq!(drain(&mut ladder)[0]["data"]["phase"], "cancelled");
    let open: i64 = a
        .state
        .db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind LIKE 'ladder:%' AND state = 'pending'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(open, 0, "its queued legs are dropped");
    let row = &a
        .call("GET", "/v1/admin/ladder/crawls", &a.admin, None)
        .await
        .json()["crawls"][0];
    assert_eq!(
        (&row["status"], &row["pendingLegs"]),
        (&json!("cancelled"), &json!(0))
    );
    assert!(row["finishedAt"].is_string());

    let again = a
        .call("DELETE", &format!("/v1/admin/ladder/crawls/{id}"), &a.admin, None)
        .await;
    assert_eq!(
        (again.status, again.json()["error"]["message"].as_str().unwrap()),
        (
            StatusCode::BAD_REQUEST,
            format!("Crawl {id} is already cancelled").as_str()
        )
    );
    let unknown = a
        .call(
            "DELETE",
            "/v1/admin/ladder/crawls/01J9ZZZZZZZZZZZZZZZZZZZZZZ",
            &a.admin,
            None,
        )
        .await;
    assert_eq!(
        (
            unknown.status,
            unknown.json()["error"]["message"].as_str().unwrap()
        ),
        (
            StatusCode::NOT_FOUND,
            "No such ladder crawl for the current key scope"
        )
    );
    assert_eq!(
        a.call("DELETE", "/v1/admin/ladder/crawls/nope", &a.admin, None)
            .await
            .json()["error"]["message"],
        "params/id must match format \"ulid\""
    );
    for (verb, uri) in [
        ("GET", "/v1/admin/ladder/crawls"),
        ("DELETE", "/v1/admin/ladder/crawls/x"),
    ] {
        assert_eq!(
            a.call(verb, uri, &a.reader, None).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn the_names_route_queues_one_pass_and_counts_whom_it_is_for() {
    let a = app(&[]).await;
    let scope = a.state.fetcher.key_scope().as_str().to_string();
    for (p, name) in [("A", None), ("B", None), ("C", Some("Named"))] {
        riot_proxy::players::upsert(
            &a.state.db,
            &scope,
            riot_proxy::players::Upsert {
                puuid: p,
                platform: "kr",
                game_name: name,
                tag_line: name.map(|_| "KR1"),
                tracked: None,
            },
            1,
        )
        .await
        .unwrap();
    }
    let r = a
        .call("POST", "/v1/admin/players/names/backfill", &a.admin, None)
        .await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::ACCEPTED, json!({"ok": true, "unnamed": 2}))
    );
    a.call("POST", "/v1/admin/players/names/backfill", &a.admin, None)
        .await;
    let queued: i64 = a
        .state
        .db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'names:backfill' AND priority = 30000",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(queued, 1, "one pass at a time");
    assert_eq!(
        a.call("POST", "/v1/admin/players/names/backfill", &a.reader, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

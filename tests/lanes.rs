//! Rate-limit-aware job claims against a wiremock Riot (SCH-01): jobs the
//! limiter has no room for give their worker back and resume where they
//! stopped; crawls of different platforms run side by side instead of one
//! after another; a walk takes turns, so a poll queued behind it still gets
//! a worker.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::ladder::{
    self, CollectJob, CrawlRequest, LadderApexHandler, LadderContext, LadderCrawlHandler, LadderWalkHandler,
    LegJob, store,
};
use riot_proxy::jobs::{Handler, Job, JobError, NewJob, Queue, Registry, Scheduler, kinds};
use riot_proxy::riot::client::MOCK_HOST_HEADER;
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::riot::limiter::headers::LimitWindow;
use riot_proxy::ws::Hub;
use serde_json::json;
use tokio::time::Instant;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SOLO: &str = "RANKED_SOLO_5x5";

/// Every request the mock answered: the Riot host it was for, its path and
/// query, and when.
type Log = Arc<Mutex<Vec<(String, String, Instant)>>>;

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    db: Db,
    scope: String,
    log: Log,
}

/// league-v4 as a crawl sees it: apex leagues empty, each division
/// `pages` pages of one entry then an empty page; match ids empty.
struct Ladder {
    log: Log,
    pages: u32,
    delay: Duration,
}

impl Respond for Ladder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let host = req
            .headers
            .get(MOCK_HOST_HEADER)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();
        let at = match req.url.query() {
            Some(q) => format!("{}?{q}", req.url.path()),
            None => req.url.path().to_string(),
        };
        self.log.lock().unwrap().push((host, at, Instant::now()));
        let body = if req.url.path().contains("leagues/by-queue") {
            json!({"entries": []})
        } else if req.url.path().contains("/entries/") {
            let page: u32 = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "page")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(1);
            if page <= self.pages {
                json!([{"puuid": format!("{}-{page}", req.url.path()), "rank": "I", "leaguePoints": 1}])
            } else {
                json!([])
            }
        } else {
            json!([])
        };
        ResponseTemplate::new(200)
            .set_body_json(body)
            .set_delay(self.delay)
    }
}

async fn env(pages: u32, delay: Duration) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let config = common::config(&[]);
    let server = MockServer::start().await;
    let log: Log = Arc::default();
    Mock::given(method("GET"))
        .and(path_regex("^/lol/"))
        .respond_with(Ladder {
            log: Arc::clone(&log),
            pages,
            delay,
        })
        .mount(&server)
        .await;
    Env {
        _dir: dir,
        server,
        db,
        scope: KeyScope::from_key(&config.riot_api_key).as_str().to_string(),
        log,
    }
}

fn window(limit: u32, seconds: u32) -> [LimitWindow; 1] {
    [LimitWindow { limit, seconds }]
}

impl Env {
    fn ctx(&self, limiter: &Arc<Limiter>, backfill_limit: u32) -> Arc<LadderContext> {
        let config = common::config(&[]);
        Arc::new(LadderContext {
            fetcher: common::fetcher(&config, &self.server.uri(), Arc::clone(limiter), None),
            queue: Queue::new(self.db.clone()),
            hub: Hub::new(),
            key_scope: self.scope.clone(),
            tier_floor: "MASTER".into(),
            backfill_limit,
            lookup_backfill_limit: 500,
            collect_overlap_s: 7_200,
            archive_timelines: false,
            rank_lookup_limit: 50_000,
            rank_lookup_recheck_s: 604_800,
            late_stamp_days: 14,
            delta_on: false,
        })
    }

    async fn start(&self, platform: &str, floor: &str) -> String {
        let req = CrawlRequest {
            platform: platform.into(),
            queue: SOLO.into(),
            tier_floor: Some(floor.into()),
        };
        ladder::start_crawl(&Queue::new(self.db.clone()), &self.scope, "MASTER", &req)
            .await
            .unwrap()
            .crawl_id
    }

    async fn crawl(&self, id: &str) -> store::Crawl {
        store::get(&self.db, &self.scope, id).await.unwrap().unwrap()
    }

    fn requests(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(_, at, _)| at.clone())
            .collect()
    }

    fn count(&self, part: &str) -> usize {
        self.requests().iter().filter(|r| r.contains(part)).count()
    }
}

fn job(kind: &str, payload: &impl serde::Serialize) -> Job {
    Job {
        id: "j".into(),
        kind: kind.into(),
        dedupe_key: None,
        priority: 20_002,
        payload: serde_json::to_string(payload).unwrap(),
        attempts: 1,
        run_after: 0,
    }
}

fn yielded(r: Result<(), JobError>) -> (i64, Option<String>) {
    match r {
        Err(JobError::Yield { retry_at, payload }) => (retry_at, payload),
        other => panic!("expected a yield, got {other:?}"),
    }
}

#[tokio::test]
async fn a_collect_that_yields_resumes_with_the_players_it_has_not_done() {
    let e = env(0, Duration::ZERO).await;
    let limiter = Arc::new(Limiter::new(1.0));
    // kr's match-v5 is asia: room for two requests in the next two minutes.
    limiter.configure_app("asia", &window(2, 120));
    let ctx = e.ctx(&limiter, 100);
    let crawl_id = e.start("kr", "CHALLENGER").await;
    let players: Vec<String> = (0..4).map(|i| format!("P{i}")).collect();
    let batch = CollectJob {
        crawl_id,
        platform: "kr".into(),
        queue: SOLO.into(),
        puuids: players.clone(),
        offset: 0,
    };

    let before = riot_proxy::clock::Clock::now().unix_ms;
    let (retry_at, payload) = yielded(ctx.collect(&job(kinds::LADDER_COLLECT, &batch)).await);
    assert!(retry_at > before + 100_000, "when asia's window has room again");
    let rest: CollectJob = serde_json::from_str(&payload.expect("the players still to do")).unwrap();
    assert_eq!(rest.puuids, players[2..], "P0 and P1 are done");
    assert_eq!((rest.offset, rest.platform.as_str()), (0, "kr"), "same leg");

    limiter.configure_app("asia", &window(100, 120));
    ctx.collect(&job(kinds::LADDER_COLLECT, &rest)).await.unwrap();
    for p in &players {
        assert_eq!(e.count(&format!("/by-puuid/{p}/ids")), 1, "{p} walked once");
    }
}

/// `players` on kr's solo ladder as this crawl's enumerate leaves them: ten
/// games each, never collected.
async fn on_ladder(db: &Db, scope: &str, crawl_id: &str, players: &[String]) {
    let (scope, crawl, players) = (scope.to_string(), crawl_id.to_string(), players.to_vec());
    db.write(move |c| {
        for p in &players {
            c.execute(
                "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points,
                    wins, losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
                 VALUES (?1, 'kr', ?2, ?3, 'CHALLENGER', 'I', 0, 6, 4, ?4, ?4, 0)",
                rusqlite::params![scope, SOLO, p, crawl],
            )?;
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

/// THR-04 (ADR-185): a player's cursor is stamped only by a walk that reached
/// its end, so a batch that yields has stamped the players it finished.
#[tokio::test]
async fn a_collect_that_yields_stamps_only_the_players_it_finished() {
    let e = env(0, Duration::ZERO).await;
    let limiter = Arc::new(Limiter::new(1.0));
    limiter.configure_app("asia", &window(2, 120));
    let ctx = e.ctx(&limiter, 100);
    let crawl_id = e.start("kr", "CHALLENGER").await;
    let players: Vec<String> = (0..4).map(|i| format!("P{i}")).collect();
    on_ladder(&e.db, &e.scope, &crawl_id, &players).await;
    let batch = CollectJob {
        crawl_id,
        platform: "kr".into(),
        queue: SOLO.into(),
        puuids: players.clone(),
        offset: 0,
    };
    let cursors = || async {
        let mut out = Vec::new();
        for p in &players {
            out.push(
                store::cursor(&e.db, &e.scope, "kr", SOLO, p)
                    .await
                    .unwrap()
                    .is_some(),
            );
        }
        out
    };

    let (_, payload) = yielded(ctx.collect(&job(kinds::LADDER_COLLECT, &batch)).await);
    assert_eq!(cursors().await, [true, true, false, false], "P0 and P1 finished");
    let rest: CollectJob = serde_json::from_str(&payload.unwrap()).unwrap();
    limiter.configure_app("asia", &window(100, 120));
    ctx.collect(&job(kinds::LADDER_COLLECT, &rest)).await.unwrap();
    assert_eq!(cursors().await, [true; 4]);
}

/// A player's match ids, `total` of them, paged by `start`/`count` as Riot
/// does; every request logged with the rest.
struct Paged {
    log: Log,
    total: u32,
    delay: Duration,
}

impl Respond for Paged {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let at = format!("{}?{}", req.url.path(), req.url.query().unwrap_or(""));
        self.log.lock().unwrap().push(("asia".into(), at, Instant::now()));
        let q: BTreeMap<String, String> = req.url.query_pairs().into_owned().collect();
        let start: u32 = q.get("start").and_then(|v| v.parse().ok()).unwrap_or(0);
        let count: u32 = q.get("count").and_then(|v| v.parse().ok()).unwrap_or(20);
        let ids: Vec<String> = (start..self.total.min(start + count))
            .map(|i| format!("KR_{i}"))
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(ids)
            .set_delay(self.delay)
    }
}

/// THR-04 (ADR-185, G3): a cursor walk that yields between pages leaves the
/// cursor as it was; the re-run asks again from the same `startTime`, from
/// the first page, and stamps the time it began.
#[tokio::test]
async fn a_cursor_walk_interrupted_between_pages_is_not_stamped_and_resumes_from_its_cursor() {
    let e = env(0, Duration::ZERO).await;
    Mock::given(method("GET"))
        .and(path_regex("/by-puuid/BIG/ids$"))
        .respond_with(Paged {
            log: Arc::clone(&e.log),
            total: 250,
            delay: Duration::from_millis(500),
        })
        .with_priority(1)
        .mount(&e.server)
        .await;
    let limiter = Arc::new(Limiter::new(1.0));
    limiter.configure_app("asia", &window(2, 120));
    let ctx = e.ctx(&limiter, 100);
    let crawl_id = e.start("kr", "CHALLENGER").await;
    on_ladder(&e.db, &e.scope, &crawl_id, &["BIG".to_string()]).await;
    let last = riot_proxy::clock::Clock::now().unix_ms - 3_600_000;
    store::stamp_cursor(&e.db, &e.scope, "kr", SOLO, "BIG", last)
        .await
        .unwrap();
    let batch = CollectJob {
        crawl_id: crawl_id.clone(),
        platform: "kr".into(),
        queue: SOLO.into(),
        puuids: vec!["BIG".into()],
        offset: 0,
    };
    let since = format!("startTime={}", last / 1000 - 7_200);

    let (_, payload) = yielded(ctx.collect(&job(kinds::LADDER_COLLECT, &batch)).await);
    let first: Vec<String> = e.requests();
    assert_eq!(first.len(), 2, "two pages, then out of room: {first:?}");
    assert!(first.iter().all(|r| r.contains(&since)), "{first:?}");
    assert_eq!(
        store::cursor(&e.db, &e.scope, "kr", SOLO, "BIG").await.unwrap(),
        Some(last),
        "a walk that stopped part-way stamps nothing"
    );

    let rest: CollectJob = serde_json::from_str(&payload.unwrap()).unwrap();
    limiter.configure_app("asia", &window(100, 120));
    let resumed = riot_proxy::clock::Clock::now().unix_ms;
    ctx.collect(&job(kinds::LADDER_COLLECT, &rest)).await.unwrap();
    let again: Vec<String> = e.requests()[2..].to_vec();
    let starts: Vec<&str> = again
        .iter()
        .map(|r| r.split('&').find(|p| p.contains("start=")).unwrap())
        .collect();
    // The first two pages are the same requests as before, answered from the
    // response cache (match ids are kept 120 s); only the third reaches Riot.
    assert_eq!(starts, ["/lol/match/v5/matches/by-puuid/BIG/ids?start=200"]);
    assert!(
        again.iter().all(|r| r.contains(&since)),
        "the same cursor: {again:?}"
    );
    let stamped = store::cursor(&e.db, &e.scope, "kr", SOLO, "BIG")
        .await
        .unwrap()
        .unwrap();
    assert!(stamped >= resumed, "the re-run's start, not the first run's");
    // When the walk began, before its one 500 ms request reached Riot, not
    // when it ended: every id it read was listed at or after the stamp.
    assert!(
        stamped - resumed < 400,
        "stamped {} ms into the re-run",
        stamped - resumed
    );
    assert_eq!(e.crawl(&crawl_id).await.counters.match_ids_seen, 250);
}

/// THR-04: a `ladder:collect` row as an older binary queued it still runs.
/// The payload shape didn't change (the cursor is read from the ladder row),
/// and this pins it.
#[tokio::test]
async fn an_old_shape_collect_payload_still_runs() {
    let e = env(0, Duration::ZERO).await;
    let limiter = Arc::new(Limiter::new(1.0));
    let ctx = e.ctx(&limiter, 100);
    let crawl_id = e.start("kr", "CHALLENGER").await;
    on_ladder(&e.db, &e.scope, &crawl_id, &["P0".to_string()]).await;
    let payload =
        json!({"crawlId": crawl_id, "platform": "kr", "queue": SOLO, "puuids": ["P0"], "offset": 0});
    ctx.collect(&job(kinds::LADDER_COLLECT, &payload)).await.unwrap();
    assert_eq!(e.count("/by-puuid/P0/ids"), 1);
    assert!(
        store::cursor(&e.db, &e.scope, "kr", SOLO, "P0")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_walk_resumes_after_yielding_and_takes_turns_every_ten_pages() {
    let e = env(25, Duration::ZERO).await;
    let limiter = Arc::new(Limiter::new(1.0));
    limiter.configure_app("kr", &window(3, 120));
    let ctx = e.ctx(&limiter, 100);
    let crawl_id = e.start("kr", "DIAMOND").await;
    let leg = LegJob {
        crawl_id: crawl_id.clone(),
        platform: "kr".into(),
        queue: SOLO.into(),
        tier: "DIAMOND".into(),
        division: Some("I".into()),
    };
    let walk = job(kinds::LADDER_WALK, &leg);
    let pages = || e.count("/DIAMOND/I?page=");

    // Out of tokens after three pages: yield until the window has room.
    let (_, payload) = yielded(ctx.walk(&walk).await);
    assert_eq!((pages(), payload), (3, None), "the cursor is the resume point");
    limiter.configure_app("kr", &window(1000, 120));

    // Then ten pages a turn, from the page after the last one stored.
    yielded(ctx.walk(&walk).await);
    assert_eq!(pages(), 13);
    yielded(ctx.walk(&walk).await);
    assert_eq!(pages(), 23);
    ctx.walk(&walk).await.unwrap();
    let expected: Vec<String> = (1..=26)
        .map(|p| format!("/lol/league/v4/entries/{SOLO}/DIAMOND/I?page={p}"))
        .collect();
    let mut seen: Vec<String> = e
        .requests()
        .into_iter()
        .filter(|r| r.contains("/DIAMOND/I?"))
        .collect();
    seen.sort_by_key(|r| r.rsplit('=').next().unwrap().parse::<u32>().unwrap());
    assert_eq!(seen, expected, "every page once, in order, up to the empty one");
    assert_eq!(e.crawl(&crawl_id).await.counters.entries_seen, 25);
}

/// The owner's report: three crawls queued together ran one platform after
/// another while the other platforms' limits sat idle.
#[tokio::test]
async fn crawls_of_three_platforms_run_side_by_side() {
    let e = env(2, Duration::ZERO).await;
    let limiter = Arc::new(Limiter::new(1.0));
    let platforms = ["na1", "euw1", "kr"];
    for p in platforms {
        // 3 requests a second each: 15 requests (3 apex, 4 divisions × 3
        // pages) take ~4 s per platform, ~12 s one after another.
        limiter.configure_app(p, &window(3, 1));
    }
    let ctx = e.ctx(&limiter, 0);
    let registry = Registry::new()
        .with(kinds::LADDER_CRAWL, LadderCrawlHandler(Arc::clone(&ctx)))
        .with(kinds::LADDER_APEX, LadderApexHandler(Arc::clone(&ctx)))
        .with(kinds::LADDER_WALK, LadderWalkHandler(Arc::clone(&ctx)));
    let mut ids = Vec::new();
    for p in platforms {
        ids.push(e.start(p, "DIAMOND").await);
    }
    let started = Instant::now();
    let workers = Scheduler::with_queue(Queue::new(e.db.clone()), registry)
        .with_limiter(Arc::clone(&limiter))
        .start(8);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut done = true;
            for id in &ids {
                done &= e.crawl(id).await.status != "running";
            }
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the crawls finish");
    let took = started.elapsed();
    workers.shutdown(Duration::from_secs(5)).await;

    for id in &ids {
        assert_eq!(e.crawl(id).await.status, "completed");
    }
    let log = e.log.lock().unwrap().clone();
    let mut first: BTreeMap<String, Duration> = BTreeMap::new();
    let mut per_host: BTreeMap<String, usize> = BTreeMap::new();
    for (host, _, at) in &log {
        first
            .entry(host.clone())
            .or_insert_with(|| at.duration_since(started));
        *per_host.entry(host.clone()).or_default() += 1;
    }
    assert_eq!(
        per_host,
        platforms.iter().map(|p| ((*p).to_string(), 15)).collect(),
        "each platform's pages once"
    );
    for (host, at) in &first {
        assert!(
            *at < Duration::from_secs(1),
            "{host} waited {at:?} for its first page"
        );
    }
    assert!(
        took < Duration::from_secs(9),
        "about the slowest crawl's time, not the sum: {took:?}"
    );
}

/// Records how many pages had been fetched when it ran.
struct Probe {
    log: Log,
    ran_after: Arc<Mutex<Option<usize>>>,
}

impl Handler for Probe {
    fn run<'a>(&'a self, _job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            let pages = self.log.lock().unwrap().len();
            *self.ran_after.lock().unwrap() = Some(pages);
            Ok(())
        })
    }
}

#[tokio::test]
async fn a_poll_queued_behind_a_long_walk_runs_within_one_turn() {
    // One division of 40 pages at 20 ms a page, one worker.
    let e = env(40, Duration::from_millis(20)).await;
    let limiter = Arc::new(Limiter::new(1.0));
    let ctx = e.ctx(&limiter, 0);
    let ran_after = Arc::new(Mutex::new(None));
    let registry = Registry::new()
        .with(kinds::LADDER_WALK, LadderWalkHandler(Arc::clone(&ctx)))
        .with(
            kinds::POLL_LIVE,
            Probe {
                log: Arc::clone(&e.log),
                ran_after: Arc::clone(&ran_after),
            },
        );
    let crawl_id = e.start("kr", "DIAMOND").await;
    // Only DIAMOND I's walk: the other legs are not this test's business.
    let id = crawl_id.clone();
    e.db.write(move |c| {
        c.execute(
            "DELETE FROM jobs WHERE json_extract(payload, '$.crawlId') = ?1
               AND NOT (kind = 'ladder:walk' AND json_extract(payload, '$.division') = 'I'
                        AND json_extract(payload, '$.tier') = 'DIAMOND')",
            [id],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let queue = Queue::new(e.db.clone());
    let workers = Scheduler::with_queue(queue.clone(), registry)
        .with_limiter(limiter)
        .start(1);
    // The walk is well under way when the poll comes in.
    tokio::time::timeout(Duration::from_secs(10), async {
        while e.count("/DIAMOND/I?page=") < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let at_enqueue = e.log.lock().unwrap().len();
    queue
        .enqueue(NewJob::new(
            kinds::POLL_LIVE,
            riot_proxy::jobs::priority::POLL,
            json!({"puuid": "p", "platform": "kr"}),
        ))
        .await
        .unwrap();
    let ran = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(n) = *ran_after.lock().unwrap() {
                return n;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the poll runs");
    workers.shutdown(Duration::from_secs(5)).await;
    assert!(
        ran <= 10 && ran >= at_enqueue,
        "the poll ran at the walk's first turn (page 10), after {ran} pages"
    );
}

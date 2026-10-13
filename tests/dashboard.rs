//! The dashboard's backend (plan P7-06): `GET /v1/admin/metrics` builds v1's
//! whole `MetricsSnapshot` from v2's own sources, and the history sampler
//! keeps the newest 1440 points for `GET /v1/admin/metrics/history`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::DbError;
use riot_proxy::jobs::Queue;
use riot_proxy::jobs::analytics::{Ladder, Run, record_run};
use riot_proxy::jobs::ladder::{self, CrawlRequest};
use serde_json::{Value, json};

struct Env {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    admin: String,
    reader: String,
    scope: String,
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
    let scope = state.fetcher.key_scope().as_str().to_string();
    Env {
        _dir: dir,
        state,
        router,
        admin: admin.key.expose().to_string(),
        reader: reader.key.expose().to_string(),
        scope,
    }
}

impl Env {
    async fn get(&self, uri: &str, key: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn sql(&self, sql: String) {
        self.state
            .db
            .write(move |c| {
                c.execute_batch(&sql)?;
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
    }
}

fn now() -> i64 {
    riot_proxy::clock::Clock::now().unix_ms
}

#[tokio::test]
async fn the_snapshot_is_v1s_whole_document_from_v2s_sources() {
    let e = env().await;
    let t = now();
    // Jobs in every state, across v1's queues.
    e.sql(format!(
        "INSERT INTO jobs (id, kind, priority, payload, state, run_after, finished_at) VALUES
           ('a', 'poll:live', 1, '{{}}', 'running', 0, NULL),
           ('b', 'poll:rank', 1, '{{}}', 'pending', 0, NULL),
           ('c', 'archive:match', 1, '{{}}', 'pending', {later}, NULL),
           ('d', 'ladder:walk', 1, '{{}}', 'failed', 0, {recent}),
           ('e', 'ladder:walk', 1, '{{}}', 'failed', 0, {old}),
           ('f', 'names:backfill', 1, '{{}}', 'done', 0, {recent}),
           ('g', 'aggregate:analytics', 1, '{{}}', 'done', 0, {old});",
        later = t + 600_000,
        recent = t - 60_000,
        old = t - 2 * 86_400_000,
    ))
    .await;
    // A running crawl (DIAMOND: 7 legs) and a finished one.
    let start = |queue: &str, floor: &str| CrawlRequest {
        platform: "kr".into(),
        queue: queue.into(),
        tier_floor: Some(floor.into()),
    };
    let q = Queue::new(e.state.db.clone());
    let running = ladder::start_crawl(&q, &e.scope, "MASTER", &start("RANKED_SOLO_5x5", "DIAMOND"))
        .await
        .unwrap();
    let done = ladder::start_crawl(&q, &e.scope, "MASTER", &start("RANKED_FLEX_SR", "CHALLENGER"))
        .await
        .unwrap();
    e.sql(format!(
        "UPDATE ladder_crawls SET status = 'completed', finished_at = {t} WHERE id = '{}';
         DELETE FROM crawl_legs WHERE crawl_id = '{}';
         DELETE FROM jobs WHERE dedupe_key LIKE '{}:%';",
        done.crawl_id, done.crawl_id, done.crawl_id
    ))
    .await;
    // An analytics run, and the stats it wrote.
    let ladder_kr = Ladder {
        platform: "kr".into(),
        queue: "RANKED_SOLO_5x5".into(),
    };
    let run = Run {
        at: t - 5_000,
        status: "completed".into(),
        ms: 1234,
        steps: BTreeMap::from([("champions".into(), 0.5), ("matchups".into(), 0.25)]),
        rows: BTreeMap::from([("champion_stats".into(), 3), ("champion_items".into(), 7)]),
        games: 30,
    };
    record_run(&e.state.db, &e.scope, &ladder_kr, &run).await.unwrap();
    e.sql(format!(
        "INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake, side,
           games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s, computed_at)
         VALUES ('{s}', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '16.19', 103, 'MIDDLE', 0, 'blue', 4, 3, 4, 4, 0,0,0,0,0,0,0,0, 1),
                ('{s}', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', '16.19', 103, 'MIDDLE', 0, 'red', 2, 0, 2, 2, 0,0,0,0,0,0,0,0, 1),
                ('{s}', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '16.19', 86, 'TOP', 0, 'blue', 5, 1, 5, 5, 0,0,0,0,0,0,0,0, 1),
                ('{s}', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '16.18', 1, 'TOP', 0, 'red', 99, 1, 5, 5, 0,0,0,0,0,0,0,0, 1);",
        s = e.scope
    ))
    .await;
    // Limits learned from Riot, as a restart restores them.
    e.sql(format!(
        "INSERT INTO limiter_state (scope, windows, frozen_until, updated_at) VALUES
           ('app:kr', '{{\"known\":true,\"windows\":[{{\"limit\":20,\"seconds\":1,\"stamps\":[]}},{{\"limit\":100,\"seconds\":120,\"stamps\":[]}}]}}', NULL, {t}),
           ('method:kr:match.byId', '{{\"windows\":[{{\"limit\":2000,\"seconds\":10,\"stamps\":[]}}]}}', NULL, {t});"
    ))
    .await;
    riot_proxy::riot::limiter::persist::restore_from(e.state.fetcher.limiter(), &e.state.db)
        .await
        .unwrap();

    let r = e.get("/v1/admin/metrics", &e.admin).await;
    assert_eq!(r.status, StatusCode::OK);
    let s = r.json();
    assert_eq!(
        s.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "analytics",
            "cache",
            "cacheStore",
            "disk",
            "events",
            "flows",
            "gates",
            "inflight",
            "keyScope",
            "ladder",
            "limiter",
            "process",
            "queues",
            "quotas",
            "requests",
            "sampledAt",
            "sqlite",
            "totals",
            "upstream",
            "v",
            "worker",
            "ws"
        ]
    );
    assert_eq!((&s["v"], &s["keyScope"]), (&json!(1), &json!(e.scope)));
    assert_eq!(
        s["totals"],
        json!({"archivedMatches": 0, "trackedPlayers": 0, "knownPlayers": 0, "activeConsumers": 2})
    );

    // v1's six queues, each with v1's seven counts.
    let queues = s["queues"].as_object().unwrap();
    assert_eq!(
        queues.keys().collect::<Vec<_>>(),
        ["archive", "backfill", "ddragon", "ladder", "maintenance", "poll"]
    );
    assert_eq!(
        s["queues"]["poll"],
        json!({"active": 1, "waiting": 1, "prioritized": 0, "delayed": 0, "scheduled": 0, "failed": 0, "completed": 0})
    );
    assert_eq!(s["queues"]["archive"]["delayed"], 1);
    // The crawl's legs are pending ladder jobs too; failed counts the last day only.
    assert_eq!(
        (
            &s["queues"]["ladder"]["waiting"],
            &s["queues"]["ladder"]["failed"]
        ),
        (&json!(7), &json!(1))
    );
    assert_eq!(
        s["queues"]["maintenance"]["completed"], 1,
        "done in the last hour"
    );

    // The crawl panel.
    let l = &s["ladder"];
    assert_eq!(l["running"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            &l["running"][0]["id"],
            &l["running"][0]["phase"],
            &l["running"][0]["pendingLegs"]
        ),
        (&json!(running.crawl_id), &json!("enumerate"), &json!(7))
    );
    assert_eq!(
        (
            &l["lastCompleted"]["id"],
            &l["lastCompleted"]["status"],
            &l["lastCompleted"]["pendingLegs"]
        ),
        (&json!(done.crawl_id), &json!("completed"), &json!(0))
    );
    assert_eq!(l["entries"], 0);

    // Analytics: the run as recorded; top champions on its ladder's latest patch, summed over tiers.
    assert_eq!(
        s["analytics"]["lastRuns"],
        json!([{"platform": "kr", "queue": "RANKED_SOLO_5x5", "at": t - 5_000, "status": "completed", "ms": 1234,
                "steps": {"champions": 0.5, "matchups": 0.25}, "rows": {"champion_items": 7, "champion_stats": 3},
                "games": 30}])
    );
    assert_eq!(
        s["analytics"]["topChampions"],
        json!([{"championId": 103, "games": 6, "winRate": 0.5}, {"championId": 86, "games": 5, "winRate": 0.2}])
    );

    // The limiter, as the dashboard draws it.
    let kr = s["limiter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["scope"] == "kr")
        .cloned()
        .unwrap();
    assert_eq!(
        (&kr["kind"], &kr["label"], &kr["frozenMs"]),
        (&json!("platform"), &json!("Korea"), &json!(0))
    );
    assert_eq!(
        kr["windows"],
        json!([{"window": "20:1", "used": 0, "limit": 20}, {"window": "100:120", "used": 0, "limit": 100}])
    );
    assert_eq!(kr["methods"][0]["method"], "match.byId");

    // The rest: present, typed as the page uses them.
    for k in ["hit", "miss", "neg", "stale"] {
        assert!(s["cache"][k].is_u64(), "cache.{k}");
    }
    assert_eq!(s["worker"], json!({"alive": true, "lastSeenMs": 0}));
    assert!(s["flows"]["backfillsQueued"].is_object() && s["flows"]["refreshClaims"].is_object());
    assert!(s["process"]["uptimeSeconds"].is_number());
    assert!(
        s["process"]["rssBytes"].as_u64().unwrap() > 0,
        "read from /proc on Linux"
    );
    assert_eq!(s["ws"], json!({"connections": 0, "subscriptions": 0}));
    // ADM-05: requests by `route|status|cache`, cumulative and global to this
    // binary, so at least the one this test made before.
    e.get("/v1/admin/stats", &e.admin).await;
    let s = e.get("/v1/admin/metrics", &e.admin).await.json();
    assert!(s["requests"]["/v1/admin/stats|200|none"].as_u64().unwrap() >= 1);
    assert!(s["upstream"].is_object());

    assert_eq!(
        e.get("/v1/admin/metrics", &e.reader).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn history_points_are_sampled_capped_at_1440_and_served_oldest_first() {
    let e = env().await;
    let r = e.get("/v1/admin/metrics/history", &e.admin).await.json();
    assert_eq!(r, json!({"intervalS": 60, "maxPoints": 1440, "points": []}));

    let t = now();
    e.state.stats.record_point(t - 60_000).await.unwrap();
    e.state.stats.record_point(t).await.unwrap();
    let r = e.get("/v1/admin/metrics/history", &e.admin).await.json();
    let points = r["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(
        (&points[0]["t"], &points[1]["t"]),
        (&json!(t - 60_000), &json!(t))
    );
    // v1's point: no activeConsumers in totals; queues summed.
    assert_eq!(
        points[1].as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "analytics",
            "cache",
            "disk",
            "queues",
            "requests",
            "t",
            "totals",
            "upstream"
        ]
    );
    assert_eq!(
        points[1]["totals"],
        json!({"archivedMatches": 0, "trackedPlayers": 0, "knownPlayers": 0})
    );
    assert_eq!(
        points[1]["queues"],
        json!({"active": 0, "pending": 0, "failed": 0})
    );
    assert_eq!(points[1]["analytics"], json!({"rows": 0, "ageSeconds": null}));

    // 1440 kept: a full history loses its oldest point to the next.
    let rows: Vec<String> = (1..=1440).map(|i| format!("({i}, '{{\"t\":{i}}}')")).collect();
    e.sql("DELETE FROM metrics_history;".into()).await;
    e.sql(format!(
        "INSERT INTO metrics_history (at, point) VALUES {};",
        rows.join(",")
    ))
    .await;
    e.state.stats.record_point(t).await.unwrap();
    let (count, oldest): (i64, i64) = e
        .state
        .db
        .read(|c| {
            Ok::<_, DbError>(
                c.query_row("SELECT COUNT(*), MIN(at) FROM metrics_history", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!((count, oldest), (1440, 2));
    // Points that do not parse as v1's point are skipped, not fatal.
    let served = e.get("/v1/admin/metrics/history", &e.admin).await.json();
    assert_eq!(served["points"].as_array().unwrap().len(), 1);

    assert_eq!(
        e.get("/v1/admin/metrics/history", &e.reader).await.status,
        StatusCode::FORBIDDEN
    );
}

/// ADM-02: the table-scanning parts are sampled in the background; a
/// snapshot reads the samples, `GET /v1/admin/stats` its archive bytes and
/// disk (its counts are live), and a history point is built from the sets it
/// sampled.
#[tokio::test]
async fn the_snapshot_and_stats_read_the_samples_until_the_next_one() {
    let e = env().await;
    let stats = &e.state.stats;
    let fast = stats.sample_fast().await.unwrap();
    let slow = stats.sample_slow().await.unwrap();

    // Written after the samples: not seen until the next one.
    e.sql(format!(
        "INSERT INTO jobs (id, kind, priority, payload, state, run_after) VALUES
           ('x', 'poll:live', 1, '{{}}', 'running', 0);
         INSERT INTO players (key_scope, puuid, platform, tracked, updated_at) VALUES
           ('{}', 'p1', 'kr', 1, 0);",
        e.scope
    ))
    .await;
    let at = json!({"fast": fast.at, "slow": slow.at});
    let s = e.get("/v1/admin/metrics", &e.admin).await.json();
    assert_eq!(
        (&s["queues"]["poll"]["active"], &s["totals"]["trackedPlayers"]),
        (&json!(0), &json!(0))
    );
    assert_eq!(s["sampledAt"], at);
    // The stats' counts are read with the request (callers poll them for
    // progress); its bytes and disk readings are the slow sample's.
    let before = now();
    let st = e.get("/v1/admin/stats", &e.admin).await.json();
    assert_eq!(
        (&st["trackedPlayers"], &st["knownPlayers"]),
        (&json!(1), &json!(1))
    );
    assert!(st["sampledAt"]["fast"].as_i64().unwrap() >= before);
    assert_eq!(st["sampledAt"]["slow"], json!(slow.at));
    assert_eq!(
        (&st["dbFileBytes"], &st["archiveStoredBytes"]),
        (&json!(slow.disk.db_bytes), &json!(slow.archive.stored_bytes))
    );

    // The next fast sample sees them; the slow set is unchanged.
    let next = stats.sample_fast().await.unwrap();
    let s = e.get("/v1/admin/metrics", &e.admin).await.json();
    assert_eq!(
        (&s["queues"]["poll"]["active"], &s["totals"]["trackedPlayers"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(s["sampledAt"], json!({"fast": next.at, "slow": slow.at}));

    // A history point samples both sets and is built from them.
    let t = now();
    stats.record_point(t).await.unwrap();
    let (f, sl) = (stats.samples.fast().unwrap(), stats.samples.slow().unwrap());
    assert!(f.at >= next.at && sl.at >= slow.at);
    let h = e.get("/v1/admin/metrics/history", &e.admin).await.json();
    let p = h["points"].as_array().unwrap().last().cloned().unwrap();
    assert_eq!(
        p["totals"],
        json!({"archivedMatches": f.totals.archived_matches, "trackedPlayers": f.totals.tracked_players,
               "knownPlayers": f.totals.known_players})
    );
    assert_eq!(p["queues"], json!({"active": 1, "pending": 0, "failed": 0}));
    assert_eq!(p["t"], json!(t));
}

/// ADM-04: the runtime gauges the new dashboard draws: cache tiers, the bulk
/// gate, quotas, the read pool and the disk.
#[tokio::test]
async fn the_snapshot_carries_the_runtime_gauges() {
    let e = env().await;
    // An L1 entry, a frozen scope and a consumer's metered requests.
    let ttls = riot_proxy::riot::endpoints::Ttls {
        soft: Some(std::time::Duration::from_secs(60)),
        hard: Some(std::time::Duration::from_secs(60)),
        negative: None,
    };
    e.state
        .fetcher
        .cache()
        .l1
        .put("probe", bytes::Bytes::from_static(b"{}"), &ttls)
        .await
        .unwrap();
    // Limits Riot named for kr (so the limiter lists it), then a freeze.
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-app-rate-limit", "20:1,100:120".parse().unwrap());
    h.insert("x-app-rate-limit-count", "1:1,1:120".parse().unwrap());
    e.state.fetcher.limiter().observe(
        "kr",
        "match.byId",
        &riot_proxy::riot::limiter::headers::RateLimitHeaders::from_headers(&h),
    );
    e.state.fetcher.limiter().freeze(
        "kr",
        std::time::Duration::from_secs(30),
        riot_proxy::riot::limiter::headers::RateLimitType::Service,
    );
    let web = riot_proxy::http::auth::Consumer {
        id: "c-web".into(),
        name: "web".into(),
        scopes: vec![Scope::Read],
        quota_per_min: 5,
    };
    for _ in 0..2 {
        e.state.quotas.check(&web).unwrap();
    }

    let s = e.get("/v1/admin/metrics", &e.admin).await.json();
    let store = &s["cacheStore"];
    assert_eq!(store["l1Entries"], 1);
    assert!(store["l1Bytes"].as_u64().unwrap() > 0);
    // The test app's L1 budget, not CACHE_L1_MAX_MB's default.
    assert_eq!(store["l1MaxBytes"], e.state.fetcher.cache().l1.max_bytes());
    assert!(store["l1Bytes"].as_u64().unwrap() < store["l1MaxBytes"].as_u64().unwrap());
    assert_eq!((&store["l2Rows"], &store["l2Dropped"]), (&json!(0), &json!(0)));

    let g = &s["gates"];
    assert_eq!(
        (
            &g["bulkUsageCeiling"],
            &g["bulkIdleCeiling"],
            &g["bulkIdleAfterS"],
            &g["clientWaitBudgetMs"]
        ),
        (&json!(0.8), &json!(0.95), &json!(30), &json!(2000))
    );
    assert_eq!(g["blockedScopes"][0]["scope"], "kr");
    let for_ms = g["blockedScopes"][0]["forMs"].as_u64().unwrap();
    assert!((25_000..=30_000).contains(&for_ms), "{for_ms}");
    assert!(g["blockedMethods"].as_array().unwrap().is_empty());
    let kr = s["limiter"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["scope"] == "kr")
        .cloned()
        .unwrap();
    assert!(kr["frozenMs"].as_u64().unwrap() > 0);
    assert_eq!(kr["interactiveWaiters"], 0);

    // Busiest first; the admin's own request is metered too.
    let quotas = s["quotas"].as_array().unwrap();
    assert_eq!(
        quotas[0],
        json!({"consumer": "c-web", "limit": 5, "used": 2, "resetSecs": 60})
    );
    assert_eq!(
        (quotas.len(), &quotas[1]["used"], &quotas[1]["limit"]),
        (2, &json!(1), &json!(10_000))
    );
    assert_eq!(s["inflight"], 0);
    assert_eq!(s["sqlite"]["readers"], 2);
    assert!(s["sqlite"]["readersFree"].as_u64().unwrap() <= 2);
    let disk = &s["disk"];
    assert!(disk["dbBytes"].as_u64().unwrap() > 0);
    assert!(disk["totalBytes"].as_u64().unwrap() >= disk["freeBytes"].as_u64().unwrap());
    for k in ["walBytes", "freelistBytes"] {
        assert!(disk[k].is_u64(), "disk.{k}");
    }

    // History points carry the disk and the request totals; points stored
    // before ADM-04 still parse, with the additions null.
    e.sql(
        r#"INSERT INTO metrics_history (at, point) VALUES (5, '{"t":5,
             "totals":{"archivedMatches":0,"trackedPlayers":0,"knownPlayers":0},
             "queues":{"active":0,"pending":0,"failed":0},
             "analytics":{"rows":0,"ageSeconds":null},
             "cache":{"hit":0,"miss":0,"neg":0,"stale":0}}');"#
            .into(),
    )
    .await;
    e.state.stats.record_point(now()).await.unwrap();
    let h = e.get("/v1/admin/metrics/history", &e.admin).await.json();
    let points = h["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(
        (&points[0]["disk"], &points[0]["requests"], &points[0]["upstream"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    assert!(points[1]["disk"]["dbBytes"].as_u64().unwrap() > 0);
    assert!(points[1]["requests"].is_u64() && points[1]["upstream"].is_u64());
}

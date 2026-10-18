//! Analytics end to end (plan P7-04): two recorded matches, a ranked game and
//! a remake on patch 16.19, archived for real, every player on the KR ladder,
//! `aggregate:analytics` run, and v1's three `/v1/lol/analytics/*` routes read
//! (ETag, 304, `?remakes=`). Plus `facts:reextract` and the admin routes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::archive::matches;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::DbError;
use riot_proxy::jobs::analytics::{AnalyticsContext, reextract_if_stale, split_if_needed, stale_matches};
use riot_proxy::jobs::{Job, Queue};
use riot_proxy::ws::Topic;
use riot_proxy::ws::protocol::LADDER;
use serde_json::{Value, json};

const RANKED: &[u8] = include_bytes!("fixtures/matches/ranked-solo.json");
const REMAKE: &[u8] = include_bytes!("fixtures/matches/remake.json");

struct Env {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    reader: String,
    admin: String,
    scope: String,
}

async fn env() -> Env {
    env_with(&[]).await
}

async fn env_with(vars: &[(&str, &str)]) -> Env {
    let (dir, state, router) = common::app_with(vars, "http://127.0.0.1:9");
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
    let scope = state.fetcher.key_scope().as_str().to_string();
    Env {
        _dir: dir,
        state,
        router,
        reader: reader.key.expose().to_string(),
        admin: admin.key.expose().to_string(),
        scope,
    }
}

fn job(kind: &str, payload: Value) -> Job {
    Job {
        id: "j".into(),
        kind: kind.into(),
        dedupe_key: None,
        priority: 30_000,
        payload: payload.to_string(),
        attempts: 1,
        run_after: 0,
    }
}

impl Env {
    fn ctx(&self) -> AnalyticsContext {
        self.ctx_with(4)
    }

    fn ctx_with(&self, patch_limit: u32) -> AnalyticsContext {
        AnalyticsContext {
            cache: Arc::clone(&self.state.analytics),
            queue: Queue::new(self.state.db.clone()),
            hub: self.state.hub.clone(),
            key_scope: self.scope.clone(),
            patch_limit,
            reextract_batch: 1,
            mirror: std::sync::Arc::clone(&self.state.ddragon),
        }
    }

    /// Put every player on the KR solo ladder at MASTER, then archive both
    /// matches, which stamps their tiers (THR-02).
    async fn seed(&self) {
        self.seed_with(|_| "MASTER").await;
    }

    /// [`Env::seed`] with each player's ladder tier from `tier_of`.
    async fn seed_with(&self, tier_of: impl Fn(&str) -> &'static str) {
        for (id, body) in [("KR_8393343196", RANKED), ("KR_8393320187", REMAKE)] {
            self.on_ladder(body, &tier_of).await;
            matches::put(&self.state.db, id, "asia", &self.scope, body.to_vec().into(), 1)
                .await
                .unwrap();
        }
    }

    /// Put `body`'s players on the KR solo ladder, each at `tier_of` their
    /// PUUID; a player already there is moved.
    async fn on_ladder(&self, body: &[u8], tier_of: &impl Fn(&str) -> &'static str) {
        let players: Vec<(String, &'static str)> = serde_json::from_slice::<Value>(body).unwrap()["metadata"]
            ["participants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let p = p.as_str().unwrap();
                (p.to_string(), tier_of(p))
            })
            .collect();
        let scope = self.scope.clone();
        self.state
            .db
            .write(move |c| {
                for (p, tier) in players {
                    c.execute(
                        "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division,
                           league_points, wins, losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
                         VALUES (?1, 'kr', 'RANKED_SOLO_5x5', ?2, ?3, 'I', 100, 1, 1, 'c', 'c', 1)
                         ON CONFLICT (key_scope, platform, queue, puuid) DO UPDATE SET tier = excluded.tier",
                        rusqlite::params![scope, p, tier],
                    )?;
                }
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
    }

    async fn aggregate(&self) {
        self.ctx()
            .aggregate(&job(
                "aggregate:analytics",
                json!({"platform": "kr", "queue": "RANKED_SOLO_5x5"}),
            ))
            .await
            .unwrap();
    }

    async fn call(
        &self,
        verb: &str,
        uri: &str,
        key: &str,
        etag: Option<&str>,
        body: Option<Value>,
    ) -> common::Reply {
        let mut req = Request::builder()
            .method(verb)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"));
        if let Some(t) = etag {
            req = req.header("if-none-match", t);
        }
        let req = req
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    async fn get(&self, uri: &str) -> common::Reply {
        self.call("GET", uri, &self.reader, None, None).await
    }
}

fn error(r: &common::Reply) -> (StatusCode, String) {
    (
        r.status,
        r.json()["error"]["message"].as_str().unwrap().to_string(),
    )
}

/// Stamps are wall-clock: snapshot everything else.
fn redact(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "computedAt" || (k != "sectionsComputedAt" && x.is_string() && k.ends_with("At"))
                    {
                        if x.is_string() {
                            *x = json!("<iso>");
                        }
                    } else {
                        walk(x);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    if let Some(s) = v.get_mut("sectionsComputedAt").and_then(Value::as_object_mut) {
        for x in s.values_mut() {
            if x.is_string() {
                *x = json!("<iso>");
            }
        }
    }
    v
}

#[tokio::test]
async fn before_any_recompute_the_routes_answer_empty_documents() {
    let e = env().await;
    let r = e.get("/v1/lol/analytics/champions").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"platform": null, "queue": "RANKED_SOLO_5x5", "tier": null, "minTier": null, "side": null, "patch": null,
               "role": null, "computedAt": null, "totalGames": 0, "champions": []})
    );
    let d = e.get("/v1/lol/analytics/champions/134").await.json();
    assert_eq!(
        (&d["patch"], &d["computedAt"], &d["stats"], &d["items"]),
        (&Value::Null, &Value::Null, &json!([]), &json!([]))
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions/134/matchups").await.json()["matchups"],
        json!([])
    );
}

#[tokio::test]
async fn the_recompute_feeds_v1s_routes_and_remakes_are_opt_in() {
    let e = env().await;
    e.seed().await;
    let mut ladder = e.state.hub.subscribe(&Topic::named(LADDER));
    e.aggregate().await;

    // analytics.updated, with rows per table (v1).
    let frame: Value = serde_json::from_str(ladder.try_recv().unwrap().as_str()).unwrap();
    assert_eq!(frame["event"], "analytics.updated");
    assert_eq!(
        frame["data"]["tables"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [
            "champion_build_parts",
            "champion_builds",
            "champion_items",
            "champion_matchup_build_parts",
            "champion_matchup_builds",
            "champion_matchups",
            "champion_runes",
            "champion_spells",
            "champion_stats"
        ]
    );

    // The ranked game only, by default: ten champions, one game each.
    let r = e.get("/v1/lol/analytics/champions").await;
    assert_eq!(
        (
            r.headers["cache-control"].to_str().unwrap(),
            r.headers["etag"].to_str().unwrap().starts_with("W/\"")
        ),
        ("private, max-age=300", true)
    );
    let body = r.json();
    assert_eq!(
        (&body["patch"], &body["totalGames"]),
        (&json!("16.19"), &json!(10))
    );
    insta::assert_json_snapshot!("analytics_champions", redact(body.clone()));

    // A matching If-None-Match: 304, empty, with the validator.
    let etag = r.headers["etag"].to_str().unwrap().to_string();
    let again = e
        .call(
            "GET",
            "/v1/lol/analytics/champions",
            &e.reader,
            Some(&format!("W/\"other\", {etag}")),
            None,
        )
        .await;
    assert_eq!((again.status, again.body.len()), (StatusCode::NOT_MODIFIED, 0));
    assert_eq!(again.headers["etag"].to_str().unwrap(), etag);
    assert_eq!(
        e.call("GET", "/v1/lol/analytics/champions", &e.reader, Some("*"), None)
            .await
            .status,
        StatusCode::NOT_MODIFIED
    );

    // With the remake: twenty games, and the ETag moves.
    let with = e.get("/v1/lol/analytics/champions?remakes=include").await;
    assert_eq!(with.json()["totalGames"], 20);
    assert_ne!(with.headers["etag"], r.headers["etag"]);
    let syndra = |v: &Value| {
        v["champions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["championId"] == 134)
            .map(|c| (c["games"].clone(), c["wins"].clone()))
    };
    assert_eq!(syndra(&body), Some((json!(1), json!(1))));
    assert_eq!(syndra(&with.json()), Some((json!(2), json!(1))));

    // Filters: role, tier, minGames, limit.
    assert_eq!(
        e.get("/v1/lol/analytics/champions?role=MIDDLE").await.json()["champions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions?tier=IRON").await.json()["champions"],
        json!([])
    );
    // UNKNOWN, players no ladder or lookup placed, is a tier a read can ask for (ADR-105).
    assert_eq!(
        e.get("/v1/lol/analytics/champions?tier=UNKNOWN").await.status,
        StatusCode::OK
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions?minGames=2").await.json()["champions"],
        json!([])
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions?minGames=2&remakes=include")
            .await
            .json()["champions"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "126 and 134 played both games"
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions?limit=3").await.json()["champions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // The detail composite and the matchups.
    let detail = e.get("/v1/lol/analytics/champions/134").await;
    assert_eq!(detail.headers["cache-control"], "private, max-age=300");
    insta::assert_json_snapshot!("analytics_champion_detail", redact(detail.json()));
    let matchups = e
        .get("/v1/lol/analytics/champions/134/matchups?remakes=include")
        .await
        .json();
    assert_eq!(
        matchups["matchups"],
        json!([{"role": "MIDDLE", "opponentId": 13, "games": 1, "wins": 0, "winRate": 0},
               {"role": "MIDDLE", "opponentId": 157, "games": 1, "wins": 1, "winRate": 1}])
    );
}

/// SITE-11: the API serves every row it has. A floor is the caller's
/// `minGames`, and a leftover `AGGREGATE_MIN_GAMES` no longer sets one.
#[tokio::test]
async fn without_min_games_every_row_is_served_even_a_one_game_one() {
    let e = env_with(&[("AGGREGATE_MIN_GAMES", "10")]).await;
    e.seed().await;
    e.seed_builds().await;
    e.aggregate().await;

    // Ten champions, one game each.
    let list = e.get("/v1/lol/analytics/champions").await.json();
    let champions = list["champions"].as_array().unwrap();
    assert_eq!(champions.len(), 10);
    assert!(champions.iter().all(|c| c["games"] == 1));

    // The detail: the stat row, its tier row and every build list.
    let d = e.get("/v1/lol/analytics/champions/134").await.json();
    assert_eq!(
        (&d["stats"][0]["games"], &d["totalGames"]),
        (&json!(1), &json!(1))
    );
    for section in ["byTier", "matchups", "items", "runes", "spells"] {
        let rows = d[section].as_array().unwrap();
        assert!(!rows.is_empty(), "{section} is empty");
        assert!(rows.iter().all(|r| r["games"] == 1), "{section}: {rows:?}");
    }

    // A set build played once.
    let b = e.get("/v1/lol/analytics/champions/134/builds").await.json();
    assert_eq!(
        (&b["builds"][0]["core"], &b["builds"][0]["games"]),
        (&json!([6655, 3157]), &json!(1))
    );

    // The caller's floor still applies.
    let floored = e.get("/v1/lol/analytics/champions/134?minGames=2").await.json();
    for section in ["stats", "byTier", "matchups", "items", "runes", "spells"] {
        assert_eq!(floored[section], json!([]), "{section}");
    }
}

#[tokio::test]
async fn analytics_routes_validate_as_v1_did() {
    let e = env().await;
    for (uri, message) in [
        (
            "/v1/lol/analytics/champions?tier=master",
            "querystring/tier must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/analytics/champions?patch=16",
            "querystring/patch must NOT have fewer than 3 characters",
        ),
        (
            "/v1/lol/analytics/champions?limit=501",
            "querystring/limit must be <= 500",
        ),
        (
            "/v1/lol/analytics/champions?minGames=-1",
            "querystring/minGames must be >= 0",
        ),
        (
            "/v1/lol/analytics/champions?queue=ARAM",
            "querystring/queue must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/analytics/champions?remakes=yes",
            "querystring/remakes must be equal to one of the allowed values",
        ),
        ("/v1/lol/analytics/champions/0", "params/championId must be >= 1"),
        (
            "/v1/lol/analytics/champions/abc/matchups",
            "params/championId must be integer",
        ),
        // A matchup is always in a lane (v1 LANE_POSITIONS).
        (
            "/v1/lol/analytics/champions/1/matchups?role=",
            "querystring/role must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/analytics/champions/1/matchups?limit=201",
            "querystring/limit must be <= 200",
        ),
        (
            "/v1/lol/analytics/champions/1?limit=51",
            "querystring/limit must be <= 50",
        ),
    ] {
        assert_eq!(
            error(&e.get(uri).await),
            (StatusCode::BAD_REQUEST, message.into()),
            "{uri}"
        );
    }
    assert_eq!(
        e.get("/v1/lol/analytics/champions?platform=xx1").await.json()["error"]["code"],
        "BAD_REGION"
    );
    // The roleless rows are their own role on the champion list.
    assert_eq!(
        e.get("/v1/lol/analytics/champions?role=").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reextract_rederives_stale_facts_and_stops_when_none_are_left() {
    let e = env().await;
    e.seed().await;
    assert_eq!(
        stale_matches(&e.state.db).await.unwrap(),
        0,
        "archived at the current version"
    );
    // Two matches archived by an older version: no gold, no remake flag, no bans.
    e.state
        .db
        .write(|c| {
            c.execute_batch(
                "UPDATE matches SET facts_version = 2, remake = NULL, game_duration = NULL;
                 UPDATE match_facts SET gold = NULL, damage = NULL, vision = NULL, facts_version = 2,
                   runes = '{\"keystone\":8010,\"subStyle\":8400}', tier = 'MASTER';
                 DELETE FROM match_bans;",
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    let queue = Queue::new(e.state.db.clone());
    assert!(reextract_if_stale(&queue).await.unwrap());
    assert!(!reextract_if_stale(&queue).await.unwrap(), "one sweep at a time");

    // Batches of one: the sweep walks both and needs no cursor.
    e.ctx()
        .reextract(&job("facts:reextract", json!({})))
        .await
        .unwrap();
    assert_eq!(stale_matches(&e.state.db).await.unwrap(), 0);
    let (gold, remakes, bans, durations, objects, stamped): (i64, i64, i64, i64, i64, i64) = e
        .state
        .db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT (SELECT COUNT(*) FROM match_facts WHERE gold IS NOT NULL AND facts_version = 4),
                        (SELECT COUNT(*) FROM matches WHERE remake = 1),
                        (SELECT COUNT(*) FROM match_bans),
                        (SELECT COUNT(*) FROM matches WHERE game_duration IS NOT NULL),
                        (SELECT COUNT(*) FROM match_facts WHERE json_type(runes) = 'object'),
                        (SELECT COUNT(*) FROM match_facts WHERE tier = 'MASTER')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?)
        })
        .await
        .unwrap();
    // Every row is rewritten in the version-4 shape (DSK-05: no keyed object
    // is left) and keeps the stamp the old table would have held (DSK-04).
    assert_eq!(
        (gold, remakes, bans, durations, objects, stamped),
        (20, 1, 16, 2, 0, 20)
    );
}

#[tokio::test]
async fn the_admin_routes_queue_a_recompute_and_a_sweep() {
    let e = env().await;
    let r = e
        .call(
            "POST",
            "/v1/admin/analytics/recompute",
            &e.admin,
            None,
            Some(json!({"platform": "kr", "queue": "RANKED_FLEX_SR"})),
        )
        .await;
    assert_eq!(
        (r.status, r.json()),
        (
            StatusCode::ACCEPTED,
            json!({"ok": true, "platform": "kr", "queue": "RANKED_FLEX_SR"})
        )
    );
    let row = |kind: &'static str| {
        let db = e.state.db.clone();
        async move {
            db.read(move |c| {
                Ok::<_, DbError>(c.query_row(
                    "SELECT dedupe_key || ' ' || priority FROM jobs WHERE kind = ?1",
                    [kind],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap()
        }
    };
    // Asked for by hand, it goes ahead of the queue (DEV-18).
    assert_eq!(row("aggregate:analytics").await, "kr:RANKED_FLEX_SR 0");
    assert_eq!(
        error(
            &e.call(
                "POST",
                "/v1/admin/analytics/recompute",
                &e.admin,
                None,
                Some(json!({"platform": "kr", "queue": "ARAM"}))
            )
            .await
        )
        .1,
        "body/queue must be equal to one of the allowed values"
    );
    // No default platform (ADR-065): the body has to name one.
    assert_eq!(
        error(
            &e.call(
                "POST",
                "/v1/admin/analytics/recompute",
                &e.admin,
                None,
                Some(json!({"queue": "RANKED_SOLO_5x5"}))
            )
            .await
        ),
        (
            StatusCode::BAD_REQUEST,
            "body must have required property 'platform'".into()
        )
    );

    let r = e
        .call("POST", "/v1/admin/analytics/reextract", &e.admin, None, None)
        .await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::ACCEPTED, json!({"ok": true, "stale": 0}))
    );
    assert_eq!(row("facts:reextract").await, "facts:reextract 30000");
    for uri in ["/v1/admin/analytics/recompute", "/v1/admin/analytics/reextract"] {
        assert_eq!(
            e.call("POST", uri, &e.reader, None, None).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    let _ = Arc::new(());
}

/// DEV-18: a recompute asked for by hand runs on the next free worker, not
/// after the crawl's downloads; a rebuild the crawl already queued moves up.
#[tokio::test]
async fn a_manual_recompute_goes_ahead_of_the_queue() {
    use riot_proxy::db::store::{SqliteStore, Store};
    use riot_proxy::jobs::NewJob;
    use riot_proxy::jobs::analytics::enqueue_aggregate;
    let e = env().await;
    let q = &e.state.jobs;
    for i in 0..3 {
        q.enqueue(NewJob::new(
            "archive:match",
            105,
            json!({"matchId": format!("KR_{i}")}),
        ))
        .await
        .unwrap();
    }
    q.enqueue(NewJob::new("ladder:walk", 20_000, json!({})))
        .await
        .unwrap();
    let crawl_end = enqueue_aggregate(q, "kr", "RANKED_SOLO_5x5").await.unwrap();

    let r = e
        .call(
            "POST",
            "/v1/admin/analytics/recompute",
            &e.admin,
            None,
            Some(json!({"platform": "kr", "queue": "RANKED_SOLO_5x5"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let first = SqliteStore::new(e.state.db.clone())
        .claim_job(i64::MAX, &riot_proxy::db::store::ClaimFilter::open())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (first.kind.as_str(), first.id.as_str(), first.priority),
        ("aggregate:analytics", crawl_end.id.as_str(), 0)
    );
}

/// ADR-065: no platform sums every ladder; `?platform=` narrows to one.
#[tokio::test]
async fn without_a_platform_the_routes_sum_every_platform() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    // The same aggregates again under na1, as if its ladder had been crawled too.
    e.state
        .db
        .write(|c| {
            for table in [
                "champion_stats",
                "champion_stats_totals",
                "champion_bans",
                "champion_ban_totals",
                "analytics_slices",
                "analytics_match_totals",
                "champion_matchups",
                "champion_items",
                "champion_runes",
                "champion_spells",
            ] {
                c.execute_batch(&format!(
                    "CREATE TEMP TABLE copy AS SELECT * FROM {table} WHERE platform = 'kr';
                     UPDATE copy SET platform = 'na1';
                     INSERT INTO {table} SELECT * FROM copy;
                     DROP TABLE copy;"
                ))?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();

    let all = e.get("/v1/lol/analytics/champions").await.json();
    let kr = e.get("/v1/lol/analytics/champions?platform=kr").await.json();
    let na = e.get("/v1/lol/analytics/champions?platform=na1").await.json();
    assert_eq!(
        (&all["platform"], &kr["platform"], &na["platform"]),
        (&json!(null), &json!("kr"), &json!("na1"))
    );
    assert_eq!(
        (&all["totalGames"], &kr["totalGames"], &na["totalGames"]),
        (&json!(20), &json!(10), &json!(10))
    );
    assert_eq!(
        all["champions"][0]["games"],
        kr["champions"][0]["games"].as_i64().unwrap() * 2
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions?platform=euw1").await.json()["totalGames"],
        0
    );
}

#[tokio::test]
async fn patch_all_sums_every_patch_and_the_patch_list_offers_them() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    // The same aggregates again on 16.18, as if an older patch had been archived too.
    e.state
        .db
        .write(|c| {
            for table in [
                "champion_stats",
                "champion_stats_totals",
                "champion_bans",
                "champion_ban_totals",
                "analytics_slices",
                "analytics_match_totals",
                "champion_matchups",
                "champion_items",
                "champion_runes",
                "champion_spells",
            ] {
                c.execute_batch(&format!(
                    "CREATE TEMP TABLE copy AS SELECT * FROM {table} WHERE patch = '16.19';
                     UPDATE copy SET patch = '16.18';
                     INSERT INTO {table} SELECT * FROM copy;
                     DROP TABLE copy;"
                ))?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    // Written behind the rebuild's back: drop what it warmed, as it would (PERF-03).
    e.state.analytics.clear();

    let newest = e.get("/v1/lol/analytics/champions").await.json();
    let all = e.get("/v1/lol/analytics/champions?patch=all").await.json();
    assert_eq!(
        (&newest["patch"], &all["patch"]),
        (&json!("16.19"), &json!("all"))
    );
    assert_eq!(
        (&newest["totalGames"], &all["totalGames"]),
        (&json!(10), &json!(20))
    );
    let first = &all["champions"][0];
    assert_eq!(first["patch"], "all");
    assert_eq!(
        first["games"],
        newest["champions"][0]["games"].as_i64().unwrap() * 2
    );
    // Matches per tier are summed too, so a rate over both patches stays put.
    assert_eq!(first["pickRate"], newest["champions"][0]["pickRate"]);

    let id = first["championId"].as_i64().unwrap();
    let d = e
        .get(&format!("/v1/lol/analytics/champions/{id}?patch=all"))
        .await
        .json();
    let d1 = e.get(&format!("/v1/lol/analytics/champions/{id}")).await.json();
    assert_eq!(
        (&d["patch"], &d["stats"][0]["patch"]),
        (&json!("all"), &json!("all"))
    );
    assert_eq!(d["totalGames"], d1["totalGames"].as_i64().unwrap() * 2);
    assert_eq!(
        d["items"][0]["games"],
        d1["items"][0]["games"].as_i64().unwrap() * 2
    );
    let m = e
        .get(&format!("/v1/lol/analytics/champions/{id}/matchups?patch=all"))
        .await
        .json();
    assert_eq!(m["patch"], "all");
    assert_eq!(m["matchups"][0]["games"], 2);

    // The patch list: newest first, games as totalGames counts them.
    let r = e.get("/v1/lol/analytics/patches?platform=kr").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "private, max-age=300");
    let body = redact(r.json());
    assert_eq!(
        body,
        json!({"platform": "kr", "queue": "RANKED_SOLO_5x5", "championId": null, "side": null, "patches": [
            {"patch": "16.19", "games": 10, "computedAt": "<iso>"},
            {"patch": "16.18", "games": 10, "computedAt": "<iso>"}]})
    );
    let tag = r.headers["etag"].to_str().unwrap().to_string();
    let again = e
        .call(
            "GET",
            "/v1/lol/analytics/patches?platform=kr",
            &e.reader,
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    // One champion's list counts its games only: one ranked game per patch (ADR-100).
    let r = e
        .get(&format!("/v1/lol/analytics/patches?platform=kr&championId={id}"))
        .await;
    assert_ne!(
        r.headers["etag"].to_str().unwrap(),
        tag,
        "the champion is in the ETag"
    );
    assert_eq!(
        redact(r.json()),
        json!({"platform": "kr", "queue": "RANKED_SOLO_5x5", "championId": id, "side": null, "patches": [
            {"patch": "16.19", "games": 1, "computedAt": "<iso>"},
            {"patch": "16.18", "games": 1, "computedAt": "<iso>"}]})
    );
    assert_eq!(
        e.get("/v1/lol/analytics/patches?platform=kr&championId=99999")
            .await
            .json()["patches"],
        json!([])
    );
    assert_eq!(
        error(&e.get("/v1/lol/analytics/patches?championId=0").await),
        (
            StatusCode::BAD_REQUEST,
            "querystring/championId must be >= 1".into()
        )
    );
    // The remake adds its players back in.
    let with = e.get("/v1/lol/analytics/patches?remakes=include").await.json();
    assert_eq!(with["platform"], Value::Null);
    assert!(with["patches"][0]["games"].as_i64().unwrap() > 10, "{with}");
    assert_eq!(
        e.get("/v1/lol/analytics/patches?platform=na1").await.json()["patches"],
        json!([])
    );
    assert_eq!(
        error(&e.get("/v1/lol/analytics/patches?queue=NORMAL").await).0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        error(&e.get("/v1/lol/analytics/champions?patch=every").await),
        (
            StatusCode::BAD_REQUEST,
            "querystring/patch must match pattern \"^[0-9]+\\.[0-9]+$\"".into()
        )
    );
}

/// SITE-08 (ADR-123): without `tier`, one row per champion summed over every
/// tier, its pick and ban rates over every match in the slice, each counted
/// once. The ranked game's blue side is moved to GRANDMASTER, so that game is
/// in two tiers' slices; the remake's players are all MASTER.
#[tokio::test]
async fn without_a_tier_each_champion_is_one_row_over_every_match() {
    let e = env().await;
    let blue: Vec<String> = serde_json::from_slice::<Value>(RANKED).unwrap()["info"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["teamId"] == 100)
        .map(|p| p["puuid"].as_str().unwrap().to_string())
        .collect();
    e.seed_with(move |p| {
        if blue.iter().any(|b| b == p) {
            "GRANDMASTER"
        } else {
            "MASTER"
        }
    })
    .await;
    e.aggregate().await;

    // The owner's query: each champion at most once, `tier: null`.
    let r = e
        .get("/v1/lol/analytics/champions?patch=all&minGames=0&limit=500&remakes=include")
        .await;
    let body = r.json();
    let rows = body["champions"].as_array().unwrap();
    let mut ids: Vec<i64> = rows.iter().map(|c| c["championId"].as_i64().unwrap()).collect();
    assert_eq!(ids.len(), 18, "ten champions a game, 126 and 134 in both");
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 18, "no champion twice");
    assert!(rows.iter().all(|c| c["tier"].is_null()), "{body}");
    assert_eq!(body["totalGames"], 20);
    let row = |v: &Value, id: i64| {
        v["champions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["championId"] == id)
            .map(|c| (c["games"].clone(), c["pickRate"].clone(), c["banRate"].clone()))
    };
    // Two matches in the slice, though the per-tier slices hold three (the
    // ranked game in MASTER and GRANDMASTER). 157 played the ranked game and
    // was banned in the remake; 64 the reverse, its ranked-game ban in both
    // tiers' rows but counted once here; 134 played both.
    assert_eq!(row(&body, 157), Some((json!(1), json!(0.5), json!(0.5))));
    assert_eq!(row(&body, 64), Some((json!(1), json!(0.5), json!(0.5))));
    assert_eq!(row(&body, 134), Some((json!(2), json!(1), json!(0))));
    // limit and minGames count champions.
    let limited = e
        .get("/v1/lol/analytics/champions?patch=all&minGames=0&limit=5&remakes=include")
        .await
        .json();
    assert_eq!(limited["champions"].as_array().unwrap().len(), 5);
    let min2 = e
        .get("/v1/lol/analytics/champions?minGames=2&remakes=include")
        .await
        .json();
    let min2_ids: Vec<&Value> = min2["champions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["championId"])
        .collect();
    assert_eq!(min2_ids, [&json!(126), &json!(134)]);

    // A named tier: that tier's rows, rates over its own matches, and its own
    // validator.
    let gm = e
        .get("/v1/lol/analytics/champions?tier=GRANDMASTER&remakes=include")
        .await;
    assert_ne!(gm.headers["etag"], r.headers["etag"]);
    let gm = gm.json();
    assert_eq!(gm["champions"].as_array().unwrap().len(), 5);
    assert!(
        gm["champions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["tier"] == "GRANDMASTER")
    );
    assert_eq!(row(&gm, 157), Some((json!(1), json!(1), json!(0))));
    let master = e
        .get("/v1/lol/analytics/champions?tier=MASTER&remakes=include")
        .await
        .json();
    assert_eq!(row(&master, 64), Some((json!(1), json!(0.5), json!(0.5))));

    // The detail: `stats` the summed row, `byTier` a row per tier.
    let d = e
        .get("/v1/lol/analytics/champions/157?remakes=include")
        .await
        .json();
    assert_eq!(d["stats"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            &d["stats"][0]["tier"],
            &d["stats"][0]["pickRate"],
            &d["stats"][0]["banRate"]
        ),
        (&Value::Null, &json!(0.5), &json!(0.5))
    );
    assert_eq!(d["totalGames"], 1);
    let by_tier = d["byTier"].as_array().unwrap();
    assert_eq!(by_tier.len(), 1);
    assert_eq!(
        (
            &by_tier[0]["tier"],
            &by_tier[0]["games"],
            &by_tier[0]["pickRate"],
            &by_tier[0]["banRate"]
        ),
        (&json!("GRANDMASTER"), &json!(1), &json!(1), &json!(0))
    );
    // Averages over the same game are the same, summed or not.
    for k in ["avgKda", "avgDamage", "avgVision", "csPerMin", "goldPerMin"] {
        assert_eq!(d["stats"][0][k], d["byTier"][0][k], "{k}");
    }
    // A named tier: both hold that tier's row.
    let d = e
        .get("/v1/lol/analytics/champions/64?remakes=include&tier=MASTER")
        .await
        .json();
    assert_eq!(d["stats"], d["byTier"]);
    assert_eq!(
        (&d["stats"][0]["tier"], &d["stats"][0]["banRate"]),
        (&json!("MASTER"), &json!(0.5))
    );
}

#[tokio::test]
async fn a_recompute_refreshes_planner_statistics_first() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    // ADR-102: the archive grew from nothing since the open, so the rebuild
    // ran with statistics for it.
    let analysed: Vec<String> = e
        .state
        .db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT tbl FROM sqlite_stat1 WHERE tbl IN ('match_facts', 'ladder_entries') ORDER BY tbl",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .unwrap();
    assert_eq!(analysed, ["ladder_entries", "match_facts"]);
}

impl Env {
    /// Build facts (BLD-01's `match_builds`) for three players of the ranked
    /// game, as `builds:extract` would write them; the harness mirrors no
    /// item.json, so the recompute's extraction step leaves them alone.
    async fn seed_builds(&self) {
        let players = serde_json::from_slice::<Value>(RANKED).unwrap()["info"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["championId"].as_i64().unwrap(),
                    p["puuid"].as_str().unwrap().to_string(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let rows = [
            // 134 (MIDDLE, win): a full build.
            (
                134,
                "[1056,2003,2003]",
                Some(3020),
                "[6655,3157,3089,4645]",
                Some("QEW"),
            ),
            // 157 (MIDDLE, loss): another champion's build.
            (157, "[1055,2003]", Some(3006), "[6672,3031,3036]", Some("QEW")),
            // 126 (TOP, win): one finished item, so no build.
            (126, "[1054]", None, "[6692]", None),
        ]
        .map(|(champ, starter, boots, items, order)| (players[&champ].clone(), starter, boots, items, order));
        let scope = self.scope.clone();
        self.state
            .db
            .write(move |c| {
                for (puuid, starter, boots, items, order) in rows {
                    c.execute(
                        "INSERT INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills,
                           skill_order, builds_version) VALUES ('KR_8393343196', ?1, ?2, ?3, ?4, ?5, '', ?6, 1)",
                        rusqlite::params![scope, puuid, starter, boots, items, order],
                    )?;
                }
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_builds_route_reads_set_builds_in_the_most_played_role() {
    let e = env().await;
    // Before any recompute: 200, no patch, no role, no builds.
    assert_eq!(
        e.get("/v1/lol/analytics/champions/134/builds").await.json(),
        json!({"championId": 134, "platform": null, "queue": "RANKED_SOLO_5x5", "tier": null, "minTier": null, "side": null,
               "patch": null, "role": null, "opponentId": null, "computedAt": null, "totalGames": 0, "builds": []})
    );
    e.seed().await;
    e.seed_builds().await;
    e.aggregate().await;

    // No role: 134's most-played role, MIDDLE, named in the response.
    let r = e.get("/v1/lol/analytics/champions/134/builds").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "private, max-age=300");
    let body = r.json();
    assert_eq!(body["role"], "MIDDLE");
    assert!(body["computedAt"].is_string());
    insta::assert_json_snapshot!("analytics_champion_builds", redact(body));

    // A matching If-None-Match: 304 with the validator.
    let tag = r.headers["etag"].to_str().unwrap().to_string();
    let again = e
        .call(
            "GET",
            "/v1/lol/analytics/champions/134/builds",
            &e.reader,
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert_eq!(again.headers["etag"].to_str().unwrap(), tag);
    // Another role has its own (empty) builds, and its own validator.
    let top = e.get("/v1/lol/analytics/champions/134/builds?role=TOP").await;
    assert_eq!(
        (
            &top.json()["role"],
            &top.json()["builds"],
            &top.json()["totalGames"]
        ),
        (&json!("TOP"), &json!([]), &json!(0))
    );
    assert_ne!(top.headers["etag"].to_str().unwrap(), tag);

    // With the remake, 134 has one MIDDLE game and one roleless one: the lane wins the tie.
    assert_eq!(
        e.get("/v1/lol/analytics/champions/134/builds?remakes=include")
            .await
            .json()["role"],
        "MIDDLE"
    );
    // minGames counts a build's games; every patch sums.
    assert_eq!(
        e.get("/v1/lol/analytics/champions/134/builds?minGames=2")
            .await
            .json()["builds"],
        json!([])
    );
    assert_eq!(
        e.get("/v1/lol/analytics/champions/134/builds?patch=all")
            .await
            .json()["builds"][0]["core"],
        json!([6655, 3157])
    );
    // A player with one finished item is in no build; a champion nobody played has no role.
    let one = e.get("/v1/lol/analytics/champions/126/builds").await.json();
    assert_eq!((&one["role"], &one["builds"]), (&json!("TOP"), &json!([])));
    let nobody = e.get("/v1/lol/analytics/champions/999/builds").await.json();
    assert_eq!((&nobody["role"], &nobody["builds"]), (&Value::Null, &json!([])));

    // Validation as the detail route, with builds' own limit.
    for (uri, message) in [
        (
            "/v1/lol/analytics/champions/0/builds",
            "params/championId must be >= 1",
        ),
        (
            "/v1/lol/analytics/champions/abc/builds",
            "params/championId must be integer",
        ),
        (
            "/v1/lol/analytics/champions/134/builds?limit=11",
            "querystring/limit must be <= 10",
        ),
        (
            "/v1/lol/analytics/champions/134/builds?limit=0",
            "querystring/limit must be >= 1",
        ),
        (
            "/v1/lol/analytics/champions/134/builds?role=SUPPORT",
            "querystring/role must be equal to one of the allowed values",
        ),
        (
            "/v1/lol/analytics/champions/134/builds?patch=16",
            "querystring/patch must NOT have fewer than 3 characters",
        ),
        (
            "/v1/lol/analytics/champions/134/builds?remakes=yes",
            "querystring/remakes must be equal to one of the allowed values",
        ),
    ] {
        assert_eq!(
            error(&e.get(uri).await),
            (StatusCode::BAD_REQUEST, message.into()),
            "{uri}"
        );
    }
}

/// THR-02 (ADR-127): a game stays in the tier its players had when it was
/// archived. Promoted after one game, every player's next game counts at the
/// new tier and the first stays at the old one, through every rebuild.
#[tokio::test]
async fn a_promotion_leaves_earlier_games_in_their_tier() {
    let e = env().await;
    e.on_ladder(RANKED, &|_| "MASTER").await;
    matches::put(
        &e.state.db,
        "KR_8393343196",
        "asia",
        &e.scope,
        RANKED.to_vec().into(),
        1,
    )
    .await
    .unwrap();
    e.aggregate().await;
    // The same ten players again, a game later, after a promotion.
    e.on_ladder(RANKED, &|_| "GRANDMASTER").await;
    let mut again: Value = serde_json::from_slice(RANKED).unwrap();
    again["metadata"]["matchId"] = json!("KR_1");
    matches::put(
        &e.state.db,
        "KR_1",
        "asia",
        &e.scope,
        serde_json::to_vec(&again).unwrap().into(),
        2,
    )
    .await
    .unwrap();
    for _ in 0..2 {
        e.aggregate().await;
        let tiers = e
            .state
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT tier, matches FROM analytics_slices WHERE platform = 'kr' ORDER BY tier",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap();
        assert_eq!(tiers, [("GRANDMASTER".to_string(), 1), ("MASTER".to_string(), 1)]);
    }
    let gm = e
        .get("/v1/lol/analytics/champions?tier=GRANDMASTER&minGames=0")
        .await
        .json();
    assert_eq!(gm["champions"].as_array().unwrap().len(), 10);
    let master = e
        .get("/v1/lol/analytics/champions?tier=MASTER&minGames=0")
        .await
        .json();
    assert_eq!(master["champions"].as_array().unwrap().len(), 10);
}

/// V0017 (FLT-01, ADR-129) copies the rows it can't split by tier and side
/// with both '' and lists their ladder in `analytics_unsplit`. The routes
/// answer as before, since every read sums over both. Boot queues a rebuild
/// for the ladder, and that rebuild covers every patch whatever
/// `AGGREGATE_PATCH_LIMIT` says: an older patch's rows are split too, then
/// the ladder leaves the list and later rebuilds keep to the limit.
#[tokio::test]
async fn rows_v0017_could_not_split_are_rebuilt_over_every_patch_once() {
    let e = env().await;
    e.seed().await;
    e.seed_builds().await;
    // The remake on an older patch, so a one-patch rebuild leaves it alone.
    e.state
        .db
        .write(|c| {
            c.execute(
                "UPDATE matches SET patch = '16.18' WHERE match_id = 'KR_8393320187'",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    let ladder = json!({"platform": "kr", "queue": "RANKED_SOLO_5x5"});
    e.ctx_with(0)
        .aggregate(&job("aggregate:analytics", ladder.clone()))
        .await
        .unwrap();
    let uris = [
        "/v1/lol/analytics/champions?patch=all&remakes=include",
        "/v1/lol/analytics/champions/134?patch=all&remakes=include",
        "/v1/lol/analytics/champions/134/matchups?patch=all&remakes=include",
        "/v1/lol/analytics/champions/134/builds?patch=all&remakes=include",
        "/v1/lol/analytics/patches?remakes=include",
    ];
    let read = || async {
        let mut out = vec![];
        for uri in uris {
            out.push(redact(e.get(uri).await.json()));
        }
        out
    };
    let before = read().await;
    assert!(
        !before[1]["matchups"].as_array().unwrap().is_empty(),
        "{:#}",
        before[1]
    );
    assert!(
        !before[3]["builds"].as_array().unwrap().is_empty(),
        "{:#}",
        before[3]
    );

    // What V0017 leaves behind. Each champion plays once per match, and the
    // two matches differ in patch, so no two rows collapse into one key.
    let unsplit = |db: riot_proxy::db::Db| async move {
        db.read(|c| {
            let mut n = 0;
            for t in [
                "champion_matchups",
                "champion_items",
                "champion_runes",
                "champion_spells",
                "champion_builds",
                "champion_build_parts",
            ] {
                n += c.query_row(
                    &format!("SELECT count(*) FROM {t} WHERE tier = '' OR side = ''"),
                    [],
                    |r| r.get::<_, i64>(0),
                )?;
            }
            n += c.query_row("SELECT count(*) FROM champion_stats WHERE side = ''", [], |r| {
                r.get::<_, i64>(0)
            })?;
            let listed: i64 = c.query_row("SELECT count(*) FROM analytics_unsplit", [], |r| r.get(0))?;
            Ok::<_, DbError>((n, listed))
        })
        .await
        .unwrap()
    };
    let scope = e.scope.clone();
    e.state
        .db
        .write(move |c| {
            for t in [
                "champion_matchups",
                "champion_items",
                "champion_runes",
                "champion_spells",
                "champion_builds",
                "champion_build_parts",
            ] {
                c.execute(&format!("UPDATE {t} SET tier = '', side = ''"), [])?;
            }
            c.execute("UPDATE champion_stats SET side = ''", [])?;
            c.execute(
                "INSERT INTO analytics_unsplit (key_scope, platform, queue) VALUES (?1, 'kr', 'RANKED_SOLO_5x5')",
                [scope],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    // Written behind the rebuild's back: drop what was cached, so the reads
    // below are of the rewritten rows (PERF-03).
    e.state.analytics.clear();
    let (rows, listed) = unsplit(e.state.db.clone()).await;
    assert!(rows > 0);
    assert_eq!(listed, 1);
    assert_eq!(read().await, before, "every read sums over tier and side");

    // Boot queues the ladder's rebuild, once.
    let queue = Queue::new(e.state.db.clone());
    assert_eq!(split_if_needed(&queue, &e.scope).await.unwrap(), 1);
    assert_eq!(
        split_if_needed(&queue, &e.scope).await.unwrap(),
        0,
        "deduplicated"
    );

    // A one-patch rebuild still splits 16.18's rows.
    e.ctx_with(1)
        .aggregate(&job("aggregate:analytics", ladder.clone()))
        .await
        .unwrap();
    assert_eq!(unsplit(e.state.db.clone()).await, (0, 0));
    assert_eq!(read().await, before);

    // Off the list, the limit holds again: 16.18's rows are left as they are.
    let stamp = |db: riot_proxy::db::Db| async move {
        db.read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT max(computed_at) FROM champion_stats WHERE patch = '16.18'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap()
    };
    let older = stamp(e.state.db.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    e.ctx_with(1)
        .aggregate(&job("aggregate:analytics", ladder))
        .await
        .unwrap();
    assert_eq!(stamp(e.state.db.clone()).await, older);
    assert_eq!(read().await, before);
}

/// FLT-02 (ADR-130): `tier` and `side` slice every analytics route. The
/// ranked game's players alternate between Master and Grandmaster, so its
/// rows split by tier and, independently, by side. Every route's tier
/// slices, and its sides, add up to its total; a route's own tier and side
/// are named in the body and the `ETag`; any other side is the routes' usual
/// 400.
#[tokio::test]
async fn tier_and_side_slice_every_route_and_add_up_to_the_total() {
    let e = env().await;
    let players: Vec<String> = serde_json::from_slice::<Value>(RANKED).unwrap()["metadata"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    let master: Vec<String> = players.iter().step_by(2).cloned().collect();
    e.seed_with(move |p| {
        if master.iter().any(|m| m == p) {
            "MASTER"
        } else {
            "GRANDMASTER"
        }
    })
    .await;
    e.seed_builds().await;
    e.aggregate().await;

    let sum = |v: &Value, key: &str| -> i64 {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["games"].as_i64().unwrap())
            .sum()
    };
    // Each route's games in one slice: (champions, detail stats, detail
    // matchups, detail items, matchups, builds, patches).
    let games = |filter: &'static str| {
        let e = &e;
        async move {
            let list = e
                .get(&format!("/v1/lol/analytics/champions?patch=all{filter}"))
                .await
                .json();
            let detail = e
                .get(&format!("/v1/lol/analytics/champions/134?patch=all{filter}"))
                .await
                .json();
            let matchups = e
                .get(&format!(
                    "/v1/lol/analytics/champions/134/matchups?patch=all{filter}"
                ))
                .await
                .json();
            let builds = e
                .get(&format!(
                    "/v1/lol/analytics/champions/134/builds?patch=all&role=MIDDLE{filter}"
                ))
                .await
                .json();
            let side = if filter.contains("side") { filter } else { "" };
            let patches = e
                .get(&format!("/v1/lol/analytics/patches?championId=134{side}"))
                .await
                .json();
            // Every body names the slice it is.
            for body in [&list, &detail, &matchups, &builds] {
                let named = |key: &str, v: &Value| {
                    filter
                        .split('&')
                        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
                        .map_or(Value::Null, |x| json!(x))
                        == *v
                };
                assert!(
                    named("tier", &body["tier"]) && named("side", &body["side"]),
                    "{filter}: {body}"
                );
            }
            [
                list["totalGames"].as_i64().unwrap(),
                detail["stats"].get(0).map_or(0, |s| s["games"].as_i64().unwrap()),
                sum(&detail, "matchups"),
                sum(&detail, "items"),
                sum(&matchups, "matchups"),
                builds["totalGames"].as_i64().unwrap(),
                sum(&patches, "patches"),
            ]
        }
    };
    let all = games("").await;
    assert!(all.iter().all(|n| *n > 0), "{all:?}");
    let add = |a: [i64; 7], b: [i64; 7]| std::array::from_fn::<i64, 7, _>(|i| a[i] + b[i]);
    let (m, gm) = (games("&tier=MASTER").await, games("&tier=GRANDMASTER").await);
    // The patch list takes no tier: each tier's call reads every tier.
    assert_eq!(&add(m, gm)[..6], &all[..6], "tiers: {m:?} + {gm:?}");
    assert_ne!(m, gm, "the tiers differ");
    let (blue, red) = (games("&side=blue").await, games("&side=red").await);
    assert_eq!(add(blue, red), all, "sides: {blue:?} + {red:?}");
    assert_ne!(blue, red, "the sides differ");

    // A side is part of every ETag.
    for uri in [
        "/v1/lol/analytics/champions",
        "/v1/lol/analytics/champions/134",
        "/v1/lol/analytics/champions/134/matchups",
        "/v1/lol/analytics/champions/134/builds",
        "/v1/lol/analytics/patches",
    ] {
        let both = e.get(uri).await;
        let sep = if uri.contains('?') { '&' } else { '?' };
        let one = e.get(&format!("{uri}{sep}side=blue")).await;
        assert_ne!(both.headers["etag"], one.headers["etag"], "{uri}");
        let bad = e.get(&format!("{uri}{sep}side=green")).await;
        assert_eq!(
            error(&bad),
            (
                StatusCode::BAD_REQUEST,
                "querystring/side must be equal to one of the allowed values".to_string()
            ),
            "{uri}"
        );
        // The routes' usual 400, as a bad `tier` or `remakes` gets.
        assert_eq!(bad.json()["error"]["code"], "VALIDATION", "{uri}");
    }
}

// ─── PERF-03: responses cached per aggregate generation (ADR-169) ───────────

/// The site's call (ninjagoldfinch.lol), warmed by every rebuild.
const SITE: &str = "/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&patch=all&minGames=0&limit=500";

fn x_cache(r: &common::Reply) -> Option<&str> {
    r.headers.get("x-cache").map(|v| v.to_str().unwrap())
}

fn tag(r: &common::Reply) -> String {
    r.headers["etag"].to_str().unwrap().to_string()
}

impl Env {
    fn reads(&self) -> u64 {
        self.state.db.reads()
    }

    /// Resolve both consumers' keys once (a 400 reads no analytics), so the
    /// key lookup's own `Db::read` isn't counted against a route.
    async fn keys_resolved(&self) {
        for key in [&self.reader, &self.admin] {
            let r = self
                .call("GET", "/v1/lol/analytics/patches?queue=X", key, None, None)
                .await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST);
        }
    }

    /// `uri`'s response and the `Db::read`s it ran.
    async fn read_counted(&self, uri: &str, etag: Option<&str>) -> (common::Reply, u64) {
        let before = self.reads();
        let r = self.call("GET", uri, &self.reader, etag, None).await;
        (r, self.reads() - before)
    }
}

#[tokio::test]
async fn a_hit_runs_no_read_and_a_304_comes_from_memory() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    let (r, reads) = e.read_counted(SITE, None).await;
    assert_eq!((r.status, x_cache(&r), reads), (StatusCode::OK, Some("HIT"), 0));
    assert_eq!(r.json()["totalGames"], 10);
    let (r304, reads) = e.read_counted(SITE, Some(&tag(&r))).await;
    assert_eq!(
        (r304.status, x_cache(&r304), reads),
        (StatusCode::NOT_MODIFIED, Some("HIT"), 0)
    );
    assert_eq!(tag(&r304), tag(&r));
    assert_eq!(r304.headers["cache-control"], "private, max-age=300");
    assert!(r304.body.is_empty());
}

#[tokio::test]
async fn a_miss_is_read_once_then_served_from_memory_with_the_same_tag() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    // A slice the rebuild doesn't warm.
    let uri = "/v1/lol/analytics/champions?patch=16.19&role=MIDDLE";
    let (miss, reads) = e.read_counted(uri, None).await;
    assert_eq!(
        (miss.status, x_cache(&miss), reads),
        (StatusCode::OK, Some("MISS"), 5)
    );
    let (hit, reads) = e.read_counted(uri, None).await;
    assert_eq!((x_cache(&hit), reads), (Some("HIT"), 0));
    assert_eq!((tag(&hit), &hit.body), (tag(&miss), &miss.body));
    // Read again from the same tables, the tag is the one served from memory.
    e.state.analytics.clear();
    let (again, reads) = e.read_counted(uri, None).await;
    assert_eq!((x_cache(&again), reads), (Some("MISS"), 5));
    assert_eq!((tag(&again), &again.body), (tag(&hit), &hit.body));
}

#[tokio::test]
async fn a_rebuild_clears_and_warms_what_sites_open_first() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    // The warm: the site's two champions calls and patch list, the showcase's
    // champions and patch list for the ladder just rebuilt.
    let keys: Vec<String> = e
        .state
        .analytics
        .responses
        .keys()
        .iter()
        .map(|k| k.as_str().replace(&e.scope, "<scope>"))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "champions|<scope>|~|kr|RANKED_SOLO_5x5|~|~|~|all|~|~|500|exclude",
            "champions|<scope>|~|~|RANKED_SOLO_5x5|~|~|~|all|~|0|500|exclude",
            "champions|<scope>|~|~|RANKED_SOLO_5x5|~|~|~|~|~|0|500|exclude",
            "patches|<scope>|~|kr|RANKED_SOLO_5x5|~|~|exclude",
            "patches|<scope>|~|~|RANKED_SOLO_5x5|~|~|exclude",
        ]
    );
    let first = e.get(SITE).await;
    assert_eq!(x_cache(&first), Some("HIT"));
    let stamp = first.json()["computedAt"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    e.aggregate().await;
    let (r, reads) = e.read_counted(SITE, None).await;
    assert_eq!((x_cache(&r), reads), (Some("HIT"), 0), "warmed again");
    assert!(
        r.json()["computedAt"].as_str().unwrap() > stamp.as_str(),
        "the new rows"
    );
    // The old validator no longer matches.
    let (stale, reads) = e.read_counted(SITE, Some(&tag(&first))).await;
    assert_eq!(
        (stale.status, x_cache(&stale), reads),
        (StatusCode::OK, Some("HIT"), 0)
    );
    assert_ne!(tag(&stale), tag(&first));
}

/// A rebuild that fails after a step committed has changed the tables too.
#[tokio::test]
async fn a_failed_rebuild_still_clears() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    assert_eq!(x_cache(&e.get(SITE).await), Some("HIT"));
    // Champions commit; matchups then fail.
    e.state
        .db
        .write(|c| Ok::<_, DbError>(c.execute_batch("DROP TABLE champion_matchups")?))
        .await
        .unwrap();
    let failed = e
        .ctx()
        .aggregate(&job(
            "aggregate:analytics",
            json!({"platform": "kr", "queue": "RANKED_SOLO_5x5"}),
        ))
        .await;
    assert!(failed.is_err());
    let (r, reads) = e.read_counted(SITE, None).await;
    assert_eq!((x_cache(&r), reads), (Some("MISS"), 5));
}

/// Design §6: at `ANALYTICS_CACHE_ENTRIES=2`, A, B, C evicts A; A again
/// evicts B; C is still there.
#[tokio::test]
async fn the_cap_evicts_the_least_recently_used() {
    let e = env_with(&[("ANALYTICS_CACHE_ENTRIES", "2")]).await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    let (a, b, c) = (
        "/v1/lol/analytics/champions?patch=16.19",
        "/v1/lol/analytics/champions?patch=all&limit=100",
        "/v1/lol/analytics/patches?remakes=include",
    );
    let mut seen = vec![];
    for uri in [a, b, c, a, c] {
        let (r, reads) = e.read_counted(uri, None).await;
        seen.push((x_cache(&r).unwrap().to_string(), reads));
    }
    let want: Vec<(String, u64)> = [("MISS", 5), ("MISS", 5), ("MISS", 1), ("MISS", 5), ("HIT", 0)]
        .iter()
        .map(|(x, n)| ((*x).to_string(), *n))
        .collect();
    assert_eq!(seen, want);
    assert_eq!(e.state.analytics.responses.len(), 2);
}

#[tokio::test]
async fn zero_keeps_nothing() {
    let e = env_with(&[("ANALYTICS_CACHE_ENTRIES", "0")]).await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    for _ in 0..2 {
        let (r, reads) = e.read_counted(SITE, None).await;
        assert_eq!((r.status, x_cache(&r), reads), (StatusCode::OK, Some("MISS"), 5));
    }
    assert!(e.state.analytics.responses.is_empty());
}

/// One query however it is spelled, and for every consumer: the key scope
/// is the process's, not the caller's (ADR-169).
#[tokio::test]
async fn reordered_queries_and_other_consumers_share_an_entry() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    let r = e
        .call(
            "GET",
            "/v1/lol/analytics/champions?queue=RANKED_SOLO_5x5&limit=7&patch=16.19",
            &e.reader,
            None,
            None,
        )
        .await;
    assert_eq!(x_cache(&r), Some("MISS"));
    let again = e
        .call(
            "GET",
            "/v1/lol/analytics/champions?patch=16.19&limit=7&queue=RANKED_SOLO_5x5&remakes=exclude",
            &e.admin,
            None,
            None,
        )
        .await;
    assert_eq!(x_cache(&again), Some("HIT"));
    assert_eq!(again.body, r.body);
}

#[tokio::test]
async fn errors_are_not_cached() {
    let e = env().await;
    e.keys_resolved().await;
    for _ in 0..2 {
        let (r, reads) = e.read_counted("/v1/lol/analytics/champions?queue=X", None).await;
        assert_eq!((r.status, x_cache(&r), reads), (StatusCode::BAD_REQUEST, None, 0));
    }
    assert!(e.state.analytics.responses.is_empty());
}

/// Design §6: ten concurrent cold requests for one slice read it once.
/// A request that joins the computation says `MISS`; one that reaches the
/// cache after it stored, which a loaded machine allows, says `HIT`.
/// Either way the slice was read once (CI-02: it failed when it wanted ten
/// `MISS`es).
#[tokio::test]
async fn concurrent_misses_read_once() {
    let e = env().await;
    e.seed().await;
    e.aggregate().await;
    e.keys_resolved().await;
    e.state.analytics.clear();
    let before = e.reads();
    let replies = futures_util::future::join_all((0..10).map(|_| e.get(SITE))).await;
    assert_eq!(e.reads() - before, 5);
    for r in &replies {
        assert_eq!(r.status, StatusCode::OK);
        assert!(matches!(x_cache(r), Some("MISS" | "HIT")), "{:?}", x_cache(r));
        assert_eq!(r.body, replies[0].body);
    }
    assert!(replies.iter().any(|r| x_cache(r) == Some("MISS")));
}

// ─── FLT-03: tier ranges (ADR-170) ──────────────────────────────────────────

impl Env {
    /// FLT-02's fixture with more tiers: the ranked game's participants
    /// alternate MASTER (even) and GRANDMASTER (odd), but Braum (4) and Neeko
    /// (9) are EMERALD and Nidalee (1) CHALLENGER. The remake's other players
    /// are EMERALD; two of them, Jayce and Syndra's players, played the
    /// ranked game at GRANDMASTER. So with remakes, CHALLENGER+ is one match
    /// of two.
    async fn seed_three_tiers(&self) {
        let players: Vec<String> =
            serde_json::from_slice::<Value>(RANKED).unwrap()["metadata"]["participants"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
        self.seed_with(move |p| match players.iter().position(|x| x == p) {
            Some(1) => "CHALLENGER",
            Some(4 | 9) | None => "EMERALD",
            Some(i) if i % 2 == 0 => "MASTER",
            Some(_) => "GRANDMASTER",
        })
        .await;
        self.seed_builds().await;
        self.aggregate().await;
    }
}

/// A range sums its tiers' rows and divides by its floor's matches, each
/// once: the ranked game has players in EMERALD, MASTER and GRANDMASTER, so
/// it is one match at `minTier=DIAMOND`, where its MASTER and GRANDMASTER
/// slices would make two.
#[tokio::test]
async fn tier_ranges_count_a_match_once_on_every_route() {
    let e = env().await;
    e.seed_three_tiers().await;
    let list = |q: &'static str| {
        let e = &e;
        async move {
            e.get(&format!("/v1/lol/analytics/champions?patch=all{q}"))
                .await
                .json()
        }
    };
    let ids = |v: &Value| -> Vec<i64> {
        let mut ids: Vec<i64> = v["champions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["championId"].as_i64().unwrap())
            .collect();
        ids.sort_unstable();
        ids
    };
    let (all, range) = (list("").await, list("&minTier=DIAMOND").await);
    let (m, gm, ch) = (
        list("&tier=MASTER").await,
        list("&tier=GRANDMASTER").await,
        list("&tier=CHALLENGER").await,
    );
    assert_eq!(
        (all["totalGames"].as_i64(), range["totalGames"].as_i64()),
        (Some(10), Some(8))
    );
    let mut tiers = [ids(&m), ids(&gm), ids(&ch)].concat();
    tiers.sort_unstable();
    assert_eq!(ids(&range), tiers, "the range is its tiers' rows");
    assert!(
        !ids(&range).contains(&201) && !ids(&range).contains(&518),
        "Braum and Neeko are EMERALD"
    );
    assert_eq!(
        (&range["tier"], &range["minTier"]),
        (&Value::Null, &json!("DIAMOND"))
    );
    assert_eq!(all["minTier"], Value::Null);
    // Hand count: Syndra (134) was picked in the one DIAMOND+ match. Over
    // the MASTER, GRANDMASTER and CHALLENGER slices summed it would be 1 / 3.
    let syndra = |v: &Value| {
        v["champions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["championId"] == 134)
            .cloned()
    };
    let s = syndra(&range).unwrap();
    assert_eq!(
        (&s["tier"], &s["games"], &s["pickRate"], &s["share"]),
        (&Value::Null, &json!(1), &json!(1), &json!(0.125))
    );
    // Every pick rate of every range is at most 1.
    for q in [
        "&minTier=EMERALD",
        "&minTier=DIAMOND",
        "&minTier=MASTER",
        "&minTier=GRANDMASTER",
        "&minTier=GRANDMASTER&remakes=include",
    ] {
        for c in list(q).await["champions"].as_array().unwrap() {
            assert!(c["pickRate"].as_f64().unwrap() <= 1.0, "{q}: {c}");
        }
    }
    // Yasuo (157) is MASTER: not GRANDMASTER and above.
    let gm_up = list("&minTier=GRANDMASTER").await;
    assert!(!ids(&gm_up).contains(&157) && ids(&gm_up).contains(&134));
    // With remakes, Nidalee (76, CHALLENGER) is in the ranked game only: 1 of
    // the 2 EMERALD+ games, but 1 of 1 CHALLENGER+ game, which the every-tier
    // total (2) would make 0.5.
    let nidalee = |v: Value| {
        v["champions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["championId"] == 76)
            .unwrap()["pickRate"]
            .clone()
    };
    assert_eq!(
        nidalee(list("&minTier=EMERALD&remakes=include").await),
        json!(0.5)
    );
    assert_eq!(
        nidalee(list("&minTier=CHALLENGER&remakes=include").await),
        json!(1)
    );
    assert_eq!(nidalee(list("&remakes=include").await), json!(0.5));
    assert_eq!(ids(&list("&minTier=CHALLENGER").await), [76]);

    // The detail: the summed row over the floor, byTier the range's tiers.
    let d = e
        .get("/v1/lol/analytics/champions/134?patch=all&minTier=DIAMOND")
        .await
        .json();
    assert_eq!((&d["tier"], &d["minTier"]), (&Value::Null, &json!("DIAMOND")));
    assert_eq!(
        (&d["stats"][0]["tier"], &d["stats"][0]["pickRate"]),
        (&Value::Null, &json!(1))
    );
    let by_tier: Vec<&Value> = d["byTier"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["tier"])
        .collect();
    assert_eq!(by_tier, [&json!("GRANDMASTER")]);
    let gm_detail = e
        .get("/v1/lol/analytics/champions/134?patch=all&tier=GRANDMASTER")
        .await
        .json();
    assert_eq!(d["matchups"], gm_detail["matchups"]);
    assert_eq!(d["items"], gm_detail["items"]);
    // Matchups and builds: the range's rows, named in the body.
    let mu = e
        .get("/v1/lol/analytics/champions/134/matchups?patch=all&minTier=DIAMOND")
        .await
        .json();
    assert_eq!((&mu["tier"], &mu["minTier"]), (&Value::Null, &json!("DIAMOND")));
    assert_eq!(mu["matchups"][0]["opponentId"], 157);
    let b = e
        .get("/v1/lol/analytics/champions/134/builds?patch=all&role=MIDDLE&minTier=DIAMOND")
        .await
        .json();
    assert_eq!(
        (&b["tier"], &b["minTier"], &b["totalGames"]),
        (&Value::Null, &json!("DIAMOND"), &json!(1))
    );
    let none = e
        .get("/v1/lol/analytics/champions/134/builds?patch=all&role=MIDDLE&minTier=CHALLENGER")
        .await
        .json();
    assert_eq!((&none["totalGames"], &none["builds"]), (&json!(0), &json!([])));

    // minTier is part of every champion route's ETag; the patch list takes
    // no tier and ignores it, as it ignores `tier`.
    for uri in [
        "/v1/lol/analytics/champions?patch=all",
        "/v1/lol/analytics/champions/134?patch=all",
        "/v1/lol/analytics/champions/134/matchups?patch=all",
        "/v1/lol/analytics/champions/134/builds?patch=all",
    ] {
        let (plain, ranged) = (e.get(uri).await, e.get(&format!("{uri}&minTier=DIAMOND")).await);
        let tier = e.get(&format!("{uri}&tier=DIAMOND")).await;
        assert_ne!(tag(&plain), tag(&ranged), "{uri}");
        assert_ne!(tag(&tier), tag(&ranged), "{uri}");
    }
    let (plain, ranged) = (
        e.get("/v1/lol/analytics/patches?championId=134").await,
        e.get("/v1/lol/analytics/patches?championId=134&minTier=DIAMOND")
            .await,
    );
    assert_eq!(ranged.status, StatusCode::OK);
    assert_eq!((tag(&plain), plain.json()), (tag(&ranged), ranged.json()));
    assert!(plain.json().get("minTier").is_none());
}

/// `minTier` is one of Riot's tiers, never UNKNOWN, in Riot's casing, and
/// never with `tier`: the routes' usual 400 otherwise, never cached.
#[tokio::test]
async fn tier_and_min_tier_are_validated_on_every_route() {
    let e = env().await;
    e.seed_three_tiers().await;
    const ALLOWED: &str = "must be equal to one of the allowed values";
    for uri in [
        "/v1/lol/analytics/champions",
        "/v1/lol/analytics/champions/134",
        "/v1/lol/analytics/champions/134/matchups",
        "/v1/lol/analytics/champions/134/builds",
    ] {
        for (q, message) in [
            (
                "tier=MASTER&minTier=DIAMOND",
                "querystring/minTier must NOT be sent with tier".to_string(),
            ),
            (
                "tier=UNKNOWN&minTier=DIAMOND",
                "querystring/minTier must NOT be sent with tier".to_string(),
            ),
            ("minTier=UNKNOWN", format!("querystring/minTier {ALLOWED}")),
            ("minTier=gold", format!("querystring/minTier {ALLOWED}")),
            ("minTier=Diamond", format!("querystring/minTier {ALLOWED}")),
            ("minTier=", format!("querystring/minTier {ALLOWED}")),
            ("tier=gold&minTier=DIAMOND", format!("querystring/tier {ALLOWED}")),
            (
                "tier=MASTER&minTier=gold",
                format!("querystring/minTier {ALLOWED}"),
            ),
            (
                "minTier=DIAMOND&side=green",
                format!("querystring/side {ALLOWED}"),
            ),
        ] {
            let r = e.get(&format!("{uri}?{q}")).await;
            assert_eq!(error(&r), (StatusCode::BAD_REQUEST, message), "{uri}?{q}");
            assert_eq!(r.json()["error"]["code"], "VALIDATION", "{uri}?{q}");
            assert_eq!(x_cache(&r), None, "{uri}?{q}: errors are not cached");
        }
        for floor in ["IRON", "EMERALD", "CHALLENGER"] {
            assert_eq!(
                e.get(&format!("{uri}?minTier={floor}")).await.status,
                StatusCode::OK,
                "{uri} {floor}"
            );
        }
    }
    let r = e.get("/v1/lol/analytics/patches?tier=MASTER&minTier=gold").await;
    assert_eq!(r.status, StatusCode::OK, "the patch list takes no tier");
}

/// A range, a tier and neither are three cache entries (PERF-03): a range
/// must not be served a tier's document.
#[tokio::test]
async fn a_range_and_a_tier_are_separate_entries() {
    let e = env().await;
    e.seed_three_tiers().await;
    let uris = [
        format!("{SITE}&minTier=DIAMOND"),
        format!("{SITE}&tier=DIAMOND"),
        format!("{SITE}&tier=MASTER"),
    ];
    let mut tags = vec![];
    for uri in &uris {
        let first = e.get(uri).await;
        assert_eq!(x_cache(&first), Some("MISS"), "{uri}");
        let second = e.get(uri).await;
        assert_eq!(x_cache(&second), Some("HIT"), "{uri}");
        assert_eq!(first.json(), second.json());
        tags.push(tag(&first));
    }
    // The site's own call was warmed by the rebuild.
    let site = e.get(SITE).await;
    assert_eq!(x_cache(&site), Some("HIT"));
    tags.push(tag(&site));
    let distinct: std::collections::BTreeSet<_> = tags.iter().collect();
    assert_eq!(distinct.len(), 4, "{tags:?}");
    assert_eq!(e.get(&uris[0]).await.json()["minTier"], "DIAMOND");
    assert_eq!(e.get(&uris[1]).await.json()["tier"], "DIAMOND");
}

impl Env {
    /// Anivia (34, blue TOP, a loss) gets a two-item build, so her lane
    /// against Jayce (126, one finished item) has a build on one side only.
    async fn seed_anivia_build(&self) {
        let puuid = serde_json::from_slice::<Value>(RANKED).unwrap()["info"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["championId"] == 34)
            .unwrap()["puuid"]
            .as_str()
            .unwrap()
            .to_string();
        let scope = self.scope.clone();
        self.state
            .db
            .write(move |c| {
                c.execute(
                    "INSERT INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills,
                       skill_order, builds_version)
                     VALUES ('KR_8393343196', ?1, ?2, '[1056]', 3020, '[6653,3020]', '', 'QWE', 1)",
                    rusqlite::params![scope, puuid],
                )?;
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
    }
}

const SYNDRA: &str = "/v1/lol/analytics/champions/134/builds?patch=all";

/// FLT-04 (ADR-171): Syndra's (134, red MIDDLE, GRANDMASTER, a win) builds
/// against Yasuo (157, blue MIDDLE, MASTER): the 3rd item, boots, runes and
/// spells of the games in that lane, with `winRateLow` on every build.
#[tokio::test]
async fn builds_against_an_opponent() {
    let e = env().await;
    e.seed_three_tiers().await;
    e.seed_anivia_build().await;
    e.aggregate().await;

    let r = e.get(&format!("{SYNDRA}&opponentId=157&role=MIDDLE")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        redact(r.json()),
        json!({"championId": 134, "platform": null, "queue": "RANKED_SOLO_5x5", "tier": null, "minTier": null,
               "side": null, "patch": "all", "role": "MIDDLE", "opponentId": 157, "computedAt": "<iso>",
               "totalGames": 1, "builds": [{"core": [6655, 3157], "games": 1, "wins": 1, "winRate": 1,
               "winRateLow": 0.2065, "pickRate": 1, "next": [[{"itemId": 3089, "games": 1, "winRate": 1}], [], []],
               "starter": [], "boots": [{"itemId": 3020, "games": 1, "winRate": 1}], "skillOrder": [],
               "runes": [{"keystoneId": 8229, "subStyleId": 8000, "games": 1, "winRate": 1}],
               "spells": [{"spellA": 4, "spellB": 12, "games": 1, "winRate": 1}]}]})
    );
    let builds = |v: &Value| (v["totalGames"].clone(), v["builds"].clone());
    let vs = builds(&r.json());

    // No role: the lane they met in, named.
    let lane = e.get(&format!("{SYNDRA}&opponentId=157")).await.json();
    assert_eq!(
        (lane["role"].clone(), builds(&lane)),
        (json!("MIDDLE"), vs.clone())
    );

    // Tier, range and side slice it as every other route.
    let empty = (json!(0), json!([]));
    for (q, want) in [
        ("&tier=GRANDMASTER", &vs),
        ("&tier=MASTER", &empty),
        ("&minTier=DIAMOND", &vs),
        ("&minTier=CHALLENGER", &empty),
        ("&side=red", &vs),
        ("&side=blue", &empty),
        // The remake's players have no position, so no lane.
        ("&remakes=include", &vs),
    ] {
        let got = e
            .get(&format!("{SYNDRA}&opponentId=157&role=MIDDLE{q}"))
            .await
            .json();
        assert_eq!(&builds(&got), want, "{q}");
    }
    let one_patch = e
        .get("/v1/lol/analytics/champions/134/builds?patch=16.19&opponentId=157&role=MIDDLE")
        .await
        .json();
    assert_eq!(builds(&one_patch), vs);

    // Yasuo's side of the lane: his own build, a loss.
    let yasuo = e
        .get("/v1/lol/analytics/champions/157/builds?patch=all&opponentId=134")
        .await
        .json();
    assert_eq!(yasuo["role"], "MIDDLE");
    assert_eq!(yasuo["builds"][0]["core"], json!([6672, 3031]));
    assert_eq!(yasuo["builds"][0]["winRateLow"], 0);
    assert_eq!(yasuo["builds"][0]["next"][0][0]["itemId"], 3036);

    // A matchup with a build on one side only: Anivia's against Jayce, none
    // the other way (Jayce finished one item).
    let anivia = e
        .get("/v1/lol/analytics/champions/34/builds?patch=all&opponentId=126")
        .await
        .json();
    assert_eq!(
        (anivia["role"].clone(), anivia["totalGames"].clone()),
        (json!("TOP"), json!(1))
    );
    assert_eq!(anivia["builds"][0]["core"], json!([6653, 3020]));
    let jayce = e
        .get("/v1/lol/analytics/champions/126/builds?patch=all&opponentId=34")
        .await
        .json();
    assert_eq!(
        (jayce["role"].clone(), builds(&jayce)),
        (Value::Null, empty.clone())
    );
    let jayce_top = e
        .get("/v1/lol/analytics/champions/126/builds?patch=all&opponentId=34&role=TOP")
        .await
        .json();
    assert_eq!(
        (jayce_top["role"].clone(), builds(&jayce_top)),
        (json!("TOP"), empty)
    );

    // Never more games than the matchups route has for that opponent.
    for (champion, opponent) in [(134, 157), (157, 134), (34, 126), (126, 34)] {
        let matchups = e
            .get(&format!(
                "/v1/lol/analytics/champions/{champion}/matchups?patch=all&minGames=0"
            ))
            .await
            .json();
        let met: i64 = matchups["matchups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["opponentId"] == opponent)
            .map(|m| m["games"].as_i64().unwrap())
            .sum();
        let got = e
            .get(&format!(
                "/v1/lol/analytics/champions/{champion}/builds?patch=all&opponentId={opponent}"
            ))
            .await
            .json();
        assert!(
            got["totalGames"].as_i64().unwrap() <= met,
            "{champion} v {opponent}"
        );
    }

    // Without an opponent: today's body, plus opponentId null and winRateLow.
    let any = e.get(&format!("{SYNDRA}&role=MIDDLE")).await.json();
    assert_eq!(any["opponentId"], Value::Null);
    assert_eq!(any["builds"][0]["winRateLow"], 0.2065);
    assert_eq!(any["builds"][0]["next"][1][0]["itemId"], 4645);
    assert_eq!(any["builds"][0]["skillOrder"][0]["order"], "QEW");
}

/// `opponentId` is a champion id, and with it `role` is a lane: v1's
/// matchups message, checked first, in v1's property order.
#[tokio::test]
async fn opponent_id_is_validated() {
    let e = env().await;
    for (q, message) in [
        (
            "opponentId=157&role=",
            "querystring/role must be equal to one of the allowed values",
        ),
        (
            "opponentId=157&role=SUPPORT",
            "querystring/role must be equal to one of the allowed values",
        ),
        ("opponentId=abc", "querystring/opponentId must be integer"),
        ("opponentId=0", "querystring/opponentId must be >= 1"),
        (
            "opponentId=abc&role=",
            "querystring/role must be equal to one of the allowed values",
        ),
    ] {
        let r = e.get(&format!("{SYNDRA}&{q}")).await;
        assert_eq!(error(&r), (StatusCode::BAD_REQUEST, message.into()), "{q}");
        assert_eq!(r.json()["error"]["code"], "VALIDATION", "{q}");
        assert!(r.headers.get("x-cache").is_none(), "{q}");
    }
    // Without an opponent, an empty role is the roleless rows, as before.
    assert_eq!(e.get(&format!("{SYNDRA}&role=")).await.status, StatusCode::OK);
}

/// An opponent never met, and the champion itself (a mirror is never
/// written), are the documented empty case.
#[tokio::test]
async fn an_unseen_opponent_or_a_mirror_is_an_empty_list() {
    let e = env().await;
    e.seed_three_tiers().await;
    for opponent in [1, 134] {
        let got = e.get(&format!("{SYNDRA}&opponentId={opponent}")).await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(
            got.json(),
            json!({"championId": 134, "platform": null, "queue": "RANKED_SOLO_5x5", "tier": null,
                   "minTier": null, "side": null, "patch": "all", "role": null, "opponentId": opponent,
                   "computedAt": null, "totalGames": 0, "builds": []}),
            "{opponent}"
        );
    }
}

/// Each opponent is its own cache entry and validator.
#[tokio::test]
async fn two_opponents_are_two_entries() {
    let e = env().await;
    e.seed_three_tiers().await;
    let uris = [
        format!("{SYNDRA}&opponentId=157"),
        format!("{SYNDRA}&opponentId=1"),
        SYNDRA.to_string(),
    ];
    let mut tags = vec![];
    for uri in &uris {
        assert_eq!(x_cache(&e.get(uri).await), Some("MISS"), "{uri}");
    }
    for uri in &uris {
        let hit = e.get(uri).await;
        assert_eq!(x_cache(&hit), Some("HIT"), "{uri}");
        tags.push(tag(&hit));
    }
    let distinct: std::collections::BTreeSet<_> = tags.iter().collect();
    assert_eq!(distinct.len(), 3, "{tags:?}");
    // Another opponent's validator is no match.
    let other = e.call("GET", SYNDRA, &e.reader, Some(&tags[0]), None).await;
    assert_eq!(other.status, StatusCode::OK);
}

//! `/v1/admin/*`, the data subset (plan P5-05), through the full app against a
//! wiremock Riot: every route, the admin scope enforced on each, and v1's
//! request rules and messages.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/replay/cold-lookup");
const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const MATCH_ID: &str = "KR_8393343196";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    admin: String,
    reader: String,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let ok = |body: Vec<u8>| ResponseTemplate::new(200).set_body_raw(body, "application/json");
    for (p, body) in [
        (
            "/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1".to_string(),
            fixture("01-account.byRiotId.body"),
        ),
        (
            format!("/lol/match/v5/matches/{MATCH_ID}"),
            fixture("06-match.byId.body"),
        ),
        (
            "/lol/status/v4/platform-data".to_string(),
            br#"{"id":"KR"}"#.to_vec(),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ok(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/lol/summoner/v4/summoners/by-puuid/{PUUID}")))
        .respond_with(
            ok(fixture("02-summoner.byPuuid.body"))
                .insert_header("x-app-rate-limit", "100:120,20:1")
                .insert_header("x-app-rate-limit-count", "1:120,1:1"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/riot/account/v1/accounts/by-riot-id/nobody/0000"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let (dir, state, router) = common::app_with(&[], &server.uri());
    let mint = |name: &str, scopes: Vec<Scope>| NewConsumer {
        name: name.into(),
        scopes,
        quota_per_min: 10_000,
        key: None,
    };
    let admin = consumers::create(&state.db, mint("ops", vec![Scope::Read, Scope::Admin]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    let reader = consumers::create(&state.db, mint("web", vec![Scope::Read]))
        .await
        .unwrap()
        .key
        .expose()
        .to_string();
    Env {
        _dir: dir,
        server,
        state,
        router,
        admin,
        reader,
    }
}

impl Env {
    async fn call(&self, verb: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> common::Reply {
        let mut req = Request::builder().method(verb).uri(uri);
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

    async fn get(&self, uri: &str) -> common::Reply {
        self.call("GET", uri, Some(&self.admin), None).await
    }

    async fn post(&self, uri: &str, body: Value) -> common::Reply {
        self.call("POST", uri, Some(&self.admin), Some(body)).await
    }

    async fn delete(&self, uri: &str) -> common::Reply {
        self.call("DELETE", uri, Some(&self.admin), None).await
    }

    async fn upstream_calls(&self) -> usize {
        self.server.received_requests().await.unwrap().len()
    }
}

fn error(r: &common::Reply) -> (StatusCode, String, String) {
    let e = &r.json()["error"];
    (
        r.status,
        e["code"].as_str().unwrap().into(),
        e["message"].as_str().unwrap().into(),
    )
}

fn bad(message: &str) -> (StatusCode, String, String) {
    (StatusCode::BAD_REQUEST, "VALIDATION".into(), message.into())
}

const ULID: &str = "01J9ZZZZZZZZZZZZZZZZZZZZZZ";

/// Every route in this subset: no key → 401, a read key → 403.
#[tokio::test]
async fn every_admin_route_needs_the_admin_scope() {
    let e = env().await;
    let routes = [
        ("GET", "/v1/admin/consumers".to_string()),
        ("POST", "/v1/admin/consumers".to_string()),
        ("DELETE", format!("/v1/admin/consumers/{ULID}")),
        ("POST", format!("/v1/admin/consumers/{ULID}/revoke-cache")),
        ("GET", "/v1/admin/tracked-players".to_string()),
        ("POST", "/v1/admin/tracked-players".to_string()),
        ("DELETE", format!("/v1/admin/tracked-players/{PUUID}")),
        ("GET", format!("/v1/admin/players/{PUUID}/archive")),
        ("GET", format!("/v1/admin/players/{PUUID}/archive/matches")),
        ("POST", "/v1/admin/cache/purge".to_string()),
        ("GET", "/v1/admin/stats".to_string()),
        ("GET", "/v1/admin/info".to_string()),
        ("GET", "/v1/admin/recent".to_string()),
        ("GET", "/v1/admin/backups".to_string()),
        ("POST", "/v1/admin/maintenance/run".to_string()),
        ("GET", "/v1/admin/limits/euw1".to_string()),
        (
            "GET",
            "/v1/admin/debug/riot?scope=kr&path=/lol/status/v4/platform-data".to_string(),
        ),
        (
            "GET",
            "/v1/admin/debug/cache?scope=kr&path=/lol/status/v4/platform-data&method=status.platformData"
                .to_string(),
        ),
    ];
    for (verb, uri) in &routes {
        let none = e.call(verb, uri, None, Some(json!({}))).await;
        assert_eq!(none.status, StatusCode::UNAUTHORIZED, "{verb} {uri}");
        let read = e.call(verb, uri, Some(&e.reader), Some(json!({}))).await;
        assert_eq!(read.status, StatusCode::FORBIDDEN, "{verb} {uri}");
    }
    assert_eq!(e.upstream_calls().await, 0);
}

// ── Consumers ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_created_key_works_and_is_listed_without_its_secret() {
    let e = env().await;
    let r = e
        .post("/v1/admin/consumers", json!({"name": "dashboard", "extra": 1}))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let created = r.json();
    assert_eq!(
        (created["scopes"].clone(), created["quotaPerMin"].clone()),
        (json!(["read"]), json!(600)),
        "v1 defaults"
    );
    assert_eq!(
        created["warning"],
        "Store this key now — it cannot be retrieved again."
    );
    let key = created["key"].as_str().unwrap();
    assert!(key.starts_with("rpx_"));

    let using = e.call("GET", "/v1/admin/stats", Some(key), None).await;
    assert_eq!(using.status, StatusCode::FORBIDDEN, "read-only by default");

    let list = e.get("/v1/admin/consumers").await.json();
    let row = list["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "dashboard")
        .unwrap()
        .clone();
    assert_eq!(row["id"], created["id"]);
    assert_eq!(row["disabledAt"], Value::Null);
    assert!(row["createdAt"].as_str().unwrap().ends_with('Z'));
    assert!(!list.to_string().contains(key), "the key is shown exactly once");
}

#[tokio::test]
async fn consumer_bodies_follow_v1_validation() {
    let e = env().await;
    let cases = [
        (json!({}), "body must have required property 'name'"),
        (
            json!({"name": ""}),
            "body/name must NOT have fewer than 1 characters",
        ),
        (
            json!({"name": "x".repeat(101)}),
            "body/name must NOT have more than 100 characters",
        ),
        (
            json!({"name": "a", "scopes": []}),
            "body/scopes must NOT have fewer than 1 items",
        ),
        (
            json!({"name": "a", "scopes": ["root"]}),
            "body/scopes/0 must be equal to one of the allowed values",
        ),
        (
            json!({"name": "a", "quotaPerMin": 0}),
            "body/quotaPerMin must be >= 1",
        ),
        (
            json!({"name": "a", "quotaPerMin": "lots"}),
            "body/quotaPerMin must be integer",
        ),
        (json!({"name": "ops"}), "a consumer named 'ops' already exists"),
    ];
    for (body, message) in cases {
        let r = e.post("/v1/admin/consumers", body.clone()).await;
        assert_eq!(error(&r), bad(message), "{body}");
    }
    let r = e
        .post(
            "/v1/admin/consumers",
            json!({"name": "svc", "scopes": "admin", "quotaPerMin": "30"}),
        )
        .await;
    let c = r.json();
    assert_eq!(
        (c["scopes"].clone(), c["quotaPerMin"].clone()),
        (json!(["admin"]), json!(30)),
        "ajv coercion"
    );

    let raw = Request::post("/v1/admin/consumers")
        .header("authorization", format!("Bearer {}", e.admin))
        .body(Body::from("{nope"))
        .unwrap();
    assert_eq!(
        error(&common::send(e.router.clone(), raw).await),
        bad("body is not valid JSON")
    );
}

#[tokio::test]
async fn revoking_a_consumer_takes_effect_at_once() {
    let e = env().await;
    let reader = e.get("/v1/admin/consumers").await.json()["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "web")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Warm the auth cache with the reader's key.
    let before = e
        .call(
            "GET",
            &format!("/v1/players/{PUUID}/champions"),
            Some(&e.reader),
            None,
        )
        .await;
    assert_eq!(before.status, StatusCode::OK);

    let r = e.delete(&format!("/v1/admin/consumers/{reader}")).await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::OK, json!({"ok": true, "id": reader}))
    );
    let after = e
        .call(
            "GET",
            &format!("/v1/players/{PUUID}/champions"),
            Some(&e.reader),
            None,
        )
        .await;
    assert_eq!(
        after.status,
        StatusCode::UNAUTHORIZED,
        "no waiting for the auth cache"
    );

    let again = e.delete(&format!("/v1/admin/consumers/{reader}")).await;
    assert_eq!(
        error(&again),
        (
            StatusCode::NOT_FOUND,
            "NOT_FOUND".into(),
            "No active consumer with that id".into()
        )
    );
    let listed = e.get("/v1/admin/consumers").await.json();
    assert!(listed.to_string().contains("disabledAt"));
    assert_eq!(
        error(&e.delete("/v1/admin/consumers/not-a-ulid").await),
        bad("params/id must match format \"ulid\"")
    );
}

#[tokio::test]
async fn revoke_cache_needs_the_consumers_own_key_hash() {
    let e = env().await;
    let list = e.get("/v1/admin/consumers").await.json();
    let id = |name: &str| {
        list["consumers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let hash = hex::encode(consumers::hash_key(&e.reader));
    let r = e
        .post(
            &format!("/v1/admin/consumers/{}/revoke-cache", id("web")),
            json!({"keyHash": hash}),
        )
        .await;
    assert_eq!(
        r.json(),
        json!({"ok": true, "id": id("web"), "stillActive": true})
    );

    // The right hash under the wrong consumer is not a match (v1).
    let wrong = e
        .post(
            &format!("/v1/admin/consumers/{}/revoke-cache", id("ops")),
            json!({"keyHash": hash}),
        )
        .await;
    assert_eq!(
        error(&wrong),
        (
            StatusCode::NOT_FOUND,
            "NOT_FOUND".into(),
            "No consumer with that id and key hash".into()
        )
    );
    let short = e
        .post(
            &format!("/v1/admin/consumers/{}/revoke-cache", id("web")),
            json!({"keyHash": "ab"}),
        )
        .await;
    assert_eq!(
        error(&short),
        bad("body/keyHash must NOT have fewer than 64 characters")
    );
}

// ── Tracked players ─────────────────────────────────────────────────────────

#[tokio::test]
async fn track_by_riot_id_then_untrack() {
    let e = env().await;
    let body = json!({"platform": "kr", "gameName": "Hide on bush", "tagLine": "KR1"});
    let r = e.post("/v1/admin/tracked-players", body.clone()).await;
    assert_eq!(r.status, StatusCode::OK);
    let mut got = r.json();
    // A timestamp: checked here, left out of the snapshot.
    let updated = got.as_object_mut().unwrap().remove("updatedAt").unwrap();
    assert!(updated.as_str().unwrap().ends_with('Z'));
    // Tracking queues the history walk once (v1 #46); its id is a fresh ULID.
    let job = std::mem::replace(&mut got["backfill"]["jobId"], json!("<ulid>"));
    assert_eq!(job.as_str().unwrap().len(), 26);
    insta::assert_json_snapshot!("admin_tracked_player", got);

    // Posting the same Riot ID again re-resolves it (the path after a key rotation).
    let again = e.post("/v1/admin/tracked-players", body).await.json();
    assert_eq!(again["puuid"], PUUID);
    let list = e.get("/v1/admin/tracked-players").await.json();
    assert_eq!(list["players"].as_array().unwrap().len(), 1);

    let off = e.delete(&format!("/v1/admin/tracked-players/{PUUID}")).await;
    assert_eq!(off.json(), json!({"ok": true, "puuid": PUUID, "tracked": false}));
    let list = e.get("/v1/admin/tracked-players").await.json();
    assert_eq!(list["players"][0]["tracked"], false, "untracked, not deleted");
    assert_eq!(list["players"][0]["gameName"], "Hide on bush");
}

#[tokio::test]
async fn track_by_puuid_keeps_a_stored_riot_id() {
    let e = env().await;
    e.post(
        "/v1/admin/tracked-players",
        json!({"platform": "kr", "gameName": "Hide on bush", "tagLine": "KR1"}),
    )
    .await;
    let r = e
        .post(
            "/v1/admin/tracked-players",
            json!({"platform": "kr", "puuid": PUUID, "tracked": false}),
        )
        .await
        .json();
    assert_eq!(
        (r["tracked"].clone(), r["gameName"].clone(), r["tagLine"].clone()),
        (json!(false), json!("Hide on bush"), json!("KR1"))
    );
    assert_eq!(e.upstream_calls().await, 1, "a PUUID needs no Riot call");
}

#[tokio::test]
async fn tracked_player_errors() {
    let e = env().await;
    let cases = [
        (json!({}), bad("body must have required property 'platform'")),
        (
            json!({"platform": "KR"}),
            (
                StatusCode::BAD_REQUEST,
                "BAD_REGION".into(),
                "body/platform must be equal to one of the allowed values".into(),
            ),
        ),
        (
            json!({"platform": "kr"}),
            bad("Provide either puuid, or gameName and tagLine"),
        ),
        (
            json!({"platform": "kr", "gameName": "x"}),
            bad("Provide either puuid, or gameName and tagLine"),
        ),
        (
            json!({"platform": "kr", "puuid": "short"}),
            bad("body/puuid must NOT have fewer than 60 characters"),
        ),
        (
            json!({"platform": "kr", "gameName": "x", "tagLine": "TOOLONG"}),
            bad("body/tagLine must NOT have more than 5 characters"),
        ),
    ];
    for (body, want) in cases {
        let r = e.post("/v1/admin/tracked-players", body.clone()).await;
        assert_eq!(error(&r), want, "{body}");
    }
    let unknown = e
        .post(
            "/v1/admin/tracked-players",
            json!({"platform": "kr", "gameName": "nobody", "tagLine": "0000"}),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "Riot's 404 comes through");
    let untrack = e.delete(&format!("/v1/admin/tracked-players/{PUUID}")).await;
    assert_eq!(
        error(&untrack),
        (
            StatusCode::NOT_FOUND,
            "NOT_FOUND".into(),
            "No such player for the current key scope".into()
        )
    );
}

// ── Cache, stats, limits ────────────────────────────────────────────────────

#[tokio::test]
async fn purge_drops_matching_entries_from_memory_and_disk() {
    let e = env().await;
    let summoner = format!("/v1/lol/summoners/by-puuid/kr/{PUUID}");
    let status = "/v1/lol/status/kr";
    e.get(&summoner).await;
    e.get(status).await;
    assert_eq!(e.get(&summoner).await.headers["x-cache"], "HIT");

    let r = e
        .post("/v1/admin/cache/purge", json!({"pattern": "summoner.byPuuid:*"}))
        .await;
    assert_eq!(
        r.json(),
        json!({"ok": true, "pattern": "summoner.byPuuid:*", "deleted": 1})
    );
    assert_eq!(e.get(&summoner).await.headers["x-cache"], "MISS", "purged");
    assert_eq!(e.get(status).await.headers["x-cache"], "HIT", "left alone");

    let nothing = e
        .post("/v1/admin/cache/purge", json!({"pattern": "nomatch*"}))
        .await;
    assert_eq!(nothing.json()["deleted"], 0);
    assert_eq!(
        error(&e.post("/v1/admin/cache/purge", json!({})).await),
        bad("body must have required property 'pattern'")
    );
}

#[tokio::test]
async fn stats_count_the_archive_and_the_players() {
    let e = env().await;
    let empty = e.get("/v1/admin/stats").await.json();
    assert_eq!(empty["archivedMatches"], 0);
    assert_eq!(empty["keyScope"], e.state.fetcher.key_scope().as_str());

    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    e.post(
        "/v1/admin/tracked-players",
        json!({"platform": "kr", "gameName": "Hide on bush", "tagLine": "KR1"}),
    )
    .await;
    let s = e.get("/v1/admin/stats").await.json();
    assert_eq!(
        (
            s["archivedMatches"].clone(),
            s["trackedPlayers"].clone(),
            s["knownPlayers"].clone(),
            s["archivedTimelines"].clone(),
            s["archiveRawBytes"].clone()
        ),
        (
            json!(1),
            json!(1),
            json!(1),
            json!(0),
            json!(fixture("06-match.byId.body").len())
        )
    );
    assert!(s["archiveStoredBytes"].as_i64().unwrap() < s["archiveRawBytes"].as_i64().unwrap());
}

/// DSK-01: the file, its WAL, the page geometry and the disk, read live.
#[tokio::test]
async fn stats_report_what_the_database_takes_on_disk() {
    let e = env().await;
    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    let s = e.get("/v1/admin/stats").await.json();
    let n = |k: &str| {
        s[k].as_u64()
            .unwrap_or_else(|| panic!("{k} missing or not a number: {s}"))
    };
    assert_eq!(n("dbPageSize"), 4096);
    assert!(n("dbFreelistPages") < n("dbPageCount"), "{s}");
    // A fresh WAL database: the file holds its first pages, the rest wait in the
    // WAL until a checkpoint, so the file is at most pages × page size.
    assert!(
        n("dbFileBytes") > 0 && n("dbFileBytes") <= n("dbPageCount") * 4096,
        "{s}"
    );
    assert!(n("walBytes") > 0, "{s}");
    assert!(
        n("diskTotalBytes") >= n("diskFreeBytes") && n("diskFreeBytes") > 0,
        "{s}"
    );
}

#[tokio::test]
async fn a_players_archive_counts_everything_stored_without_calling_riot() {
    let e = env().await;
    let empty = e.get(&format!("/v1/admin/players/{PUUID}/archive")).await;
    assert_eq!(empty.status, StatusCode::OK);
    let j = empty.json();
    assert_eq!(
        (j["player"].clone(), j["archive"]["matches"].clone()),
        (Value::Null, json!(0))
    );
    assert_eq!(
        j["jobs"]["archive:match"],
        json!({"pending": 0, "running": 0, "done": 0, "failed": 0})
    );
    assert_eq!(j["jobs"]["backfill:player"]["pending"], 0);

    e.get(&format!("/v1/lol/matches/asia/{MATCH_ID}")).await;
    e.post(
        "/v1/admin/tracked-players",
        json!({"platform": "kr", "gameName": "Hide on bush", "tagLine": "KR1"}),
    )
    .await;
    let before = e.upstream_calls().await;

    let j = e.get(&format!("/v1/admin/players/{PUUID}/archive")).await.json();
    assert_eq!(j["puuid"], PUUID);
    assert_eq!(j["keyScope"], e.state.fetcher.key_scope().as_str());
    assert_eq!(j["player"]["tracked"], true);
    assert_eq!(
        j["archive"],
        json!({
            "matches": 1, "remakes": 0, "wins": 1, "timelines": 0,
            "oldestGameEnd": "2026-09-24T11:00:23.902Z", "newestGameEnd": "2026-09-24T11:00:23.902Z",
            "byQueue": [{"queueId": 420, "matches": 1}]
        })
    );
    // Tracking queued this player's history walk.
    assert_eq!(j["jobs"]["backfill:player"]["pending"], 1);
    assert_eq!(j["latestJobs"][0]["kind"], "backfill:player");

    let page = e
        .get(&format!("/v1/admin/players/{PUUID}/archive/matches?count=10"))
        .await
        .json();
    assert_eq!(
        page,
        json!({
            "puuid": PUUID, "total": 1, "start": 0, "count": 10,
            "matches": [{
                "matchId": MATCH_ID, "queueId": 420, "gameEndTimestamp": 1_790_247_623_902_i64,
                "gameDuration": 1682, "remake": false, "championId": 134, "position": "MIDDLE",
                "win": true, "kills": 8, "deaths": 6, "assists": 14, "cs": 206
            }]
        })
    );
    let other_queue = e
        .get(&format!("/v1/admin/players/{PUUID}/archive/matches?queue=440"))
        .await
        .json();
    assert_eq!(
        (other_queue["total"].clone(), other_queue["matches"].clone()),
        (json!(0), json!([]))
    );
    assert_eq!(e.upstream_calls().await, before, "archive reads never call Riot");
}

#[tokio::test]
async fn a_players_archive_validates_its_input() {
    let e = env().await;
    for uri in [
        "/v1/admin/players/short/archive".to_string(),
        "/v1/admin/players/short/archive/matches".to_string(),
        format!("/v1/admin/players/{PUUID}/archive/matches?count=0"),
        format!("/v1/admin/players/{PUUID}/archive/matches?count=101"),
        format!("/v1/admin/players/{PUUID}/archive/matches?start=-1"),
        format!("/v1/admin/players/{PUUID}/archive/matches?queue=x"),
    ] {
        let r = e.get(&uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(r.json()["error"]["code"], "VALIDATION", "{uri}");
    }
}

#[tokio::test]
async fn limits_report_bucket_usage() {
    let e = env().await;
    // Before Riot names the limits, a bucket runs on the bootstrap limits.
    let before = e.get("/v1/admin/limits/kr").await.json();
    assert_eq!(
        before,
        json!({"scope": "kr", "usage": [
            {"window": "20:1", "used": 0, "limit": 20},
            {"window": "100:120", "used": 0, "limit": 100}
        ], "frozenMs": 0})
    );
    e.get(&format!("/v1/lol/summoners/by-puuid/kr/{PUUID}")).await;
    let after = e.get("/v1/admin/limits/kr").await.json();
    let used: Vec<(String, u64)> = after["usage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["window"].as_str().unwrap().to_string(),
                w["used"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        used,
        [("20:1".to_string(), 1), ("100:120".to_string(), 1)],
        "{after}"
    );
    assert_eq!(
        e.get("/v1/admin/limits/europe").await.status,
        StatusCode::OK,
        "regions are buckets too"
    );
    assert_eq!(
        error(&e.get("/v1/admin/limits/mars").await),
        bad("params/scope must be equal to one of the allowed values")
    );
}

// ── Debug ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn debug_riot_reaches_any_path_through_the_limiter_and_cache() {
    let e = env().await;
    let r = e
        .get("/v1/admin/debug/riot?scope=KR&path=/lol/status/v4/platform-data")
        .await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::OK, br#"{"id":"KR"}"#.as_slice())
    );
    assert_eq!(r.headers["x-cache"], "MISS");
    let again = e
        .get("/v1/admin/debug/riot?scope=kr&path=/lol/status/v4/platform-data")
        .await;
    assert_eq!(again.headers["x-cache"], "HIT");
    let fresh = e
        .get("/v1/admin/debug/riot?scope=kr&path=/lol/status/v4/platform-data&noCache=true")
        .await;
    assert_eq!(fresh.headers["x-cache"], "BYPASS");

    // Named a method: the same request, and cache entry, as the route.
    e.get(&format!("/v1/lol/summoners/by-puuid/kr/{PUUID}")).await;
    let via_debug = e
        .get(&format!(
            "/v1/admin/debug/riot?scope=kr&method=summoner.byPuuid&path=/lol/summoner/v4/summoners/by-puuid/{PUUID}"
        ))
        .await;
    assert_eq!(via_debug.headers["x-cache"], "HIT");

    for (uri, want) in [
        (
            "/v1/admin/debug/riot?path=/x",
            bad("querystring must have required property 'scope'"),
        ),
        (
            "/v1/admin/debug/riot?scope=kr&path=lol",
            bad("path must start with '/'"),
        ),
        (
            "/v1/admin/debug/riot?scope=mars&path=/x",
            (
                StatusCode::BAD_REQUEST,
                "BAD_REGION".into(),
                "'mars' is neither a platform nor a region".into(),
            ),
        ),
        (
            "/v1/admin/debug/riot?scope=kr&path=/x&method=nope",
            bad("querystring/method must be equal to one of the allowed values"),
        ),
        (
            "/v1/admin/debug/riot?scope=kr&path=/wrong&method=summoner.byPuuid",
            bad("path does not match summoner.byPuuid's route /lol/summoner/v4/summoners/by-puuid/{puuid}"),
        ),
    ] {
        assert_eq!(error(&e.get(uri).await), want, "{uri}");
    }
}

#[tokio::test]
async fn debug_cache_reports_an_entry_without_fetching() {
    let e = env().await;
    let uri = format!(
        "/v1/admin/debug/cache?scope=kr&method=summoner.byPuuid&path=/lol/summoner/v4/summoners/by-puuid/{PUUID}"
    );
    let before = e.get(&uri).await.json();
    assert_eq!(
        (before["present"].clone(), before["ageSeconds"].clone()),
        (json!(false), Value::Null)
    );
    assert_eq!(e.upstream_calls().await, 0);

    e.get(&format!("/v1/lol/summoners/by-puuid/kr/{PUUID}")).await;
    let after = e.get(&uri).await.json();
    assert_eq!(
        (
            after["present"].clone(),
            after["stale"].clone(),
            after["ageSeconds"].clone()
        ),
        (json!(true), json!(false), json!(0))
    );
    assert_eq!(after["key"], before["key"]);
    assert!(
        after["key"]
            .as_str()
            .unwrap()
            .ends_with(&format!(":summoner.byPuuid:kr.api.riotgames.com:{PUUID}"))
    );
    assert_eq!(
        error(&e.get("/v1/admin/debug/cache?scope=kr&path=/x").await),
        bad("querystring must have required property 'method'")
    );
}

/// ADM-03: `GET /v1/admin/info` names the build and the configuration in
/// effect, and leaks no key: not the Riot key, not the bootstrap key, not the
/// caller's own.
#[tokio::test]
async fn info_reports_the_build_and_config_without_secrets() {
    const BOOTSTRAP: &str = "rpx_bootstrap_info_test_secret";
    let (_dir, state, router) = common::app_with(
        &[("BOOTSTRAP_ADMIN_KEY", BOOTSTRAP), ("JOB_CONCURRENCY", "3")],
        "http://127.0.0.1:9",
    );
    let mint = |name: &str, scopes: Vec<Scope>| NewConsumer {
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
    let get = |key: String| {
        let router = router.clone();
        async move {
            let req = Request::get("/v1/admin/info")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap();
            common::send(router, req).await
        }
    };

    let r = get(admin.key.expose().to_string()).await;
    assert_eq!(r.status, StatusCode::OK);
    let raw = String::from_utf8(r.body.clone()).unwrap();
    for secret in ["rpx_", "RGAPI", BOOTSTRAP, common::TEST_KEY] {
        assert!(!raw.contains(secret), "{secret} in {raw}");
    }
    let b = r.json();
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(b["gitSha"], riot_proxy::build_info::GIT_SHA);
    assert!(b["rustc"].as_str().unwrap().starts_with("rustc "));
    assert!(b["builtAt"].as_i64().unwrap() > 0);
    let started = b["startedAt"].as_i64().unwrap();
    assert!(started <= riot_proxy::clock::Clock::now().unix_ms && b["uptimeSeconds"].is_u64());
    assert_eq!(
        (&b["env"], &b["role"], &b["workers"]),
        (&json!("development"), &json!("all"), &json!(3))
    );
    assert_eq!(b["keyScope"], state.fetcher.key_scope().as_str());
    assert_eq!(b["features"]["dashboardUi"], true);
    assert_eq!(b["features"]["postgres"], cfg!(feature = "postgres"));
    let config = b["config"].as_object().unwrap();
    assert!(!config.contains_key("RIOT_API_KEY"));
    assert_eq!(config.len(), riot_proxy::config::VARS.len() - 1);
    assert_eq!(config["BOOTSTRAP_ADMIN_KEY"], "[REDACTED]");
    assert_eq!(config["JOB_CONCURRENCY"], "3");
    assert_eq!(config["RIOT_BASE_URL"], Value::Null);

    assert_eq!(
        get(reader.key.expose().to_string()).await.status,
        StatusCode::FORBIDDEN
    );
}

/// ADM-05: the last requests and errors, by request id. The rings are global
/// to this test binary, so the test finds its own entries by the ids it sent
/// and never asserts on what else is there.
#[tokio::test]
async fn recent_requests_and_errors_are_kept_by_request_id() {
    let e = env().await;
    let send = |uri: &str, id: &str, key: Option<&str>| {
        let mut req = Request::get(uri).header("x-request-id", id);
        if let Some(k) = key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        common::send(e.router.clone(), req.body(Body::empty()).unwrap())
    };
    let tag = ulid::Ulid::generate().to_string();
    let id = |n: &str| format!("adm05-{tag}-{n}");
    assert_eq!(
        send("/v1/admin/stats", &id("ok"), Some(&e.admin)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send("/nowhere/at/all", &id("404"), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send("/v1/admin/stats", &id("403"), Some(&e.reader)).await.status,
        StatusCode::FORBIDDEN
    );
    // Not recorded: probes and the dashboard's own polling.
    send("/healthz", &id("probe"), None).await;
    send("/v1/admin/metrics", &id("poll"), Some(&e.admin)).await;

    let all = e.get("/v1/admin/recent?kind=requests&limit=500").await.json();
    assert_eq!(
        (&all["kind"], &all["capacity"]),
        (&json!("requests"), &json!(500))
    );
    let mine: Vec<Value> = all["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["requestId"].as_str().unwrap_or_default().contains(&tag))
        .cloned()
        .collect();
    let by = |n: &str| mine.iter().find(|r| r["requestId"] == id(n)).cloned();
    assert_eq!(mine.len(), 3, "{mine:?}");
    let ok = by("ok").unwrap();
    assert_eq!(
        (
            &ok["method"],
            &ok["route"],
            &ok["path"],
            &ok["status"],
            &ok["cache"],
            &ok["consumer"]
        ),
        (
            &json!("GET"),
            &json!("/v1/admin/stats"),
            &json!("/v1/admin/stats"),
            &json!(200),
            &json!("none"),
            &json!("ops")
        )
    );
    assert!(ok["latencyMs"].is_u64() && ok["at"].is_i64());
    let missing = by("404").unwrap();
    assert_eq!(
        (&missing["route"], &missing["status"], &missing["consumer"]),
        (&json!("unmatched"), &json!(404), &Value::Null)
    );
    assert_eq!(by("403").unwrap()["status"], 403);
    assert!(by("probe").is_none() && by("poll").is_none());
    // Newest first.
    let order: Vec<&str> = mine.iter().map(|r| r["requestId"].as_str().unwrap()).collect();
    assert_eq!(order, [id("403"), id("404"), id("ok")]);

    let errs = e.get("/v1/admin/recent?kind=errors&limit=200").await.json();
    assert_eq!(
        (&errs["kind"], &errs["capacity"]),
        (&json!("errors"), &json!(200))
    );
    let err = |n: &str| {
        errs["errors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["requestId"] == id(n))
            .cloned()
    };
    let nf = err("404").unwrap();
    assert_eq!(
        (&nf["code"], &nf["status"], &nf["message"]),
        (
            &json!("NOT_FOUND"),
            &json!(404),
            &json!("No route for GET /nowhere/at/all")
        )
    );
    assert_eq!(err("403").unwrap()["code"], "FORBIDDEN");
    assert!(err("ok").is_none());

    // A 500, raised the way every handler raises one.
    let rid = id("500");
    riot_proxy::http::request_id::CURRENT
        .scope(riot_proxy::http::request_id::RequestId(rid.clone()), async {
            axum::response::IntoResponse::into_response(riot_proxy::http::ApiError::internal())
        })
        .await;
    let errs = e.get("/v1/admin/recent?kind=errors").await.json();
    let five = errs["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["requestId"] == rid.as_str())
        .cloned()
        .unwrap();
    assert_eq!(
        (&five["code"], &five["status"]),
        (&json!("INTERNAL"), &json!(500))
    );

    assert_eq!(
        error(&e.get("/v1/admin/recent?kind=everything").await),
        bad("querystring/kind must be equal to one of the allowed values")
    );
    assert_eq!(
        error(&e.get("/v1/admin/recent?limit=0").await),
        bad("querystring/limit must be >= 1")
    );
    let one = e.get("/v1/admin/recent?limit=1").await.json();
    assert_eq!(one["requests"].as_array().unwrap().len(), 1);
}

/// ADM-06: the backups beside the database, the last maintenance run, and a
/// run queued on demand under the tick's dedupe key.
#[tokio::test]
async fn backups_list_the_files_and_the_last_run_and_run_now_dedupes() {
    let e = env().await;
    let m = std::sync::Arc::clone(&e.state.maintenance);

    // No directory yet: an empty list, not an error.
    let r = e.get("/v1/admin/backups").await;
    assert_eq!(r.status, StatusCode::OK);
    let b = r.json();
    assert_eq!(
        (&b["files"], &b["lastRun"], &b["keep"]),
        (&json!([]), &Value::Null, &json!(14))
    );
    assert_eq!(b["dir"], m.backup_dir.display().to_string());
    assert!(
        b["diskFreeBytes"].as_u64().unwrap() > 0,
        "the parent's free space"
    );

    std::fs::create_dir_all(&m.backup_dir).unwrap();
    std::fs::write(m.backup_dir.join("riot-proxy-2026-10-01.db"), vec![0u8; 10]).unwrap();
    std::fs::write(m.backup_dir.join("riot-proxy-2026-10-02.db"), vec![0u8; 20]).unwrap();
    std::fs::write(m.backup_dir.join("notes.txt"), b"not a backup").unwrap();
    let b = e.get("/v1/admin/backups").await.json();
    let files = b["files"].as_array().unwrap();
    assert_eq!(
        files
            .iter()
            .map(|f| (f["name"].clone(), f["bytes"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("riot-proxy-2026-10-02.db"), json!(20)),
            (json!("riot-proxy-2026-10-01.db"), json!(10))
        ],
        "newest first, only our names"
    );
    assert!(files[0]["modifiedAt"].as_i64().unwrap() > 0);

    // A run is reported in camelCase, as it ran.
    let now = riot_proxy::clock::Clock::now().unix_ms;
    let report = m.run_once(now).await.unwrap();
    let b = e.get("/v1/admin/backups").await.json();
    let last = &b["lastRun"];
    assert_eq!((&last["at"], &last["error"]), (&json!(now), &Value::Null));
    assert!(last["ms"].is_u64());
    assert_eq!(last["report"]["jobsDeleted"], json!(report.jobs_deleted));
    assert!(
        last["report"]["backup"].as_str().unwrap().ends_with(".db"),
        "{last}"
    );

    // Run now: queued, then joined.
    let first = e.post("/v1/admin/maintenance/run", json!({})).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    let first = first.json();
    assert_eq!((&first["ok"], &first["status"]), (&json!(true), &json!("queued")));
    let again = e.post("/v1/admin/maintenance/run", json!({})).await.json();
    assert_eq!(
        (&again["status"], &again["jobId"]),
        (&json!("already-queued"), &first["jobId"])
    );
}

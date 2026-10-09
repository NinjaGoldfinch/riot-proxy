//! `/v1/players/*` through the full app against a wiremock Riot (plan P5-04),
//! ported from v1 `test/players-composite.test.ts` and `player-champions.test.ts`.
//! Riot's side is the real recording in tests/fixtures/replay/cold-lookup.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use riot_proxy::consumers::{self, NewConsumer, Scope};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/replay/cold-lookup");
const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const MATCH_IDS: [&str; 5] = [
    "KR_8393343196",
    "KR_8393320187",
    "KR_8393276163",
    "KR_8393234454",
    "KR_8393207026",
];

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

fn ok(body: Vec<u8>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    key: String,
}

/// Which Riot calls fail, by path fragment.
async fn env(failing: &[&str]) -> Env {
    let server = MockServer::start().await;
    for f in failing {
        Mock::given(path_regex(format!(".*{f}.*")))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    let routes = [
        (
            "/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1".to_string(),
            fixture("01-account.byRiotId.body"),
        ),
        (
            format!("/riot/account/v1/accounts/by-puuid/{PUUID}"),
            fixture("01-account.byRiotId.body"),
        ),
        (
            format!("/lol/summoner/v4/summoners/by-puuid/{PUUID}"),
            fixture("02-summoner.byPuuid.body"),
        ),
        (
            format!("/lol/league/v4/entries/by-puuid/{PUUID}"),
            fixture("03-league.entriesByPuuid.body"),
        ),
        (
            format!("/lol/champion-mastery/v4/champion-masteries/by-puuid/{PUUID}/top"),
            fixture("04-mastery.topByPuuid.body"),
        ),
        (
            format!("/lol/match/v5/matches/by-puuid/{PUUID}/ids"),
            fixture("05-match.idsByPuuid.body"),
        ),
    ];
    for (p, body) in routes {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ok(body))
            .with_priority(5)
            .mount(&server)
            .await;
    }
    for (i, id) in MATCH_IDS.iter().enumerate() {
        Mock::given(method("GET"))
            .and(path(format!("/lol/match/v5/matches/{id}")))
            .respond_with(ok(fixture(&format!("{:02}-match.byId.body", i + 6))))
            .with_priority(5)
            .mount(&server)
            .await;
    }
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let key = consumers::create(
        &state.db,
        NewConsumer {
            name: "web".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 10_000,
            key: None,
        },
    )
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
        key,
    }
}

impl Env {
    async fn get(&self, uri: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {}", self.key))
            .body(Body::empty())
            .unwrap();
        common::send(self.router.clone(), req).await
    }

    /// Upstream paths called so far, in order.
    async fn calls(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect()
    }

    async fn calls_to(&self, fragment: &str) -> usize {
        self.calls().await.iter().filter(|p| p.contains(fragment)).count()
    }
}

fn x_cache(r: &common::Reply) -> (&str, &str) {
    (
        r.headers["x-cache"].to_str().unwrap(),
        r.headers["x-cache-age"].to_str().unwrap(),
    )
}

const PROFILE: &str = "/v1/players/by-riot-id/Hide%20on%20bush/KR1/profile?platform=kr";

// ── Profile ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resolves_a_riot_id_and_reuses_that_account_as_the_composite_part() {
    let e = env(&[]).await;
    let r = e.get(PROFILE).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(x_cache(&r), ("MISS", "0"));
    assert_eq!(
        e.calls_to("/accounts/by-puuid/").await,
        0,
        "account fetched once, by Riot ID"
    );
    let body = r.json();
    assert_eq!(
        body["account"],
        serde_json::from_slice::<Value>(&fixture("01-account.byRiotId.body")).unwrap()
    );
    insta::assert_json_snapshot!("players_profile", body);

    // The player is remembered, named.
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    let row = riot_proxy::players::get(&e.state.db, &scope, PUUID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.platform.as_str(),
            row.game_name.as_deref(),
            row.tag_line.as_deref(),
            row.tracked
        ),
        ("kr", Some("Hide on bush"), Some("KR1"), false)
    );

    let again = e.get(PROFILE).await;
    assert_eq!(x_cache(&again).0, "HIT");
    assert_eq!(e.calls().await.len(), 4, "every part cached");
}

#[tokio::test]
async fn the_puuid_route_fetches_the_account_itself() {
    let e = env(&[]).await;
    let r = e
        .get(&format!("/v1/players/{PUUID}/profile?platform=kr&topMastery=3"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(e.calls_to("/accounts/by-puuid/").await, 1);
    let mastery = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|q| q.url.path().ends_with("/top"))
        .unwrap();
    assert_eq!(mastery.url.query(), Some("count=3"));
    assert_eq!(r.json()["region"], "asia");
}

#[tokio::test]
async fn a_riot_id_that_resolves_to_no_puuid_is_a_404() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok(br#"{"gameName":"x"}"#.to_vec()))
        .mount(&server)
        .await;
    let (_dir, state, router) = common::app_with(&[("AUTH_DISABLED", "true")], &server.uri());
    let _ = state;
    let r = common::get(router, "/v1/players/by-riot-id/x/y/profile?platform=kr").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        r.json()["error"]["message"],
        "Riot ID 'x#y' did not resolve to a PUUID"
    );
}

#[tokio::test]
async fn one_upstream_5xx_degrades_to_a_warning() {
    let e = env(&["/entries/by-puuid/"]).await;
    let r = e.get(PROFILE).await;
    assert_eq!(r.status, StatusCode::OK, "partial, not failed");
    let body = r.json();
    assert_eq!(body["league"], Value::Null);
    assert_eq!(body["ageSeconds"]["league"], Value::Null);
    assert!(body["summoner"].is_object() && body["mastery"].is_array());
    assert_eq!(
        body["warnings"],
        json!(["league unavailable (UPSTREAM_ERROR: Upstream request failed)"])
    );
}

#[tokio::test]
async fn every_part_failing_is_a_404() {
    let e = env(&["/by-puuid/"]).await;
    let r = e.get(&format!("/v1/players/{PUUID}/profile?platform=kr")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        r.json()["error"]["message"],
        "No profile data available for this PUUID"
    );
}

#[tokio::test]
async fn profile_queries_follow_v1_validation() {
    let e = env(&[]).await;
    for (uri, code, message) in [
        // No default platform (ADR-065).
        (
            format!("/v1/players/{PUUID}/profile"),
            "VALIDATION",
            "querystring must have required property 'platform'",
        ),
        (
            "/v1/players/by-riot-id/Hide%20on%20bush/KR1/profile".to_string(),
            "VALIDATION",
            "querystring must have required property 'platform'",
        ),
        (
            format!("/v1/players/{PUUID}/matches"),
            "VALIDATION",
            "querystring must have required property 'platform'",
        ),
        (
            format!("/v1/players/{PUUID}/profile?platform=KR"),
            "BAD_REGION",
            "querystring/platform must be equal to one of the allowed values",
        ),
        (
            format!("/v1/players/{PUUID}/profile?platform=kr&topMastery=21"),
            "VALIDATION",
            "querystring/topMastery must be <= 20",
        ),
        (
            format!("/v1/players/{PUUID}/profile?platform=kr&refresh=yes"),
            "VALIDATION",
            "querystring/refresh must be boolean",
        ),
        (
            "/v1/players/short/profile?platform=kr".to_string(),
            "VALIDATION",
            "params/puuid must NOT have fewer than 60 characters",
        ),
    ] {
        let r = e.get(&uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            (
                r.json()["error"]["code"].clone(),
                r.json()["error"]["message"].clone()
            ),
            (json!(code), json!(message))
        );
    }
    assert!(e.calls().await.is_empty(), "validation never reaches upstream");
}

// ── Match page ──────────────────────────────────────────────────────────────

fn page(query: &str) -> String {
    format!("/v1/players/{PUUID}/matches?platform=kr{query}")
}

#[tokio::test]
async fn hydrates_every_id_on_the_page_then_serves_it_from_the_archive() {
    let e = env(&[]).await;
    let r = e.get(&page("&count=5")).await;
    assert_eq!((r.status, x_cache(&r).0), (StatusCode::OK, "MISS"));
    let mut body = r.json();
    assert_eq!(body["matchIds"], json!(MATCH_IDS));
    assert_eq!(body["matches"].as_array().unwrap().len(), 5);
    assert_eq!(body["hasMore"], true);
    // The first lookup queues the player's history walk (v1 #44).
    let job = std::mem::replace(&mut body["backfill"]["jobId"], json!("<ulid>"));
    assert_eq!(job.as_str().unwrap().len(), 26);
    assert_eq!(
        (
            body["backfill"]["status"].clone(),
            body["backfill"]["limit"].clone()
        ),
        (json!("queued"), json!(u32::MAX)),
        "uncapped by default (ADR-081)"
    );
    insta::assert_json_snapshot!("players_match_page", body);
    let ids = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|q| q.url.path().ends_with("/ids"))
        .unwrap();
    assert_eq!(ids.url.query(), Some("start=0&count=5"));

    // Second view: the ids are cached and every match comes from the archive.
    let again = e.get(&page("&count=5")).await;
    assert_eq!(x_cache(&again).0, "HIT");
    assert_eq!(again.json()["matches"], body["matches"]);
    assert_eq!(e.calls_to("/matches/KR_").await, 5, "each match fetched once");

    // The lookup recorded the player (no Riot ID known on this path).
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    let row = riot_proxy::players::get(&e.state.db, &scope, PUUID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.platform.as_str(), row.game_name), ("kr", None));
}

#[tokio::test]
async fn a_short_page_has_no_more() {
    let e = env(&[]).await;
    let r = e.get(&page("")).await;
    let body = r.json();
    assert_eq!(
        (body["count"].clone(), body["hasMore"].clone()),
        (json!(10), json!(false))
    );
}

#[tokio::test]
async fn an_unavailable_match_is_dropped_into_warnings() {
    let e = env(&["KR_8393320187"]).await;
    let r = e.get(&page("&count=5")).await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(
        body["matchIds"].as_array().unwrap().len(),
        5,
        "the id page is Riot's"
    );
    assert_eq!(body["matches"].as_array().unwrap().len(), 4);
    assert_eq!(
        body["warnings"],
        json!(["match KR_8393320187 unavailable (UPSTREAM_ERROR: Upstream request failed)"])
    );
}

#[tokio::test]
async fn a_failed_id_lookup_fails_the_page() {
    let e = env(&["/ids"]).await;
    let r = e.get(&page("")).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json()["error"]["code"], "UPSTREAM_ERROR");
}

#[tokio::test]
async fn a_match_that_does_not_mention_the_player_is_named_not_served() {
    let e = env(&[]).await;
    let other = "Q".repeat(78);
    Mock::given(method("GET"))
        .and(path(format!("/lol/match/v5/matches/by-puuid/{other}/ids")))
        .respond_with(ok(br#"["KR_8393343196"]"#.to_vec()))
        .with_priority(1)
        .mount(&e.server)
        .await;
    let r = e.get(&format!("/v1/players/{other}/matches?platform=kr")).await;
    let body = r.json();
    assert_eq!(body["matches"], json!([]));
    assert_eq!(
        body["warnings"],
        json!(["match KR_8393343196 unavailable (no participant for this player)"])
    );
}

#[tokio::test]
async fn the_fan_out_is_capped_at_twenty() {
    let e = env(&[]).await;
    let r = e.get(&page("&count=21")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["message"], "querystring/count must be <= 20");
    let r = e.get(&page("&type=arena")).await;
    assert_eq!(
        r.json()["error"]["message"],
        "querystring/type must be equal to one of the allowed values"
    );
}

// ── Champion filter (SITE-02) ───────────────────────────────────────────────

/// In the fixture, the player is on 134 in the two newest games, then 143,
/// 516 and 105.
const ON_134: [&str; 2] = [MATCH_IDS[0], MATCH_IDS[1]];

#[tokio::test]
async fn a_champion_page_reads_riots_newest_ids_then_pages_the_archive() {
    let e = env(&[]).await;
    let r = e.get(&page("&champion=134")).await;
    assert_eq!(r.status, StatusCode::OK);
    let b = r.json();
    assert_eq!(b["matchIds"], json!(ON_134));
    let picked: Vec<&Value> = b["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["player"]["championId"])
        .collect();
    assert_eq!(picked, [&json!(134), &json!(134)]);
    assert_eq!(
        (b["champion"].clone(), b["hasMore"].clone(), b["archive"].clone()),
        (json!(134), json!(false), json!({"complete": false})),
        "the backfill this lookup queued hasn't run"
    );
    // Riot's newest page, whatever page was asked for, and each match once.
    let ids = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|q| q.url.path().ends_with("/ids"))
        .unwrap();
    assert_eq!(ids.url.query(), Some("start=0&count=20"));
    assert_eq!(e.calls_to("/matches/KR_").await, 5, "every recent match archived");

    // Paging is the archive's, and exact.
    let first = e.get(&page("&champion=134&count=1")).await.json();
    assert_eq!(
        (first["matchIds"].clone(), first["hasMore"].clone()),
        (json!([ON_134[0]]), json!(true))
    );
    let second = e.get(&page("&champion=134&count=1&start=1")).await.json();
    assert_eq!(
        (second["matchIds"].clone(), second["hasMore"].clone()),
        (json!([ON_134[1]]), json!(false))
    );
    assert_eq!(e.calls_to("/matches/KR_").await, 5, "nothing fetched twice");

    // `queue` narrows it further: 516 was played in queue 1750.
    let q = e.get(&page("&champion=516&queue=420")).await.json();
    assert_eq!(q["matchIds"], json!([]));
    let q = e.get(&page("&champion=516")).await.json();
    assert_eq!(q["matchIds"], json!([MATCH_IDS[3]]));
}

#[tokio::test]
async fn an_unfiltered_page_has_no_champion_or_archive_fields() {
    let e = env(&[]).await;
    let b = e.get(&page("&count=5")).await.json();
    assert!(b.get("champion").is_none() && b.get("archive").is_none(), "{b}");
}

#[tokio::test]
async fn a_champion_page_is_complete_once_a_backfill_has_reached_the_start() {
    let e = env(&[]).await;
    e.get(&page("&count=5")).await;
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    let done = serde_json::to_string(&riot_proxy::jobs::archive::BackfillState {
        done_at: Some(1),
        ..Default::default()
    })
    .unwrap();
    e.state
        .db
        .write(move |c| {
            c.execute(
                "UPDATE players SET backfill_state = ?1 WHERE key_scope = ?2 AND puuid = ?3",
                [done, scope, PUUID.to_string()],
            )
            .map_err(riot_proxy::db::DbError::from)
        })
        .await
        .unwrap();
    let b = e.get(&page("&champion=134")).await.json();
    assert_eq!(b["archive"], json!({"complete": true}));
    assert_eq!(b["backfill"], Value::Null, "nothing left to queue");
}

#[tokio::test]
async fn a_recent_match_that_cannot_be_archived_is_named() {
    let e = env(&[MATCH_IDS[2]]).await;
    let b = e.get(&page("&champion=134")).await.json();
    assert_eq!(b["matchIds"], json!(ON_134));
    let w = b["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].as_str()
            .unwrap()
            .starts_with(&format!("recent match {} not archived", MATCH_IDS[2])),
        "{w:?}"
    );
}

#[tokio::test]
async fn champion_is_validated_and_does_not_combine_with_type() {
    let e = env(&[]).await;
    for (q, message) in [
        ("&champion=0", "querystring/champion must be >= 1"),
        ("&champion=10001", "querystring/champion must be <= 10000"),
        ("&champion=x", "querystring/champion must be integer"),
        (
            "&champion=134&type=ranked",
            "querystring/type must not be set with champion",
        ),
    ] {
        let r = e.get(&page(q)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(r.json()["error"]["message"], message, "{q}");
    }
    assert!(e.calls().await.is_empty(), "refused before any Riot call");
}

// ── Manual refresh ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_refresh_bypasses_the_cache_once_a_minute() {
    let e = env(&[]).await;
    e.get(PROFILE).await;
    assert_eq!(e.calls().await.len(), 4);

    let first = e.get(&format!("{PROFILE}&refresh=true")).await;
    let b = first.json();
    assert_eq!(
        (b["refreshed"].clone(), b["refreshAvailableIn"].clone()),
        (json!(true), json!(60))
    );
    assert_eq!(x_cache(&first).0, "MISS");
    // The Riot ID mapping stays cached; the account is re-read by PUUID.
    assert_eq!(e.calls().await.len(), 8);
    assert_eq!(e.calls_to("/accounts/by-puuid/").await, 1);

    let second = e.get(&format!("{PROFILE}&refresh=true")).await;
    assert_eq!(second.json()["refreshed"], false, "refused inside the window");
    assert_eq!(x_cache(&second).0, "HIT");
    assert_eq!(e.calls().await.len(), 8);

    // A plain lookup reports the running cooldown, whichever way it came in.
    let plain = e.get(&format!("/v1/players/{PUUID}/profile?platform=kr")).await;
    let left = plain.json()["refreshAvailableIn"].as_u64().unwrap();
    assert!((59..=60).contains(&left), "{left}");
}

#[tokio::test]
async fn a_match_page_refresh_rereads_only_the_id_list() {
    let e = env(&[]).await;
    e.get(&page("&count=5")).await;
    let r = e.get(&page("&count=5&refresh=true")).await;
    assert_eq!(r.json()["refreshed"], true);
    assert_eq!(e.calls_to("/ids").await, 2);
    assert_eq!(e.calls_to("/matches/KR_").await, 5, "matches are immutable");
    // Profile and matches are metered independently.
    assert_eq!(
        e.get(&format!("{PROFILE}&refresh=true")).await.json()["refreshed"],
        true
    );
}

// ── Fetch age (SITE-01) ─────────────────────────────────────────────────────

const PARTS: [&str; 4] = ["account", "summoner", "league", "mastery"];

/// ninjagoldfinch.lol's report: a refresh that changes nothing resets each
/// part's fetch age, while its content age keeps counting.
#[tokio::test]
async fn a_refresh_that_changes_nothing_resets_only_the_fetch_age() {
    let e = env(&[]).await;
    let profile = format!("/v1/players/{PUUID}/profile?platform=kr");
    e.get(&profile).await;
    tokio::time::sleep(Duration::from_millis(1600)).await;

    // From cache: both ages have grown.
    let cached = e.get(&profile).await;
    let b = cached.json();
    for part in PARTS {
        assert_eq!(
            (
                b["ageSeconds"][part].clone(),
                b["fetchedAgeSeconds"][part].clone()
            ),
            (json!(2), json!(2)),
            "{part}"
        );
    }
    assert_eq!(cached.headers["x-cache-fetched-age"], "2");

    // Riot answers with the same bytes: the content is still 2 s old, but it
    // was read just now.
    let r = e.get(&format!("{profile}&refresh=true")).await;
    let b = r.json();
    assert_eq!(b["refreshed"], true);
    for part in PARTS {
        assert_eq!(
            (
                b["ageSeconds"][part].clone(),
                b["fetchedAgeSeconds"][part].clone()
            ),
            (json!(2), json!(0)),
            "{part}"
        );
    }
    assert_eq!(x_cache(&r), ("MISS", "2"));
    assert_eq!(r.headers["x-cache-fetched-age"], "0");
}

#[tokio::test]
async fn a_failed_part_has_no_fetch_age() {
    let e = env(&["/league/"]).await;
    let b = e.get(PROFILE).await.json();
    assert_eq!(b["fetchedAgeSeconds"]["league"], Value::Null);
    assert_eq!(b["fetchedAgeSeconds"]["summoner"], json!(0));
}

#[tokio::test]
async fn the_match_page_says_when_its_id_list_was_read() {
    let e = env(&[]).await;
    let first = e.get(&page("&count=5")).await;
    let b = first.json();
    assert_eq!(
        (
            b["matchIdsAgeSeconds"].clone(),
            b["matchIdsFetchedAgeSeconds"].clone()
        ),
        (json!(0), json!(0))
    );
    assert_eq!(first.headers["x-cache-fetched-age"], "0");
    tokio::time::sleep(Duration::from_millis(1600)).await;

    let r = e.get(&page("&count=5&refresh=true")).await;
    let b = r.json();
    assert_eq!(
        (
            b["matchIdsAgeSeconds"].clone(),
            b["matchIdsFetchedAgeSeconds"].clone()
        ),
        (json!(2), json!(0)),
        "the same ids, read again"
    );
    // Every match now comes from the archive, so the id list alone sets it.
    assert_eq!(r.headers["x-cache-fetched-age"], "0");
}

#[tokio::test]
async fn a_passthrough_sends_the_fetch_age_except_from_the_archive() {
    let e = env(&[]).await;
    let summoner = format!("/v1/lol/summoners/by-puuid/kr/{PUUID}");
    assert_eq!(e.get(&summoner).await.headers["x-cache-fetched-age"], "0");
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let hit = e.get(&summoner).await;
    assert_eq!(
        (
            x_cache(&hit).0,
            hit.headers["x-cache-fetched-age"].to_str().unwrap()
        ),
        ("HIT", "2")
    );

    let m = format!("/v1/lol/matches/asia/{}", MATCH_IDS[0]);
    assert_eq!(e.get(&m).await.headers["x-cache-fetched-age"], "0");
    let archived = e.get(&m).await;
    assert_eq!(x_cache(&archived).0, "ARCHIVE");
    assert!(
        !archived.headers.contains_key("x-cache-fetched-age"),
        "an archived match is never fetched again"
    );
}

// ── Champion pool ───────────────────────────────────────────────────────────

fn pool(query: &str) -> String {
    format!("/v1/players/{PUUID}/champions{query}")
}

#[tokio::test]
async fn the_pool_is_grouped_from_the_archive_and_never_calls_riot() {
    let e = env(&[]).await;
    let empty = e.get(&pool("")).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(
        empty.json(),
        json!({"puuid": PUUID, "platform": null, "queue": null, "patch": null, "archivedGames": 0, "champions": []})
    );

    e.get(&page("&count=5")).await;
    let before = e.calls().await.len();
    // A different filter set, so not the cached empty pool above.
    let r = e.get(&pool("?platform=kr")).await;
    assert_eq!((r.status, x_cache(&r).0), (StatusCode::OK, "MISS"));
    let body = r.json();
    assert_eq!(body["archivedGames"], 5);
    insta::assert_json_snapshot!("players_champion_pool", body);

    let again = e.get(&pool("?platform=kr")).await;
    assert_eq!(x_cache(&again).0, "HIT");
    assert_eq!(again.json(), body);
    assert_eq!(e.calls().await.len(), before, "no Riot calls");

    let ranked = e.get(&pool("?queue=420&patch=16.19&limit=1")).await.json();
    assert_eq!(
        (ranked["queue"].clone(), ranked["patch"].clone()),
        (json!(420), json!("16.19"))
    );
    assert_eq!(ranked["champions"].as_array().unwrap().len(), 1);
    assert_eq!(e.get(&pool("?platform=euw1")).await.json()["archivedGames"], 0);
}

#[tokio::test]
async fn the_pool_names_the_champions_the_mirror_knows() {
    let e = env(&[]).await;
    // A mirrored patch that knows two of the four champions played.
    let dir = e.state.ddragon.dir().join("16.19.1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("champion.json"),
        r#"{"data":{"Syndra":{"key":"134","name":"Syndra"},"Ziggs":{"key":"115","name":"Ziggs"},"Zyra":{"key":"143","name":"Zyra"}}}"#,
    )
    .unwrap();
    std::fs::write(dir.join("versions.json"), r#"["16.19.1"]"#).unwrap();

    e.get(&page("&count=5")).await;
    let body = e.get(&pool("?platform=kr")).await.json();
    let names: Vec<(i64, Option<&str>)> = body["champions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["championId"].as_i64().unwrap(), c["championName"].as_str()))
        .collect();
    // Unknown ids carry no name at all (v1: absent, not guessed).
    assert_eq!(
        names,
        [
            (134, Some("Syndra")),
            (105, None),
            (143, Some("Zyra")),
            (516, None)
        ]
    );
}

#[tokio::test]
async fn the_pool_rejects_what_is_not_a_patch_or_platform() {
    let e = env(&[]).await;
    let bad = e.get(&pool("?patch=latest")).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["error"]["code"], "VALIDATION");
    let bad = e.get(&pool("?platform=nowhere")).await;
    assert_eq!(bad.json()["error"]["code"], "BAD_REGION");
    let bad = e.get(&pool("?limit=501")).await;
    assert_eq!(bad.json()["error"]["message"], "querystring/limit must be <= 500");
}

#[tokio::test]
async fn the_players_routes_need_a_read_key() {
    let e = env(&[]).await;
    let r = common::get(e.router.clone(), &page("")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

//! The P7 exit check (docs/IMPLEMENTATION.md), end to end through `serve`:
//! `LADDER_QUEUES=RANKED_SOLO_5x5 LADDER_TIER_FLOOR=MASTER`, a crawl against a
//! wiremock ladder of 30 players completes enumerate → collect → archive →
//! done, each match fetched exactly once (wiremock request count), and the
//! dashboard's endpoints show the live numbers.
//!
//! Ignored by default (a few seconds):
//!     cargo test --test p7_exit -- --ignored --nocapture
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use riot_proxy::cli::serve::{ServeOptions, serve_with};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MATCH: &[u8] = include_bytes!("fixtures/replay/cold-lookup/06-match.byId.body");
const PLAYERS: usize = 30;
const MATCHES: usize = 12;

/// Generous limits, so the limiter never waits on a mock.
fn riot(status: u16) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .insert_header("x-app-rate-limit", "30000:10,1800000:600")
        .insert_header("x-app-rate-limit-count", "1:10,1:600")
        .insert_header("x-method-rate-limit", "30000:10")
        .insert_header("x-method-rate-limit-count", "1:10")
}

fn puuid(i: usize) -> String {
    format!("P{i:0>77}")
}

/// Match `k`'s ten players: a sliding window, so every player is in four
/// matches and every match is reachable from ten walks.
fn players_of(k: usize) -> Vec<usize> {
    (0..10).map(|j| (k * 5 + j) % PLAYERS).collect()
}

fn match_id(k: usize) -> String {
    format!("KR_{}", 1000 + k)
}

fn match_body(k: usize) -> Value {
    let mut body: Value = serde_json::from_slice(MATCH).unwrap();
    let players = players_of(k);
    body["metadata"]["matchId"] = json!(match_id(k));
    body["metadata"]["participants"] = json!(players.iter().map(|p| puuid(*p)).collect::<Vec<_>>());
    body["info"]["gameId"] = json!(1000 + k);
    for (slot, p) in players.iter().enumerate() {
        let part = &mut body["info"]["participants"][slot];
        part["puuid"] = json!(puuid(*p));
        part["riotIdGameName"] = json!(format!("Player{p}"));
        part["riotIdTagline"] = json!("KR1");
    }
    body
}

async fn mock_riot(server: &MockServer) {
    let ladder: Vec<Value> = (0..PLAYERS)
        .map(|i| json!({"puuid": puuid(i), "leaguePoints": 1000 - i, "wins": 50, "losses": 40}))
        .collect();
    let league = |tier: &str, entries: Vec<Value>| {
        Mock::given(method("GET"))
            .and(path(format!(
                "/lol/league/v4/{}leagues/by-queue/RANKED_SOLO_5x5",
                tier.to_ascii_lowercase()
            )))
            .respond_with(
                riot(200)
                    .set_body_json(json!({"tier": tier, "queue": "RANKED_SOLO_5x5", "entries": entries})),
            )
    };
    league("MASTER", ladder).mount(server).await;
    league("GRANDMASTER", vec![]).mount(server).await;
    league("CHALLENGER", vec![]).mount(server).await;
    for p in 0..PLAYERS {
        let mut ids: Vec<usize> = (0..MATCHES).filter(|k| players_of(*k).contains(&p)).collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Mock::given(method("GET"))
            .and(path(format!("/lol/match/v5/matches/by-puuid/{}/ids", puuid(p))))
            .and(query_param("queue", "420"))
            .respond_with(riot(200).set_body_json(ids.iter().map(|k| match_id(*k)).collect::<Vec<_>>()))
            .mount(server)
            .await;
    }
    for k in 0..MATCHES {
        Mock::given(method("GET"))
            .and(path(format!("/lol/match/v5/matches/{}", match_id(k))))
            .respond_with(riot(200).set_body_json(match_body(k)))
            .mount(server)
            .await;
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[ignore = "end to end through serve; run at the phase gate"]
async fn a_master_crawl_of_thirty_players_runs_every_stage_and_fetches_each_match_once() {
    let server = MockServer::start().await;
    mock_riot(&server).await;
    let data = tempfile::tempdir().unwrap();
    let port = free_port();
    let config = common::config(&[
        ("DATA_DIR", data.path().to_str().unwrap()),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("LADDER_QUEUES", "RANKED_SOLO_5x5"),
        ("LADDER_TIER_FLOOR", "MASTER"),
        ("LADDER_PLATFORMS", "kr"),
        ("DEFAULT_PLATFORM", "kr"),
        ("AGGREGATE_MIN_GAMES", "0"),
        ("AUTH_DISABLED", "true"),
        ("METRICS_INTERVAL_S", "1"),
        ("LOG_LEVEL", "warn"),
    ]);
    let options = ServeOptions {
        riot_base_url: Some(server.uri()),
        skip_tracing_init: true,
        // Unmocked: the sync fails and retries, but never leaves the machine.
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock(&server.uri())),
        tls_pem: None,
    };
    let task = tokio::spawn(async move { serve_with(config, options).await.unwrap() });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    for _ in 0..200 {
        if http
            .get(format!("{base}/readyz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Watch the crawl on the ladder topic, as the dashboard does.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/ws"))
        .await
        .unwrap();
    ws.next().await.unwrap().unwrap();
    ws.send(Message::Text(
        json!({"op": "subscribe", "topics": ["ladder", "metrics"]})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    let started: Value = http
        .post(format!("{base}/v1/admin/ladder/crawl"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (
            &started["status"],
            &started["platform"],
            &started["queue"],
            &started["legs"]
        ),
        (
            &json!("started"),
            &json!("kr"),
            &json!("RANKED_SOLO_5x5"),
            &json!(3)
        ),
        "MASTER floor: the three apex leagues"
    );
    let crawl_id = started["crawlId"].as_str().unwrap().to_string();

    // Every ladder event until the analytics recompute that a clean crawl queues.
    let mut ladder_events = vec![];
    let mut snapshots = 0;
    let at = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(Ok(msg)) = ws.next().await {
            let Message::Text(t) = msg else { continue };
            let frame: Value = serde_json::from_str(&t).unwrap();
            match frame["event"].as_str() {
                Some("metrics.snapshot") => snapshots += 1,
                Some(event) => {
                    let phase = frame["data"]["phase"].as_str().unwrap_or("").to_string();
                    ladder_events.push((event.to_string(), phase));
                    if event == "analytics.updated" {
                        return;
                    }
                }
                None => {}
            }
        }
    })
    .await
    .expect("the crawl completes and its analytics land");
    let took = at.elapsed();
    // The live snapshot the dashboard draws, after the crawl.
    let live: Value = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            let Message::Text(t) = msg else { continue };
            let frame: Value = serde_json::from_str(&t).unwrap();
            if frame["event"] == "metrics.snapshot" {
                snapshots += 1;
                if frame["data"]["ladder"]["lastCompleted"]["status"] == "completed" {
                    return frame["data"].clone();
                }
            }
        }
        Value::Null
    })
    .await
    .expect("a live snapshot after the crawl");

    let crawls: Value = http
        .get(format!("{base}/v1/admin/ladder/crawls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let crawl = &crawls["crawls"][0];
    // Every match fetched exactly once, however many of its ten players were walked.
    let requests = server.received_requests().await.unwrap();
    let fetches: Vec<usize> = (0..MATCHES)
        .map(|k| {
            let p = format!("/lol/match/v5/matches/{}", match_id(k));
            requests.iter().filter(|r| r.url.path() == p).count()
        })
        .collect();
    // Let the queue drain (names:backfill), then read what the dashboard reads.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snap: Value = http
        .get(format!("{base}/v1/admin/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dashboard = http
        .get(format!("{base}/dashboard"))
        .send()
        .await
        .unwrap()
        .status();

    println!("crawl {crawl_id}: {} in {took:.1?}", crawl["status"]);
    println!("ladder events: {ladder_events:?}");
    println!("match fetches: {fetches:?}");
    println!(
        "counters: entries {} players {} walked {} match ids {} queued {}",
        crawl["entriesSeen"],
        crawl["playersDiscovered"],
        crawl["backfillsEnqueued"],
        crawl["matchIdsSeen"],
        crawl["matchesQueued"]
    );
    println!(
        "dashboard: archivedMatches {} knownPlayers {} ladder.entries {} lastCompleted {} analytics {} topChampions {} snapshots {snapshots}",
        snap["totals"]["archivedMatches"],
        snap["totals"]["knownPlayers"],
        snap["ladder"]["entries"],
        snap["ladder"]["lastCompleted"]["status"],
        snap["analytics"]["lastRuns"][0]["status"],
        snap["analytics"]["topChampions"].as_array().map_or(0, Vec::len),
    );

    assert_eq!(crawl["id"], json!(crawl_id));
    assert_eq!(
        (&crawl["status"], &crawl["phase"]),
        (&json!("completed"), &json!("archive"))
    );
    assert_eq!(
        ladder_events,
        [
            ("crawl.phase".to_string(), "collect".to_string()),
            ("crawl.phase".to_string(), "archive".to_string()),
            ("crawl.phase".to_string(), "completed".to_string()),
            ("ladder.crawl.completed".to_string(), String::new()),
            ("analytics.updated".to_string(), String::new()),
        ]
    );
    assert_eq!(fetches, vec![1; MATCHES], "each match fetched exactly once");
    assert_eq!(
        (
            &crawl["entriesSeen"],
            &crawl["backfillsEnqueued"],
            &crawl["matchIdsSeen"],
            &crawl["matchesQueued"]
        ),
        (&json!(30), &json!(30), &json!(12), &json!(12))
    );
    assert_eq!(snap["totals"]["archivedMatches"], 12);
    assert_eq!(snap["ladder"]["entries"], 30);
    assert_eq!(snap["ladder"]["lastCompleted"]["status"], "completed");
    assert_eq!(snap["analytics"]["lastRuns"][0]["status"], "completed");
    assert!(snapshots > 0, "the metrics topic ticked while held");
    assert_eq!(
        (&live["totals"]["archivedMatches"], &live["ladder"]["entries"]),
        (&json!(12), &json!(30)),
        "the live snapshot on /v1/ws"
    );
    assert_eq!(dashboard, reqwest::StatusCode::OK);
    task.abort();
}

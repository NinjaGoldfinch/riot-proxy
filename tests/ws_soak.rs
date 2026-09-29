//! Soak (plan P6-01 acceptance): 1 000 sockets held open for 20 s with a 1 s
//! heartbeat and a steady trickle of events; resident memory must not grow once
//! they are connected. Ignored by default:
//!     cargo test --release --test ws_soak -- --ignored --nocapture
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use riot_proxy::ws::protocol::PATCH;
use riot_proxy::ws::socket::serve_socket_with;
use riot_proxy::ws::{Client, Hub, Topic};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

const SOCKETS: usize = 1_000;
const RUN: Duration = Duration::from_secs(20);
/// Growth allowed between the two samples, once every socket is connected.
const SLACK_BYTES: u64 = 16 * 1024 * 1024;

async fn upgrade(
    State(hub): State<Hub>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let who = q.get("who").cloned().unwrap_or_default();
    let client = Client {
        consumer_id: who.clone(),
        consumer: who,
        admin: false,
    };
    ws.on_upgrade(move |socket| serve_socket_with(hub, socket, client, Duration::from_secs(1)))
}

/// Resident set size of this process (Linux).
fn rss_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

fn puuid(i: usize) -> String {
    format!("{}{i:06}", "s".repeat(60))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "20 s soak; run for the P6-01 acceptance check"]
async fn a_thousand_idle_sockets_hold_steady() {
    let hub = Hub::new();
    let app = Router::new().route("/ws", get(upgrade)).with_state(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let received = Arc::new(AtomicU64::new(0));
    let resyncs = Arc::new(AtomicU64::new(0));
    let mut clients = Vec::with_capacity(SOCKETS);
    for i in 0..SOCKETS {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?who=c{i}"))
            .await
            .unwrap();
        let sub = json!({"op": "subscribe", "topics": [PATCH, format!("player:{}", puuid(i))]});
        ws.send(Message::Text(sub.to_string().into())).await.unwrap();
        let (received, resyncs) = (Arc::clone(&received), Arc::clone(&resyncs));
        // Reading keeps answering the server's pings.
        clients.push(tokio::spawn(async move {
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(t) = msg {
                    if t.contains(r#""op":"event""#) {
                        received.fetch_add(1, Ordering::Relaxed);
                    } else if t.contains(r#""op":"resync""#) {
                        resyncs.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for _ in 0..500 {
        if hub.connections() == SOCKETS && hub.subscriptions() == 2 * SOCKETS as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hub.connections(), SOCKETS);

    let publish = |n: u64| {
        let patch = Topic::named(PATCH);
        let frame = json!({"op": "event", "event": "patch.new", "topic": PATCH, "at": 1, "data": {"n": n}});
        hub.publish(&patch, "patch.new", frame.to_string().into());
        let player = Topic::player(&puuid(usize::try_from(n).unwrap() % SOCKETS));
        let frame =
            json!({"op": "event", "event": "game.started", "topic": player.as_str(), "at": 1, "data": {}});
        hub.publish(&player, "game.started", frame.to_string().into());
    };
    // Warm up allocators and buffers before the first sample.
    for n in 0..200 {
        publish(n);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = rss_bytes();

    let started = tokio::time::Instant::now();
    let mut n = 200;
    while started.elapsed() < RUN {
        publish(n);
        n += 1;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = rss_bytes();

    let growth = after.saturating_sub(before);
    println!(
        "sockets {} topics {} events published {} delivered {} resyncs {} rss {:.1} MiB → {:.1} MiB (+{:.1} MiB)",
        hub.connections(),
        hub.live_topics(),
        n * 2,
        received.load(Ordering::Relaxed),
        resyncs.load(Ordering::Relaxed),
        before as f64 / 1_048_576.0,
        after as f64 / 1_048_576.0,
        growth as f64 / 1_048_576.0,
    );
    assert_eq!(hub.connections(), SOCKETS, "every socket survived 20 heartbeats");
    assert_eq!(hub.live_topics(), SOCKETS + 1);
    assert!(growth < SLACK_BYTES, "memory grew by {growth} bytes");
    for c in clients {
        c.abort();
    }
}

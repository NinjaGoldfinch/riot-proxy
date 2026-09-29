//! The WebSocket hub spike (plan P6-01) over real sockets: v1's protocol with
//! v2's additions (ADR-045), topic isolation, lag → resync, the heartbeat and
//! server-side closes. Auth and the real `/v1/ws` route are P6-07, so the test
//! route takes the client's identity from the query string.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use riot_proxy::ws::protocol::{FIREHOSE, PATCH};
use riot_proxy::ws::socket::serve_socket_with;
use riot_proxy::ws::{Client, Hub, Topic};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const P1: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const P2: &str = "ay_eRDcdLU9vOeezaiq1OkZo0QBj7na1KF2ZiIT9SffG_fjhCjwD9BEs3qBZXrBX2EBBg4bZdFAcww";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
struct Srv {
    hub: Hub,
    heartbeat: Duration,
}

async fn upgrade(
    State(s): State<Srv>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let who = q.get("who").cloned().unwrap_or_else(|| "web".into());
    let client = Client {
        consumer_id: who.clone(),
        consumer: who.clone(),
        admin: who == "ops",
    };
    ws.on_upgrade(move |socket| serve_socket_with(s.hub, socket, client, s.heartbeat))
}

async fn server(hub: Hub, heartbeat: Duration) -> SocketAddr {
    let app = Router::new()
        .route("/ws", get(upgrade))
        .with_state(Srv { hub, heartbeat });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn connect(addr: SocketAddr, who: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?who={who}"))
        .await
        .unwrap();
    ws
}

/// The next text frame as JSON, within a second; `pong.at` is normalised.
async fn next(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("a frame within 1 s")
            .expect("socket open")
            .unwrap();
        if let Message::Text(t) = msg {
            let mut v: Value = serde_json::from_str(&t).unwrap();
            if v["op"] == "pong" {
                v["at"] = json!("<now>");
            }
            return v;
        }
    }
}

/// No text frame within 150 ms.
async fn quiet(ws: &mut Ws) {
    let got = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return Some(t.to_string()),
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    })
    .await;
    assert!(matches!(got, Err(_) | Ok(None)), "unexpected frame: {got:?}");
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

fn event(name: &str, topic: &Topic, n: u32) -> axum::extract::ws::Utf8Bytes {
    json!({"op": "event", "event": name, "topic": topic.as_str(), "at": 1, "data": {"n": n}})
        .to_string()
        .into()
}

/// Wait until the hub agrees with the condition (sockets are torn down asynchronously).
async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

const LONG: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn a_session_speaks_v1s_protocol() {
    let hub = Hub::new();
    let addr = server(hub.clone(), LONG).await;
    let mut ws = connect(addr, "web").await;
    let player = Topic::player(P1);
    let mut log = vec![next(&mut ws).await];

    send(
        &mut ws,
        json!({"op": "subscribe", "topics": ["patch", format!("player:{P1}"), "plyer:typo", "metrics", 7]}),
    )
    .await;
    for _ in 0..3 {
        log.push(next(&mut ws).await);
    }
    send(&mut ws, json!({"op": "ping"})).await;
    log.push(next(&mut ws).await);

    hub.publish(
        &Topic::named(PATCH),
        "patch.new",
        event("patch.new", &Topic::named(PATCH), 1),
    );
    hub.publish(&player, "game.started", event("game.started", &player, 2));
    hub.publish(
        &Topic::player(P2),
        "game.started",
        event("game.started", &Topic::player(P2), 3),
    );
    // Order holds within a topic, not across topics (StreamMap polls fairly).
    let mut pair = vec![next(&mut ws).await, next(&mut ws).await];
    pair.sort_by_key(|v| v["data"]["n"].as_u64());
    log.extend(pair);

    send(&mut ws, json!({"op": "unsubscribe", "topics": ["patch"]})).await;
    log.push(next(&mut ws).await);
    hub.publish(
        &Topic::named(PATCH),
        "patch.new",
        event("patch.new", &Topic::named(PATCH), 4),
    );
    send(&mut ws, json!({"op": "dance"})).await;
    log.push(next(&mut ws).await);
    ws.send(Message::Text("{not json".into())).await.unwrap();
    log.push(next(&mut ws).await);
    quiet(&mut ws).await;

    insta::assert_json_snapshot!("ws_session", log);
}

#[tokio::test]
async fn two_clients_see_only_their_own_topics() {
    let hub = Hub::new();
    let addr = server(hub.clone(), LONG).await;
    let (mut a, mut b) = (connect(addr, "a").await, connect(addr, "b").await);
    next(&mut a).await;
    next(&mut b).await;
    send(
        &mut a,
        json!({"op": "subscribe", "topics": [format!("player:{P1}")]}),
    )
    .await;
    send(
        &mut b,
        json!({"op": "subscribe", "topics": [format!("player:{P2}")]}),
    )
    .await;
    next(&mut a).await;
    next(&mut b).await;

    hub.publish(
        &Topic::player(P1),
        "game.started",
        event("game.started", &Topic::player(P1), 1),
    );
    hub.publish(
        &Topic::player(P2),
        "rank.changed",
        event("rank.changed", &Topic::player(P2), 2),
    );
    assert_eq!(next(&mut a).await["data"]["n"], 1);
    assert_eq!(next(&mut b).await["data"]["n"], 2);
    quiet(&mut a).await;
    quiet(&mut b).await;
}

#[tokio::test]
async fn the_firehose_delivers_each_event_once_to_admins_only() {
    let hub = Hub::new();
    let addr = server(hub.clone(), LONG).await;
    let mut ops = connect(addr, "ops").await;
    next(&mut ops).await;
    send(
        &mut ops,
        json!({"op": "subscribe", "topics": ["firehose", format!("player:{P1}")]}),
    )
    .await;
    assert_eq!(
        next(&mut ops).await,
        json!({"op": "subscribed", "topics": ["firehose", format!("player:{P1}")]})
    );
    hub.publish(
        &Topic::player(P1),
        "game.started",
        event("game.started", &Topic::player(P1), 1),
    );
    hub.publish(
        &Topic::player(P2),
        "game.started",
        event("game.started", &Topic::player(P2), 2),
    );
    assert_eq!(next(&mut ops).await["data"]["n"], 1);
    assert_eq!(next(&mut ops).await["data"]["n"], 2);
    quiet(&mut ops).await;

    let mut web = connect(addr, "web").await;
    next(&mut web).await;
    send(&mut web, json!({"op": "subscribe", "topics": [FIREHOSE]})).await;
    assert_eq!(
        next(&mut web).await,
        json!({"op": "error", "error": {"code": "FORBIDDEN", "message": "Topic 'firehose' requires the admin scope"}})
    );
    assert_eq!(next(&mut web).await, json!({"op": "subscribed", "topics": []}));
}

#[tokio::test]
async fn a_socket_that_falls_behind_gets_a_resync() {
    let hub = Hub::with_capacity(4);
    let addr = server(hub.clone(), LONG).await;
    let mut ws = connect(addr, "web").await;
    next(&mut ws).await;
    send(&mut ws, json!({"op": "subscribe", "topics": ["patch"]})).await;
    next(&mut ws).await;

    // Twenty events before the socket task runs again: it can hold four.
    let t = Topic::named(PATCH);
    for n in 0..20 {
        hub.publish(&t, "patch.new", event("patch.new", &t, n));
    }
    assert_eq!(
        next(&mut ws).await,
        json!({"op": "resync", "topic": "patch", "dropped": 16})
    );
    for n in 16..20 {
        assert_eq!(next(&mut ws).await["data"]["n"], n);
    }
    quiet(&mut ws).await;
}

#[tokio::test]
async fn subscriptions_are_capped_and_released_on_disconnect() {
    let hub = Hub::new();
    let addr = server(hub.clone(), LONG).await;
    let mut ws = connect(addr, "web").await;
    next(&mut ws).await;
    let topics: Vec<String> = (0..205).map(|i| format!("player:{}{i:03}", &P1[..60])).collect();
    send(&mut ws, json!({"op": "subscribe", "topics": topics})).await;
    let held = next(&mut ws).await;
    assert_eq!(held["topics"].as_array().unwrap().len(), 200, "v1's cap");
    assert_eq!((hub.live_topics(), hub.connections()), (200, 1));

    ws.close(None).await.unwrap();
    drop(ws);
    eventually("the socket's topics to be released", || {
        hub.live_topics() == 0 && hub.connections() == 0
    })
    .await;
    assert_eq!(hub.subscriptions(), 0);
}

#[tokio::test]
async fn an_unresponsive_socket_is_dropped_and_a_responsive_one_kept() {
    let hub = Hub::new();
    let addr = server(hub.clone(), Duration::from_millis(40)).await;

    // Reading answers pings (tungstenite queues the pong), so this one stays.
    let mut alive = connect(addr, "alive").await;
    next(&mut alive).await;
    let reader = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        while tokio::time::Instant::now() < deadline {
            if tokio::time::timeout(Duration::from_millis(20), alive.next())
                .await
                .is_ok_and(|m| m.is_none())
            {
                return false;
            }
        }
        true
    });

    // This one never reads, so it never answers: gone after two missed pings.
    let _silent = connect(addr, "silent").await;
    eventually("both sockets to connect", || hub.connections() == 2).await;
    eventually("the silent socket to be dropped", || hub.connections() == 1).await;
    assert!(reader.await.unwrap(), "the responsive socket stayed open");
}

#[tokio::test]
async fn revoking_a_key_and_shutting_down_close_sockets() {
    let hub = Hub::new();
    let addr = server(hub.clone(), LONG).await;
    let mut a = connect(addr, "a").await;
    let mut b = connect(addr, "b").await;
    next(&mut a).await;
    next(&mut b).await;

    assert_eq!(hub.close_consumer("a"), 1);
    let close = |m: Option<Result<Message, _>>| match m {
        Some(Ok(Message::Close(Some(f)))) => (f.code, f.reason.to_string()),
        other => panic!("expected a close frame, got {other:?}"),
    };
    assert_eq!(
        close(a.next().await),
        (CloseCode::Library(4401), "key revoked".into())
    );

    hub.shutdown();
    assert_eq!(
        close(b.next().await),
        (CloseCode::Away, "server shutting down".into())
    );
    eventually("both sockets to go", || hub.connections() == 0).await;
}

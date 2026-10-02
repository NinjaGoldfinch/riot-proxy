//! `/v1/ws` through the full app over real TCP (plan P6-07): v1's handshake
//! auth (bearer or `?token=`, an error frame and 4401 for a bad key), admin
//! topics gated on scope and the IP allowlist, the quota, revocation closing
//! sockets, and the `metrics` topic ticking only while held.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use riot_proxy::app::AppState;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::events::{self, Event};
use riot_proxy::ws::protocol::METRICS;
use riot_proxy::ws::{Topic, metrics};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Env {
    _dir: tempfile::TempDir,
    state: AppState,
    addr: SocketAddr,
}

async fn env(vars: &[(&str, &str)]) -> Env {
    let (dir, state, router) = common::app_with(vars, "http://127.0.0.1:9");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Env {
        _dir: dir,
        state,
        addr,
    }
}

impl Env {
    async fn key(&self, name: &str, scopes: Vec<Scope>, quota: u32) -> (String, String) {
        let c = consumers::create(
            &self.state.db,
            NewConsumer {
                name: name.into(),
                scopes,
                quota_per_min: quota,
                key: None,
            },
        )
        .await
        .unwrap();
        (c.consumer.id, c.key.expose().to_string())
    }

    async fn connect(&self, bearer: Option<&str>, token: Option<&str>) -> Result<Ws, WsError> {
        let url = match token {
            Some(t) => format!("ws://{}/v1/ws?token={t}", self.addr),
            None => format!("ws://{}/v1/ws", self.addr),
        };
        let mut req = url.into_client_request().unwrap();
        if let Some(b) = bearer {
            req.headers_mut()
                .insert("authorization", format!("Bearer {b}").parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
    }
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("a frame")
            .expect("open")
            .unwrap()
        {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Close(f) => panic!("closed: {f:?}"),
            _ => {}
        }
    }
}

async fn closed(ws: &mut Ws) -> (CloseCode, String) {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("a frame")
        {
            Some(Ok(Message::Close(Some(f)))) => return (f.code, f.reason.to_string()),
            Some(Ok(_)) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

#[tokio::test]
async fn a_socket_without_a_valid_key_gets_v1s_error_and_4401() {
    let e = env(&[]).await;
    for (bearer, token) in [(None, None), (Some("rpx_nope"), None), (None, Some("rpx_nope"))] {
        let mut ws = e.connect(bearer, token).await.unwrap();
        assert_eq!(
            next(&mut ws).await,
            json!({"op": "error", "error": {"code": "UNAUTHORIZED", "message": "Invalid key"}})
        );
        assert_eq!(
            closed(&mut ws).await,
            (CloseCode::Library(4401), "unauthorized".into())
        );
    }
    assert_eq!(e.state.hub.connections(), 0);
}

#[tokio::test]
async fn a_read_key_by_header_or_query_holds_player_topics_but_not_admin_ones() {
    let e = env(&[]).await;
    let (_, key) = e.key("web", vec![Scope::Read], 100).await;
    for (bearer, token) in [(Some(key.as_str()), None), (None, Some(key.as_str()))] {
        let mut ws = e.connect(bearer, token).await.unwrap();
        assert_eq!(next(&mut ws).await, json!({"op": "ready", "consumer": "web"}));
        send(
            &mut ws,
            json!({"op": "subscribe", "topics": ["metrics", format!("player:{P}")]}),
        )
        .await;
        assert_eq!(next(&mut ws).await["error"]["code"], "FORBIDDEN");
        assert_eq!(
            next(&mut ws).await,
            json!({"op": "subscribed", "topics": [format!("player:{P}")]})
        );

        // A job's event reaches the socket.
        events::publish(
            &e.state.hub,
            &Event::PatchNew {
                version: "ignored".into(),
            },
        );
        let started = Event::GameStarted {
            puuid: P.into(),
            platform: "kr".into(),
            game_id: 7,
            queue_id: None,
            champion_id: None,
        };
        events::publish(&e.state.hub, &started);
        let frame = next(&mut ws).await;
        assert_eq!(
            (frame["op"].as_str(), frame["event"].as_str()),
            (Some("event"), Some("game.started"))
        );
    }
}

#[tokio::test]
async fn admin_topics_need_the_admin_scope_and_the_allowlist() {
    let e = env(&[]).await;
    let (_, admin) = e.key("ops", vec![Scope::Read, Scope::Admin], 100).await;
    let mut ws = e.connect(Some(&admin), None).await.unwrap();
    next(&mut ws).await;
    send(
        &mut ws,
        json!({"op": "subscribe", "topics": ["firehose", "metrics", "ladder"]}),
    )
    .await;
    assert_eq!(
        next(&mut ws).await,
        json!({"op": "subscribed", "topics": ["firehose", "metrics", "ladder"]})
    );

    // The same admin key from outside ADMIN_IP_ALLOWLIST is only a reader (v1).
    let far = env(&[("ADMIN_IP_ALLOWLIST", "10.0.0.0/8")]).await;
    let (_, admin) = far.key("ops", vec![Scope::Read, Scope::Admin], 100).await;
    let mut ws = far.connect(Some(&admin), None).await.unwrap();
    next(&mut ws).await;
    send(&mut ws, json!({"op": "subscribe", "topics": ["firehose"]})).await;
    assert_eq!(next(&mut ws).await["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn the_handshake_counts_against_the_quota() {
    let e = env(&[]).await;
    let (_, key) = e.key("tiny", vec![Scope::Read], 1).await;
    let _first = e.connect(Some(&key), None).await.unwrap();
    match e.connect(Some(&key), None).await {
        Err(WsError::Http(res)) => assert_eq!(res.status(), 429),
        other => panic!("expected a 429, got {other:?}"),
    }
}

#[tokio::test]
async fn revoking_a_key_closes_its_open_sockets() {
    let e = env(&[]).await;
    let (id, key) = e.key("web", vec![Scope::Read], 100).await;
    let (_, admin) = e.key("ops", vec![Scope::Read, Scope::Admin], 100).await;
    let mut ws = e.connect(Some(&key), None).await.unwrap();
    next(&mut ws).await;
    let mut other = e.connect(Some(&admin), None).await.unwrap();
    next(&mut other).await;

    let req = axum::http::Request::delete(format!("/v1/admin/consumers/{id}"))
        .header("authorization", format!("Bearer {admin}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let router = riot_proxy::app::router(e.state.clone(), riot_proxy::telemetry::metrics_handle().unwrap());
    assert_eq!(common::send(router, req).await.status, 200);
    assert_eq!(
        closed(&mut ws).await,
        (CloseCode::Library(4401), "key revoked".into())
    );
    send(&mut other, json!({"op": "ping"})).await;
    assert_eq!(
        next(&mut other).await["op"],
        "pong",
        "other consumers are untouched"
    );
}

#[tokio::test]
async fn the_metrics_topic_ticks_only_while_held() {
    let e = env(&[]).await;
    let stats = &e.state.stats;
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    assert!(!metrics::tick(stats).await, "nobody listening: no work");

    let (_, admin) = e.key("ops", vec![Scope::Read, Scope::Admin], 100).await;
    let mut ws = e.connect(Some(&admin), None).await.unwrap();
    next(&mut ws).await;
    send(&mut ws, json!({"op": "subscribe", "topics": [METRICS]})).await;
    next(&mut ws).await;
    assert_eq!(e.state.hub.receivers(&Topic::named(METRICS)), 1);
    assert!(metrics::tick(stats).await);
    let frame = next(&mut ws).await;
    assert_eq!(
        (frame["event"].as_str(), frame["topic"].as_str()),
        (Some("metrics.snapshot"), Some("metrics"))
    );
    let data = &frame["data"];
    assert_eq!(
        (data["v"].clone(), data["keyScope"].as_str()),
        (json!(1), Some(scope.as_str()))
    );
    assert_eq!(data["ws"], json!({"connections": 1, "subscriptions": 1}));
    assert_eq!(data["totals"]["archivedMatches"], 0);
    // v1's whole document: every section the dashboard reads.
    assert_eq!(
        data.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "analytics",
            "cache",
            "events",
            "flows",
            "keyScope",
            "ladder",
            "limiter",
            "process",
            "queues",
            "totals",
            "v",
            "worker",
            "ws"
        ]
    );
}

#[tokio::test]
async fn the_socket_is_documented_under_the_ws_tag() {
    let doc = riot_proxy::routes::docs::spec();
    let json = serde_json::to_value(&doc).unwrap();
    let op = &json["paths"]["/v1/ws"]["get"];
    assert_eq!(op["tags"], json!(["ws"]));
    let text = op["description"].as_str().unwrap();
    for word in ["player:<puuid>", "resync", "metrics.snapshot", "4401", "?token="] {
        assert!(text.contains(word), "{word} missing from the /v1/ws description");
    }
}

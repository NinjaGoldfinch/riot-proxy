//! One task per socket (design/06): `select!` over the inbound frames, one
//! broadcast receiver per held topic, the heartbeat and the close signal.
//!
//! A socket that falls behind a topic's buffer gets `{"op":"resync"}` naming
//! the topic and how many events it missed, then carries on (design/06). A
//! socket that holds the firehose ignores its other topics, which the firehose
//! already carries, so no event arrives twice (v1 relayed once per socket).
//!
//! Order is kept within a topic, and so on the firehose, but not across two
//! topics: their receivers are polled fairly. v1's single relay kept global
//! order; nothing in the protocol promised it (ADR-045).

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio_stream::StreamMap;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::clock::Clock;
use crate::ws::hub::{Closing, Hub};
use crate::ws::protocol::{ClientFrame, MAX_MISSED_PONGS, MAX_TOPICS_PER_SOCKET, ServerFrame, Topic};

/// Who is on the other end, resolved at the handshake (P6-07).
#[derive(Debug, Clone)]
pub struct Client {
    pub consumer_id: String,
    /// Sent in the `ready` frame (v1).
    pub consumer: String,
    /// Admin scope *and* the admin IP allowlist, checked once at the handshake
    /// (v1), so admin topics need no per-frame authorisation.
    pub admin: bool,
}

/// v1 closed with 1001 on shutdown; a revoked key closes with 4401, the code v1
/// used for a handshake with a bad key.
fn close_frame(why: Closing) -> Message {
    let (code, reason) = match why {
        Closing::Revoked => (4401, "key revoked"),
        _ => (1001, "server shutting down"),
    };
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Run a socket to completion.
pub async fn serve_socket(hub: Hub, socket: WebSocket, client: Client) {
    serve_socket_with(hub, socket, client, crate::ws::protocol::HEARTBEAT).await;
}

/// [`serve_socket`] with a chosen heartbeat (tests).
pub async fn serve_socket_with(hub: Hub, socket: WebSocket, client: Client, heartbeat: Duration) {
    let mut registration = hub.register(&client.consumer_id);
    let (mut tx, mut rx) = socket.split();
    let mut held: Vec<Topic> = Vec::new();
    let mut streams: StreamMap<String, BroadcastStream<Utf8Bytes>> = StreamMap::new();
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    let mut missed = 0u32;

    let ready = ServerFrame::Ready {
        consumer: client.consumer.clone(),
    };
    if tx.send(text(&ready)).await.is_err() {
        return;
    }
    tracing::debug!(consumer = %client.consumer, "websocket connected");

    loop {
        tokio::select! {
            changed = registration.close.changed() => {
                let why = *registration.close.borrow();
                if changed.is_ok() && why != Closing::Open {
                    let _ = tx.send(close_frame(why)).await;
                }
                break;
            }
            inbound = rx.next() => {
                let frame = match inbound {
                    Some(Ok(Message::Text(t))) => t.as_str().to_string(),
                    // v1 read frames with `raw.toString()`, so binary JSON works too.
                    Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(Message::Pong(_))) => {
                        missed = 0;
                        continue;
                    }
                    // axum answers pings itself.
                    Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                };
                let replies = handle(&hub, &client, &frame, &mut held, &mut streams);
                let mut failed = false;
                for reply in replies {
                    if tx.send(text(&reply)).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
            Some((topic, item)) = streams.next(), if !streams.is_empty() => {
                // The firehose already carries this topic's events.
                if held.iter().any(Topic::is_firehose) && topic != crate::ws::protocol::FIREHOSE {
                    continue;
                }
                let out = match item {
                    Ok(frame) => Message::Text(frame),
                    Err(BroadcastStreamRecvError::Lagged(dropped)) => {
                        tracing::debug!(consumer = %client.consumer, %topic, dropped, "websocket lagged");
                        text(&ServerFrame::Resync { topic, dropped })
                    }
                };
                if tx.send(out).await.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                if missed >= MAX_MISSED_PONGS {
                    tracing::debug!(consumer = %client.consumer, "dropping unresponsive websocket");
                    break;
                }
                missed += 1;
                if tx.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
        }
    }

    drop(streams);
    for topic in &held {
        hub.release(topic);
    }
    tracing::debug!(consumer = %client.consumer, "websocket closed");
}

fn text(frame: &ServerFrame) -> Message {
    Message::Text(frame.to_json().into())
}

/// v1 `onMessage`, with v2's topic validation (ADR-045).
fn handle(
    hub: &Hub,
    client: &Client,
    raw: &str,
    held: &mut Vec<Topic>,
    streams: &mut StreamMap<String, BroadcastStream<Utf8Bytes>>,
) -> Vec<ServerFrame> {
    let frame = match ClientFrame::parse(raw) {
        Ok(f) => f,
        Err(e) => return vec![e],
    };
    let mut out = Vec::new();
    match frame {
        ClientFrame::Subscribe(topics) => {
            for raw in topics {
                let Some(topic) = Topic::parse(&raw) else {
                    out.push(ServerFrame::error("VALIDATION", format!("Unknown topic '{raw}'")));
                    continue;
                };
                if topic.is_admin() && !client.admin {
                    out.push(ServerFrame::error(
                        "FORBIDDEN",
                        format!("Topic '{topic}' requires the admin scope"),
                    ));
                    continue;
                }
                if held.contains(&topic) {
                    continue;
                }
                // v1: further topics are ignored once the cap is reached.
                if held.len() >= MAX_TOPICS_PER_SOCKET {
                    break;
                }
                streams.insert(
                    topic.as_str().to_string(),
                    BroadcastStream::new(hub.subscribe(&topic)),
                );
                held.push(topic);
            }
        }
        ClientFrame::Unsubscribe(topics) => {
            for raw in topics {
                if let Some(pos) = held.iter().position(|t| t.as_str() == raw) {
                    let topic = held.remove(pos);
                    streams.remove(topic.as_str());
                    hub.release(&topic);
                }
            }
        }
        ClientFrame::Ping => {
            out.push(ServerFrame::Pong {
                at: Clock::now().unix_ms,
            });
            return out;
        }
    }
    // The acknowledgement lists what the socket holds, so a refused topic is
    // visible by its absence (v1).
    out.push(ServerFrame::Subscribed {
        topics: held.iter().map(|t| t.as_str().to_string()).collect(),
    });
    out
}

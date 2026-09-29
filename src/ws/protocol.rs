//! The `/v1/ws` wire protocol: v1's (`src/ws/index.ts`, §11) plus four
//! additions the owner approved (ADR-045). Every server frame carries `op`.
//!
//! ```jsonc
//! // client → server
//! {"op":"subscribe","topics":["player:<puuid>","patch"]}
//! {"op":"unsubscribe","topics":["patch"]}
//! {"op":"ping"}
//! // server → client
//! {"op":"ready","consumer":"web"}
//! {"op":"subscribed","topics":["player:<puuid>"]}   // what the socket now holds
//! {"op":"event","event":"game.started","topic":"player:<puuid>","at":1726400000000,"data":{…}}
//! {"op":"resync","topic":"player:<puuid>","dropped":12}   // v2: this socket fell behind
//! {"op":"pong","at":1726400000000}
//! {"op":"error","error":{"code":"VALIDATION","message":"Invalid JSON"}}
//! ```

use serde::{Deserialize, Serialize};

use crate::http::validate;

/// v1 `HEARTBEAT_MS`: how often the server pings an idle socket.
pub const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);
/// v1 `MAX_MISSED_PONGS`: unanswered pings before the socket is dropped.
pub const MAX_MISSED_PONGS: u32 = 2;
/// v1 `MAX_TOPICS_PER_SOCKET`: further topics are ignored.
pub const MAX_TOPICS_PER_SOCKET: usize = 200;
/// v1 ignored topic strings longer than this.
pub const MAX_TOPIC_LEN: usize = 200;

/// Every event the service publishes (admin).
pub const FIREHOSE: &str = "firehose";
/// Periodic operational snapshots, published while anyone listens (admin).
pub const METRICS: &str = "metrics";
/// Ladder crawl progress (admin).
pub const LADDER: &str = "ladder";
/// Global patch events.
pub const PATCH: &str = "patch";
const PLAYER_PREFIX: &str = "player:";

/// A topic a client may hold. v1 accepted any string; v2 refuses what can never
/// fire, so a typo is an error rather than a silent subscription (ADR-045).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Topic(String);

impl Topic {
    pub fn parse(raw: &str) -> Option<Self> {
        let ok = match raw.strip_prefix(PLAYER_PREFIX) {
            Some(puuid) => validate::puuid(puuid).is_ok(),
            None => [FIREHOSE, METRICS, LADDER, PATCH].contains(&raw),
        };
        ok.then(|| Self(raw.to_string()))
    }

    /// `player:<puuid>`: anything about one player.
    pub fn player(puuid: &str) -> Self {
        Self(format!("{PLAYER_PREFIX}{puuid}"))
    }

    pub fn named(name: &'static str) -> Self {
        Self(name.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Topics that expose operational internals, or every player at once (v1 `ADMIN_TOPICS`).
    pub fn is_admin(&self) -> bool {
        [FIREHOSE, METRICS, LADDER].contains(&self.0.as_str())
    }

    pub fn is_firehose(&self) -> bool {
        self.0 == FIREHOSE
    }
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A client frame, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    Subscribe(Vec<String>),
    Unsubscribe(Vec<String>),
    Ping,
}

#[derive(Deserialize)]
struct RawFrame {
    op: Option<serde_json::Value>,
    topics: Option<serde_json::Value>,
}

impl ClientFrame {
    /// v1 `onMessage`: invalid JSON and unknown ops are error frames, not
    /// disconnects. Non-string or over-long topics are dropped silently (v1).
    pub fn parse(text: &str) -> Result<Self, ServerFrame> {
        let raw: RawFrame =
            serde_json::from_str(text).map_err(|_| ServerFrame::error("VALIDATION", "Invalid JSON"))?;
        let topics = || -> Vec<String> {
            match &raw.topics {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|t| t.as_str())
                    .filter(|t| t.chars().count() <= MAX_TOPIC_LEN)
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            }
        };
        match raw.op.as_ref().and_then(|o| o.as_str()) {
            Some("subscribe") => Ok(Self::Subscribe(topics())),
            Some("unsubscribe") => Ok(Self::Unsubscribe(topics())),
            Some("ping") => Ok(Self::Ping),
            _ => Err(ServerFrame::error(
                "VALIDATION",
                format!("Unknown op '{}'", js_string(raw.op.as_ref())),
            )),
        }
    }
}

/// JavaScript's `String(value)` for the few shapes an `op` arrives as.
fn js_string(v: Option<&serde_json::Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) => "null".into(),
        Some(serde_json::Value::Array(_)) => String::new(),
        Some(serde_json::Value::Object(_)) => "[object Object]".into(),
        Some(other) => other.to_string(),
    }
}

/// A control frame from the server. Event frames are built once per event by
/// the publisher and shared between sockets (see [`crate::ws::hub::Hub::publish`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ServerFrame {
    Ready { consumer: String },
    Subscribed { topics: Vec<String> },
    Resync { topic: String, dropped: u64 },
    Pong { at: i64 },
    Error { error: FrameError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrameError {
    pub code: &'static str,
    pub message: String,
}

impl ServerFrame {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::Error {
            error: FrameError {
                code,
                message: message.into(),
            },
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"op":"error"}"#.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

    #[test]
    fn topics() {
        for ok in ["patch", "metrics", "firehose", "ladder"] {
            assert!(Topic::parse(ok).is_some(), "{ok}");
        }
        assert_eq!(Topic::parse(&format!("player:{P}")), Some(Topic::player(P)));
        for bad in ["", "Patch", "plyer:x", "player:", "player:short", "game", "rank"] {
            assert_eq!(Topic::parse(bad), None, "{bad}");
        }
        assert!(Topic::named(METRICS).is_admin() && Topic::named(FIREHOSE).is_admin());
        assert!(!Topic::named(PATCH).is_admin() && !Topic::player(P).is_admin());
    }

    #[test]
    fn client_frames_follow_v1() {
        assert_eq!(
            ClientFrame::parse(r#"{"op":"subscribe","topics":["patch",7,null,"x"]}"#),
            Ok(ClientFrame::Subscribe(vec!["patch".into(), "x".into()]))
        );
        let long = "x".repeat(201);
        assert_eq!(
            ClientFrame::parse(&format!(r#"{{"op":"subscribe","topics":["{long}"]}}"#)),
            Ok(ClientFrame::Subscribe(vec![]))
        );
        assert_eq!(
            ClientFrame::parse(r#"{"op":"unsubscribe"}"#),
            Ok(ClientFrame::Unsubscribe(vec![]))
        );
        assert_eq!(ClientFrame::parse(r#"{"op":"ping"}"#), Ok(ClientFrame::Ping));
        let err = |t: &str| ClientFrame::parse(t).unwrap_err().to_json();
        assert_eq!(
            err("nope"),
            r#"{"op":"error","error":{"code":"VALIDATION","message":"Invalid JSON"}}"#
        );
        assert_eq!(
            err("{}"),
            r#"{"op":"error","error":{"code":"VALIDATION","message":"Unknown op 'undefined'"}}"#
        );
        assert_eq!(
            err(r#"{"op":"kick"}"#),
            r#"{"op":"error","error":{"code":"VALIDATION","message":"Unknown op 'kick'"}}"#
        );
    }

    #[test]
    fn server_frames_serialise_with_op() {
        assert_eq!(
            ServerFrame::Ready {
                consumer: "web".into()
            }
            .to_json(),
            r#"{"op":"ready","consumer":"web"}"#
        );
        assert_eq!(
            ServerFrame::Resync {
                topic: "patch".into(),
                dropped: 3
            }
            .to_json(),
            r#"{"op":"resync","topic":"patch","dropped":3}"#
        );
        assert_eq!(ServerFrame::Pong { at: 5 }.to_json(), r#"{"op":"pong","at":5}"#);
    }
}

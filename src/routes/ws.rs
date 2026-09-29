//! `/v1/ws`: the realtime socket (docs/design/06 §Realtime, ADR-045).
//!
//! Auth happens at the handshake, with the bearer key or `?token=` (a browser
//! cannot set headers on a WebSocket). As in v1, a bad key still gets its
//! upgrade, then an error frame and a 4401 close, so a browser client sees why.
//! The consumer quota counts the handshake like any request.

use std::net::SocketAddr;

use axum::extract::ws::{CloseFrame, Message, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{Extensions, HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::ws::protocol::ServerFrame;
use crate::ws::{Client, serve_socket};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(socket))
}

const PROTOCOL: &str = "\
Upgrade with `Authorization: Bearer rpx_…`, or `?token=rpx_…` from a browser. The server sends \
`{\"op\":\"ready\",\"consumer\":…}`, then:

- `{\"op\":\"subscribe\",\"topics\":[…]}` / `{\"op\":\"unsubscribe\",\"topics\":[…]}` → \
`{\"op\":\"subscribed\",\"topics\":[…]}`, listing everything the socket holds. A topic that was refused is absent.
- `{\"op\":\"ping\"}` → `{\"op\":\"pong\",\"at\":…}`.
- Events arrive as `{\"op\":\"event\",\"event\":…,\"topic\":…,\"at\":…,\"data\":{…}}`.
- `{\"op\":\"resync\",\"topic\":…,\"dropped\":n}` means this socket fell behind and missed `n` events on that topic; re-read what it covers.
- Problems arrive as `{\"op\":\"error\",\"error\":{\"code\",\"message\"}}` without closing the socket.

**Topics:** `player:<puuid>` (game, rank and archive events for one player), `patch`, and admin only: \
`metrics` (a snapshot every `METRICS_INTERVAL_S` while subscribed), `ladder`, `firehose` (every event). \
At most 200 per socket. The server pings every 30 s and drops a socket after two unanswered pings.

**Events:** `game.started`, `game.ended`, `rank.changed`, `match.archived` (player topics); `patch.new`; \
`crawl.phase`, `ladder.crawl.completed`, `analytics.updated` (`ladder`); `metrics.snapshot` (`metrics`).

**Closes:** 4401 for an invalid or revoked key, 1001 on server shutdown.";

#[utoipa::path(
    get, path = "/v1/ws", tag = "ws",
    summary = "Realtime events over a WebSocket",
    description = PROTOCOL,
    params(("token" = Option<String>, Query, description = "The consumer key, for browsers that cannot set headers")),
    responses(
        (status = 101, description = "Switching protocols"),
        (status = 429, description = "Your consumer quota is spent", body = crate::http::error::ErrorResponse),
    ),
    security(("bearerAuth" = []), ("tokenQuery" = [])),
)]
async fn socket(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    extensions: Extensions,
    ws: WebSocketUpgrade,
) -> Response {
    let peer = extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let (consumer, admin) = match state.auth.socket(&headers, &uri, peer).await {
        Ok(ok) => ok,
        Err(_) => {
            // v1: the upgrade completes, then an error frame and 4401.
            return ws.on_upgrade(|mut socket| async move {
                let frame = ServerFrame::error("UNAUTHORIZED", "Invalid key").to_json();
                let _ = socket.send(Message::Text(frame.into())).await;
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: 4401,
                        reason: "unauthorized".into(),
                    })))
                    .await;
            });
        }
    };
    let quota = match state.quotas.check(&consumer) {
        Ok(q) => q,
        Err((err, quota)) => {
            let mut res = err.into_response();
            quota.apply(res.headers_mut());
            return res;
        }
    };
    let client = Client {
        consumer_id: consumer.id.clone(),
        consumer: consumer.name.clone(),
        admin,
    };
    let hub = state.hub.clone();
    let mut res = ws.on_upgrade(move |socket| serve_socket(hub, socket, client));
    quota.apply(res.headers_mut());
    res
}

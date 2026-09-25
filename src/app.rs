//! Builds the axum `Router` and runs it with graceful shutdown (docs/design/03
//! §Process model).

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, Uri};
use axum::response::{IntoResponse, Response};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::config::Config;
use crate::db::Db;
use crate::fetcher::Fetcher;
use crate::http::ApiError;
use crate::http::auth::Auth;
use crate::http::quota::Quotas;
use crate::http::request_id::{RequestId, request_id};
use crate::riot::limiter::Limiter;
use crate::{routes, telemetry};

/// v1's Fastify `bodyLimit`.
pub const BODY_LIMIT: usize = 1_048_576;

/// How long in-flight requests get after SIGTERM before the process exits
/// anyway. Matches Docker's default stop timeout, so we finish before SIGKILL.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub limiter: Arc<Limiter>,
    /// Consumer key resolution and scope/allowlist policy.
    pub auth: Arc<Auth>,
    /// Per-consumer request quotas.
    pub quotas: Arc<Quotas>,
    /// The read funnel every Riot-backed route uses.
    pub fetcher: Fetcher,
    /// Set once the limiter checkpoint has been restored (`/readyz`).
    pub limiter_restored: Arc<std::sync::atomic::AtomicBool>,
}

pub fn router(state: AppState, metrics: PrometheusHandle) -> Router {
    let (api, doc) = routes::docs::api_router(Some(state.clone())).split_for_parts();
    let docs_ui = state.config.docs_ui;
    let ui = routes::ui::router(&state.config);
    let mut router = api
        .with_state(state)
        .merge(telemetry::metrics_router(metrics))
        .merge(ui);
    if docs_ui {
        router = router.merge(routes::docs::docs_router(routes::docs::finish(doc)));
    }
    router
        .fallback(not_found)
        // Fastify 404s a known path with the wrong method; so does v2.
        .method_not_allowed_fallback(not_found)
        // Innermost first: metrics → body limit → compression → trace → panic → request id.
        .layer(axum::middleware::from_fn(record_request))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<_>| {
                    // The request_id layer runs first, so the extension is always set.
                    let id = req.extensions().get::<RequestId>().map(RequestId::as_str).unwrap_or_default();
                    tracing::info_span!("http", request_id = %id, method = %req.method(), path = %req.uri().path(), consumer = tracing::field::Empty)
                })
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CatchPanicLayer::custom(|_: Box<dyn std::any::Any + Send>| panic_response()))
        .layer(axum::middleware::from_fn(request_id))
}

/// `proxy_requests_total{route,status,cache}` (v1's onResponse hook). `route` is the
/// matched template in v1's `:param` form; unmatched requests are `unmatched`
/// rather than v1's raw URL, which made the label unbounded (ADR-036).
async fn record_request(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |m| fastify_style(m.as_str()));
    let res = next.run(req).await;
    let cache = match res.headers().get("x-cache").and_then(|v| v.to_str().ok()) {
        Some("HIT-NEG") => "neg",
        Some("HIT") => "hit",
        Some("MISS") => "miss",
        Some("STALE") => "stale",
        Some("ARCHIVE") => "archive",
        Some("BYPASS") => "bypass",
        _ => "none",
    };
    metrics::counter!(crate::metrics::REQUESTS_TOTAL, "route" => route, "status" => res.status().as_str().to_string(), "cache" => cache)
        .increment(1);
    res
}

/// `/a/{b}/c` → `/a/:b/c`.
fn fastify_style(template: &str) -> String {
    template
        .split('/')
        .map(
            |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => format!(":{name}"),
                None => seg.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

/// v1 `setNotFoundHandler`: `No route for <METHOD> <url>`.
async fn not_found(method: Method, uri: Uri) -> ApiError {
    ApiError::not_found(format!("No route for {method} {uri}"))
}

fn panic_response() -> Response {
    ApiError::internal().into_response()
}

/// Serve until `shutdown` resolves, then stop accepting and let in-flight requests
/// finish, for at most [`SHUTDOWN_GRACE`].
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_with_grace(listener, app, shutdown, SHUTDOWN_GRACE).await
}

pub async fn serve_with_grace(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> std::io::Result<()> {
    let (fired_tx, fired_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown.await;
            tracing::info!(grace_s = grace.as_secs(), "shutdown signal received; draining");
            let _ = fired_tx.send(());
        });
    let deadline = async move {
        match fired_rx.await {
            Ok(()) => tokio::time::sleep(grace).await,
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        res = server => res,
        () = deadline => {
            tracing::warn!("drain exceeded the grace period; exiting with requests in flight");
            Ok(())
        }
    }
}

/// Resolves on SIGTERM or SIGINT. Handlers are installed when this is called, not
/// when the future is first polled, so a signal can't slip through during startup.
#[cfg(unix)]
pub fn shutdown_signal() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM"),
            _ = int.recv() => tracing::info!("SIGINT"),
        }
    })
}

#[cfg(not(unix))]
pub fn shutdown_signal() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

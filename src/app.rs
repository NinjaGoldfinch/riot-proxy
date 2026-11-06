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
    /// The per-player `?refresh=true` cooldowns of `/v1/players/*`.
    pub refresh: Arc<crate::routes::players::RefreshWindows>,
    /// The durable job queue (lookups and admin routes enqueue backfills).
    pub jobs: crate::jobs::Queue,
    /// What this process's workers are doing (DEV-19); empty without workers.
    pub activity: crate::jobs::activity::Activity,
    /// The realtime hub `/v1/ws` sockets and job events share.
    pub hub: crate::ws::Hub,
    /// The Data Dragon mirror `/v1/static/*` and champion names read.
    pub ddragon: Arc<crate::r#static::Mirror>,
    /// What the dashboard's snapshot and history are read from.
    pub stats: Arc<crate::stats::Stats>,
    /// The daily housekeeping, for its backups and last run (ADM-06).
    pub maintenance: Arc<crate::jobs::maintenance::Maintenance>,
    /// The analytics routes' reads and their responses cache (PERF-03), shared
    /// with `aggregate:analytics`, which clears and warms it.
    pub analytics: Arc<crate::routes::analytics::cache::Analytics>,
    /// What `/status` knows about this process (STS-02).
    pub status: Arc<crate::status::Status>,
}

pub fn router(state: AppState, metrics: PrometheusHandle) -> Router {
    let (api, doc) = routes::docs::api_router(Some(state.clone())).split_for_parts();
    let docs_ui = state.config.docs_ui;
    let doc = routes::docs::finish(doc);
    let openapi_json: Arc<str> = doc.to_pretty_json().unwrap_or_else(|_| "{}".into()).into();
    let ui = routes::ui::router(&state.config, openapi_json);
    let ddragon = routes::statics::files(Arc::clone(&state.ddragon));
    // `/dev/reset` lives and dies with the dev explorer (design/10, ADR-077).
    let dev_reset = state.config.dev_ui.then(|| routes::dev::router(state.clone()));
    let cors = crate::http::cors::admin(&state.config.admin_cors_origins);
    let mut router = api
        .with_state(state)
        .merge(telemetry::metrics_router(metrics))
        .merge(ui)
        .merge(ddragon);
    if docs_ui {
        router = router.merge(routes::docs::docs_router(doc));
    }
    if let Some(dev_reset) = dev_reset {
        router = router.merge(dev_reset);
    }
    router = router
        .fallback(not_found)
        // Fastify 404s a known path with the wrong method; so does v2.
        .method_not_allowed_fallback(not_found);
    // Opt-in CORS for the admin API (ADM-07): outside the routes' auth and the
    // fallbacks above, so a preflight is answered without a key.
    if let Some(cors) = cors {
        router = router.layer(axum::middleware::from_fn_with_state(
            cors,
            crate::http::cors::scoped,
        ));
    }
    router
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
/// rather than v1's raw URL, which made the label unbounded (ADR-036). The
/// request also goes to the recent-requests ring (ADM-05) unless its path is
/// a probe or the dashboard's own polling.
async fn record_request(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let started = std::time::Instant::now();
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |m| fastify_style(m.as_str()));
    let path = req.uri().path();
    let group = route_group(path);
    let seen = crate::http::recent::recorded(path).then(|| {
        (
            req.method().to_string(),
            crate::http::recent::clip(path),
            req.extensions()
                .get::<RequestId>()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
        )
    });
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
    let consumer = res
        .extensions()
        .get::<crate::http::recent::ServedTo>()
        .map(|c| c.0.clone());
    metrics::counter!(
        crate::metrics::CONSUMER_REQUESTS_TOTAL,
        "consumer" => consumer.clone().unwrap_or_else(|| ANONYMOUS.to_string()),
        "group" => group,
        "class" => status_class(res.status()),
    )
    .increment(1);
    if let Some((method, path, request_id)) = seen {
        crate::http::recent::push_request(crate::http::recent::RecentRequest {
            at: crate::clock::Clock::now().unix_ms,
            method,
            route: route.clone(),
            path,
            status: res.status().as_u16(),
            cache: cache.to_string(),
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            consumer,
            request_id,
        });
    }
    metrics::counter!(crate::metrics::REQUESTS_TOTAL, "route" => route, "status" => res.status().as_str().to_string(), "cache" => cache)
        .increment(1);
    res
}

/// `consumer_requests_total`'s `consumer` for a request no key was resolved for.
pub const ANONYMOUS: &str = "anonymous";

/// `consumer_requests_total`'s `group` (design/14 §Metrics): the path's
/// prefix, matched on whole segments. `ui`'s "static" is Data Dragon's
/// `/v1/static` and `/ddragon` (ADR-205).
pub fn route_group(path: &str) -> &'static str {
    let under = |prefix: &str| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    if under("/v1/lol/analytics") {
        "analytics"
    } else if under("/v1/lol") {
        "lol"
    } else if under("/v1/riot") {
        "riot"
    } else if under("/v1/players") {
        "players"
    } else if under("/v1/admin") {
        "admin"
    } else if under("/v1/ws") {
        "ws"
    } else if [
        "/healthz",
        "/readyz",
        "/metrics",
        "/status",
        "/openapi.json",
        "/openapi.yaml",
        "/docs",
    ]
    .iter()
    .any(|p| under(p))
    {
        "ops"
    } else if ["/dashboard", "/dev", "/v1/static", "/ddragon"]
        .iter()
        .any(|p| under(p))
    {
        "ui"
    } else {
        "other"
    }
}

/// `consumer_requests_total`'s `class`: `2xx`…`5xx`, and `1xx` for a
/// WebSocket upgrade's 101.
pub fn status_class(status: axum::http::StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
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

/// v1 `setNotFoundHandler`: `No route for <METHOD> <url>`, with a `token` query
/// parameter's value redacted: the message is logged and kept in the admin
/// error ring, and auth accepts `?token=` on every route (REV-01, ADR-186).
async fn not_found(method: Method, uri: Uri) -> ApiError {
    ApiError::not_found(format!("No route for {method} {}", redact_token(&uri)))
}

/// `uri` as displayed, with the value of every `token` query pair replaced by
/// `<redacted>`. The key is matched as `http::auth` matches it: exactly.
fn redact_token(uri: &Uri) -> String {
    let full = uri.to_string();
    let Some(query) = uri.query() else { return full };
    let is_token = |pair: &str| pair.split_once('=').is_some_and(|(k, _)| k == "token");
    if !query.split('&').any(is_token) {
        return full;
    }
    let redacted = query
        .split('&')
        .map(|pair| if is_token(pair) { "token=<redacted>" } else { pair })
        .collect::<Vec<_>>()
        .join("&");
    match full.strip_suffix(query) {
        Some(head) => format!("{head}{redacted}"),
        None => format!("{}?{redacted}", uri.path()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// design/14 §Metrics: groups by path prefix, on whole segments.
    #[test]
    fn route_groups() {
        for (path, group) in [
            ("/v1/riot/account/v1/accounts/by-riot-id/europe/a/b", "riot"),
            ("/v1/lol/summoners/by-puuid/euw1/P", "lol"),
            ("/v1/lol/analytics/champions", "analytics"),
            ("/v1/lol/analytics", "analytics"),
            ("/v1/lol/analyticsx", "lol"),
            ("/v1/players/P/profile", "players"),
            ("/v1/admin/info", "admin"),
            ("/v1/ws", "ws"),
            ("/healthz", "ops"),
            ("/readyz", "ops"),
            ("/metrics", "ops"),
            ("/status", "ops"),
            ("/status/ratelimits", "ops"),
            ("/openapi.json", "ops"),
            ("/openapi.yaml", "ops"),
            ("/docs", "ops"),
            ("/dashboard", "ui"),
            ("/dev", "ui"),
            ("/dev/showcase", "ui"),
            ("/v1/static/versions", "ui"),
            ("/ddragon/15.1.1/img/champion/Ahri.png", "ui"),
            ("/", "other"),
            ("/statusx", "other"),
            ("/v1/adminx", "other"),
            ("/nope", "other"),
        ] {
            assert_eq!(route_group(path), group, "{path}");
        }
    }

    #[test]
    fn status_classes() {
        use axum::http::StatusCode;
        assert_eq!(status_class(StatusCode::SWITCHING_PROTOCOLS), "1xx");
        assert_eq!(status_class(StatusCode::OK), "2xx");
        assert_eq!(status_class(StatusCode::NOT_MODIFIED), "3xx");
        assert_eq!(status_class(StatusCode::TOO_MANY_REQUESTS), "4xx");
        assert_eq!(status_class(StatusCode::BAD_GATEWAY), "5xx");
    }
}

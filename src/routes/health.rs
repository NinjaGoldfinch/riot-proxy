//! `/healthz` and `/readyz`. Both are public: no key, no quota (v1 `routes/health.ts`).
//! `/metrics` is served by `telemetry::metrics_router`.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::cache::keys::KeyScope;
use crate::db::DbError;

/// A readiness probe that cannot get a write slot this quickly counts as not ready.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(healthz))
        .routes(routes!(readyz))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    pub ok: bool,
}

/// Liveness: answers while the process is up. Touches nothing.
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "ops",
    summary = "Liveness",
    description = "Answers as long as the process is up. It touches nothing, so a 200 says only that \
                   the process is serving; use `/readyz` to decide whether to send traffic.",
    security(()),
    responses((status = 200, description = "Up", body = Health)),
)]
async fn healthz() -> Json<Health> {
    Json(Health { ok: true })
}

/// v1 returned `{ok, redis, postgres, keyScope}`. v2 has one store, so `sqlite` replaces
/// the two backend booleans, and `limiter` says the checkpoint was restored
/// (design/07). `keyScope` is v1's (ADR-011).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub ok: bool,
    pub sqlite: bool,
    pub limiter: bool,
    pub key_scope: String,
}

/// Readiness: the writer thread is alive and can take SQLite's write lock, and the
/// limiter checkpoint has been restored.
/// 503 carries the same body, so it names what is not ready (v1 behaviour).
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "ops",
    summary = "Readiness",
    description = "SQLite can take a write and the limiter checkpoint is restored. **The 503 carries the \
                   same body as the 200**, so the booleans name what is not ready.",
    security(()),
    responses(
        (status = 200, description = "Ready", body = Ready),
        (status = 503, description = "Not ready", body = Ready),
    ),
)]
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Ready>) {
    let probe = state.db.write(|c| {
        c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
            .map_err(DbError::from)
    });
    let sqlite = matches!(tokio::time::timeout(READY_TIMEOUT, probe).await, Ok(Ok(())));
    let limiter = state.limiter_restored.load(std::sync::atomic::Ordering::Acquire);
    let ok = sqlite && limiter;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Ready {
            ok,
            sqlite,
            limiter,
            key_scope: KeyScope::from_key(&state.config.riot_api_key).to_string(),
        }),
    )
}

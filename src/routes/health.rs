//! `/healthz` and `/readyz`. Both are public: no key, no quota (v1 `routes/health.ts`).
//! `/metrics` is served by `telemetry::metrics_router`.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app::AppState;
use crate::db::DbError;

/// A readiness probe that cannot get a write slot this quickly counts as not ready.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

#[derive(Debug, Serialize)]
pub struct Health {
    pub ok: bool,
}

/// Liveness: answers while the process is up. Touches nothing.
async fn healthz() -> Json<Health> {
    Json(Health { ok: true })
}

/// v1 returned `{ok, redis, postgres, keyScope}`. v2 has one store, so `sqlite` replaces
/// the two backend booleans. `keyScope` returns with P3-01 and `limiter` with P2-06 (ADR-011).
#[derive(Debug, Serialize)]
pub struct Ready {
    pub ok: bool,
    pub sqlite: bool,
}

/// Readiness: the writer thread is alive and can take SQLite's write lock.
/// 503 carries the same body, so it names what is not ready (v1 behaviour).
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Ready>) {
    let probe = state.db.write(|c| {
        c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
            .map_err(DbError::from)
    });
    let sqlite = matches!(tokio::time::timeout(READY_TIMEOUT, probe).await, Ok(Ok(())));
    let status = if sqlite {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(Ready { ok: sqlite, sqlite }))
}

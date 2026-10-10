//! `/dev/reset`: wipe every piece of fetched data, for the dev explorer's Reset tab
//! (DEV-09, design/10 §Reset tab, ADR-077). It is mounted beside `/dev`, so it
//! exists only when the dev explorer does, and never in production. Both
//! methods need an admin key, and the `POST` needs `{"confirm":"reset"}` as well.
//! The routes stay out of the OpenAPI document, so the explorer's forms never
//! offer them.
//!
//! The workers are stopped first (DEV-22): no claims, every running job
//! aborted, so nothing writes into the emptied tables afterwards. They resume
//! on the empty queue once the wipe is done.
//!
//! What goes: the archive, facts, analytics, ladder crawls, both cache tiers,
//! every job row and the metrics history. What stays: consumers (the keys this
//! page signs in with) and `limiter_state`, which mirrors Riot's live counts for
//! the key; forgetting them would let the proxy overrun the real limit.

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, extract::State};
use rusqlite::Connection;
use serde::Serialize;

use crate::app::AppState;
use crate::clock::Clock;
use crate::db::DbError;
use crate::http::body::Body;
use crate::http::{ApiError, ErrorCode};

/// How long the reset waits for aborted jobs to hand their workers back.
pub const HALT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The word the `POST` body must carry.
pub const CONFIRM: &str = "reset";

/// Tables the reset empties, children before parents so foreign keys hold.
pub const WIPED: &[&str] = &[
    "timelines",
    "match_bans",
    "match_builds",
    "match_tiers",
    "match_facts",
    "crawl_legs",
    "crawl_match_ids",
    "ladder_entries",
    "ladder_crawls",
    "player_ranks",
    "rank_lookups",
    "rank_lookup_queue",
    "matches",
    "players",
    "champion_stats",
    "champion_bans",
    "champion_ban_totals",
    "champion_builds",
    "champion_build_parts",
    "champion_matchups",
    "champion_items",
    "champion_runes",
    "champion_spells",
    "analytics_slices",
    "analytics_match_totals",
    "analytics_runs",
    "cache",
    "jobs",
    "metrics_history",
];

/// Tables it leaves alone. Every table in the schema is in exactly one of the
/// two lists (tested), so a new migration has to choose.
pub const KEPT: &[&str] = &["consumers", "limiter_state", "refinery_schema_history"];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TableRows {
    name: &'static str,
    rows: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Preview {
    /// What a reset would delete, per table.
    tables: Vec<TableRows>,
    /// In-memory (L1) cache entries.
    l1_entries: u64,
    /// Jobs running now; a reset stops them before it deletes anything.
    running_jobs: i64,
    kept: &'static [&'static str],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Done {
    ok: bool,
    /// Rows deleted, per table.
    tables: Vec<TableRows>,
    l1_entries: usize,
    /// Running jobs the reset stopped before the wipe.
    stopped_jobs: usize,
    /// Job rows marked running when they were deleted: the stopped jobs, plus
    /// any with no worker here (another process's, or a dead one's).
    running_jobs: i64,
    took_ms: i64,
}

fn count(c: &Connection, table: &str) -> rusqlite::Result<i64> {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
}

fn running(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT count(*) FROM jobs WHERE state = 'running'", [], |r| {
        r.get(0)
    })
}

/// Delete every row of [`WIPED`] in one transaction. Returns rows per table and
/// how many jobs were running when it happened.
pub fn wipe(c: &mut Connection) -> rusqlite::Result<(Vec<(&'static str, i64)>, i64)> {
    let tx = c.transaction()?;
    let running = running(&tx)?;
    let mut deleted = Vec::with_capacity(WIPED.len());
    for &table in WIPED {
        let n = tx.execute(&format!("DELETE FROM {table}"), [])?;
        deleted.push((table, i64::try_from(n).unwrap_or(i64::MAX)));
    }
    tx.commit()?;
    Ok((deleted, running))
}

fn no_store(status: StatusCode, body: &impl Serialize) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

fn internal(e: &DbError, what: &str) -> Response {
    tracing::error!(error = %e, "{what}");
    ApiError::internal().into_response()
}

async fn preview(State(state): State<AppState>) -> Response {
    let counted = state
        .db
        .read(|c| {
            let tables = WIPED
                .iter()
                .map(|&name| {
                    Ok(TableRows {
                        name,
                        rows: count(c, name)?,
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, DbError>((tables, running(c)?))
        })
        .await;
    let (tables, running_jobs) = match counted {
        Ok(v) => v,
        Err(e) => return internal(&e, "could not count rows for the reset preview"),
    };
    let l1 = &state.fetcher.cache().l1;
    l1.sync().await;
    no_store(
        StatusCode::OK,
        &Preview {
            tables,
            l1_entries: l1.entry_count(),
            running_jobs,
            kept: KEPT,
        },
    )
}

async fn reset(State(state): State<AppState>, bytes: Bytes) -> Response {
    let confirmed = Body::parse(&bytes).and_then(|b| {
        b.required(&["confirm"])?;
        match b.string("confirm", 1, 20)?.as_deref() {
            Some(CONFIRM) => Ok(()),
            _ => Err(ApiError::new(
                ErrorCode::Validation,
                format!("body/confirm must be equal to '{CONFIRM}'"),
            )),
        }
    });
    if let Err(e) = confirmed {
        return e.into_response();
    }
    let started = Clock::now().unix_ms;
    // Workers first, so no job writes into the tables once they are empty.
    // Held until the wipe is done; dropping it lets them claim again.
    let halt = state.jobs.halt(HALT_WAIT).await;
    if !halt.settled {
        tracing::warn!("dev reset: a job did not stop within {HALT_WAIT:?}; wiping anyway");
    }
    // L1 next, so nothing read between the two steps comes from a dropped row.
    let l1_entries = state.fetcher.cache().l1.invalidate_where(|_| true).await.len();
    let wiped = state.db.write(|c| Ok::<_, DbError>(wipe(c)?)).await;
    let stopped_jobs = halt.stopped;
    drop(halt);
    let (deleted, running_jobs) = match wiped {
        Ok(v) => v,
        Err(e) => return internal(&e, "dev reset failed"),
    };
    let tables: Vec<TableRows> = deleted
        .into_iter()
        .map(|(name, rows)| TableRows { name, rows })
        .collect();
    let rows: i64 = tables.iter().map(|t| t.rows).sum();
    tracing::warn!(
        rows,
        l1_entries,
        stopped_jobs,
        running_jobs,
        "dev reset: all fetched data deleted"
    );
    no_store(
        StatusCode::OK,
        &Done {
            ok: true,
            tables,
            l1_entries,
            stopped_jobs,
            running_jobs,
            took_ms: Clock::now().unix_ms - started,
        },
    )
}

/// `GET /dev/reset` (what would go) and `POST /dev/reset` (do it), admin only.
/// The caller mounts this only when `dev_ui` is on.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/dev/reset", get(preview).post(reset))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::http::auth::require_admin,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table in the migrated schema is either wiped or kept, never both.
    #[test]
    fn every_table_is_classified() {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        let mut stmt = c
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for t in &tables {
            let wiped = WIPED.contains(&t.as_str());
            let kept = KEPT.contains(&t.as_str());
            assert!(wiped != kept, "table {t} must be in exactly one of WIPED / KEPT");
        }
        for t in WIPED.iter().chain(KEPT) {
            assert!(
                tables.iter().any(|x| x == t),
                "{t} is listed but not in the schema"
            );
        }
    }
}

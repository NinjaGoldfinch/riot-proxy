//! `/v1/admin/*`, the data subset (plan P5-05; v1 `routes/admin.ts`,
//! `health.ts` and `debug.ts`): consumers, tracked players, cache purge, stats,
//! rate-limit usage and the two debug routes. Every route needs the `admin`
//! scope and passes the admin IP allowlist (the auth guard); quotas apply.
//!
//! The job routes (P6-08) are v2's: v1's queues were BullMQ's. Ladder,
//! analytics and metrics routes arrive with their features (P7).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::archive::matches;
use crate::cache::keys::{cache_key, glob_match, scoped_purge_pattern};
use crate::cache::l1::Lookup;
use crate::cache::l2;
use crate::clock::{Clock, iso_ms};
use crate::consumers::{self, ConsumerError, DEFAULT_QUOTA_PER_MIN, NewConsumer, Scope};
use crate::fetcher::FetchOptions;
use crate::http::body::Body;
use crate::http::{ApiError, ErrorCode, validate};
use crate::players;
use crate::riot::client::RiotRequest;
use crate::riot::endpoints::{ENDPOINTS, Endpoint, Target};
use crate::riot::routing::{Platform, Region};
use crate::routes::passthrough::{JSON, LocalErrors, PassthroughResponses, UpstreamErrors, respond};
use crate::routes::riot::bad_path;

mod ladder_probe;

type Q = Query<HashMap<String, String>>;
type Who = Extension<Arc<crate::http::auth::Consumer>>;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_consumer, list_consumers))
        .routes(routes!(revoke_consumer))
        .routes(routes!(revoke_cache))
        .routes(routes!(list_players, track_player))
        .routes(routes!(untrack_player))
        .routes(routes!(player_archive))
        .routes(routes!(player_archive_matches))
        .routes(routes!(purge_cache))
        .routes(routes!(stats))
        .routes(routes!(limits))
        .routes(routes!(debug_riot))
        .routes(routes!(debug_cache))
        .routes(routes!(list_jobs))
        .routes(routes!(job_stats))
        .routes(routes!(job_queue))
        .routes(routes!(job_activity))
        .routes(routes!(job_trace))
        .routes(routes!(retry_job))
        .routes(routes!(cancel_job))
        .routes(routes!(queue_backfill))
        .routes(routes!(queue_ddragon_sync))
        .routes(routes!(start_ladder_crawl))
        .routes(routes!(ladder_options))
        .routes(routes!(ladder_probe::ladder_probe))
        .routes(routes!(list_ladder_crawls))
        .routes(routes!(cancel_ladder_crawl, crawl_activity))
        .routes(routes!(queue_names_backfill))
        .routes(routes!(recompute_analytics))
        .routes(routes!(reextract_facts))
        .routes(routes!(metrics_snapshot))
        .routes(routes!(metrics_history))
}

fn json<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(b) => (status, [(header::CONTENT_TYPE, JSON)], b).into_response(),
        Err(_) => ApiError::internal().into_response(),
    }
}

fn ok<T: Serialize>(body: &T) -> Response {
    json(StatusCode::OK, body)
}

fn now_ms() -> i64 {
    Clock::now().unix_ms
}

fn scope_of(state: &AppState) -> String {
    state.fetcher.key_scope().as_str().to_string()
}

/// A store failure: logged, answered as `INTERNAL` without detail.
fn internal(e: &impl std::fmt::Display, what: &str) -> Response {
    tracing::error!(error = %e, "{what}");
    ApiError::internal().into_response()
}

// ── Consumers ───────────────────────────────────────────────────────────────

/// A consumer, never its key (v1 `ConsumerSummary`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerSummary {
    /// A ULID (v1 used UUIDs).
    id: String,
    name: String,
    #[schema(value_type = Vec<String>)]
    scopes: Vec<Scope>,
    /// Requests per minute allowed to this consumer.
    quota_per_min: u32,
    created_at: Option<String>,
    /// Set when the key was revoked; revoked consumers are kept, never deleted.
    #[schema(required = true)]
    disabled_at: Option<String>,
}

impl From<consumers::Consumer> for ConsumerSummary {
    fn from(c: consumers::Consumer) -> Self {
        Self {
            id: c.id,
            name: c.name,
            scopes: c.scopes,
            quota_per_min: c.quota_per_min,
            created_at: iso_ms(c.created_at),
            disabled_at: c.revoked_at.and_then(iso_ms),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConsumerList {
    consumers: Vec<ConsumerSummary>,
}

/// `POST /v1/admin/consumers` body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct CreateConsumer {
    /// 1–100 characters; unique.
    name: String,
    /// `read` and/or `admin`; default `["read"]`.
    scopes: Option<Vec<String>>,
    /// 1–1 000 000; default 600.
    quota_per_min: Option<u32>,
}

/// The new consumer and its key, shown exactly once.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedConsumer {
    id: String,
    name: String,
    #[schema(value_type = Vec<String>)]
    scopes: Vec<Scope>,
    quota_per_min: u32,
    /// The plaintext key. Only its SHA-256 is stored.
    key: String,
    warning: &'static str,
}

#[utoipa::path(
    post, path = "/v1/admin/consumers", tag = "admin",
    summary = "Create a consumer",
    description = "Issues a key. The plaintext key is in this response and nowhere else: only its SHA-256 is stored.",
    request_body = CreateConsumer,
    responses((status = 200, description = "The consumer and its key", body = CreatedConsumer), LocalErrors),
)]
async fn create_consumer(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["name"])?;
        let name = b.string("name", 1, 100)?.unwrap_or_default();
        let scopes = b.enum_array("scopes", &["read", "admin"], 1)?;
        let quota = b.integer("quotaPerMin", 1, 1_000_000)?;
        let mut scopes: Vec<Scope> = scopes
            .unwrap_or_else(|| vec!["read"])
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        scopes.sort();
        scopes.dedup();
        Ok::<_, ApiError>(NewConsumer {
            name,
            scopes,
            quota_per_min: quota.map_or(DEFAULT_QUOTA_PER_MIN, |q| u32::try_from(q).unwrap_or(u32::MAX)),
            key: None,
        })
    })();
    let new = match parsed {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    match consumers::create(&state.db, new).await {
        Ok(created) => {
            tracing::info!(id = %created.consumer.id, name = %created.consumer.name, "consumer created");
            ok(&CreatedConsumer {
                id: created.consumer.id,
                name: created.consumer.name,
                scopes: created.consumer.scopes,
                quota_per_min: created.consumer.quota_per_min,
                key: created.key.expose().to_string(),
                warning: "Store this key now — it cannot be retrieved again.",
            })
        }
        Err(e @ (ConsumerError::DuplicateName(_) | ConsumerError::Invalid(_))) => {
            ApiError::new(ErrorCode::Validation, e.to_string()).into_response()
        }
        Err(e) => internal(&e, "could not create consumer"),
    }
}

#[utoipa::path(
    get, path = "/v1/admin/consumers", tag = "admin",
    summary = "List consumers",
    description = "Every consumer, including revoked ones (`disabledAt` tells them apart). Keys are never \
                   returned: only their SHA-256 is stored, and not even that is exposed here.",
    responses((status = 200, description = "The consumers", body = ConsumerList), LocalErrors),
)]
async fn list_consumers(State(state): State<AppState>, Extension(_c): Who) -> Response {
    match consumers::list(&state.db).await {
        Ok(list) => ok(&ConsumerList {
            consumers: list.into_iter().map(ConsumerSummary::from).collect(),
        }),
        Err(e) => internal(&e, "could not list consumers"),
    }
}

#[utoipa::path(
    delete, path = "/v1/admin/consumers/{id}", tag = "admin",
    summary = "Revoke a consumer",
    description = "The key stops working immediately: it is also dropped from the auth cache. The row is kept, \
                   so the key can never be reissued.",
    params(("id" = String, Path, description = "Consumer id (a ULID)")),
    responses((status = 200, description = "`{ok, id}`", body = serde_json::Value), LocalErrors),
)]
async fn revoke_consumer(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    match consumers::revoke_by_id(&state.db, &id).await {
        Ok(Some((consumer, hash))) => {
            // v1 left a revoked key working until its auth-cache entry expired
            // and offered `revoke-cache` for the impatient; v2 drops it here.
            state.auth.invalidate(hash).await;
            // And its open sockets (ADR-045).
            let closed = state.hub.close_consumer(&consumer.id);
            tracing::info!(id = %consumer.id, sockets = closed, "consumer disabled");
            ok(&serde_json::json!({"ok": true, "id": consumer.id}))
        }
        Ok(None) => ApiError::not_found("No active consumer with that id").into_response(),
        Err(e) => internal(&e, "could not revoke consumer"),
    }
}

/// `POST /v1/admin/consumers/{id}/revoke-cache` body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct RevokeCache {
    /// The key's SHA-256, 64 hex characters.
    key_hash: String,
}

#[utoipa::path(
    post, path = "/v1/admin/consumers/{id}/revoke-cache", tag = "admin",
    summary = "Drop a key from the auth cache",
    description = "Forces the next request with this key to be re-checked against the database. The key hash \
                   must belong to the consumer in the path. Revoking a consumer already does this.",
    params(("id" = String, Path, description = "Consumer id (a ULID)")),
    request_body = RevokeCache,
    responses((status = 200, description = "`{ok, id, stillActive}`", body = serde_json::Value), LocalErrors),
)]
async fn revoke_cache(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
    bytes: Bytes,
) -> Response {
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let parsed = validate::consumer_id(&id).and_then(|()| {
        let b = Body::parse(&bytes)?;
        b.required(&["keyHash"])?;
        Ok(b.string("keyHash", 64, 64)?.unwrap_or_default())
    });
    let key_hash = match parsed {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let not_found = || ApiError::not_found("No consumer with that id and key hash").into_response();
    let Some(hash) = hex::decode(&key_hash)
        .ok()
        .and_then(|h| <[u8; 32]>::try_from(h).ok())
    else {
        return not_found();
    };
    match consumers::find_by_id_and_hash(&state.db, &id, hash).await {
        Ok(Some(consumer)) => {
            state.auth.invalidate(hash).await;
            ok(
                &serde_json::json!({"ok": true, "id": consumer.id, "stillActive": consumer.revoked_at.is_none()}),
            )
        }
        Ok(None) => not_found(),
        Err(e) => internal(&e, "could not look up consumer"),
    }
}

// ── Tracked players ─────────────────────────────────────────────────────────

/// A `players` row (v1 `PlayerSummary`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerRecord {
    puuid: String,
    /// Fingerprint of the Riot key this PUUID was encrypted for.
    key_scope: String,
    platform: String,
    #[schema(required = true)]
    game_name: Option<String>,
    #[schema(required = true)]
    tag_line: Option<String>,
    tracked: bool,
    /// Cursor the match poller resumes from.
    #[schema(required = true)]
    last_seen_match_id: Option<String>,
    /// Set with `historyBackfilledAt` still null means a walk that died mid-way.
    #[schema(required = true)]
    history_backfill_started_at: Option<String>,
    #[schema(required = true)]
    history_backfilled_at: Option<String>,
    #[schema(required = true)]
    history_backfill_depth: Option<i64>,
    updated_at: Option<String>,
}

impl PlayerRecord {
    fn new(scope: &str, p: players::Player) -> Self {
        let walk = crate::jobs::archive::BackfillState::parse(p.backfill_state.as_deref());
        Self {
            puuid: p.puuid,
            key_scope: scope.to_string(),
            platform: p.platform,
            game_name: p.game_name,
            tag_line: p.tag_line,
            tracked: p.tracked,
            last_seen_match_id: p.last_seen_match_id,
            history_backfill_started_at: walk.as_ref().and_then(|w| iso_ms(w.started_at)),
            history_backfilled_at: walk.as_ref().and_then(|w| w.done_at).and_then(iso_ms),
            history_backfill_depth: walk.as_ref().map(|w| w.depth),
            updated_at: iso_ms(p.updated_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlayerList {
    players: Vec<PlayerRecord>,
}

#[utoipa::path(
    get, path = "/v1/admin/tracked-players", tag = "admin",
    summary = "List known players",
    description = "Every player row for the current `keyScope`, tracked or not: tracking is a flag on the row. \
                   Rows written under a previous Riot key are not returned, because PUUIDs are encrypted per key.",
    responses((status = 200, description = "The players", body = PlayerList), LocalErrors),
)]
async fn list_players(State(state): State<AppState>, Extension(_c): Who) -> Response {
    let scope = scope_of(&state);
    match players::list(&state.db, &scope).await {
        Ok(rows) => ok(&PlayerList {
            players: rows.into_iter().map(|p| PlayerRecord::new(&scope, p)).collect(),
        }),
        Err(e) => internal(&e, "could not list players"),
    }
}

/// `POST /v1/admin/tracked-players` body: a PUUID, or a Riot ID to resolve.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct TrackPlayer {
    /// Platform routing value, e.g. `euw1`.
    platform: String,
    puuid: Option<String>,
    game_name: Option<String>,
    tag_line: Option<String>,
    /// Default `true`; `false` records the player without polling it.
    tracked: Option<bool>,
}

/// The stored row, plus the history walk tracking queued, if it did (v1 #46).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackedPlayer {
    #[serde(flatten)]
    player: PlayerRecord,
    #[schema(required = true)]
    backfill: Option<crate::routes::players::BackfillNotice>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    puuid: Option<String>,
    game_name: Option<String>,
    tag_line: Option<String>,
}

#[utoipa::path(
    post, path = "/v1/admin/tracked-players", tag = "admin",
    summary = "Track a player, or re-resolve one by Riot ID",
    description = "Give a PUUID, or a Riot ID (`gameName` + `tagLine`) which is resolved through account-v1 on \
                   the normal cached read path. Re-posting by Riot ID after a key rotation re-resolves the \
                   player under the new key.",
    request_body = TrackPlayer,
    responses((status = 200, description = "The stored player", body = TrackedPlayer), UpstreamErrors),
)]
async fn track_player(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["platform"])?;
        let platform = b
            .with("platform", validate::platform_at)?
            .ok_or_else(ApiError::internal)?;
        let puuid = b.string("puuid", 0, usize::MAX)?;
        if let Some(p) = &puuid {
            validate::puuid_at("body", p)?;
        }
        let game_name = b.string("gameName", 0, usize::MAX)?;
        if let Some(g) = &game_name {
            validate::game_name_at("body", g)?;
        }
        let tag_line = b.string("tagLine", 0, usize::MAX)?;
        if let Some(t) = &tag_line {
            validate::tag_line_at("body", t)?;
        }
        let tracked = b.boolean("tracked")?.unwrap_or(true);
        Ok::<_, ApiError>((platform, puuid, game_name, tag_line, tracked))
    })();
    let (platform, puuid, mut game_name, mut tag_line, tracked) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let puuid = match puuid {
        Some(p) => p,
        None => {
            let (Some(name), Some(tag)) = (game_name.clone(), tag_line.clone()) else {
                return ApiError::new(
                    ErrorCode::Validation,
                    "Provide either puuid, or gameName and tagLine",
                )
                .into_response();
            };
            match resolve(&state, &name, &tag).await {
                Ok(account) => {
                    // Riot's spelling of the Riot ID wins over the caller's (v1).
                    game_name = account.game_name.or(game_name);
                    tag_line = account.tag_line.or(tag_line);
                    match account.puuid {
                        Some(p) => p,
                        None => {
                            return ApiError::not_found(format!(
                                "Riot ID '{name}#{tag}' did not resolve to a PUUID"
                            ))
                            .into_response();
                        }
                    }
                }
                Err(res) => return *res,
            }
        }
    };

    let scope = scope_of(&state);
    let row = players::Upsert {
        puuid: &puuid,
        platform: platform.as_str(),
        game_name: game_name.as_deref(),
        tag_line: tag_line.as_deref(),
        tracked: Some(tracked),
    };
    match players::upsert(&state.db, &scope, row, now_ms()).await {
        Ok(p) => {
            tracing::info!(puuid = %p.puuid, tracked = p.tracked, "tracked player upserted");
            // v1 #46: tracking only archived what the poller caught from then on,
            // so walk the history the way a first lookup does, once.
            let walked = crate::jobs::archive::BackfillState::parse(p.backfill_state.as_deref())
                .is_some_and(|w| w.done_at.is_some());
            let limit = state.config.lookup_backfill_limit;
            let backfill = if p.tracked && !walked && limit > 0 {
                let walk = crate::jobs::archive::BackfillPlayer {
                    puuid: p.puuid.clone(),
                    platform: p.platform.clone(),
                    limit,
                    fetch_timeline: None,
                    queue_id: None,
                    reason: Some("track".into()),
                };
                match crate::jobs::archive::enqueue_backfill(&state.jobs, &walk).await {
                    Ok(q) => Some(crate::routes::players::BackfillNotice {
                        job_id: q.id.clone(),
                        status: q.status().to_string(),
                        limit,
                    }),
                    Err(e) => {
                        // Tracking succeeded; a queue that is down must not undo that (v1).
                        tracing::warn!(error = %e, puuid = %p.puuid, "could not queue backfill on track");
                        None
                    }
                }
            } else {
                None
            };
            ok(&TrackedPlayer {
                player: PlayerRecord::new(&scope, p),
                backfill,
            })
        }
        Err(e) => internal(&e, "could not upsert player"),
    }
}

/// account-v1 by Riot ID, through the fetcher (cached like any read), on
/// whichever cluster has room (ADR-066).
async fn resolve(state: &AppState, name: &str, tag: &str) -> Result<Account, Box<Response>> {
    let req = Endpoint::by_id("account.byRiotId")
        .ok_or_else(ApiError::internal)
        .and_then(|e| RiotRequest::account(e, &[name, tag]).map_err(|_| ApiError::internal()))
        .map_err(|e| Box::new(e.into_response()))?;
    match state.fetcher.fetch(req, FetchOptions::default()).await {
        Ok(r) => serde_json::from_slice(&r.body).map_err(|_| Box::new(ApiError::upstream().into_response())),
        Err(e) => Err(Box::new(respond(Err(e)))),
    }
}

#[utoipa::path(
    delete, path = "/v1/admin/tracked-players/{puuid}", tag = "admin",
    summary = "Stop tracking a player",
    description = "Untracks rather than deletes: the row keeps the identity and backfill state a later lookup reuses.",
    params(("puuid" = String, Path, description = "Encrypted player UUID")),
    responses((status = 200, description = "`{ok, puuid, tracked: false}`", body = serde_json::Value), LocalErrors),
)]
async fn untrack_player(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::puuid(&puuid) {
        return e.into_response();
    }
    match players::set_tracked(&state.db, &scope_of(&state), &puuid, false, now_ms()).await {
        Ok(true) => ok(&serde_json::json!({"ok": true, "puuid": puuid, "tracked": false})),
        Ok(false) => ApiError::not_found("No such player for the current key scope").into_response(),
        Err(e) => internal(&e, "could not untrack player"),
    }
}

// ── One player's archive (DEV-03) ───────────────────────────────────────────

/// Archived matches in one queue.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueCount {
    queue_id: i64,
    matches: i64,
}

/// What the archive holds for one player: exact counts from `match_facts`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTotals {
    matches: i64,
    /// Matches Riot flagged as remakes; counted in `matches`.
    remakes: i64,
    wins: i64,
    /// Of `matches`, how many also have a stored timeline.
    timelines: i64,
    #[schema(required = true)]
    oldest_game_end: Option<String>,
    #[schema(required = true)]
    newest_game_end: Option<String>,
    /// Most matches first.
    by_queue: Vec<QueueCount>,
}

/// `GET /v1/admin/players/{puuid}/archive`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerArchive {
    puuid: String,
    key_scope: String,
    /// The player's row; null if they were never looked up or tracked under this key.
    #[schema(required = true)]
    player: Option<PlayerRecord>,
    archive: ArchiveTotals,
    /// This player's `archive:match` and `backfill:player` jobs by state. Uncapped;
    /// `done` rows are kept for seven days.
    jobs: std::collections::BTreeMap<String, StateCounts>,
    /// The newest job of each of those kinds, if any.
    latest_jobs: Vec<JobSummary>,
}

const PLAYER_JOB_KINDS: &[&str] = &[
    crate::jobs::kinds::ARCHIVE_MATCH,
    crate::jobs::kinds::BACKFILL_PLAYER,
];

fn puuid_path(path: Result<Path<String>, PathRejection>) -> Result<String, Box<Response>> {
    let Path(puuid) = path.map_err(|e| Box::new(bad_path(&e).into_response()))?;
    validate::puuid(&puuid).map_err(|e| Box::new(e.into_response()))?;
    Ok(puuid)
}

#[utoipa::path(
    get, path = "/v1/admin/players/{puuid}/archive", tag = "admin",
    summary = "One player's archive and backfill",
    description = "Exact counts of what the archive holds for one player under the current `keyScope` (every \
                   archived match they played in, by queue, with the date range), their row's history-walk \
                   state, and their `archive:match` / `backfill:player` jobs by state. Nothing is capped.",
    params(("puuid" = String, Path, description = "Encrypted player UUID")),
    responses((status = 200, description = "The player's archive", body = PlayerArchive), LocalErrors),
)]
async fn player_archive(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let puuid = match puuid_path(path) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let scope = scope_of(&state);
    let (row, totals, jobs) = tokio::join!(
        players::get(&state.db, &scope, &puuid),
        crate::archive::player::summary(&state.db, &scope, &puuid),
        state.jobs.for_puuid(&puuid, PLAYER_JOB_KINDS),
    );
    let (row, totals, (counts, latest)) = match (row, totals, jobs) {
        (Ok(r), Ok(t), Ok(j)) => (r, t, j),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            return internal(&e, "could not read the player's archive");
        }
    };
    let mut jobs = std::collections::BTreeMap::<String, StateCounts>::new();
    for k in PLAYER_JOB_KINDS {
        jobs.insert((*k).to_string(), StateCounts::default());
    }
    for (kind, st, n) in counts {
        jobs.entry(kind).or_default().add(&st, n);
    }
    ok(&PlayerArchive {
        player: row.map(|p| PlayerRecord::new(&scope, p)),
        archive: ArchiveTotals {
            matches: totals.matches,
            remakes: totals.remakes,
            wins: totals.wins,
            timelines: totals.timelines,
            oldest_game_end: totals.oldest_game_end_ms.and_then(iso_ms),
            newest_game_end: totals.newest_game_end_ms.and_then(iso_ms),
            by_queue: totals
                .by_queue
                .into_iter()
                .map(|(queue_id, matches)| QueueCount { queue_id, matches })
                .collect(),
        },
        jobs,
        latest_jobs: latest.into_iter().map(JobSummary::from).collect(),
        puuid,
        key_scope: scope,
    })
}

/// The player's line in one archived match.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedLine {
    match_id: String,
    queue_id: i64,
    game_end_timestamp: i64,
    /// Seconds.
    #[schema(required = true)]
    game_duration: Option<i64>,
    /// Null until the match's facts have been re-extracted (V0005).
    #[schema(required = true)]
    remake: Option<bool>,
    champion_id: i64,
    #[schema(required = true)]
    position: Option<String>,
    win: bool,
    #[schema(required = true)]
    kills: Option<i64>,
    #[schema(required = true)]
    deaths: Option<i64>,
    #[schema(required = true)]
    assists: Option<i64>,
    #[schema(required = true)]
    cs: Option<i64>,
}

/// `GET /v1/admin/players/{puuid}/archive/matches`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedPage {
    puuid: String,
    /// Every archived match for this player and filter, not just this page.
    total: i64,
    start: i64,
    count: i64,
    matches: Vec<ArchivedLine>,
}

#[utoipa::path(
    get, path = "/v1/admin/players/{puuid}/archive/matches", tag = "admin",
    summary = "One player's archived matches",
    description = "Their line in every archived match, newest first, from `match_facts`: no Riot call, no quota. \
                   `total` counts all of them for the filter.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("start" = Option<i64>, Query, description = "0–1000000, default 0"),
           ("count" = Option<i64>, Query, description = "1–100, default 25"),
           ("queue" = Option<i64>, Query, description = "Queue id, 0–5000")),
    responses((status = 200, description = "The page", body = ArchivedPage), LocalErrors),
)]
async fn player_archive_matches(
    State(state): State<AppState>,
    Extension(_c): Who,
    Query(query): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let puuid = match puuid_path(path) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let q = |k: &str| query.get(k).map(String::as_str);
    let parsed = (|| {
        let start = validate::int_query("start", q("start"), 0, 1_000_000)?.unwrap_or(0);
        let count = validate::int_query("count", q("count"), 1, 100)?.unwrap_or(25);
        let queue = validate::int_query("queue", q("queue"), 0, 5000)?;
        Ok::<_, ApiError>((start, count, queue))
    })();
    let (start, count, queue) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let scope = scope_of(&state);
    match crate::archive::player::lines(&state.db, &scope, &puuid, queue, start, count).await {
        Ok((total, rows)) => ok(&ArchivedPage {
            puuid,
            total,
            start,
            count,
            matches: rows
                .into_iter()
                .map(|l| ArchivedLine {
                    match_id: l.match_id,
                    queue_id: l.queue_id,
                    game_end_timestamp: l.game_end_ms,
                    game_duration: l.game_duration,
                    remake: l.remake,
                    champion_id: l.champion_id,
                    position: l.position,
                    win: l.win,
                    kills: l.kills,
                    deaths: l.deaths,
                    assists: l.assists,
                    cs: l.cs,
                })
                .collect(),
        }),
        Err(e) => internal(&e, "could not list the player's archived matches"),
    }
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// `POST /v1/admin/cache/purge` body.
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct Purge {
    /// A Redis-style glob (`*`, `?`, `[…]`) over cache keys, 1–200 characters. Scoped to the
    /// current key unless it already starts with a key scope.
    pattern: String,
}

#[utoipa::path(
    post, path = "/v1/admin/cache/purge", tag = "admin",
    summary = "Purge cache entries by pattern",
    description = "Removes matching entries from memory and from the persisted cache. Keys look like \
                   `{keyScope}:{endpointId}:{host}:{params…}`, e.g. `summoner.byPuuid:*`. The match archive \
                   is not a cache and is never purged.",
    request_body = Purge,
    responses((status = 200, description = "`{ok, pattern, deleted}`", body = serde_json::Value), LocalErrors),
)]
async fn purge_cache(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    let parsed = Body::parse(&bytes).and_then(|b| {
        b.required(&["pattern"])?;
        Ok(b.string("pattern", 1, 200)?.unwrap_or_default())
    });
    let pattern = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let scoped = scoped_purge_pattern(state.fetcher.key_scope(), &pattern);
    let mut gone: HashSet<String> = state
        .fetcher
        .cache()
        .l1
        .invalidate_where(|k| glob_match(&scoped, k))
        .await
        .into_iter()
        .collect();
    let matcher = scoped.clone();
    match l2::delete_where(&state.db, move |k| glob_match(&matcher, k)).await {
        Ok(rows) => gone.extend(rows),
        Err(e) => return internal(&e, "could not purge the persisted cache"),
    }
    tracing::info!(pattern = %scoped, deleted = gone.len(), "cache purged");
    ok(&serde_json::json!({"ok": true, "pattern": pattern, "deleted": gone.len()}))
}

// ── Status ──────────────────────────────────────────────────────────────────

/// Archive and tracking counts (v1's three fields, plus archive size).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminStats {
    key_scope: String,
    /// The whole archive, which is not key-scoped and survives a key rotation.
    archived_matches: i64,
    /// Scoped to the current key: zero right after a rotation.
    tracked_players: i64,
    /// Player rows for the current key, tracked or not.
    known_players: i64,
    archived_timelines: i64,
    /// Compressed bytes the archive's match bodies take.
    archive_stored_bytes: i64,
    /// What they decompress to.
    archive_raw_bytes: i64,
}

#[utoipa::path(
    get, path = "/v1/admin/stats", tag = "admin",
    summary = "Archive and tracking counts",
    description = "`archivedMatches` counts the whole archive, which is not key-scoped: match ids are not \
                   encrypted, so it survives a key rotation. `trackedPlayers` is scoped to the current key and \
                   reads zero right after one.",
    responses((status = 200, description = "The counts", body = AdminStats), LocalErrors),
)]
async fn stats(State(state): State<AppState>, Extension(_c): Who) -> Response {
    let scope = scope_of(&state);
    let archive = match matches::stats(&state.db).await {
        Ok(s) => s,
        Err(e) => return internal(&e, "could not read archive stats"),
    };
    let (known, tracked) = match players::counts(&state.db, &scope).await {
        Ok(c) => c,
        Err(e) => return internal(&e, "could not count players"),
    };
    ok(&AdminStats {
        key_scope: scope,
        archived_matches: archive.matches,
        tracked_players: tracked,
        known_players: known,
        archived_timelines: archive.timelines,
        archive_stored_bytes: archive.stored_bytes,
        archive_raw_bytes: archive.raw_bytes,
    })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WindowUsage {
    /// `limit:seconds`, e.g. `20:1`.
    window: String,
    used: u32,
    limit: u32,
}

/// One rate-limit bucket (v1 `LimitsResponse`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    scope: String,
    /// Per-window usage as the limiter sees it; empty before Riot has named the limits.
    usage: Vec<WindowUsage>,
    /// Milliseconds until a 429-induced freeze on this bucket lifts; 0 when it
    /// is not frozen (v1 `isFrozen` answered 0, never null).
    frozen_ms: u64,
}

#[utoipa::path(
    get, path = "/v1/admin/limits/{scope}", tag = "admin",
    summary = "Rate-limit bucket usage",
    description = "Current usage of one Riot rate-limit bucket, and how long any 429 freeze on it has left. \
                   The scope is a host, not a game region: platform endpoints bucket by platform (`euw1`), \
                   account-v1 and match-v5 by region (`europe`).",
    params(("scope" = String, Path, description = "A platform (`euw1`) or a region (`europe`)")),
    responses((status = 200, description = "The bucket", body = Limits), LocalErrors),
)]
async fn limits(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(scope) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let scope = match validate::limiter_scope(&scope) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let limiter = state.fetcher.limiter();
    ok(&Limits {
        scope: scope.to_string(),
        usage: limiter
            .usage(scope)
            .into_iter()
            .map(|w| WindowUsage {
                window: w.window,
                used: w.used,
                limit: w.limit,
            })
            .collect(),
        frozen_ms: limiter
            .frozen_for(scope)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    })
}

// ── Debug ───────────────────────────────────────────────────────────────────

/// The request the debug routes describe: `scope` (a platform or region, any
/// case), `path` and, optionally, the endpoint whose bucket and cache key it
/// uses. Without a method, v1's conservative default bucket
/// (`status.platformData`) is borrowed and the whole path is the cache key.
fn debug_request(scope: &str, path: &str, method: Option<&str>) -> Result<RiotRequest, ApiError> {
    if !path.starts_with('/') {
        return Err(ApiError::new(ErrorCode::Validation, "path must start with '/'"));
    }
    let lower = scope.to_ascii_lowercase();
    let target = match (lower.parse::<Region>(), lower.parse::<Platform>()) {
        (Ok(r), _) => Target::Region(r),
        (_, Ok(p)) => Target::Platform(p),
        _ => {
            return Err(ApiError::bad_region(format!(
                "'{scope}' is neither a platform nor a region"
            )));
        }
    };
    let (raw_path, raw_query) = path.split_once('?').unwrap_or((path, ""));
    let explicit = method.is_some();
    let endpoint = Endpoint::by_id(method.unwrap_or("status.platformData")).ok_or_else(ApiError::internal)?;
    let params = match endpoint.parse_path(raw_path) {
        Some(p) => p,
        // Named a method: the path must be that route, or the cache key and any
        // archive write would describe a different request.
        None if explicit => {
            return Err(ApiError::new(
                ErrorCode::Validation,
                format!(
                    "path does not match {}'s route {}",
                    endpoint.id, endpoint.path_template
                ),
            ));
        }
        None => vec![raw_path.to_string()],
    };
    let mut req = RiotRequest {
        endpoint,
        target,
        path: raw_path.to_string(),
        params,
        query: Vec::new(),
        // The debug route names its host: never moved to another cluster.
        pick_cluster: false,
    };
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        req = req
            .query(k, Some(v))
            .map_err(|e| ApiError::new(ErrorCode::Validation, e.to_string()))?;
    }
    Ok(req)
}

fn method_query(query: &HashMap<String, String>, required: bool) -> Result<Option<&'static str>, ApiError> {
    let ids: Vec<&'static str> = ENDPOINTS.iter().map(|e| e.id).collect();
    let raw = query.get("method").map(String::as_str);
    if required && raw.is_none() {
        return Err(ApiError::new(
            ErrorCode::Validation,
            "querystring must have required property 'method'",
        ));
    }
    validate::query_one_of("method", raw, &ids)
}

fn scope_and_path(query: &HashMap<String, String>, bounded: bool) -> Result<(String, String), ApiError> {
    let missing = |n: &str| {
        ApiError::new(
            ErrorCode::Validation,
            format!("querystring must have required property '{n}'"),
        )
    };
    let scope = query.get("scope").ok_or_else(|| missing("scope"))?;
    let path = query.get("path").ok_or_else(|| missing("path"))?;
    if bounded {
        let len = |name: &str, v: &str, min: usize, max: usize| {
            let n = v.chars().count();
            if n < min {
                Err(validate::invalid(
                    "querystring",
                    name,
                    &format!("must NOT have fewer than {min} characters"),
                ))
            } else if n > max {
                Err(validate::invalid(
                    "querystring",
                    name,
                    &format!("must NOT have more than {max} characters"),
                ))
            } else {
                Ok(())
            }
        };
        len("scope", scope, 2, 12)?;
        len("path", path, 1, 400)?;
    }
    Ok((scope.clone(), path.clone()))
}

#[utoipa::path(
    get, path = "/v1/admin/debug/riot", tag = "admin",
    summary = "Raw Riot passthrough",
    description = "Fetch any Riot path on a platform or region host, through the limiter and cache like any \
                   read. `method` picks the endpoint whose bucket, TTL and cache key apply, and the path must \
                   then be that endpoint's route; without it, a conservative default bucket is used. \
                   `noCache=true` skips the cache read.",
    params(("scope" = String, Query, description = "Platform or region, e.g. euw1 or europe"),
           ("path" = String, Query, description = "Riot path starting with `/`, query string allowed"),
           ("method" = Option<String>, Query, description = "Endpoint id, e.g. summoner.byPuuid"),
           ("noCache" = Option<bool>, Query, description = "Skip the cache read")),
    responses(PassthroughResponses),
)]
async fn debug_riot(State(state): State<AppState>, Extension(_c): Who, Query(query): Q) -> Response {
    let req = (|| {
        let (scope, path) = scope_and_path(&query, true)?;
        let method = method_query(&query, false)?;
        let no_cache =
            validate::bool_query("noCache", query.get("noCache").map(String::as_str))?.unwrap_or(false);
        Ok::<_, ApiError>((debug_request(&scope, &path, method)?, no_cache))
    })();
    match req {
        Ok((req, bypass)) => respond(
            state
                .fetcher
                .fetch(
                    req,
                    FetchOptions {
                        bypass,
                        ..FetchOptions::default()
                    },
                )
                .await,
        ),
        Err(e) => e.into_response(),
    }
}

/// What the cache holds for one request.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CacheProbe {
    key: String,
    present: bool,
    #[schema(required = true)]
    age_seconds: Option<u64>,
    /// Past its soft TTL (served as `STALE`), or null when absent.
    #[schema(required = true)]
    stale: Option<bool>,
}

#[utoipa::path(
    get, path = "/v1/admin/debug/cache", tag = "admin",
    summary = "Inspect a cache entry",
    description = "What the proxy has cached for a request, without fetching it: the key, whether an entry \
                   is present, its age and whether it is stale.",
    params(("scope" = String, Query, description = "Platform or region, e.g. euw1 or europe"),
           ("path" = String, Query, description = "Riot path starting with `/`"),
           ("method" = String, Query, description = "Endpoint id, e.g. summoner.byPuuid")),
    responses((status = 200, description = "The entry", body = CacheProbe), LocalErrors),
)]
async fn debug_cache(State(state): State<AppState>, Extension(_c): Who, Query(query): Q) -> Response {
    let req = (|| {
        let (scope, path) = scope_and_path(&query, false)?;
        let method = method_query(&query, true)?;
        debug_request(&scope, &path, method)
    })();
    let req = match req {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let key = cache_key(state.fetcher.key_scope(), &req);
    let now = tokio::time::Instant::now();
    let (present, age, stale) = match state.fetcher.cache().l1.get(&key).await {
        Lookup::Fresh(e) => (true, Some(e.age(now)), Some(false)),
        Lookup::Stale(e) => (true, Some(e.age(now)), Some(true)),
        Lookup::Miss => (false, None, None),
    };
    ok(&CacheProbe {
        key,
        present,
        age_seconds: age.map(|d| u64::try_from(d.as_millis().saturating_add(500) / 1000).unwrap_or(u64::MAX)),
        stale,
    })
}

// ── Jobs (P6-08) ────────────────────────────────────────────────────────────

/// A row of the durable queue (design/06).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    /// A ULID.
    id: String,
    kind: String,
    #[schema(required = true)]
    dedupe_key: Option<String>,
    /// Lower runs first (design/06 bands).
    priority: i64,
    /// `pending`, `running`, `done` or `failed`.
    state: String,
    attempts: u32,
    /// Not before this instant; the backoff after a failure.
    run_after: Option<String>,
    #[schema(required = true)]
    claimed_at: Option<String>,
    #[schema(required = true)]
    finished_at: Option<String>,
    /// The last failure, or `cancelled`.
    #[schema(required = true)]
    error: Option<String>,
    #[schema(value_type = Object)]
    payload: serde_json::Value,
}

impl From<crate::jobs::JobRow> for JobSummary {
    fn from(j: crate::jobs::JobRow) -> Self {
        Self {
            id: j.id,
            kind: j.kind,
            dedupe_key: j.dedupe_key,
            priority: j.priority,
            state: j.state,
            attempts: j.attempts,
            run_after: iso_ms(j.run_after),
            claimed_at: j.claimed_at.and_then(iso_ms),
            finished_at: j.finished_at.and_then(iso_ms),
            error: j.error,
            payload: serde_json::from_str(&j.payload).unwrap_or(serde_json::Value::Null),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobList {
    jobs: Vec<JobSummary>,
}

fn job_error(e: crate::jobs::JobAction) -> Response {
    use crate::jobs::JobAction;
    match e {
        JobAction::NotFound => ApiError::not_found("No such job").into_response(),
        JobAction::WrongState { id, state } => {
            ApiError::new(ErrorCode::Validation, format!("Job {id} is {state}")).into_response()
        }
        JobAction::Duplicate(other) => ApiError::new(
            ErrorCode::Validation,
            format!("An identical job is already queued ({other})"),
        )
        .into_response(),
        JobAction::Db(e) => internal(&e, "job action failed"),
    }
}

#[utoipa::path(
    get, path = "/v1/admin/jobs", tag = "admin",
    summary = "List jobs",
    description = "Rows of the durable job queue, newest first. Filter by `state` and `kind`.",
    params(("state" = Option<String>, Query, description = "pending, running, done or failed"),
           ("kind" = Option<String>, Query, description = "e.g. archive:match"),
           ("limit" = Option<i64>, Query, description = "1–500, default 50")),
    responses((status = 200, description = "The jobs", body = JobList), LocalErrors),
)]
async fn list_jobs(State(state): State<AppState>, Extension(_c): Who, Query(query): Q) -> Response {
    let parsed = (|| {
        let st = validate::query_one_of(
            "state",
            query.get("state").map(String::as_str),
            &crate::jobs::scheduler::STATES,
        )?;
        let kind = match query.get("kind") {
            Some(k) if k.chars().count() > 40 => {
                return Err(validate::invalid(
                    "querystring",
                    "kind",
                    "must NOT have more than 40 characters",
                ));
            }
            other => other.cloned(),
        };
        let limit =
            validate::int_query("limit", query.get("limit").map(String::as_str), 1, 500)?.unwrap_or(50);
        Ok::<_, ApiError>((st, kind, u32::try_from(limit).unwrap_or(50)))
    })();
    let (st, kind, limit) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    match state.jobs.list(st, kind, limit).await {
        Ok(rows) => ok(&JobList {
            jobs: rows.into_iter().map(JobSummary::from).collect(),
        }),
        Err(e) => internal(&e, "could not list jobs"),
    }
}

/// Rows per state, for one kind or all of them.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct StateCounts {
    pending: i64,
    running: i64,
    done: i64,
    failed: i64,
}

impl StateCounts {
    fn add(&mut self, state: &str, n: i64) {
        match state {
            "pending" => self.pending += n,
            "running" => self.running += n,
            "done" => self.done += n,
            "failed" => self.failed += n,
            _ => {}
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobStats {
    /// By kind.
    kinds: std::collections::BTreeMap<String, StateCounts>,
    totals: StateCounts,
}

#[utoipa::path(
    get, path = "/v1/admin/jobs/stats", tag = "admin",
    summary = "Job counts",
    description = "Rows of the job queue by kind and state. `done` rows are kept for seven days (design/06).",
    responses((status = 200, description = "The counts", body = JobStats), LocalErrors),
)]
async fn job_stats(State(state): State<AppState>, Extension(_c): Who) -> Response {
    match state.jobs.stats().await {
        Ok(rows) => {
            let mut stats = JobStats {
                kinds: std::collections::BTreeMap::new(),
                totals: StateCounts::default(),
            };
            for (kind, st, n) in rows {
                stats.kinds.entry(kind).or_default().add(&st, n);
                stats.totals.add(&st, n);
            }
            ok(&stats)
        }
        Err(e) => internal(&e, "could not count jobs"),
    }
}

#[utoipa::path(
    post, path = "/v1/admin/jobs/{id}/retry", tag = "admin",
    summary = "Retry a failed job",
    description = "Puts a `failed` job back to `pending` now, with its attempts reset. Refused when the same \
                   work is already queued.",
    params(("id" = String, Path, description = "Job id (a ULID)")),
    responses((status = 200, description = "The job", body = JobSummary), LocalErrors),
)]
async fn retry_job(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    match state.jobs.retry(&id).await {
        Ok(row) => {
            tracing::info!(id = %row.id, kind = %row.kind, "job retried");
            ok(&JobSummary::from(row))
        }
        Err(e) => job_error(e),
    }
}

#[utoipa::path(
    delete, path = "/v1/admin/jobs/{id}", tag = "admin",
    summary = "Cancel a pending job",
    description = "A pending job becomes `failed` with error `cancelled`. A running job cannot be taken back \
                   from its worker.",
    params(("id" = String, Path, description = "Job id (a ULID)")),
    responses((status = 200, description = "The job", body = JobSummary), LocalErrors),
)]
async fn cancel_job(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    match state.jobs.cancel(&id).await {
        Ok(row) => {
            tracing::info!(id = %row.id, kind = %row.kind, "job cancelled");
            ok(&JobSummary::from(row))
        }
        Err(e) => job_error(e),
    }
}

/// `POST /v1/admin/backfill` body (v1).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct QueueBackfill {
    puuid: String,
    /// Platform routing value.
    platform: String,
    /// 1–10 000, default 500.
    limit: Option<u32>,
    /// Default false.
    fetch_timeline: Option<bool>,
}

#[utoipa::path(
    post, path = "/v1/admin/backfill", tag = "admin",
    summary = "Queue a history walk",
    description = "Queues `backfill:player` for one player (v1). One walk per player runs at a time.",
    request_body = QueueBackfill,
    responses((status = 200, description = "`{ok, jobId, status}`", body = serde_json::Value), LocalErrors),
)]
async fn queue_backfill(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["puuid", "platform"])?;
        let puuid = b.with("puuid", |loc, v| {
            validate::puuid_at(loc, v).map(|()| v.to_string())
        })?;
        let platform = b.with("platform", validate::platform_at)?;
        let limit = b.integer("limit", 1, 10_000)?.unwrap_or(500);
        let fetch_timeline = b.boolean("fetchTimeline")?.unwrap_or(false);
        Ok::<_, ApiError>((
            puuid.unwrap_or_default(),
            platform.ok_or_else(ApiError::internal)?,
            limit,
            fetch_timeline,
        ))
    })();
    let (puuid, platform, limit, fetch_timeline) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let walk = crate::jobs::archive::BackfillPlayer {
        puuid,
        platform: platform.as_str().to_string(),
        limit: u32::try_from(limit).unwrap_or(500),
        fetch_timeline: Some(fetch_timeline),
        queue_id: None,
        reason: Some("admin".into()),
    };
    match crate::jobs::archive::enqueue_backfill(&state.jobs, &walk).await {
        Ok(q) => ok(&serde_json::json!({"ok": true, "jobId": q.id, "status": q.status()})),
        Err(e) => internal(&e, "could not queue backfill"),
    }
}

/// `POST /v1/admin/ddragon/sync` body (v1).
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct QueueDdragonSync {
    /// Re-download a patch that is already mirrored. Default false.
    force: Option<bool>,
}

#[utoipa::path(
    post, path = "/v1/admin/ddragon/sync", tag = "admin",
    summary = "Queue a Data Dragon sync",
    description = "Queues `ddragon:sync` (v1). Without `force` it joins the hourly tick's job when one is \
        queued; a `force` sync is queued once beside it.",
    request_body = QueueDdragonSync,
    responses((status = 200, description = "`{ok, jobId}`", body = serde_json::Value), LocalErrors),
)]
async fn queue_ddragon_sync(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    use crate::jobs::{NewJob, kinds, priority};
    let force = match Body::parse(&bytes).and_then(|b| b.boolean("force")) {
        Ok(f) => f.unwrap_or(false),
        Err(e) => return e.into_response(),
    };
    let dedupe = if force {
        "ddragon:sync:force"
    } else {
        kinds::DDRAGON_SYNC
    };
    let job = NewJob::new(
        kinds::DDRAGON_SYNC,
        priority::MAINTENANCE,
        serde_json::to_value(crate::jobs::ddragon::SyncPayload { force }).unwrap_or_default(),
    )
    .dedupe(dedupe);
    match state.jobs.enqueue(job).await {
        Ok(q) => ok(&serde_json::json!({"ok": true, "jobId": q.id})),
        Err(e) => internal(&e, "could not queue ddragon:sync"),
    }
}

// ── Ladder ──────────────────────────────────────────────────────────────────

/// `POST /v1/admin/ladder/crawl` body (v1 `LadderCrawlBody`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct StartLadderCrawl {
    /// Platform routing value. Required: there is no default platform.
    platform: String,
    /// `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`; default the first of `LADDER_QUEUES`.
    queue: Option<String>,
    /// Lowest tier to enumerate, inclusive. Defaults to `LADDER_TIER_FLOOR`. `MASTER` and above is
    /// three requests per queue; `IRON` is the whole ladder, ~15–20 k pages.
    tier_floor: Option<String>,
}

/// A body value from a closed set (ajv `enum`).
fn body_enum(loc: &str, name: &str, value: &str, all: &[&'static str]) -> Result<&'static str, ApiError> {
    all.iter()
        .copied()
        .find(|a| *a == value)
        .ok_or_else(|| validate::invalid(loc, name, "must be equal to one of the allowed values"))
}

/// The queue a crawl uses when none is named (v1).
fn default_ladder_queue(state: &AppState) -> String {
    state
        .config
        .ladder_queues
        .first()
        .cloned()
        .unwrap_or_else(|| "RANKED_SOLO_5x5".into())
}

#[utoipa::path(
    post, path = "/v1/admin/ladder/crawl", tag = "admin",
    summary = "Start a ladder crawl",
    description = "Creates the crawl and fans out its jobs, then answers 202 with the crawl id (v1). \
        One crawl runs per ladder: a second start answers `already-running` with the live crawl's id.",
    request_body = StartLadderCrawl,
    responses((status = 202, description = "`{crawlId, status: started|already-running, platform, queue, legs}`",
        body = serde_json::Value), LocalErrors),
)]
async fn start_ladder_crawl(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    use crate::riot::ladder::RANKED_QUEUES;
    let tiers: Vec<&'static str> = crate::riot::ladder::tiers().collect();
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["platform"])?;
        let platform = b
            .with("platform", validate::platform_at)?
            .ok_or_else(ApiError::internal)?;
        let queue = b.with("queue", |loc, v| body_enum(loc, "queue", v, &RANKED_QUEUES))?;
        let floor = b.with("tierFloor", |loc, v| body_enum(loc, "tierFloor", v, &tiers))?;
        Ok::<_, ApiError>(crate::jobs::ladder::CrawlRequest {
            platform: platform.as_str().to_string(),
            queue: queue.map_or_else(|| default_ladder_queue(&state), str::to_string),
            tier_floor: floor.map(str::to_string),
        })
    })();
    let req = match parsed {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let floor = &state.config.ladder_tier_floor;
    match crate::jobs::ladder::start_crawl(&state.jobs, &scope_of(&state), floor, &req).await {
        Ok(s) => json(
            StatusCode::ACCEPTED,
            &serde_json::json!({
                "crawlId": s.crawl_id,
                // Not a 409: the caller is told which crawl already answers (v1).
                "status": if s.created { "started" } else { "already-running" },
                "platform": s.platform,
                "queue": s.queue,
                "legs": s.legs,
            }),
        ),
        Err(crate::jobs::ladder::StartError::Invalid(e)) => e.into_response(),
        Err(crate::jobs::ladder::StartError::Db(e)) => internal(&e, "could not start the crawl"),
    }
}

#[utoipa::path(
    get, path = "/v1/admin/ladder/options", tag = "admin",
    summary = "Ladders this deployment can crawl, and its configured defaults",
    description = "For the dashboard's start form (v1): every platform with its label, the ranked queues, \
        the tiers ascending, and the `LADDER_*` defaults.",
    responses((status = 200, description = "`{platforms, queues, tiers, defaults}`", body = serde_json::Value), LocalErrors),
)]
async fn ladder_options(State(state): State<AppState>, Extension(_c): Who) -> Response {
    ok(&serde_json::json!({
        "platforms": Platform::ALL.iter().map(|p| serde_json::json!({"id": p.as_str(), "label": p.label()})).collect::<Vec<_>>(),
        "queues": crate::riot::ladder::RANKED_QUEUES,
        "tiers": crate::riot::ladder::tiers().collect::<Vec<_>>(),
        "defaults": {
            // The first scheduled ladder, if any; only preselects the form (ADR-065).
            "platform": state.config.ladder_platforms.first().map(|p| p.as_str()),
            "queue": default_ladder_queue(&state),
            "tierFloor": state.config.ladder_tier_floor,
            "backfillLimit": state.config.ladder_backfill_limit,
        },
    }))
}

/// One crawl in `GET /v1/admin/ladder/crawls` (v1 `LadderCrawlSummary`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LadderCrawlSummary {
    id: String,
    platform: String,
    queue: String,
    /// How far down the ladder this run was told to enumerate.
    tier_floor: String,
    /// `running`, `completed`, `failed` or `cancelled`.
    status: String,
    /// Which stage a running crawl is in. `enumerate` walks the ladder, `collect` gathers every
    /// discovered player's match ids, and `archive` fetches the matches behind them — in that
    /// order, so a match shared by ten players is fetched once.
    phase: String,
    started_at: Option<String>,
    #[schema(required = true)]
    finished_at: Option<String>,
    pages_fetched: i64,
    entries_seen: i64,
    players_discovered: i64,
    /// Players whose match history the collect stage was asked to walk.
    backfills_enqueued: i64,
    /// Distinct matches those players have played, after de-duplication.
    match_ids_seen: i64,
    /// How many of those were not already archived, and so cost a fetch.
    matches_queued: i64,
    /// Legs of the current stage still outstanding — apex leagues and (tier, division) walks,
    /// then match-id batches, then the archive hand-off. 0 for a finished run.
    pending_legs: i64,
    /// Apex tiers whose league Riot returned at its cap of 10,000 entries, e.g. `["MASTER"]`:
    /// Riot lists only the top of those tiers, so the crawl is missing their lowest players.
    /// `[]` when none.
    apex_capped: Vec<String>,
}

impl LadderCrawlSummary {
    pub(crate) fn new(c: crate::jobs::ladder::store::Crawl, pending_legs: i64) -> Self {
        Self {
            started_at: iso_ms(c.started_at),
            finished_at: c.finished_at.and_then(iso_ms),
            pages_fetched: c.counters.pages_fetched,
            entries_seen: c.counters.entries_seen,
            players_discovered: c.counters.players_discovered,
            backfills_enqueued: c.counters.backfills_enqueued,
            match_ids_seen: c.counters.match_ids_seen,
            matches_queued: c.counters.matches_queued,
            id: c.id,
            platform: c.platform,
            queue: c.queue,
            tier_floor: c.tier_floor,
            status: c.status,
            phase: c.phase,
            pending_legs,
            apex_capped: c.apex_capped,
        }
    }
}

#[utoipa::path(
    get, path = "/v1/admin/ladder/crawls", tag = "admin",
    summary = "Recent ladder crawls",
    description = "Newest first, with each crawl's counters and outstanding legs (v1).",
    params(
        ("platform" = Option<String>, Query, description = "Platform routing value"),
        ("queue" = Option<String>, Query, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR"),
        ("limit" = Option<i64>, Query, description = "1–100, default 20"),
    ),
    responses((status = 200, description = "`{crawls}`", body = serde_json::Value), LocalErrors),
)]
async fn list_ladder_crawls(State(state): State<AppState>, Extension(_c): Who, Query(q): Q) -> Response {
    let parsed = (|| {
        let platform = validate::query_platform(q.get("platform").map(String::as_str))?;
        let queue = validate::query_one_of(
            "queue",
            q.get("queue").map(String::as_str),
            &crate::riot::ladder::RANKED_QUEUES,
        )?;
        let limit = validate::int_query("limit", q.get("limit").map(String::as_str), 1, 100)?.unwrap_or(20);
        Ok::<_, ApiError>((platform, queue, limit))
    })();
    let (platform, queue, limit) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    match crate::jobs::ladder::store::list(
        &state.db,
        &scope_of(&state),
        platform.map(|p| p.as_str().to_string()),
        queue.map(str::to_string),
        limit,
    )
    .await
    {
        Ok(rows) => ok(&serde_json::json!({
            "crawls": rows.into_iter().map(|(c, p)| LadderCrawlSummary::new(c, p)).collect::<Vec<_>>(),
        })),
        Err(e) => internal(&e, "could not list crawls"),
    }
}

#[utoipa::path(
    delete, path = "/v1/admin/ladder/crawls/{id}", tag = "admin",
    summary = "Cancel a running ladder crawl",
    description = "Marks the crawl cancelled, drops its queued jobs, and lets the running ones stop at \
        their next status check (v1). A finished crawl cannot be cancelled.",
    params(("id" = String, Path, description = "Crawl id (ULID)")),
    responses((status = 200, description = "`{ok, crawlId, status, droppedJobs}`", body = serde_json::Value), LocalErrors),
)]
async fn cancel_ladder_crawl(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    use crate::jobs::ladder::store::{self, CancelError};
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    match store::cancel(&state.db, &scope_of(&state), &id, now_ms()).await {
        Ok((crawl, dropped)) => {
            tracing::info!(crawl = %id, dropped, "ladder crawl cancelled");
            crate::events::publish(&state.hub, &crate::jobs::ladder::phase_event(&crawl));
            ok(&serde_json::json!({"ok": true, "crawlId": id, "status": "cancelled", "droppedJobs": dropped}))
        }
        Err(CancelError::NotFound) => {
            ApiError::not_found("No such ladder crawl for the current key scope").into_response()
        }
        Err(CancelError::NotRunning { id, status }) => {
            ApiError::new(ErrorCode::Validation, format!("Crawl {id} is already {status}")).into_response()
        }
        Err(CancelError::Db(e)) => internal(&e, "could not cancel the crawl"),
    }
}

// ── Activity: the queue and one crawl's progress (DEV-13) ──────────────────

/// The window "recently" means for pace and ETAs.
const PACE_WINDOW_MS: i64 = 10 * 60_000;
/// Rows per job list in the activity views.
const ACTIVITY_ROWS: u32 = 15;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobQueue {
    /// Running now, oldest claim first.
    running: Vec<JobSummary>,
    /// What the workers claim next, in claim order (`priority`, then `runAfter`, then id).
    next: Vec<JobSummary>,
    /// Pending and due.
    ready: i64,
    /// Pending but waiting out a backoff.
    delayed: i64,
    /// When the soonest delayed job comes due.
    #[schema(required = true)]
    next_delayed_at: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/admin/jobs/queue", tag = "admin",
    summary = "What is running and what runs next",
    description = "The running jobs and the next ones a worker would claim, in the claim order \
        (design/06 §Claiming), with how many are ready and how many wait out a backoff.",
    params(("limit" = Option<i64>, Query, description = "1–100 jobs in `next`, default 15")),
    responses((status = 200, description = "The queue", body = JobQueue), LocalErrors),
)]
async fn job_queue(State(state): State<AppState>, Extension(_c): Who, Query(q): Q) -> Response {
    let limit = match validate::int_query("limit", q.get("limit").map(String::as_str), 1, 100) {
        Ok(l) => u32::try_from(l.unwrap_or(i64::from(ACTIVITY_ROWS))).unwrap_or(ACTIVITY_ROWS),
        Err(e) => return e.into_response(),
    };
    match state.jobs.view(now_ms(), limit).await {
        Ok(v) => ok(&JobQueue {
            running: v.running.into_iter().map(JobSummary::from).collect(),
            next: v.next.into_iter().map(JobSummary::from).collect(),
            ready: v.ready,
            delayed: v.delayed,
            next_delayed_at: v.next_delayed_at.and_then(iso_ms),
        }),
        Err(e) => internal(&e, "could not read the job queue"),
    }
}

/// What each worker is doing (DEV-19).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobActivity {
    /// This process's workers, in order, each with its job's latest step. Empty in a process that
    /// runs no workers (`ROLE=api`): the record is kept in memory where the jobs run.
    workers: Vec<crate::jobs::activity::WorkerView>,
    /// Jobs that finished in this process since it started, newest first.
    finished: Vec<crate::jobs::activity::FinishedView>,
}

#[utoipa::path(
    get, path = "/v1/admin/jobs/activity", tag = "admin",
    summary = "What each worker is doing",
    description = "Every worker of this process with the job it runs and that job's latest step (a Riot call, a \
        rate-limit wait, a page of a walk, …), and the jobs that finished here lately. Kept in memory by the \
        process that runs the workers, for the last 200 jobs (DEV-19).",
    params(("limit" = Option<i64>, Query, description = "1–200 jobs in `finished`, default 50")),
    responses((status = 200, description = "The workers", body = JobActivity), LocalErrors),
)]
async fn job_activity(State(state): State<AppState>, Extension(_c): Who, Query(q): Q) -> Response {
    let limit = match validate::int_query("limit", q.get("limit").map(String::as_str), 1, 200) {
        Ok(l) => usize::try_from(l.unwrap_or(50)).unwrap_or(50),
        Err(e) => return e.into_response(),
    };
    ok(&JobActivity {
        workers: state.activity.workers(),
        finished: state.activity.finished_jobs(limit),
    })
}

/// One job: its row and what its latest run did (DEV-19).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobTrace {
    /// The queue row; `null` once it is gone (done rows are kept seven days).
    #[schema(required = true)]
    job: Option<JobSummary>,
    /// The latest run in this process, from event `after` on; `null` when no worker here has run it
    /// since the process started, or its trace aged out.
    #[schema(required = true)]
    trace: Option<crate::jobs::activity::TraceView>,
}

#[utoipa::path(
    get, path = "/v1/admin/jobs/{id}/activity", tag = "admin",
    summary = "What a job is doing",
    description = "The job's row and the trace of its latest run: when it was claimed and by which worker, each \
        step, each Riot call with how it was answered and how long it took, rate-limit waits and backoffs, and how \
        it ended. Pass `after` (the last `nextSeq`) to get only new events (DEV-19).",
    params(("id" = String, Path, description = "Job id (a ULID)"),
           ("after" = Option<i64>, Query, description = "Only events with `seq` ≥ this, default 0")),
    responses((status = 200, description = "The job", body = JobTrace), LocalErrors),
)]
async fn job_trace(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
    Query(q): Q,
) -> Response {
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    let after = match validate::int_query("after", q.get("after").map(String::as_str), 0, i64::MAX) {
        Ok(a) => u64::try_from(a.unwrap_or(0)).unwrap_or(0),
        Err(e) => return e.into_response(),
    };
    let job = match state.jobs.get(&id).await {
        Ok(row) => row.map(JobSummary::from),
        Err(e) => return internal(&e, "could not read the job"),
    };
    let trace = state.activity.trace(&id, after);
    if job.is_none() && trace.is_none() {
        return ApiError::not_found("No such job").into_response();
    }
    ok(&JobTrace { job, trace })
}

/// One stage of a crawl and how far through it is.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrawlStage {
    /// `enumerate`, `collect` or `archive`.
    name: &'static str,
    /// `done`, `now`, `waiting`, or for a crawl that ended early `stopped` (where it ended) and
    /// `skipped` (never reached).
    state: &'static str,
    /// Units finished; `null` where it cannot be known (a stage a stopped crawl ended in).
    #[schema(required = true)]
    done: Option<i64>,
    /// Units in the stage; `null` until the stage before has ended.
    #[schema(required = true)]
    total: Option<i64>,
    /// `legs` (apex leagues and division walks), `batches` (25 players' match ids) or `ids` (handed
    /// to the archive queue).
    unit: &'static str,
    /// Units this crawl finished in the last ten minutes.
    recent: i64,
    /// Seconds left at that pace; `null` when idle or not running.
    #[schema(required = true)]
    eta_seconds: Option<i64>,
}

/// A walk in flight: a (tier, division) leg and the page it is on.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenWalk {
    /// `ladder:walk:GOLD:II`, `ladder:apex:MASTER`, `ladder:collect:50`, `ladder:archive`.
    leg: String,
    /// A walk's next page; `null` for a leg that has not started paging.
    #[schema(required = true)]
    page: Option<i64>,
}

/// The crawl-found matches being fetched on this crawl's platform. After the hand-off an
/// `archive:match` job only names its match, so these count every crawl on the platform.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrawlDownloads {
    platform: String,
    ready: i64,
    delayed: i64,
    running: i64,
    failed: i64,
    /// Fetched in the last ten minutes.
    recent: i64,
    /// Seconds to fetch what is queued at that pace; `null` when idle.
    #[schema(required = true)]
    eta_seconds: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrawlActivity {
    crawl: LadderCrawlSummary,
    #[schema(required = true)]
    as_of: Option<String>,
    stages: Vec<CrawlStage>,
    /// Legs still outstanding, with a walk's page.
    open_legs: Vec<OpenWalk>,
    /// This crawl's jobs running now.
    running: Vec<JobSummary>,
    /// Its next jobs in claim order: ready first, then any waiting out a backoff.
    next: Vec<JobSummary>,
    /// Ready jobs of any kind a worker takes before this crawl's next one; `null` when it has
    /// nothing ready.
    #[schema(required = true)]
    ahead: Option<i64>,
    /// Its failed jobs, newest first.
    failed: Vec<JobSummary>,
    downloads: CrawlDownloads,
}

/// Seconds to finish `left` units at `recent` units per pace window.
fn eta(left: i64, recent: i64) -> Option<i64> {
    (left > 0 && recent > 0).then(|| left.saturating_mul(PACE_WINDOW_MS / 1000) / recent)
}

const STAGES: [(&str, &str); 3] = [("enumerate", "legs"), ("collect", "batches"), ("archive", "ids")];

fn crawl_stages(a: &crate::jobs::ladder::store::Activity) -> Vec<CrawlStage> {
    use crate::jobs::{kinds, ladder};
    let c = &a.crawl;
    let at = STAGES.iter().position(|(n, _)| *n == c.phase).unwrap_or(0);
    let open = |prefix: &str| {
        i64::try_from(a.legs.iter().filter(|l| l.leg.starts_with(prefix)).count()).unwrap_or(0)
    };
    let recent = |names: &[&str]| -> i64 {
        a.done_recently
            .iter()
            .filter(|(k, _)| names.contains(&k.as_str()))
            .map(|(_, n)| n)
            .sum()
    };
    let batch = i64::try_from(ladder::COLLECT_BATCH).unwrap_or(25);
    let totals = [
        Some(i64::try_from(ladder::enumerate_legs(&c.tier_floor)).unwrap_or(0)),
        (at > 0 || c.status == "completed").then(|| (c.counters.backfills_enqueued + batch - 1) / batch),
        (at > 1 || c.status == "completed").then_some(c.counters.match_ids_seen),
    ];
    let in_flight = [
        open(kinds::LADDER_APEX) + open(kinds::LADDER_WALK),
        open(kinds::LADDER_COLLECT),
        a.ids_in_set,
    ];
    let paces = [
        recent(&[kinds::LADDER_APEX, kinds::LADDER_WALK]),
        recent(&[kinds::LADDER_COLLECT]),
        // The hand-off moves 100 ids a job; ids are the unit worth a rate.
        0,
    ];
    STAGES
        .iter()
        .enumerate()
        .map(|(i, (name, unit))| {
            let state = match c.status.as_str() {
                "completed" => "done",
                "running" if i < at => "done",
                "running" if i == at => "now",
                "running" => "waiting",
                _ if i < at => "done",
                _ if i == at => "stopped",
                _ => "skipped",
            };
            let total = totals[i];
            let done = match state {
                "done" => total,
                "now" => total.map(|t| (t - in_flight[i]).max(0)),
                "waiting" | "skipped" => Some(0),
                _ => None,
            };
            let left = match (state, total, done) {
                ("now", Some(t), Some(d)) => t - d,
                _ => 0,
            };
            CrawlStage {
                name,
                state,
                done,
                total,
                unit,
                recent: paces[i],
                eta_seconds: eta(left, paces[i]),
            }
        })
        .collect()
}

#[utoipa::path(
    get, path = "/v1/admin/ladder/crawls/{id}", tag = "admin",
    summary = "One crawl's progress and jobs",
    description = "Where a crawl is: each stage's progress (legs, match-id batches, ids handed to the \
        archive queue) with pace and ETA over the last ten minutes, the legs in flight with a walk's \
        page, the crawl's running, next and failed jobs, how many queued jobs are ahead of it, and \
        the platform's crawl-found match downloads. Admin views only: it scans the job queue.",
    params(("id" = String, Path, description = "Crawl id (ULID)")),
    responses((status = 200, description = "The crawl's activity", body = CrawlActivity), LocalErrors),
)]
async fn crawl_activity(
    State(state): State<AppState>,
    Extension(_c): Who,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    use crate::jobs::ladder::{order, store};
    let Path(id) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    if let Err(e) = validate::consumer_id(&id) {
        return e.into_response();
    }
    let now = now_ms();
    let activity = match store::activity(
        &state.db,
        &scope_of(&state),
        &id,
        now,
        PACE_WINDOW_MS,
        ACTIVITY_ROWS,
        order::MATCH,
    )
    .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            return ApiError::not_found("No such ladder crawl for the current key scope").into_response();
        }
        Err(e) => return internal(&e, "could not read the crawl's activity"),
    };
    let stages = crawl_stages(&activity);
    let d = &activity.downloads;
    let downloads = CrawlDownloads {
        platform: activity.crawl.platform.clone(),
        ready: d.ready,
        delayed: d.delayed,
        running: d.running,
        failed: d.failed,
        recent: d.done_recently,
        eta_seconds: eta(d.ready + d.delayed + d.running, d.done_recently),
    };
    let pending = i64::try_from(activity.legs.len()).unwrap_or(0);
    let store::Activity {
        crawl,
        legs,
        running,
        next,
        ahead,
        failed,
        ..
    } = activity;
    ok(&CrawlActivity {
        crawl: LadderCrawlSummary::new(crawl, pending),
        as_of: iso_ms(now),
        stages,
        open_legs: legs
            .into_iter()
            .map(|l| OpenWalk {
                leg: l.leg,
                page: l.cursor,
            })
            .collect(),
        running: running.into_iter().map(JobSummary::from).collect(),
        next: next.into_iter().map(JobSummary::from).collect(),
        ahead,
        failed: failed.into_iter().map(JobSummary::from).collect(),
        downloads,
    })
}

#[utoipa::path(
    post, path = "/v1/admin/players/names/backfill", tag = "admin",
    summary = "Fill in player Riot IDs from the archive",
    description = "Reads `riotIdGameName`/`riotIdTagline` out of each nameless player's most recent \
        archived matches. No upstream request is made, and a player who already has a name is left \
        alone — a Riot ID from `account-v1` outranks one from a past game.",
    responses((status = 202, description = "`{ok, unnamed}`: players without a name before the pass runs",
        body = serde_json::Value), LocalErrors),
)]
async fn queue_names_backfill(State(state): State<AppState>, Extension(_c): Who) -> Response {
    let unnamed = match crate::jobs::names::count_unnamed(&state.db, &scope_of(&state)).await {
        Ok(n) => n,
        Err(e) => return internal(&e, "could not count unnamed players"),
    };
    match crate::jobs::names::enqueue(&state.jobs).await {
        Ok(_) => json(
            StatusCode::ACCEPTED,
            &serde_json::json!({"ok": true, "unnamed": unnamed}),
        ),
        Err(e) => internal(&e, "could not queue names:backfill"),
    }
}

// ── Analytics ───────────────────────────────────────────────────────────────

/// `POST /v1/admin/analytics/recompute` body (v1).
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct RecomputeAnalytics {
    /// Platform routing value. Required: there is no default platform.
    platform: String,
    /// `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`; default the first of `LADDER_QUEUES`.
    queue: Option<String>,
}

#[utoipa::path(
    post, path = "/v1/admin/analytics/recompute", tag = "admin",
    summary = "Recompute the analytics tables from the archive",
    description = "Queues `aggregate:analytics` for one ladder and answers 202 — the scan runs on the next free worker, \
        ahead of every queued job (a rebuild already queued for the ladder is moved up), from the matches archived so far. \
        Bounded by `AGGREGATE_PATCH_LIMIT`: only the latest N patches are rebuilt (v1).",
    request_body = RecomputeAnalytics,
    responses((status = 202, description = "`{ok, platform, queue}`", body = serde_json::Value), LocalErrors),
)]
async fn recompute_analytics(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    use crate::riot::ladder::RANKED_QUEUES;
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["platform"])?;
        let platform = b
            .with("platform", validate::platform_at)?
            .ok_or_else(ApiError::internal)?;
        let queue = b.with("queue", |loc, v| body_enum(loc, "queue", v, &RANKED_QUEUES))?;
        Ok::<_, ApiError>((
            platform.as_str().to_string(),
            queue.map_or_else(|| default_ladder_queue(&state), str::to_string),
        ))
    })();
    let (platform, queue) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    match crate::jobs::analytics::enqueue_aggregate_now(&state.jobs, &platform, &queue).await {
        Ok(_) => json(
            StatusCode::ACCEPTED,
            &serde_json::json!({"ok": true, "platform": platform, "queue": queue}),
        ),
        Err(e) => internal(&e, "could not queue aggregate:analytics"),
    }
}

#[utoipa::path(
    post, path = "/v1/admin/analytics/reextract", tag = "admin",
    summary = "Re-derive match facts from the archive",
    description = "Queues `facts:reextract`: every match whose facts an older version derived is re-read from its \
        stored body, in batches of `FACTS_REEXTRACT_BATCH`. No Riot call. A second request joins the sweep \
        already queued. Answers 202 with the number of matches to sweep.",
    responses((status = 202, description = "`{ok, stale}`", body = serde_json::Value), LocalErrors),
)]
async fn reextract_facts(State(state): State<AppState>, Extension(_c): Who) -> Response {
    let stale = match crate::jobs::analytics::stale_matches(&state.db).await {
        Ok(n) => n,
        Err(e) => return internal(&e, "could not count stale facts"),
    };
    match state.jobs.enqueue(crate::jobs::analytics::reextract_job()).await {
        Ok(_) => json(
            StatusCode::ACCEPTED,
            &serde_json::json!({"ok": true, "stale": stale}),
        ),
        Err(e) => internal(&e, "could not queue facts:reextract"),
    }
}

// ── Metrics ─────────────────────────────────────────────────────────────────

#[utoipa::path(
    get, path = "/v1/admin/metrics", tag = "admin",
    summary = "The dashboard's snapshot",
    description = "v1's `MetricsSnapshot`, the same document the `metrics` topic sends: totals, job queues, \
        WebSocket counts, events, cache reads, rate-limit usage, flows, ladder crawls, analytics runs and the \
        process. Counters are cumulative since the process started.",
    responses((status = 200, description = "The snapshot", body = crate::stats::Snapshot), LocalErrors),
)]
async fn metrics_snapshot(State(state): State<AppState>, Extension(_c): Who) -> Response {
    match state.stats.snapshot().await {
        Ok(s) => ok(&s),
        Err(e) => internal(&e, "could not build the metrics snapshot"),
    }
}

/// `GET /v1/admin/metrics/history` (v1 `MetricsHistoryResponse`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsHistory {
    /// `METRICS_HISTORY_INTERVAL_S`.
    interval_s: u32,
    max_points: i64,
    /// Oldest first.
    points: Vec<crate::stats::HistoryPoint>,
}

#[utoipa::path(
    get, path = "/v1/admin/metrics/history", tag = "admin",
    summary = "The dashboard's history",
    description = "A point every `METRICS_HISTORY_INTERVAL_S`, the newest 1440 kept (24 hours at 60 s), oldest \
        first (v1).",
    responses((status = 200, description = "`{intervalS, maxPoints, points}`", body = MetricsHistory), LocalErrors),
)]
async fn metrics_history(State(state): State<AppState>, Extension(_c): Who) -> Response {
    match state.stats.history().await {
        Ok(points) => ok(&MetricsHistory {
            interval_s: state.config.metrics_history_interval_s,
            max_points: crate::stats::HISTORY_MAX_POINTS,
            points,
        }),
        Err(e) => internal(&e, "could not read the metrics history"),
    }
}

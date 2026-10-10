//! `/v1/riot/*`: account-v1, passed through (v1 `routes/riot.ts`). FR-2: the
//! canonical way to resolve a player; the deprecated summoner-by-name is absent.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::http::auth::Consumer;
use crate::http::{ApiError, ErrorCode, validate};
use crate::riot::client::RiotRequest;
use crate::riot::endpoints::Endpoint;
use crate::routes::passthrough::{PassthroughResponses, options, respond};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(account_by_riot_id))
        .routes(routes!(account_by_puuid))
        .routes(routes!(account_by_riot_id_any))
        .routes(routes!(account_by_puuid_any))
        .routes(routes!(account_region_by_puuid))
}

/// Malformed path segments (e.g. invalid UTF-8) in the envelope, not axum's text.
pub fn bad_path(e: &PathRejection) -> ApiError {
    ApiError::new(ErrorCode::Validation, format!("params {}", e.body_text()))
}

fn account(id: &str, region: &str, params: &[&str]) -> Result<RiotRequest, ApiError> {
    let region = validate::region(region)?;
    let ep = Endpoint::by_id(id).ok_or_else(ApiError::internal)?;
    // `sea` is a valid region here; account-v1 has no sea host, so it goes to asia.
    let target = ep.target_for_region(region).ok_or_else(ApiError::internal)?;
    RiotRequest::new(ep, target, params).map_err(|_| ApiError::internal())
}

/// account-v1 on whichever cluster has room (ADR-066).
fn any_cluster(id: &str, params: &[&str]) -> Result<RiotRequest, ApiError> {
    let ep = Endpoint::by_id(id).ok_or_else(ApiError::internal)?;
    RiotRequest::account(ep, params).map_err(|_| ApiError::internal())
}

const PICKED: &str = "Riot serves every account from each of its account-v1 clusters, so the proxy picks one: \
                      `asia`, then `americas`, then `europe`. It takes the first whose rate limits have room, \
                      and moves on when Riot answers 429. One cached copy serves this route and the \
                      region-pinned one.";

#[utoipa::path(
    get,
    path = "/v1/riot/accounts/by-riot-id/{gameName}/{tagLine}",
    tag = "riot",
    summary = "Account by Riot ID, any cluster",
    description = PICKED,
    params(
        ("gameName" = String, Path, description = "The part of a Riot ID before the `#` (1–16 characters)"),
        ("tagLine" = String, Path, description = "The part of a Riot ID after the `#` (1–5 characters)"),
    ),
    responses(PassthroughResponses),
)]
async fn account_by_riot_id_any(
    State(state): State<AppState>,
    Extension(consumer): Extension<Arc<Consumer>>,
    path: Result<Path<(String, String)>, PathRejection>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Path((game_name, tag_line)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let req = match validate::game_name(&game_name)
        .and_then(|()| validate::tag_line(&tag_line))
        .and_then(|()| any_cluster("account.byRiotId", &[&game_name, &tag_line]))
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    respond(state.fetcher.fetch(req, options(&consumer, &query)).await)
}

#[utoipa::path(
    get,
    path = "/v1/riot/accounts/by-puuid/{puuid}",
    tag = "riot",
    summary = "Account by PUUID, any cluster",
    description = PICKED,
    params(("puuid" = String, Path, description = "Encrypted player UUID (60–128 characters, [A-Za-z0-9_-])")),
    responses(PassthroughResponses),
)]
async fn account_by_puuid_any(
    State(state): State<AppState>,
    Extension(consumer): Extension<Arc<Consumer>>,
    path: Result<Path<String>, PathRejection>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let req = match validate::puuid(&puuid).and_then(|()| any_cluster("account.byPuuid", &[&puuid])) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    respond(state.fetcher.fetch(req, options(&consumer, &query)).await)
}

#[utoipa::path(
    get,
    path = "/v1/riot/accounts/region/by-puuid/{puuid}",
    tag = "riot",
    summary = "A player's League of Legends platform, any cluster",
    description = "account-v1's active region for `lol`: Riot's `AccountRegionDTO`, unmodified, whose \
                   `region` is the platform routing value the player plays on (`oc1`, `kr`). A Riot ID \
                   belongs to one platform, so this is how to route its summoner, league, mastery and \
                   match calls without asking. The cluster is picked as for the other account routes.",
    params(("puuid" = String, Path, description = "Encrypted player UUID (60–128 characters, [A-Za-z0-9_-])")),
    responses(PassthroughResponses),
)]
async fn account_region_by_puuid(
    State(state): State<AppState>,
    Extension(consumer): Extension<Arc<Consumer>>,
    path: Result<Path<String>, PathRejection>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let req =
        match validate::puuid(&puuid).and_then(|()| any_cluster("account.regionByPuuid", &["lol", &puuid])) {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };
    respond(state.fetcher.fetch(req, options(&consumer, &query)).await)
}

#[utoipa::path(
    get,
    path = "/v1/riot/accounts/by-riot-id/{region}/{gameName}/{tagLine}",
    tag = "riot",
    summary = "Account by Riot ID",
    description = "account-v1 by Riot ID. The body is Riot's, unmodified. `sea` is accepted and routed \
                   to `asia`, since account-v1 has no `sea` host.",
    params(
        ("region" = String, Path, description = "Regional routing value: americas, europe, asia or sea"),
        ("gameName" = String, Path, description = "The part of a Riot ID before the `#` (1–16 characters)"),
        ("tagLine" = String, Path, description = "The part of a Riot ID after the `#` (1–5 characters)"),
    ),
    responses(PassthroughResponses),
)]
async fn account_by_riot_id(
    State(state): State<AppState>,
    Extension(consumer): Extension<Arc<Consumer>>,
    path: Result<Path<(String, String, String)>, PathRejection>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Path((region, game_name, tag_line)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let req = match validate::game_name(&game_name)
        .and_then(|()| validate::tag_line(&tag_line))
        .and_then(|()| account("account.byRiotId", &region, &[&game_name, &tag_line]))
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    respond(state.fetcher.fetch(req, options(&consumer, &query)).await)
}

#[utoipa::path(
    get,
    path = "/v1/riot/accounts/by-puuid/{region}/{puuid}",
    tag = "riot",
    summary = "Account by PUUID",
    description = "account-v1 by PUUID. The body is Riot's, unmodified. PUUIDs are encrypted per API key.",
    params(
        ("region" = String, Path, description = "Regional routing value: americas, europe, asia or sea"),
        ("puuid" = String, Path, description = "Encrypted player UUID (60–128 characters, [A-Za-z0-9_-])"),
    ),
    responses(PassthroughResponses),
)]
async fn account_by_puuid(
    State(state): State<AppState>,
    Extension(consumer): Extension<Arc<Consumer>>,
    path: Result<Path<(String, String)>, PathRejection>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Path((region, puuid)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let req = match validate::puuid(&puuid).and_then(|()| account("account.byPuuid", &region, &[&puuid])) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    respond(state.fetcher.fetch(req, options(&consumer, &query)).await)
}

//! `/v1/lol/*`: League of Legends endpoints (v1 `routes/lol.ts`). Riot's bytes are
//! passed through unmodified, as v1 did (`PassthroughResponse`); what is typed is
//! the request: v1's parameter and query rules, validated before any upstream call.
//!
//! v1's three `/v1/lol/analytics/*` routes read the analytics tables and arrive
//! with them in P7-04 (owner decision, ADR-037).

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
use crate::http::{ApiError, validate};
use crate::riot::endpoints::{Endpoint, Target};
use crate::riot::ladder::{APEX_TIERS, DIVISIONS, PAGED_TIERS, RANKED_QUEUES, apex_endpoint};
use crate::riot::routing::{Platform, Region};
use crate::routes::passthrough::{PassthroughResponses, fetch, request};
use crate::routes::riot::bad_path;

type Q = Query<HashMap<String, String>>;
type Who = Extension<Arc<Consumer>>;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(summoner_by_puuid))
        .routes(routes!(league_entries_by_puuid))
        .routes(routes!(league_apex))
        .routes(routes!(league_entries_page))
        .routes(routes!(match_ids))
        .routes(routes!(match_by_id))
        .routes(routes!(match_timeline))
        .routes(routes!(spectator_active))
        .routes(routes!(mastery_by_puuid))
        .routes(routes!(rotations))
        .routes(routes!(platform_status))
}

fn on_platform(id: &str, platform: Platform) -> Option<Target> {
    Endpoint::by_id(id).map(|e| e.target_for_platform(platform))
}

fn on_region(id: &str, region: Region) -> Option<Target> {
    Endpoint::by_id(id).and_then(|e| e.target_for_region(region))
}

/// v1 `resolveMatchRegion`: a match id's platform prefix must agree with the path.
fn match_region(region: &str, match_id: &str) -> Result<Region, ApiError> {
    let declared = validate::region(region)?;
    validate::match_id(match_id)?;
    match Platform::from_match_id(match_id).map(Platform::region) {
        Some(derived) if derived != declared => Err(ApiError::bad_region(format!(
            "Match {match_id} belongs to region '{derived}', not '{declared}'"
        ))),
        Some(derived) => Ok(derived),
        None => Ok(declared),
    }
}

fn q<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query.get(name).map(String::as_str)
}

/// A platform-and-PUUID route: the most common shape.
async fn platform_puuid(
    state: AppState,
    consumer: Arc<Consumer>,
    query: HashMap<String, String>,
    path: Result<Path<(String, String)>, PathRejection>,
    id: &'static str,
) -> Response {
    let Path((platform, puuid)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    fetch(&state, &consumer, &query, || {
        let platform = validate::platform(&platform)?;
        validate::puuid(&puuid)?;
        request(id, on_platform(id, platform), &[&puuid], &[])
    })
    .await
}

#[utoipa::path(
    get, path = "/v1/lol/summoners/by-puuid/{platform}/{puuid}", tag = "lol",
    summary = "Summoner by PUUID", description = "summoner-v4, passed through.",
    params(("platform" = String, Path, description = "Platform routing value, e.g. euw1"),
           ("puuid" = String, Path, description = "Encrypted player UUID")),
    responses(PassthroughResponses),
)]
async fn summoner_by_puuid(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    platform_puuid(s, c, q, path, "summoner.byPuuid").await
}

#[utoipa::path(
    get, path = "/v1/lol/league/entries/by-puuid/{platform}/{puuid}", tag = "lol",
    summary = "Ranked entries by PUUID", description = "league-v4 entries for one player, passed through.",
    params(("platform" = String, Path, description = "Platform routing value"),
           ("puuid" = String, Path, description = "Encrypted player UUID")),
    responses(PassthroughResponses),
)]
async fn league_entries_by_puuid(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    platform_puuid(s, c, q, path, "league.entriesByPuuid").await
}

#[utoipa::path(
    get, path = "/v1/lol/league/apex/{platform}/{tier}/{queue}", tag = "lol",
    summary = "Read a whole apex league",
    description = "MASTER, GRANDMASTER or CHALLENGER for one queue. One request returns every entry; not paged.",
    params(("platform" = String, Path, description = "Platform routing value"),
           ("tier" = String, Path, description = "MASTER, GRANDMASTER or CHALLENGER"),
           ("queue" = String, Path, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR")),
    responses(PassthroughResponses),
)]
async fn league_apex(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String, String)>, PathRejection>,
) -> Response {
    let Path((platform, tier, queue)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    fetch(&s, &c, &q, || {
        let platform = validate::platform(&platform)?;
        let tier = validate::one_of_str("tier", &tier, &APEX_TIERS)?;
        let queue = validate::one_of_str("queue", &queue, &RANKED_QUEUES)?;
        let id = apex_endpoint(tier).ok_or_else(ApiError::internal)?;
        request(id, on_platform(id, platform), &[queue], &[])
    })
    .await
}

#[utoipa::path(
    get, path = "/v1/lol/league/entries/{platform}/{queue}/{tier}/{division}", tag = "lol",
    summary = "Walk one page of a tier and division",
    description = "One ~205-entry page of the ladder. Pages are 1-based; a page past the end of the division \
                   returns an empty array rather than a 404.",
    params(("platform" = String, Path, description = "Platform routing value"),
           ("queue" = String, Path, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR"),
           ("tier" = String, Path, description = "IRON … DIAMOND (apex tiers have their own route)"),
           ("division" = String, Path, description = "I … IV"),
           ("page" = Option<i64>, Query, description = "1-based page, default 1 (max 100000)")),
    responses(PassthroughResponses),
)]
async fn league_entries_page(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String, String, String)>, PathRejection>,
) -> Response {
    let Path((platform, queue, tier, division)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let page = self::q(&q, "page").map(str::to_string);
    fetch(&s, &c, &q, || {
        let platform = validate::platform(&platform)?;
        let queue = validate::one_of_str("queue", &queue, &RANKED_QUEUES)?;
        let tier = validate::one_of_str("tier", &tier, &PAGED_TIERS)?;
        let division = validate::one_of_str("division", &division, &DIVISIONS)?;
        // v1 defaulted page to 1, so it is always sent.
        let page = validate::int_query("page", page.as_deref(), 1, 100_000)?.unwrap_or(1);
        let id = "league.entriesByTier";
        request(
            id,
            on_platform(id, platform),
            &[queue, tier, division],
            &[("page", Some(page.to_string()))],
        )
    })
    .await
}

#[utoipa::path(
    get, path = "/v1/lol/matches/ids/{region}/{puuid}", tag = "lol",
    summary = "Match ids by PUUID", description = "match-v5 id list, passed through. `start` defaults to 0 and \
                   `count` to 20 (1–100).",
    params(("region" = String, Path, description = "Regional routing value"),
           ("puuid" = String, Path, description = "Encrypted player UUID"),
           ("start" = Option<i64>, Query, description = "0–10000, default 0"),
           ("count" = Option<i64>, Query, description = "1–100, default 20"),
           ("queue" = Option<i64>, Query, description = "Queue id, 0–5000"),
           ("type" = Option<String>, Query, description = "ranked, normal, tourney or tutorial"),
           ("startTime" = Option<i64>, Query, description = "Epoch seconds, ≥ 0"),
           ("endTime" = Option<i64>, Query, description = "Epoch seconds, ≥ 0")),
    responses(PassthroughResponses),
)]
async fn match_ids(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let Path((region, puuid)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    fetch(&s, &c, &q, || {
        let region = validate::region(&region)?;
        validate::puuid(&puuid)?;
        // v1 applied these defaults, so start and count are always sent.
        let start = validate::int_query("start", self::q(&q, "start"), 0, 10_000)?.unwrap_or(0);
        let count = validate::int_query("count", self::q(&q, "count"), 1, 100)?.unwrap_or(20);
        let queue = validate::int_query("queue", self::q(&q, "queue"), 0, 5000)?;
        let kind = validate::query_one_of(
            "type",
            self::q(&q, "type"),
            &["ranked", "normal", "tourney", "tutorial"],
        )?;
        let start_time = validate::int_query("startTime", self::q(&q, "startTime"), 0, i64::MAX)?;
        let end_time = validate::int_query("endTime", self::q(&q, "endTime"), 0, i64::MAX)?;
        let id = "match.idsByPuuid";
        request(
            id,
            on_region(id, region),
            &[&puuid],
            &[
                ("start", Some(start.to_string())),
                ("count", Some(count.to_string())),
                ("queue", queue.map(|v| v.to_string())),
                ("type", kind.map(str::to_string)),
                ("startTime", start_time.map(|v| v.to_string())),
                ("endTime", end_time.map(|v| v.to_string())),
            ],
        )
    })
    .await
}

async fn match_route(
    s: AppState,
    c: Arc<Consumer>,
    q: HashMap<String, String>,
    path: Result<Path<(String, String)>, PathRejection>,
    id: &'static str,
) -> Response {
    let Path((region, match_id)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    fetch(&s, &c, &q, || {
        let region = match_region(&region, &match_id)?;
        request(id, on_region(id, region), &[&match_id], &[])
    })
    .await
}

#[utoipa::path(
    get, path = "/v1/lol/matches/{region}/{matchId}", tag = "lol",
    summary = "Match by id",
    description = "match-v5, passed through. Matches are immutable and served from the archive once seen \
                   (`X-Cache: ARCHIVE`). The match id's platform must belong to `region`.",
    params(("region" = String, Path, description = "Regional routing value"),
           ("matchId" = String, Path, description = "Platform-prefixed id, e.g. EUW1_7381937461")),
    responses(PassthroughResponses),
)]
async fn match_by_id(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    match_route(s, c, q, path, "match.byId").await
}

#[utoipa::path(
    get, path = "/v1/lol/matches/{region}/{matchId}/timeline", tag = "lol",
    summary = "Match timeline",
    description = "match-v5 timeline, passed through. Immutable, like the match.",
    params(("region" = String, Path, description = "Regional routing value"),
           ("matchId" = String, Path, description = "Platform-prefixed id")),
    responses(PassthroughResponses),
)]
async fn match_timeline(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    match_route(s, c, q, path, "match.timeline").await
}

#[utoipa::path(
    get, path = "/v1/lol/spectator/active/{platform}/{puuid}", tag = "lol",
    summary = "Active game", description = "spectator-v5, passed through. Not in a game is a 404, negatively \
                   cached briefly.",
    params(("platform" = String, Path, description = "Platform routing value"),
           ("puuid" = String, Path, description = "Encrypted player UUID")),
    responses(PassthroughResponses),
)]
async fn spectator_active(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    platform_puuid(s, c, q, path, "spectator.activeGame").await
}

#[utoipa::path(
    get, path = "/v1/lol/mastery/by-puuid/{platform}/{puuid}", tag = "lol",
    summary = "Champion mastery", description = "champion-mastery-v4, passed through. `top=N` returns the top N.",
    params(("platform" = String, Path, description = "Platform routing value"),
           ("puuid" = String, Path, description = "Encrypted player UUID"),
           ("top" = Option<i64>, Query, description = "1–200: only the top N champions")),
    responses(PassthroughResponses),
)]
async fn mastery_by_puuid(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let Path((platform, puuid)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let top = self::q(&q, "top").map(str::to_string);
    fetch(&s, &c, &q, || {
        let platform = validate::platform(&platform)?;
        validate::puuid(&puuid)?;
        match validate::int_query("top", top.as_deref(), 1, 200)? {
            Some(n) => {
                let id = "mastery.topByPuuid";
                request(
                    id,
                    on_platform(id, platform),
                    &[&puuid],
                    &[("count", Some(n.to_string()))],
                )
            }
            None => request(
                "mastery.byPuuid",
                on_platform("mastery.byPuuid", platform),
                &[&puuid],
                &[],
            ),
        }
    })
    .await
}

async fn platform_only(
    s: AppState,
    c: Arc<Consumer>,
    q: HashMap<String, String>,
    path: Result<Path<String>, PathRejection>,
    id: &'static str,
) -> Response {
    let Path(platform) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    fetch(&s, &c, &q, || {
        let platform = validate::platform(&platform)?;
        request(id, on_platform(id, platform), &[], &[])
    })
    .await
}

#[utoipa::path(
    get, path = "/v1/lol/rotations/{platform}", tag = "lol",
    summary = "Free champion rotation", description = "champion-v3 rotations, passed through.",
    params(("platform" = String, Path, description = "Platform routing value")),
    responses(PassthroughResponses),
)]
async fn rotations(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    platform_only(s, c, q, path, "platform.championRotations").await
}

#[utoipa::path(
    get, path = "/v1/lol/status/{platform}", tag = "lol",
    summary = "Platform status", description = "lol-status-v4 platform data, passed through.",
    params(("platform" = String, Path, description = "Platform routing value")),
    responses(PassthroughResponses),
)]
async fn platform_status(
    State(s): State<AppState>,
    Extension(c): Who,
    Query(q): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    platform_only(s, c, q, path, "status.platformData").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ErrorCode;

    #[test]
    fn match_region_must_agree_with_the_match_id() {
        assert_eq!(match_region("europe", "EUW1_1").unwrap(), Region::Europe);
        assert_eq!(match_region("sea", "OC1_123456").unwrap(), Region::Sea);
        let e = match_region("americas", "EUW1_123456").unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRegion);
        assert_eq!(
            e.message,
            "Match EUW1_123456 belongs to region 'europe', not 'americas'"
        );
        // An unknown prefix falls back to the declared region (v1).
        assert_eq!(match_region("asia", "XX9_123456").unwrap(), Region::Asia);
    }
}

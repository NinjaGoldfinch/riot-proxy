//! `/v1/lol/analytics/*` (plan P7-04; v1 `routes/lol.ts`): champion stats,
//! lane matchups and the champion detail composite, read from the analytics
//! tables (`archive::analytics`). Never calls Riot.
//!
//! As v1: the newest aggregated patch by default, `Cache-Control: private,
//! max-age=300`, and a weak `ETag` from the rows' `computed_at`, the canonical
//! query and the mirrored Data Dragon version (names come from it); a matching
//! `If-None-Match` (or `*`) gets 304. v2 adds `?remakes=exclude|include`
//! (default exclude, ADR-056), which is part of the `ETag` too.

use std::collections::HashMap;

use axum::Extension;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::Serialize;
use sha2::Digest;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::archive::analytics::{self, Facet, FacetRow, PatchRow, Read, StatRow};
use crate::clock::iso_ms;
use crate::db::DbError;
use crate::http::{ApiError, validate};
use crate::riot::ladder::RANKED_QUEUES;
use crate::routes::passthrough::{JSON, LocalErrors};
use crate::routes::players::{js_number, js_opt_number, round4};
use crate::routes::riot::bad_path;

type Q = Query<HashMap<String, String>>;
type Who = Extension<std::sync::Arc<crate::http::auth::Consumer>>;

/// v1 `ANALYTICS_CACHE_CONTROL`.
pub const CACHE_CONTROL: &str = "private, max-age=300";
/// v1 `TEAM_POSITIONS`: the lanes, and `''` for queues without positions.
const TEAM_POSITIONS: [&str; 6] = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY", ""];
/// v1 `LANE_POSITIONS`: a matchup is always in a lane.
const LANE_POSITIONS: [&str; 5] = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(champions))
        .routes(routes!(champion_detail))
        .routes(routes!(champion_matchups))
        .routes(routes!(patches))
}

/// One champion at one tier (v1 `ChampionStatEntry`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionStatEntry {
    champion_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    champion_name: Option<String>,
    tier: String,
    patch: String,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
    /// Of the games in the rows returned.
    #[serde(serialize_with = "js_number")]
    share: f64,
    /// Matches picked over the tier's matches, capped at 1. Absent without a slice.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    pick_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    ban_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    avg_kda: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    avg_damage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    avg_vision: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    cs_per_min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    gold_per_min: Option<f64>,
}

/// v1 `ChampionStatsResponse`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionStatsResponse {
    /// `null` when the request named no platform: every platform summed.
    #[schema(required = true)]
    platform: Option<String>,
    queue: String,
    #[schema(required = true)]
    tier: Option<String>,
    #[schema(required = true)]
    patch: Option<String>,
    #[schema(required = true)]
    role: Option<String>,
    /// The newest row's recompute time.
    #[schema(required = true)]
    computed_at: Option<String>,
    total_games: i64,
    champions: Vec<ChampionStatEntry>,
}

/// v1 `ChampionMatchupEntry`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionMatchupEntry {
    role: String,
    opponent_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    opponent_name: Option<String>,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
}

/// v1 `ChampionMatchupsResponse`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionMatchupsResponse {
    champion_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    champion_name: Option<String>,
    /// `null` when the request named no platform: every platform summed.
    #[schema(required = true)]
    platform: Option<String>,
    queue: String,
    #[schema(required = true)]
    patch: Option<String>,
    #[schema(required = true)]
    role: Option<String>,
    #[schema(required = true)]
    computed_at: Option<String>,
    matchups: Vec<ChampionMatchupEntry>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionItemEntry {
    item_id: i64,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionRuneEntry {
    keystone_id: i64,
    sub_style_id: i64,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionSpellEntry {
    spell_a: i64,
    spell_b: i64,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
}

/// Each section's recompute time: they are rebuilt in separate transactions.
#[derive(Debug, Serialize, ToSchema)]
pub struct SectionsComputedAt {
    #[schema(required = true)]
    stats: Option<String>,
    #[schema(required = true)]
    matchups: Option<String>,
    #[schema(required = true)]
    items: Option<String>,
    #[schema(required = true)]
    runes: Option<String>,
    #[schema(required = true)]
    spells: Option<String>,
}

/// v1 `ChampionDetailResponse`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChampionDetailResponse {
    champion_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    champion_name: Option<String>,
    /// `null` when the request named no platform: every platform summed.
    #[schema(required = true)]
    platform: Option<String>,
    queue: String,
    #[schema(required = true)]
    tier: Option<String>,
    #[schema(required = true)]
    patch: Option<String>,
    #[schema(required = true)]
    role: Option<String>,
    /// The oldest section's recompute time: how fresh the whole document is.
    #[schema(required = true)]
    computed_at: Option<String>,
    sections_computed_at: SectionsComputedAt,
    total_games: i64,
    stats: Vec<ChampionStatEntry>,
    matchups: Vec<ChampionMatchupEntry>,
    items: Vec<ChampionItemEntry>,
    runes: Vec<ChampionRuneEntry>,
    spells: Vec<ChampionSpellEntry>,
}

/// One aggregated patch (ADR-094).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsPatchEntry {
    /// `major.minor`.
    patch: String,
    /// Participant games, as `totalGames` counts them on the champions route;
    /// with `championId`, that champion's games.
    games: i64,
    #[schema(required = true)]
    computed_at: Option<String>,
}

/// The patches a ladder has analytics for (ADR-094).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsPatchesResponse {
    /// `null` when the request named no platform: every platform summed.
    #[schema(required = true)]
    platform: Option<String>,
    queue: String,
    /// The champion asked for, whose games each entry then counts; `null` for every champion.
    #[schema(required = true)]
    champion_id: Option<i64>,
    /// Newest first.
    patches: Vec<AnalyticsPatchEntry>,
}

/// `?patch=all`: every aggregated patch summed (ADR-094).
const ALL_PATCHES: &str = "all";

/// What every analytics query names, validated in v1's property order.
struct Common {
    /// `None`: every platform (ADR-065).
    platform: Option<String>,
    queue: String,
    tier: Option<String>,
    /// A `major.minor`, or [`ALL_PATCHES`].
    patch: Option<String>,
    role: Option<String>,
    /// As the caller sent it (the ETag's `query.minGames`).
    min_games: Option<i64>,
    limit: i64,
    remakes: bool,
}

fn q<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query.get(name).map(String::as_str)
}

/// `tier` is `None` for the matchups route, which has none; `roles` and the
/// limit bounds differ by route (v1's three query schemas).
fn common(
    state: &AppState,
    query: &HashMap<String, String>,
    tier: bool,
    roles: &[&'static str],
    limit: (i64, i64, i64),
) -> Result<Common, ApiError> {
    let ladder = ladder_query(state, query)?;
    let tiers: Vec<&'static str> = crate::riot::ladder::tiers()
        .chain([analytics::UNKNOWN_TIER])
        .collect();
    let tier = if tier {
        validate::query_one_of("tier", q(query, "tier"), &tiers)?
    } else {
        None
    };
    // `all` sums every patch (ADR-094); anything else is a `major.minor`.
    let patch = match q(query, "patch") {
        Some(ALL_PATCHES) => Some(ALL_PATCHES),
        raw => validate::patch_query(raw)?,
    };
    let role = validate::query_one_of("role", q(query, "role"), roles)?;
    let min_games = validate::int_query("minGames", q(query, "minGames"), 0, i64::MAX)?;
    let (min, max, default) = limit;
    let limit = validate::int_query("limit", q(query, "limit"), min, max)?.unwrap_or(default);
    let remakes = validate::remakes_query(q(query, "remakes"))?;
    Ok(Common {
        platform: ladder.platform,
        queue: ladder.queue,
        tier: tier.map(str::to_string),
        patch: patch.map(str::to_string),
        role: role.map(str::to_string),
        min_games,
        limit,
        remakes,
    })
}

/// The ladder a query names: platform (`None`: every platform) and queue
/// (default the first of `LADDER_QUEUES`), validated in that order (v1).
struct LadderQuery {
    platform: Option<String>,
    queue: String,
}

fn ladder_query(state: &AppState, query: &HashMap<String, String>) -> Result<LadderQuery, ApiError> {
    let platform = validate::query_platform(q(query, "platform"))?;
    let queue = validate::query_one_of("queue", q(query, "queue"), &RANKED_QUEUES)?;
    Ok(LadderQuery {
        platform: platform.map(|p| p.as_str().to_string()),
        queue: queue.map_or_else(
            || {
                state
                    .config
                    .ladder_queues
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "RANKED_SOLO_5x5".into())
            },
            str::to_string,
        ),
    })
}

/// `params/championId`: an integer ≥ 1 (v1 `ChampionIdParam`).
fn champion_id(raw: &str) -> Result<i64, ApiError> {
    let n: i64 = raw
        .parse()
        .map_err(|_| validate::invalid("params", "championId", "must be integer"))?;
    if n < 1 {
        return Err(validate::invalid("params", "championId", "must be >= 1"));
    }
    Ok(n)
}

/// v1 `analyticsEtag`'s material: the parts joined with `|`, a missing part
/// as `~`, hashed and base64url-encoded into a weak validator. v1 used SHA-1;
/// v2 uses the SHA-256 it already has. An ETag is opaque to clients.
fn etag(parts: &[Option<String>]) -> String {
    let material = parts
        .iter()
        .map(|p| p.as_deref().unwrap_or("~"))
        .collect::<Vec<_>>()
        .join("|");
    let digest = sha2::Sha256::digest(material.as_bytes());
    format!(
        "W/\"{}\"",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
}

/// v1 `notModified`: `If-None-Match` is a list, and `*` matches anything.
fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').map(str::trim).any(|t| t == etag || t == "*"))
}

/// The document, or 304, with the ETag and v1's cache header either way.
fn respond<T: Serialize>(headers: &HeaderMap, etag: &str, body: &T) -> Response {
    let mut res = if not_modified(headers, etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        match serde_json::to_vec(body) {
            Ok(b) => (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], b).into_response(),
            Err(_) => return ApiError::internal().into_response(),
        }
    };
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    res
}

fn some<T: ToString>(v: T) -> Option<String> {
    Some(v.to_string())
}

#[allow(clippy::cast_precision_loss)]
fn rate(n: i64, d: i64) -> f64 {
    if d == 0 { 0.0 } else { round4(n as f64 / d as f64) }
}

/// v1 `enrichChampionStats`: rates, averages and shares over exactly `rows`.
#[allow(clippy::cast_precision_loss)]
fn enrich(
    rows: Vec<StatRow>,
    slices: &[(String, i64)],
    bans: &[(String, i64, i64)],
    names: &HashMap<i64, String>,
) -> (i64, Vec<ChampionStatEntry>) {
    let slice: HashMap<&str, i64> = slices.iter().map(|(t, m)| (t.as_str(), *m)).collect();
    let banned: HashMap<(&str, i64), i64> = bans.iter().map(|(t, c, b)| ((t.as_str(), *c), *b)).collect();
    let total: i64 = rows.iter().map(|r| r.games).sum();
    let entries = rows
        .into_iter()
        .map(|r| {
            let matches = slice.get(r.tier.as_str()).copied().filter(|m| *m > 0);
            // No ban row in a slice that exists is a computed zero (v1).
            let bans = banned
                .get(&(r.tier.as_str(), r.champion_id))
                .copied()
                .unwrap_or(0);
            let minutes = r.duration_s as f64 / 60.0;
            let stated = r.stated_games > 0;
            ChampionStatEntry {
                champion_id: r.champion_id,
                champion_name: names.get(&r.champion_id).cloned(),
                win_rate: rate(r.wins, r.games),
                share: rate(r.games, total),
                // A champion picked in two roles of one match counts it twice
                // when roles are summed: clamped, as v1 did.
                pick_rate: matches.map(|m| rate(r.matches_picked, m).min(1.0)),
                ban_rate: matches.map(|m| rate(bans, m)),
                avg_kda: stated.then(|| round4((r.kills + r.assists) as f64 / r.deaths.max(1) as f64)),
                avg_damage: stated.then(|| rate(r.damage, r.stated_games)),
                avg_vision: stated.then(|| rate(r.vision, r.stated_games)),
                cs_per_min: (minutes > 0.0).then(|| round4(r.cs as f64 / minutes)),
                gold_per_min: (minutes > 0.0).then(|| round4(r.gold as f64 / minutes)),
                tier: r.tier,
                patch: r.patch,
                games: r.games,
                wins: r.wins,
            }
        })
        .collect();
    (total, entries)
}

fn matchup(r: &FacetRow, names: &HashMap<i64, String>) -> ChampionMatchupEntry {
    let opponent = r.ids.first().copied().unwrap_or(0);
    ChampionMatchupEntry {
        role: r.role.clone(),
        opponent_id: opponent,
        opponent_name: names.get(&opponent).cloned(),
        games: r.games,
        wins: r.wins,
        win_rate: rate(r.wins, r.games),
    }
}

fn newest(stamps: impl IntoIterator<Item = i64>) -> Option<String> {
    stamps.into_iter().max().and_then(iso_ms)
}

fn internal(e: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %e, "analytics read failed");
    ApiError::internal().into_response()
}

async fn patch_or_latest(state: &AppState, c: &Common) -> Result<Option<String>, DbError> {
    if c.patch.is_some() {
        return Ok(c.patch.clone());
    }
    let scope = state.fetcher.key_scope().as_str().to_string();
    analytics::latest_patch(&state.db, &scope, c.platform.as_deref(), &c.queue).await
}

impl Common {
    fn read(&self, state: &AppState, patch: &str) -> Read {
        Read {
            key_scope: state.fetcher.key_scope().as_str().to_string(),
            platform: self.platform.clone(),
            queue: self.queue.clone(),
            patch: (patch != ALL_PATCHES).then(|| patch.to_string()),
            tier: self.tier.clone(),
            role: self.role.clone(),
            champion_id: None,
            min_games: 0,
            limit: self.limit,
            remakes: self.remakes,
        }
    }

    fn remakes_part(&self) -> Option<String> {
        some(if self.remakes { "include" } else { "exclude" })
    }
}

#[utoipa::path(
    get, path = "/v1/lol/analytics/champions", tag = "lol",
    summary = "Champion pick and win rates by tier",
    description = "Aggregated from every archived match on the platform. Each participant is placed at the tier the \
        ladder crawl or the latest league lookup of that player found them at, whichever is newer, and under \
        `UNKNOWN` when neither has. Recomputed per (platform, queue) when a crawl completes. Sends an `ETag`; a matching \
        `If-None-Match` gets 304. Games Riot flagged as remakes are left out unless `remakes=include`.",
    params(
        ("platform" = Option<String>, Query, description = "Only this platform's ladder. Omitted sums every platform"),
        ("queue" = Option<String>, Query, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR; default the first of `LADDER_QUEUES`"),
        ("tier" = Option<String>, Query, description = "IRON … CHALLENGER, or UNKNOWN; default every tier"),
        ("patch" = Option<String>, Query, description = "`major.minor`, or `all` to sum every aggregated patch; default the newest aggregated patch"),
        ("role" = Option<String>, Query, description = "TOP, JUNGLE, MIDDLE, BOTTOM, UTILITY or empty; default every role summed"),
        ("minGames" = Option<i64>, Query, description = "≥ 0; default `AGGREGATE_MIN_GAMES`"),
        ("limit" = Option<i64>, Query, description = "1–500, default 200"),
        ("remakes" = Option<String>, Query, description = "`exclude` (default) or `include`"),
    ),
    responses((status = 200, description = "The champions, most played first", body = ChampionStatsResponse),
        (status = 304, description = "Not modified"), LocalErrors),
)]
async fn champions(
    State(state): State<AppState>,
    Extension(_c): Who,
    headers: HeaderMap,
    Query(query): Q,
) -> Response {
    let c = match common(&state, &query, true, &TEAM_POSITIONS, (1, 500, 200)) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let min_games = c.min_games.unwrap_or(i64::from(state.config.aggregate_min_games));
    let patch = match patch_or_latest(&state, &c).await {
        Ok(p) => p,
        Err(e) => return internal(&e),
    };
    let (rows, slices, bans) = match &patch {
        Some(p) => {
            let read = Read {
                min_games,
                ..c.read(&state, p)
            };
            let got = tokio::try_join!(
                analytics::stats(&state.db, read.clone()),
                analytics::slices(&state.db, read.clone()),
                analytics::bans(&state.db, Read { role: None, ..read }),
            );
            match got {
                Ok(g) => g,
                Err(e) => return internal(&e),
            }
        }
        None => (vec![], vec![], vec![]),
    };
    let computed_at = newest(rows.iter().map(|r| r.computed_at));
    let ids: Vec<i64> = rows.iter().map(|r| r.champion_id).collect();
    let names = state.ddragon.champion_names(&ids).await;
    let (total_games, champions) = enrich(rows, &slices, &bans, &names);
    let tag = etag(&[
        some("champions"),
        computed_at.clone(),
        state.ddragon.current_version().await,
        c.platform.clone(),
        some(&c.queue),
        c.tier.clone(),
        patch.clone(),
        c.role.clone(),
        some(min_games),
        some(c.limit),
        c.remakes_part(),
    ]);
    respond(
        &headers,
        &tag,
        &ChampionStatsResponse {
            platform: c.platform,
            queue: c.queue,
            tier: c.tier,
            patch,
            role: c.role,
            computed_at,
            total_games,
            champions,
        },
    )
}

#[utoipa::path(
    get, path = "/v1/lol/analytics/champions/{championId}/matchups", tag = "lol",
    summary = "A champion's lane matchups",
    description = "Sends an `ETag`; a matching `If-None-Match` gets 304. Every lane matchup this champion has archived \
        data for. No tier dimension: sample sizes die fast enough per (champion, opponent, role) alone, and the two \
        laners can sit in different tiers anyway. Mirror lanes are excluded — their win rate is 50% by construction. \
        Every archived lane is recorded from both sides, so the opposite champion's view of the same lane mirrors \
        this one.",
    params(
        ("championId" = i64, Path, description = "Champion id, ≥ 1"),
        ("platform" = Option<String>, Query, description = "Only this platform's ladder. Omitted sums every platform"),
        ("queue" = Option<String>, Query, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR"),
        ("patch" = Option<String>, Query, description = "`major.minor`, or `all` to sum every aggregated patch; default the newest aggregated patch"),
        ("role" = Option<String>, Query, description = "TOP, JUNGLE, MIDDLE, BOTTOM or UTILITY; default every lane"),
        ("minGames" = Option<i64>, Query, description = "≥ 0; no default"),
        ("limit" = Option<i64>, Query, description = "1–200, default 50"),
        ("remakes" = Option<String>, Query, description = "`exclude` (default) or `include`"),
    ),
    responses((status = 200, description = "The matchups, most played first", body = ChampionMatchupsResponse),
        (status = 304, description = "Not modified"), LocalErrors),
)]
async fn champion_matchups(
    State(state): State<AppState>,
    Extension(_c): Who,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    Query(query): Q,
) -> Response {
    let Path(raw) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let checked = champion_id(&raw)
        .and_then(|id| Ok((id, common(&state, &query, false, &LANE_POSITIONS, (1, 200, 50))?)));
    let (id, c) = match checked {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let patch = match patch_or_latest(&state, &c).await {
        Ok(p) => p,
        Err(e) => return internal(&e),
    };
    let rows = match &patch {
        Some(p) => {
            let read = Read {
                champion_id: Some(id),
                min_games: c.min_games.unwrap_or(0),
                ..c.read(&state, p)
            };
            match analytics::facet(&state.db, Facet::Matchups, read).await {
                Ok(r) => r,
                Err(e) => return internal(&e),
            }
        }
        None => vec![],
    };
    let mut ids = vec![id];
    ids.extend(rows.iter().filter_map(|r| r.ids.first().copied()));
    let names = state.ddragon.champion_names(&ids).await;
    let computed_at = newest(rows.iter().map(|r| r.computed_at));
    let tag = etag(&[
        some("matchups"),
        computed_at.clone(),
        state.ddragon.current_version().await,
        c.platform.clone(),
        some(&c.queue),
        patch.clone(),
        some(id),
        c.role.clone(),
        c.min_games.map(|n| n.to_string()),
        some(c.limit),
        c.remakes_part(),
    ]);
    respond(
        &headers,
        &tag,
        &ChampionMatchupsResponse {
            champion_id: id,
            champion_name: names.get(&id).cloned(),
            platform: c.platform,
            queue: c.queue,
            patch,
            role: c.role,
            computed_at,
            matchups: rows.iter().map(|r| matchup(r, &names)).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/v1/lol/analytics/champions/{championId}", tag = "lol",
    summary = "Champion detail composite",
    description = "Sends an `ETag`; a matching `If-None-Match` gets 304. The stat row(s) at this slice plus this \
        champion's top lane matchups, items, runes and summoner spells — one call for a champion page. Each section \
        is independently trimmed by `minGames`/`limit`; a champion nobody has data for yet still returns 200 with \
        empty arrays rather than a 404.",
    params(
        ("championId" = i64, Path, description = "Champion id, ≥ 1"),
        ("platform" = Option<String>, Query, description = "Only this platform's ladder. Omitted sums every platform"),
        ("queue" = Option<String>, Query, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR"),
        ("tier" = Option<String>, Query, description = "IRON … CHALLENGER, or UNKNOWN; applies to `stats`"),
        ("patch" = Option<String>, Query, description = "`major.minor`, or `all` to sum every aggregated patch; default the newest aggregated patch"),
        ("role" = Option<String>, Query, description = "TOP, JUNGLE, MIDDLE, BOTTOM, UTILITY or empty"),
        ("minGames" = Option<i64>, Query, description = "≥ 0; default `AGGREGATE_MIN_GAMES`"),
        ("limit" = Option<i64>, Query, description = "1–50, default 10, per section"),
        ("remakes" = Option<String>, Query, description = "`exclude` (default) or `include`"),
    ),
    responses((status = 200, description = "The champion's page", body = ChampionDetailResponse),
        (status = 304, description = "Not modified"), LocalErrors),
)]
async fn champion_detail(
    State(state): State<AppState>,
    Extension(_c): Who,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    Query(query): Q,
) -> Response {
    let Path(raw) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let checked = champion_id(&raw)
        .and_then(|id| Ok((id, common(&state, &query, true, &TEAM_POSITIONS, (1, 50, 10))?)));
    let (id, c) = match checked {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let min_games = c.min_games.unwrap_or(i64::from(state.config.aggregate_min_games));
    let patch = match patch_or_latest(&state, &c).await {
        Ok(p) => p,
        Err(e) => return internal(&e),
    };
    type Sections = (
        Vec<StatRow>,
        Vec<(String, i64)>,
        Vec<(String, i64, i64)>,
        Vec<FacetRow>,
        Vec<FacetRow>,
        Vec<FacetRow>,
        Vec<FacetRow>,
    );
    let (stats, slices, bans, matchups, items, runes, spells): Sections = match &patch {
        Some(p) => {
            let base = Read {
                champion_id: Some(id),
                min_games,
                ..c.read(&state, p)
            };
            // The stats section is not limited (v1: its default, 500).
            let stats = Read {
                limit: 500,
                ..base.clone()
            };
            let facets = Read {
                tier: None,
                ..base.clone()
            };
            let got = tokio::try_join!(
                analytics::stats(&state.db, stats),
                analytics::slices(&state.db, base.clone()),
                analytics::bans(&state.db, base.clone()),
                analytics::facet(&state.db, Facet::Matchups, facets.clone()),
                analytics::facet(&state.db, Facet::Items, facets.clone()),
                analytics::facet(&state.db, Facet::Runes, facets.clone()),
                analytics::facet(&state.db, Facet::Spells, facets),
            );
            match got {
                Ok(g) => g,
                Err(e) => return internal(&e),
            }
        }
        None => Default::default(),
    };
    let mut ids = vec![id];
    ids.extend(matchups.iter().filter_map(|r| r.ids.first().copied()));
    let names = state.ddragon.champion_names(&ids).await;
    let stamp = |rows: &[FacetRow]| newest(rows.iter().map(|r| r.computed_at));
    let sections = SectionsComputedAt {
        stats: newest(stats.iter().map(|r| r.computed_at)),
        matchups: stamp(&matchups),
        items: stamp(&items),
        runes: stamp(&runes),
        spells: stamp(&spells),
    };
    let section_stamps = [
        sections.stats.clone(),
        sections.matchups.clone(),
        sections.items.clone(),
        sections.runes.clone(),
        sections.spells.clone(),
    ];
    // ISO stamps of one format sort as their times do.
    let computed_at = section_stamps.iter().flatten().min().cloned();
    let (total_games, stat_entries) = enrich(stats, &slices, &bans, &names);
    let pair = |r: &FacetRow| {
        (
            r.ids.first().copied().unwrap_or(0),
            r.ids.get(1).copied().unwrap_or(0),
        )
    };
    let mut parts = vec![some("detail")];
    parts.extend(section_stamps);
    parts.extend([
        state.ddragon.current_version().await,
        c.platform.clone(),
        some(&c.queue),
        c.tier.clone(),
        patch.clone(),
        some(id),
        c.role.clone(),
        some(min_games),
        some(c.limit),
        c.remakes_part(),
    ]);
    let tag = etag(&parts);
    respond(
        &headers,
        &tag,
        &ChampionDetailResponse {
            champion_id: id,
            champion_name: names.get(&id).cloned(),
            platform: c.platform,
            queue: c.queue,
            tier: c.tier,
            patch,
            role: c.role,
            computed_at,
            sections_computed_at: sections,
            total_games,
            stats: stat_entries,
            matchups: matchups.iter().map(|r| matchup(r, &names)).collect(),
            items: items
                .iter()
                .map(|r| ChampionItemEntry {
                    item_id: pair(r).0,
                    games: r.games,
                    wins: r.wins,
                    win_rate: rate(r.wins, r.games),
                })
                .collect(),
            runes: runes
                .iter()
                .map(|r| ChampionRuneEntry {
                    keystone_id: pair(r).0,
                    sub_style_id: pair(r).1,
                    games: r.games,
                    wins: r.wins,
                    win_rate: rate(r.wins, r.games),
                })
                .collect(),
            spells: spells
                .iter()
                .map(|r| ChampionSpellEntry {
                    spell_a: pair(r).0,
                    spell_b: pair(r).1,
                    games: r.games,
                    wins: r.wins,
                    win_rate: rate(r.wins, r.games),
                })
                .collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/v1/lol/analytics/patches", tag = "lol",
    summary = "Patches with analytics",
    description = "The patches the analytics tables hold for a ladder, newest first, with each one's games: what \
        a client offers as `patch` choices on the other analytics routes (`all` sums them). With `championId`, \
        only the patches that champion was played on, each with its games. Sends an `ETag`; a matching \
        `If-None-Match` gets 304.",
    params(
        ("platform" = Option<String>, Query, description = "Only this platform's ladder. Omitted sums every platform"),
        ("queue" = Option<String>, Query, description = "RANKED_SOLO_5x5 or RANKED_FLEX_SR; default the first of `LADDER_QUEUES`"),
        ("championId" = Option<i64>, Query, description = "≥ 1: count only this champion's games"),
        ("remakes" = Option<String>, Query, description = "`exclude` (default) or `include`"),
    ),
    responses((status = 200, description = "The patches, newest first", body = AnalyticsPatchesResponse),
        (status = 304, description = "Not modified"), LocalErrors),
)]
async fn patches(
    State(state): State<AppState>,
    Extension(_c): Who,
    headers: HeaderMap,
    Query(query): Q,
) -> Response {
    let checked = ladder_query(&state, &query).and_then(|l| {
        let champion = validate::int_query("championId", q(&query, "championId"), 1, i64::MAX)?;
        Ok((l, champion, validate::remakes_query(q(&query, "remakes"))?))
    });
    let (c, champion_id, remakes) = match checked {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let scope = state.fetcher.key_scope().as_str().to_string();
    let rows: Vec<PatchRow> = match analytics::patches(
        &state.db,
        &scope,
        c.platform.as_deref(),
        &c.queue,
        champion_id,
        remakes,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return internal(&e),
    };
    let tag = etag(&[
        some("patches"),
        newest(rows.iter().map(|r| r.computed_at)),
        some(rows.len()),
        c.platform.clone(),
        some(&c.queue),
        champion_id.map(|id| id.to_string()),
        some(if remakes { "include" } else { "exclude" }),
    ]);
    respond(
        &headers,
        &tag,
        &AnalyticsPatchesResponse {
            platform: c.platform,
            queue: c.queue,
            champion_id,
            patches: rows
                .into_iter()
                .map(|r| AnalyticsPatchEntry {
                    patch: r.patch,
                    games: r.games,
                    computed_at: iso_ms(r.computed_at),
                })
                .collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_etag_is_a_weak_hash_of_the_parts() {
        let tag = etag(&[some("a"), None, some(3)]);
        assert!(tag.starts_with("W/\"") && tag.ends_with('"'), "{tag}");
        assert_eq!(tag.len(), 3 + 43 + 1, "32 bytes of base64url, unpadded");
        assert_eq!(tag, etag(&[some("a"), None, some(3)]), "stable");
        assert_ne!(tag, etag(&[some("a"), some("~x"), some(3)]));
    }

    #[test]
    fn if_none_match_is_a_list_and_star_matches() {
        let mut h = HeaderMap::new();
        assert!(!not_modified(&h, "W/\"x\""));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("W/\"a\", W/\"x\""),
        );
        assert!(not_modified(&h, "W/\"x\""));
        h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(not_modified(&h, "W/\"y\""));
    }

    #[test]
    fn rates_and_averages_follow_v1() {
        let row = StatRow {
            champion_id: 1,
            tier: "MASTER".into(),
            patch: "14.18".into(),
            games: 3,
            wins: 2,
            matches_picked: 3,
            stated_games: 2,
            kills: 10,
            deaths: 0,
            assists: 5,
            cs: 600,
            gold: 30_000,
            damage: 50_001,
            vision: 41,
            duration_s: 3600,
            computed_at: 0,
        };
        let names = HashMap::from([(1, "Annie".to_string())]);
        let (total, e) = enrich(
            vec![
                row.clone(),
                StatRow {
                    champion_id: 2,
                    games: 1,
                    wins: 0,
                    stated_games: 0,
                    duration_s: 0,
                    ..row
                },
            ],
            &[("MASTER".into(), 2)],
            &[("MASTER".into(), 1, 1)],
            &names,
        );
        assert_eq!(total, 4);
        let v = serde_json::to_value(&e[0]).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"championId": 1, "championName": "Annie", "tier": "MASTER", "patch": "14.18",
                "games": 3, "wins": 2, "winRate": 0.6667, "share": 0.75, "pickRate": 1, "banRate": 0.5,
                "avgKda": 15, "avgDamage": 25_000.5, "avgVision": 20.5, "csPerMin": 10, "goldPerMin": 500})
        );
        // No stated games, no length: no averages; no name: no field. The
        // fields keep v1's order on the wire.
        let wire = serde_json::to_string(&e[1]).unwrap();
        assert!(
            wire.starts_with(
                r#"{"championId":2,"tier":"MASTER","patch":"14.18","games":1,"wins":0,"winRate":0,"share":0.25,"pickRate":1,"banRate":0}"#
            ),
            "{wire}"
        );
        // Without a slice, no pick or ban rate.
        let (_, e) = enrich(
            vec![StatRow {
                champion_id: 3,
                ..e_row()
            }],
            &[],
            &[],
            &HashMap::new(),
        );
        assert!(e[0].pick_rate.is_none() && e[0].ban_rate.is_none());
    }

    fn e_row() -> StatRow {
        StatRow {
            champion_id: 0,
            tier: "IRON".into(),
            patch: "1.1".into(),
            games: 1,
            wins: 0,
            matches_picked: 1,
            stated_games: 0,
            kills: 0,
            deaths: 0,
            assists: 0,
            cs: 0,
            gold: 0,
            damage: 0,
            vision: 0,
            duration_s: 0,
            computed_at: 0,
        }
    }

    #[test]
    fn champion_ids_are_positive_integers() {
        assert_eq!(champion_id("103").unwrap(), 103);
        assert_eq!(
            champion_id("0").unwrap_err().message,
            "params/championId must be >= 1"
        );
        assert_eq!(
            champion_id("x").unwrap_err().message,
            "params/championId must be integer"
        );
    }
}

//! `/v1/players/*`: the composite reads (v1 `routes/players.ts`). One call fans
//! out to several Riot calls server-side, each cached on its own, and returns
//! what succeeded plus `warnings[]`: one failing part never fails the document.
//!
//! Reads are cache-first. `?refresh=true` spends quota to re-read, at most once a
//! minute per player and part ([`RefreshWindows`]); unlike the passthrough
//! routes' admin-only refresh, any consumer may ask, because it is metered.
//!
//! The composite `X-Cache` is `MISS` if any part went upstream and `HIT`
//! otherwise; `X-Cache-Age` is the stalest part's age (v1 `summarise`).

mod refresh;
mod summary;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use self::refresh::{RefreshWindows, Window};
pub use self::summary::{MatchSummary, PerksSummary, PlayerSummary};
use crate::app::AppState;
use crate::archive::{matches, pool};
use crate::cache::keys::{canonical_query, derived_key};
use crate::cache::l1::Lookup;
use crate::clock::Clock;
use crate::fetcher::{FetchError, FetchOptions, FetchResult, XCache};
use crate::http::{ApiError, validate};
use crate::metrics::CACHE_READS_TOTAL;
use crate::riot::client::RiotRequest;
use crate::riot::endpoints::{Endpoint, Target, Ttls};
use crate::riot::routing::Platform;
use crate::routes::passthrough::{JSON, LocalErrors, UpstreamErrors, request, respond};
use crate::routes::riot::bad_path;

type Q = Query<HashMap<String, String>>;
type Who = Extension<Arc<crate::http::auth::Consumer>>;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(profile))
        .routes(routes!(profile_by_riot_id))
        .routes(routes!(match_page))
        .routes(routes!(champions))
}

// ── Shared ──────────────────────────────────────────────────────────────────

/// Whole seconds, rounded, as `X-Cache-Age` (v1).
fn secs(d: Duration) -> u64 {
    u64::try_from(d.as_millis().saturating_add(500) / 1000).unwrap_or(u64::MAX)
}

/// How a composite reports its cache state: v1 `summarise`.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    upstream: bool,
    age: u64,
}

impl Tally {
    fn add(&mut self, x_cache: XCache, age: Duration) {
        // v1 only knew HIT and MISS: a part that went upstream (a miss or a won
        // refresh) makes the document a MISS; everything else reads as a hit.
        self.upstream |= matches!(x_cache, XCache::Miss | XCache::Bypass);
        self.age = self.age.max(secs(age));
    }

    fn x_cache(self) -> &'static str {
        if self.upstream { "MISS" } else { "HIT" }
    }
}

/// Serialise the proxy's own document with the composite's cache headers.
fn document<T: Serialize>(body: &T, x_cache: &'static str, age: u64) -> Response {
    let bytes = match serde_json::to_vec(body) {
        Ok(b) => b,
        Err(_) => return ApiError::internal().into_response(),
    };
    let mut res = (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], bytes).into_response();
    let h = res.headers_mut();
    h.insert("x-cache", HeaderValue::from_static(x_cache));
    h.insert("x-cache-age", HeaderValue::from(age));
    res
}

fn q<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query.get(name).map(String::as_str)
}

fn on_platform(id: &str, platform: Platform) -> Option<Target> {
    Endpoint::by_id(id).map(|e| e.target_for_platform(platform))
}

/// account-v1 lives on the platform's account region (`sea` platforms → asia).
fn on_account_region(id: &str, platform: Platform) -> Option<Target> {
    Endpoint::by_id(id).and_then(|e| e.target_for_region(platform.account_region()))
}

/// `?platform=`, defaulting to `DEFAULT_PLATFORM` (v1).
fn platform_or_default(state: &AppState, query: &HashMap<String, String>) -> Result<Platform, ApiError> {
    Ok(validate::query_platform(q(query, "platform"))?.unwrap_or(state.config.default_platform))
}

/// One part of a fan-out: its Riot payload verbatim, or `None` with a warning
/// naming why (v1 `collector`).
fn part(
    name: &str,
    outcome: &Result<FetchResult, FetchError>,
    warnings: &mut Vec<String>,
) -> Option<Box<RawValue>> {
    let err = match outcome {
        Ok(r) => match serde_json::from_slice::<Box<RawValue>>(&r.body) {
            Ok(v) => return Some(v),
            Err(_) => ApiError::upstream(),
        },
        Err(e) => e.api.clone(),
    };
    warnings.push(format!(
        "{name} unavailable ({}: {})",
        err.code.as_str(),
        err.message
    ));
    tracing::debug!(part = name, code = err.code.as_str(), "composite part failed");
    None
}

async fn fetch(
    state: &AppState,
    req: Result<RiotRequest, ApiError>,
    bypass: bool,
) -> Result<FetchResult, FetchError> {
    let req = req.map_err(|api| FetchError { api, x_cache: None })?;
    state
        .fetcher
        .fetch(
            req,
            FetchOptions {
                bypass,
                ..FetchOptions::default()
            },
        )
        .await
}

fn now_ms() -> i64 {
    Clock::now().unix_ms
}

// ── Profile ─────────────────────────────────────────────────────────────────

/// Per part, how long its content has been unchanged; `null` for a failed part.
#[derive(Debug, Serialize, ToSchema)]
pub struct PartAges {
    #[schema(required = true)]
    account: Option<u64>,
    #[schema(required = true)]
    summoner: Option<u64>,
    #[schema(required = true)]
    league: Option<u64>,
    #[schema(required = true)]
    mastery: Option<u64>,
}

/// The composite profile. `account`, `summoner`, `league` and `mastery` are
/// Riot's payloads verbatim, or `null` if that part failed (see `warnings`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBody {
    puuid: String,
    platform: &'static str,
    region: &'static str,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    account: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    summoner: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    league: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    mastery: Option<Box<RawValue>>,
    /// `X-Cache-Age` is the stalest of these.
    age_seconds: PartAges,
    /// Whether this request won the refresh window and went upstream.
    refreshed: bool,
    /// Seconds until another `?refresh=true` is allowed for this player; 0 when now.
    refresh_available_in: u64,
    /// Names each part that could not be fetched.
    warnings: Vec<String>,
}

struct ProfileAsk {
    platform: Platform,
    top_mastery: i64,
    refresh: bool,
}

fn profile_query(state: &AppState, query: &HashMap<String, String>) -> Result<ProfileAsk, ApiError> {
    Ok(ProfileAsk {
        platform: platform_or_default(state, query)?,
        top_mastery: validate::int_query("topMastery", q(query, "topMastery"), 1, 20)?.unwrap_or(5),
        refresh: validate::bool_query("refresh", q(query, "refresh"))?.unwrap_or(false),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    puuid: Option<String>,
    game_name: Option<String>,
    tag_line: Option<String>,
}

/// v1 `composeProfile`. `account` is the already-fetched part when the caller
/// came in by Riot ID and no refresh was won.
async fn compose_profile(
    state: &AppState,
    puuid: &str,
    ask: &ProfileAsk,
    window: Window,
    account: Option<Result<FetchResult, FetchError>>,
) -> Response {
    let p = ask.platform;
    let bypass = window.refreshed;
    let account = async {
        match account {
            Some(known) => known,
            None => {
                let id = "account.byPuuid";
                fetch(
                    state,
                    request(id, on_account_region(id, p), &[puuid], &[]),
                    bypass,
                )
                .await
            }
        }
    };
    let top = ask.top_mastery.to_string();
    let (account, summoner, league, mastery) = tokio::join!(
        account,
        fetch(
            state,
            request(
                "summoner.byPuuid",
                on_platform("summoner.byPuuid", p),
                &[puuid],
                &[]
            ),
            bypass
        ),
        fetch(
            state,
            request(
                "league.entriesByPuuid",
                on_platform("league.entriesByPuuid", p),
                &[puuid],
                &[]
            ),
            bypass,
        ),
        fetch(
            state,
            request(
                "mastery.topByPuuid",
                on_platform("mastery.topByPuuid", p),
                &[puuid],
                &[("count", Some(top))],
            ),
            bypass,
        ),
    );

    let mut warnings = Vec::new();
    let mut tally = Tally::default();
    let age = |o: &Result<FetchResult, FetchError>| o.as_ref().ok().map(|r| secs(r.cache_age));
    for r in [&account, &summoner, &league, &mastery].into_iter().flatten() {
        tally.add(r.x_cache, r.cache_age);
    }
    let body = ProfileBody {
        puuid: puuid.to_string(),
        platform: p.as_str(),
        region: p.region().as_str(),
        account: part("account", &account, &mut warnings),
        summoner: part("summoner", &summoner, &mut warnings),
        league: part("league", &league, &mut warnings),
        mastery: part("mastery", &mastery, &mut warnings),
        age_seconds: PartAges {
            account: age(&account),
            summoner: age(&summoner),
            league: age(&league),
            mastery: age(&mastery),
        },
        refreshed: window.refreshed,
        refresh_available_in: window.available_in,
        warnings,
    };
    // Every part failing means the player does not resolve at all (v1).
    if body.account.is_none() && body.summoner.is_none() && body.league.is_none() && body.mastery.is_none() {
        return ApiError::not_found("No profile data available for this PUUID").into_response();
    }

    // Remember who this PUUID is: a lookup by name arrives already named (v1).
    let who = body
        .account
        .as_ref()
        .and_then(|a| serde_json::from_str::<Identity>(a.get()).ok());
    let upsert = crate::players::Upsert {
        puuid,
        platform: p.as_str(),
        game_name: who.as_ref().and_then(|w| w.game_name.as_deref()),
        tag_line: who.as_ref().and_then(|w| w.tag_line.as_deref()),
        tracked: None,
    };
    if let Err(e) =
        crate::players::upsert(&state.db, state.fetcher.key_scope().as_str(), upsert, now_ms()).await
    {
        // Bookkeeping: a profile is still a profile without it.
        tracing::warn!(error = %e, "could not record player identity");
    }

    document(&body, tally.x_cache(), tally.age)
}

#[utoipa::path(
    get, path = "/v1/players/{puuid}/profile", tag = "players",
    summary = "A player's profile in one call",
    description = "Account, summoner, ranked entries and top mastery, fetched concurrently and each cached on \
                   its own. A part that fails is `null` and named in `warnings`; only every part failing is a \
                   404. `refresh=true` re-reads every part upstream, at most once a minute per player.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("platform" = Option<String>, Query, description = "Platform routing value; default `DEFAULT_PLATFORM`"),
           ("topMastery" = Option<i64>, Query, description = "1–20, default 5"),
           ("refresh" = Option<bool>, Query, description = "Spend quota to re-read; once a minute per player")),
    responses((status = 200, description = "The profile", body = ProfileBody), UpstreamErrors),
)]
async fn profile(
    State(state): State<AppState>,
    Extension(_c): Who,
    Query(query): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let ask = match validate::puuid(&puuid).and_then(|()| profile_query(&state, &query)) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let window = state.refresh.window("profile", &puuid, ask.refresh);
    compose_profile(&state, &puuid, &ask, window, None).await
}

#[utoipa::path(
    get, path = "/v1/players/by-riot-id/{gameName}/{tagLine}/profile", tag = "players",
    summary = "A player's profile by Riot ID",
    description = "The same document, entered by Riot ID. The account lookup is reused as the `account` part \
                   rather than fetched twice.",
    params(("gameName" = String, Path, description = "The part of a Riot ID before the `#` (1–16 characters)"),
           ("tagLine" = String, Path, description = "The part of a Riot ID after the `#` (1–5 characters)"),
           ("platform" = Option<String>, Query, description = "Platform routing value; default `DEFAULT_PLATFORM`"),
           ("topMastery" = Option<i64>, Query, description = "1–20, default 5"),
           ("refresh" = Option<bool>, Query, description = "Spend quota to re-read; once a minute per player")),
    responses((status = 200, description = "The profile", body = ProfileBody), UpstreamErrors),
)]
async fn profile_by_riot_id(
    State(state): State<AppState>,
    Extension(_c): Who,
    Query(query): Q,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let Path((game_name, tag_line)) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let ask = match validate::game_name(&game_name)
        .and_then(|()| validate::tag_line(&tag_line))
        .and_then(|()| profile_query(&state, &query))
    {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    // Always the cached mapping, even on a refresh: it only turns the Riot ID
    // into the PUUID the cooldown is keyed on. A won refresh re-reads the
    // account by PUUID with the rest (v1).
    let id = "account.byRiotId";
    let req = request(
        id,
        on_account_region(id, ask.platform),
        &[&game_name, &tag_line],
        &[],
    );
    let account = match fetch(&state, req, false).await {
        Ok(a) => a,
        Err(e) => return respond(Err(e)),
    };
    let Some(puuid) = serde_json::from_slice::<Identity>(&account.body)
        .ok()
        .and_then(|i| i.puuid)
    else {
        return ApiError::not_found(format!(
            "Riot ID '{game_name}#{tag_line}' did not resolve to a PUUID"
        ))
        .into_response();
    };
    // Keyed on the PUUID, so both ways in share one window (v1).
    let window = state.refresh.window("profile", &puuid, ask.refresh);
    let known = (!window.refreshed).then_some(Ok(account));
    compose_profile(&state, &puuid, &ask, window, known).await
}

// ── Match page ──────────────────────────────────────────────────────────────

/// Present when this lookup queued the player's history for archiving.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillNotice {
    job_id: String,
    /// `queued`, or `already-queued` when another request got there first.
    status: String,
    /// How far back the walk will go, in matches.
    limit: u32,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchPage {
    puuid: String,
    platform: &'static str,
    region: &'static str,
    start: i64,
    count: i64,
    /// The id page as Riot returned it, including ids no summary could be built for.
    match_ids: Vec<String>,
    /// One summary per id that resolved: the requesting player's line in each game.
    matches: Vec<MatchSummary>,
    /// A full page came back, so there is probably another behind it.
    has_more: bool,
    /// How long the id list has been unchanged.
    match_ids_age_seconds: u64,
    #[schema(required = true)]
    backfill: Option<BackfillNotice>,
    refreshed: bool,
    refresh_available_in: u64,
    warnings: Vec<String>,
}

/// v1 `maybeBackfill`: the first page of a lookup records the player. Queueing
/// the history walk needs the job queue (plan P6-06); until then no notice.
async fn maybe_backfill(
    state: &AppState,
    puuid: &str,
    platform: Platform,
    start: i64,
) -> Option<BackfillNotice> {
    if state.config.lookup_backfill_limit == 0 || start != 0 {
        return None;
    }
    let row = crate::players::Upsert {
        puuid,
        platform: platform.as_str(),
        ..crate::players::Upsert::default()
    };
    if let Err(e) = crate::players::upsert(&state.db, state.fetcher.key_scope().as_str(), row, now_ms()).await
    {
        tracing::warn!(error = %e, "could not record looked-up player");
    }
    None
}

#[utoipa::path(
    get, path = "/v1/players/{puuid}/matches", tag = "players",
    summary = "A page of match history, summarised",
    description = "The id page, then every match on it, fanned out concurrently. Archived matches come from \
                   one archive read at no quota cost. Each entry is the player's own line in the game; the \
                   full match stays one call away at `/v1/lol/matches/{region}/{matchId}`. A match that \
                   cannot be fetched is left out and named in `warnings`; a failed id lookup fails the page.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("platform" = Option<String>, Query, description = "Platform routing value; default `DEFAULT_PLATFORM`"),
           ("start" = Option<i64>, Query, description = "0–10000, default 0"),
           ("count" = Option<i64>, Query, description = "1–20, default 10: every id is its own upstream call"),
           ("queue" = Option<i64>, Query, description = "Queue id, 0–5000"),
           ("type" = Option<String>, Query, description = "ranked, normal, tourney or tutorial"),
           ("refresh" = Option<bool>, Query, description = "Re-read the id list; once a minute per player")),
    responses((status = 200, description = "The page", body = MatchPage), UpstreamErrors),
)]
async fn match_page(
    State(state): State<AppState>,
    Extension(_c): Who,
    Query(query): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let parsed = (|| {
        validate::puuid(&puuid)?;
        let platform = platform_or_default(&state, &query)?;
        let start = validate::int_query("start", q(&query, "start"), 0, 10_000)?.unwrap_or(0);
        let count = validate::int_query("count", q(&query, "count"), 1, 20)?.unwrap_or(10);
        let queue = validate::int_query("queue", q(&query, "queue"), 0, 5000)?;
        let kind = validate::query_one_of(
            "type",
            q(&query, "type"),
            &["ranked", "normal", "tourney", "tutorial"],
        )?;
        let refresh = validate::bool_query("refresh", q(&query, "refresh"))?.unwrap_or(false);
        Ok::<_, ApiError>((platform, start, count, queue, kind, refresh))
    })();
    let (platform, start, count, queue, kind, refresh) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let region = platform.region();
    let window = state.refresh.window("matches", &puuid, refresh);

    // The id page cannot fail softly: no ids, no matches. It is also the only
    // part a refresh re-reads; the matches behind it are immutable (v1).
    let id = "match.idsByPuuid";
    let target = Endpoint::by_id(id).and_then(|e| e.target_for_region(region));
    let ids_req = request(
        id,
        target,
        &[&puuid],
        &[
            ("start", Some(start.to_string())),
            ("count", Some(count.to_string())),
            ("queue", queue.map(|n| n.to_string())),
            ("type", kind.map(str::to_string)),
        ],
    );
    let ids = match fetch(&state, ids_req, window.refreshed).await {
        Ok(r) => r,
        Err(e) => return respond(Err(e)),
    };
    let match_ids: Vec<String> = serde_json::from_slice(&ids.body).unwrap_or_default();
    let backfill = maybe_backfill(&state, &puuid, platform, start).await;

    // The whole page from the archive in one read, then fan out over the rest
    // (v1 #54). A degraded archive reads as "nothing archived".
    let archived = matches::get_many(&state.db, &match_ids)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, matches = match_ids.len(), "batched archive read failed");
            HashMap::new()
        });
    if !archived.is_empty() {
        // The reads the fetcher would have counted as archive hits.
        metrics::counter!(CACHE_READS_TOTAL, "state" => "hit").increment(archived.len() as u64);
    }
    let fetched = join_all(match_ids.iter().filter(|m| !archived.contains_key(*m)).map(|m| {
        let state = &state;
        async move {
            let id = "match.byId";
            let target = Endpoint::by_id(id).and_then(|e| e.target_for_region(region));
            (
                m.clone(),
                fetch(state, request(id, target, &[m], &[]), false).await,
            )
        }
    }))
    .await;
    let mut fetched: HashMap<String, Result<FetchResult, FetchError>> = fetched.into_iter().collect();

    let mut tally = Tally::default();
    tally.add(ids.x_cache, ids.cache_age);
    let mut warnings = Vec::new();
    let mut summaries = Vec::with_capacity(match_ids.len());
    for m in &match_ids {
        let body = if let Some(b) = archived.get(m) {
            b.clone()
        } else {
            match fetched.remove(m) {
                Some(Ok(r)) => {
                    tally.add(r.x_cache, r.cache_age);
                    r.body
                }
                Some(Err(e)) => {
                    warnings.push(format!(
                        "match {m} unavailable ({}: {})",
                        e.api.code.as_str(),
                        e.api.message
                    ));
                    continue;
                }
                // A duplicate id on the page, already consumed.
                None => continue,
            }
        };
        match summary::summarise(&body, &puuid, m) {
            Some(s) => summaries.push(s),
            None => warnings.push(format!("match {m} unavailable (no participant for this player)")),
        }
    }

    let body = MatchPage {
        puuid,
        platform: platform.as_str(),
        region: region.as_str(),
        start,
        count,
        has_more: i64::try_from(match_ids.len()).is_ok_and(|n| n == count),
        match_ids,
        matches: summaries,
        match_ids_age_seconds: secs(ids.cache_age),
        backfill,
        refreshed: window.refreshed,
        refresh_available_in: window.available_in,
        warnings,
    };
    document(&body, tally.x_cache(), tally.age)
}

// ── Champion pool ───────────────────────────────────────────────────────────

/// The derived pool document's lifetime (v1 `POOL_TTL_S`).
const POOL_TTL: Duration = Duration::from_secs(300);

/// One champion in a player's pool.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerChampionEntry {
    champion_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    champion_name: Option<String>,
    games: i64,
    wins: i64,
    #[serde(serialize_with = "js_number")]
    win_rate: f64,
    /// (kills + assists) / deaths, deaths floored at 1. Absent until a game
    /// carries a K/D/A.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    avg_kda: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "js_opt_number")]
    cs_per_min: Option<f64>,
    /// End of the most recent archived game on this champion (ISO 8601).
    #[schema(required = true)]
    last_played_at: Option<String>,
}

/// A player's champion pool, computed from the archive at read time: a fact
/// about what this deployment has archived, not the player's whole history.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerChampions {
    puuid: String,
    /// Echoes the `platform` filter; null means every platform.
    #[schema(required = true)]
    platform: Option<String>,
    /// Echoes the `queue` filter; null means every queue.
    #[schema(required = true)]
    queue: Option<i64>,
    /// Echoes the `patch` filter; null means every patch.
    #[schema(required = true)]
    patch: Option<String>,
    /// Games summed across `champions`.
    archived_games: i64,
    champions: Vec<PlayerChampionEntry>,
}

/// A number as JavaScript prints it: a whole value has no `.0` (v1 wrote `1`).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn js_number<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
    let whole = v.fract() == 0.0 && v.abs() < 9.0e15;
    if whole {
        #[allow(clippy::cast_possible_truncation)]
        return s.serialize_i64(*v as i64);
    }
    s.serialize_f64(*v)
}

#[allow(clippy::ref_option)]
fn js_opt_number<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(n) => js_number(n, s),
        None => s.serialize_none(),
    }
}

/// Four decimal places, as v1's analytics published.
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn pool_body(puuid: &str, f: &pool::Filter, rows: Vec<pool::ChampionRow>) -> PlayerChampions {
    PlayerChampions {
        puuid: puuid.to_string(),
        platform: f.platform.clone(),
        queue: f.queue_id,
        patch: f.patch.clone(),
        archived_games: rows.iter().map(|r| r.games).sum(),
        champions: rows
            .into_iter()
            .map(|r| {
                #[allow(clippy::cast_precision_loss)]
                let (games, wins) = (r.games as f64, r.wins as f64);
                #[allow(clippy::cast_precision_loss)]
                let kda = (r.stated_games > 0)
                    .then(|| round4((r.kills + r.assists) as f64 / r.deaths.max(1) as f64));
                PlayerChampionEntry {
                    champion_id: r.champion_id,
                    // Champion names come from the Data Dragon mirror (plan P7-01).
                    champion_name: None,
                    games: r.games,
                    wins: r.wins,
                    win_rate: round4(wins / games),
                    avg_kda: kda,
                    // Absent until a game carries both CS and a length (v1).
                    cs_per_min: (r.cs_seconds > 0)
                        .then(|| round4(r.cs as f64 / (r.cs_seconds as f64 / 60.0))),
                    last_played_at: r.last_played_ms.and_then(crate::clock::iso_ms),
                }
            })
            .collect(),
    }
}

#[utoipa::path(
    get, path = "/v1/players/{puuid}/champions", tag = "players",
    summary = "A player's champion pool",
    description = "Per champion: games, wins, win rate, average KDA, CS per minute and when it was last played, grouped at \
                   read time from the archive, most played first. Never contacts Riot, so it costs no quota; \
                   it only reports the games this deployment has archived for the player.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("platform" = Option<String>, Query, description = "Only games on this platform (by match id prefix)"),
           ("queue" = Option<i64>, Query, description = "Queue id, 0–5000"),
           ("patch" = Option<String>, Query, description = "`major.minor`, e.g. 14.18"),
           ("limit" = Option<i64>, Query, description = "1–500, default 200")),
    responses((status = 200, description = "The pool", body = PlayerChampions), LocalErrors),
)]
async fn champions(
    State(state): State<AppState>,
    Extension(_c): Who,
    Query(query): Q,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(puuid) = match path {
        Ok(p) => p,
        Err(e) => return bad_path(&e).into_response(),
    };
    let parsed = (|| {
        validate::puuid(&puuid)?;
        // A filter here, not a routing decision, so no default (v1).
        let platform = validate::query_platform(q(&query, "platform"))?;
        let queue = validate::int_query("queue", q(&query, "queue"), 0, 5000)?;
        let patch = validate::patch_query(q(&query, "patch"))?;
        let limit = validate::int_query("limit", q(&query, "limit"), 1, 500)?.unwrap_or(200);
        Ok::<_, ApiError>(pool::Filter {
            platform: platform.map(|p| p.as_str().to_string()),
            queue_id: queue,
            patch: patch.map(str::to_string),
            limit: u32::try_from(limit).unwrap_or(200),
        })
    })();
    let filter = match parsed {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };

    let scope = state.fetcher.key_scope();
    let target = format!(
        "/players/{puuid}/champions?{}",
        canonical_query(&[
            ("platform", filter.platform.clone().unwrap_or_default()),
            (
                "queue",
                filter.queue_id.map(|n| n.to_string()).unwrap_or_default()
            ),
            ("patch", filter.patch.clone().unwrap_or_default()),
            ("limit", filter.limit.to_string()),
        ])
    );
    let key = derived_key(scope, "pool", &target);
    let l1 = &state.fetcher.cache().l1;
    // Not counted in `proxy_cache_reads_total`: no upstream call was saved (v1).
    if let Lookup::Fresh(hit) = l1.get(&key).await {
        let age = secs(hit.age(tokio::time::Instant::now()));
        let mut res = (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], hit.body.clone()).into_response();
        res.headers_mut()
            .insert("x-cache", HeaderValue::from_static("HIT"));
        res.headers_mut().insert("x-cache-age", HeaderValue::from(age));
        return res;
    }

    let rows = match pool::champions(&state.db, scope.as_str(), &puuid, filter.clone()).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "champion pool read failed");
            return ApiError::internal().into_response();
        }
    };
    let body = pool_body(&puuid, &filter, rows);
    if let Ok(bytes) = serde_json::to_vec(&body) {
        let ttls = Ttls {
            soft: Some(POOL_TTL),
            hard: Some(POOL_TTL),
            negative: None,
        };
        l1.put(&key, bytes.into(), &ttls).await;
    }
    document(&body, "MISS", 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_numbers_print_as_javascript_does() {
        let e = PlayerChampionEntry {
            champion_id: 1,
            champion_name: None,
            games: 1,
            wins: 1,
            win_rate: 1.0,
            avg_kda: Some(2.5),
            cs_per_min: None,
            last_played_at: None,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"championId":1,"games":1,"wins":1,"winRate":1,"avgKda":2.5,"lastPlayedAt":null}"#
        );
    }

    #[test]
    fn iso_matches_javascript() {
        assert_eq!(
            crate::clock::iso_ms(1_790_247_623_902).as_deref(),
            Some("2026-09-24T11:00:23.902Z")
        );
        assert_eq!(
            crate::clock::iso_ms(0).as_deref(),
            Some("1970-01-01T00:00:00.000Z")
        );
    }

    #[test]
    fn pool_averages_follow_v1() {
        let row = |stated| pool::ChampionRow {
            champion_id: 64,
            games: 3,
            wins: 2,
            stated_games: stated,
            kills: 6,
            deaths: 0,
            assists: 10,
            cs: 390,
            cs_seconds: 3000,
            last_played_ms: None,
        };
        let f = pool::Filter::default();
        let b = pool_body("p", &f, vec![row(2)]);
        let e = &b.champions[0];
        assert_eq!(
            (e.win_rate, e.avg_kda, e.cs_per_min),
            (0.6667, Some(16.0), Some(7.8))
        );
        assert_eq!(b.archived_games, 3);
        let unswept = pool_body("p", &f, vec![row(0)]);
        assert_eq!(unswept.champions[0].avg_kda, None, "absent, not zero");
        let no_length = pool_body(
            "p",
            &f,
            vec![pool::ChampionRow {
                cs_seconds: 0,
                ..row(2)
            }],
        );
        assert_eq!(no_length.champions[0].cs_per_min, None);
    }
}

//! `/v1/players/*`: the composite reads (v1 `routes/players.ts`). One call fans
//! out to several Riot calls server-side, each cached on its own, and returns
//! what succeeded plus `warnings[]`: one failing part never fails the document.
//!
//! Reads are cache-first. `?refresh=true` spends quota to re-read, at most once a
//! minute per player and part ([`RefreshWindows`]); unlike the passthrough
//! routes' admin-only refresh, any consumer may ask, because it is metered.
//!
//! The composite `X-Cache` is `MISS` if any part went upstream and `HIT`
//! otherwise; `X-Cache-Age` is the stalest part's age (v1 `summarise`), and
//! `X-Cache-Fetched-Age` the oldest part's last read from Riot (SITE-01).

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
    /// `None` while every part counted came from the archive.
    fetched_age: Option<u64>,
}

impl Tally {
    fn add(&mut self, r: &FetchResult) {
        // v1 only knew HIT and MISS: a part that went upstream (a miss or a won
        // refresh) makes the document a MISS; everything else reads as a hit.
        self.upstream |= matches!(r.x_cache, XCache::Miss | XCache::Bypass);
        self.age = self.age.max(secs(r.cache_age));
        if let Some(f) = r.fetched_age {
            self.fetched_age = Some(self.fetched_age.unwrap_or(0).max(secs(f)));
        }
    }

    fn x_cache(self) -> &'static str {
        if self.upstream { "MISS" } else { "HIT" }
    }
}

/// Serialise the proxy's own document with the composite's cache headers.
fn document<T: Serialize>(body: &T, tally: Tally) -> Response {
    let bytes = match serde_json::to_vec(body) {
        Ok(b) => b,
        Err(_) => return ApiError::internal().into_response(),
    };
    let mut res = (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], bytes).into_response();
    let h = res.headers_mut();
    h.insert("x-cache", HeaderValue::from_static(tally.x_cache()));
    h.insert("x-cache-age", HeaderValue::from(tally.age));
    if let Some(f) = tally.fetched_age {
        h.insert("x-cache-fetched-age", HeaderValue::from(f));
    }
    res
}

fn q<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query.get(name).map(String::as_str)
}

fn on_platform(id: &str, platform: Platform) -> Option<Target> {
    Endpoint::by_id(id).map(|e| e.target_for_platform(platform))
}

/// account-v1 on whichever cluster has room (ADR-066).
fn account_request(id: &str, params: &[&str]) -> Result<RiotRequest, ApiError> {
    let ep = Endpoint::by_id(id).ok_or_else(ApiError::internal)?;
    RiotRequest::account(ep, params).map_err(|_| ApiError::internal())
}

/// `?platform=`, required: there is no default platform (ADR-065).
fn required_platform(query: &HashMap<String, String>) -> Result<Platform, ApiError> {
    validate::required_query_platform(q(query, "platform"))
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
    let req = req.map_err(FetchError::from)?;
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

/// Per part, in seconds; `null` for a failed part.
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
    #[schema(value_type = Option<crate::routes::riot_schemas::AccountDto>, required = true)]
    account: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<crate::routes::riot_schemas::SummonerDto>, required = true)]
    summoner: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<Vec<crate::routes::riot_schemas::LeagueEntryDto>>, required = true)]
    league: Option<Box<RawValue>>,
    /// Riot's payload, verbatim, or `null` if that part failed; see `warnings`.
    #[schema(value_type = Option<Vec<crate::routes::riot_schemas::ChampionMasteryDto>>, required = true)]
    mastery: Option<Box<RawValue>>,
    /// How long each part's content has been unchanged: a refetch that returns
    /// the same bytes does not reset it. `X-Cache-Age` is the stalest of these.
    age_seconds: PartAges,
    /// How long ago each part was last read from Riot, whether or not it
    /// changed. `X-Cache-Fetched-Age` is the stalest of these.
    fetched_age_seconds: PartAges,
    /// Whether this request won the refresh window and went upstream.
    refreshed: bool,
    /// Seconds until another `?refresh=true` is allowed for this player; 0 when now.
    refresh_available_in: u64,
    /// Names each part that could not be fetched.
    warnings: Vec<String>,
}

struct ProfileAsk {
    /// `None`: the player's own, from account-v1's active region (SITE-09).
    platform: Option<Platform>,
    top_mastery: i64,
    refresh: bool,
}

fn profile_query(query: &HashMap<String, String>) -> Result<ProfileAsk, ApiError> {
    Ok(ProfileAsk {
        platform: validate::query_platform(q(query, "platform"))?,
        top_mastery: validate::int_query("topMastery", q(query, "topMastery"), 1, 20)?.unwrap_or(5),
        refresh: validate::bool_query("refresh", q(query, "refresh"))?.unwrap_or(false),
    })
}

/// account-v1's `AccountRegionDTO`; only `region` is read.
#[derive(Deserialize)]
struct ActiveRegion {
    region: Option<String>,
}

/// The platform a profile is read on: the caller's, else the one Riot says
/// this player plays League on (SITE-09). Riot's 404 for an unknown player
/// passes through; a `region` that isn't a platform we route is a 502.
async fn profile_platform(
    state: &AppState,
    puuid: &str,
    given: Option<Platform>,
) -> Result<Platform, FetchError> {
    if let Some(p) = given {
        return Ok(p);
    }
    let found = fetch(
        state,
        account_request("account.regionByPuuid", &["lol", puuid]),
        false,
    )
    .await?;
    serde_json::from_slice::<ActiveRegion>(&found.body)
        .ok()
        .and_then(|r| r.region)
        .and_then(|r| r.parse::<Platform>().ok())
        .ok_or_else(|| FetchError::from(ApiError::upstream()))
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
    platform: Platform,
    ask: &ProfileAsk,
    window: Window,
    account: Option<Result<FetchResult, FetchError>>,
) -> Response {
    let p = platform;
    let bypass = window.refreshed;
    let account = async {
        match account {
            Some(known) => known,
            None => fetch(state, account_request("account.byPuuid", &[puuid]), bypass).await,
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
    let fetched = |o: &Result<FetchResult, FetchError>| o.as_ref().ok().and_then(|r| r.fetched_age).map(secs);
    for r in [&account, &summoner, &league, &mastery].into_iter().flatten() {
        tally.add(r);
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
        fetched_age_seconds: PartAges {
            account: fetched(&account),
            summoner: fetched(&summoner),
            league: fetched(&league),
            mastery: fetched(&mastery),
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

    document(&body, tally)
}

#[utoipa::path(
    get, path = "/v1/players/{puuid}/profile", tag = "players",
    summary = "A player's profile in one call",
    description = "Account, summoner, ranked entries and top mastery, fetched concurrently and each cached on \
                   its own. A part that fails is `null` and named in `warnings`; only every part failing is a \
                   404. `refresh=true` re-reads every part upstream, at most once a minute per player. \
                   Without `platform`, the proxy reads the player's platform from account-v1's active region \
                   (cached like an account) and echoes it as `platform`, so the player's other routes can \
                   be routed from the profile alone.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("platform" = Option<String>, Query, description = "Platform routing value, e.g. `oc1`. Optional: \
                                                              left out, it is the player's own, from account-v1's active region"),
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
    let ask = match validate::puuid(&puuid).and_then(|()| profile_query(&query)) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let platform = match profile_platform(&state, &puuid, ask.platform).await {
        Ok(p) => p,
        Err(e) => return respond(Err(e)),
    };
    let window = state.refresh.window("profile", &puuid, ask.refresh);
    compose_profile(&state, &puuid, platform, &ask, window, None).await
}

#[utoipa::path(
    get, path = "/v1/players/by-riot-id/{gameName}/{tagLine}/profile", tag = "players",
    summary = "A player's profile by Riot ID",
    description = "The same document, entered by Riot ID. The account lookup is reused as the `account` part \
                   rather than fetched twice.",
    params(("gameName" = String, Path, description = "The part of a Riot ID before the `#` (1–16 characters)"),
           ("tagLine" = String, Path, description = "The part of a Riot ID after the `#` (1–5 characters)"),
           ("platform" = Option<String>, Query, description = "Platform routing value, e.g. `oc1`. Optional: \
                                                              left out, it is the player's own, from account-v1's active region"),
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
        .and_then(|()| profile_query(&query))
    {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    // Always the cached mapping, even on a refresh: it only turns the Riot ID
    // into the PUUID the cooldown is keyed on. A won refresh re-reads the
    // account by PUUID with the rest (v1).
    let req = account_request("account.byRiotId", &[&game_name, &tag_line]);
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
    let platform = match profile_platform(&state, &puuid, ask.platform).await {
        Ok(p) => p,
        Err(e) => return respond(Err(e)),
    };
    // Keyed on the PUUID, so both ways in share one window (v1).
    let window = state.refresh.window("profile", &puuid, ask.refresh);
    let known = (!window.refreshed).then_some(Ok(account));
    compose_profile(&state, &puuid, platform, &ask, window, known).await
}

// ── Match page ──────────────────────────────────────────────────────────────

/// Present when this lookup queued the player's history for archiving.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillNotice {
    pub job_id: String,
    /// `queued`, or `already-queued` when another request got there first.
    pub status: String,
    /// How far back the walk will go, in matches. `4294967295` (the default,
    /// `LOOKUP_BACKFILL_LIMIT`) means the whole history: the walk stops where
    /// Riot's id list ends (ADR-081).
    pub limit: u32,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchPage {
    puuid: String,
    platform: &'static str,
    region: &'static str,
    start: i64,
    count: i64,
    /// The id page as Riot returned it, including ids no summary could be built
    /// for. With `champion`, the archive's ids for that champion instead.
    match_ids: Vec<String>,
    /// One summary per id that resolved: the requesting player's line in each game.
    matches: Vec<MatchSummary>,
    /// A full page came back, so there is probably another behind it. With
    /// `champion` it is exact: the archive holds another game behind this page.
    has_more: bool,
    /// Echoes the `champion` filter; absent without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    champion: Option<i64>,
    /// With `champion` only: how much of the player's history the page draws on.
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<ArchiveCoverage>,
    /// How long the id list has been unchanged: a refetch that returns the
    /// same ids does not reset it.
    match_ids_age_seconds: u64,
    /// How long ago the id list was last read from Riot, whether or not it changed.
    match_ids_fetched_age_seconds: u64,
    #[schema(required = true)]
    backfill: Option<BackfillNotice>,
    refreshed: bool,
    refresh_available_in: u64,
    warnings: Vec<String>,
}

/// How much of a player's history a champion-filtered page draws on (SITE-02).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCoverage {
    /// A backfill has walked the player's whole history (it stamped `doneAt`).
    /// Until then the page holds only the games archived so far: their newest
    /// games, and whatever earlier pages, tracking or a walk in progress stored.
    complete: bool,
}

/// With `champion`, Riot's newest ids read first, so games played since the
/// archive last saw the player are in it before it is filtered. Riot's default
/// page size, and the most one unfiltered page can fetch.
const RECENT_IDS: i64 = 20;

/// v1 `maybeBackfill` (#44): the first page of a lookup records the player
/// and, unless a completed walk already accounts for their history, queues one
/// at bulk priority, so the next page view costs no quota. A walk in flight is
/// deduped by the queue; a walk that died part-way is queued again.
async fn maybe_backfill(
    state: &AppState,
    puuid: &str,
    platform: Platform,
    start: i64,
) -> Option<BackfillNotice> {
    let limit = state.config.lookup_backfill_limit;
    if limit == 0 || start != 0 {
        return None;
    }
    let row = crate::players::Upsert {
        puuid,
        platform: platform.as_str(),
        ..crate::players::Upsert::default()
    };
    let player =
        match crate::players::upsert(&state.db, state.fetcher.key_scope().as_str(), row, now_ms()).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "could not record looked-up player");
                return None;
            }
        };
    let done = crate::jobs::archive::BackfillState::parse(player.backfill_state.as_deref())
        .is_some_and(|s| s.done_at.is_some());
    if done {
        return None;
    }
    let walk = crate::jobs::archive::BackfillPlayer {
        puuid: puuid.to_string(),
        platform: platform.as_str().to_string(),
        limit,
        fetch_timeline: None,
        queue_id: None,
        reason: Some("lookup".into()),
    };
    match crate::jobs::archive::enqueue_backfill(&state.jobs, &walk).await {
        Ok(q) => {
            tracing::info!(puuid, job = %q.id, status = q.status(), limit, "queued backfill on first lookup");
            Some(BackfillNotice {
                job_id: q.id.clone(),
                status: q.status().to_string(),
                limit,
            })
        }
        Err(e) => {
            // Archiving is an optimisation: a queue that is down must not take
            // the match history down with it (v1).
            tracing::warn!(error = %e, puuid, "could not queue lookup backfill");
            None
        }
    }
}

#[utoipa::path(
    get, path = "/v1/players/{puuid}/matches", tag = "players",
    summary = "A page of match history, summarised",
    description = "The id page, then every match on it, fanned out concurrently. Archived matches come from \
                   one archive read at no quota cost. Each entry is the player's own line in the game; the \
                   full match stays one call away at `/v1/lol/matches/{region}/{matchId}`. A match that \
                   cannot be fetched is left out and named in `warnings`; a failed id lookup fails the page.\n\n\
                   **`champion`** filters by champion. Riot's id list cannot, so the page comes from this \
                   deployment's archive: Riot's newest 20 ids are read and archived first, then the archive \
                   is paged for that champion's games, newest first. Older games appear as the player's \
                   backfill archives them; `archive.complete` says whether it has reached the start of their \
                   history. `type` cannot be combined with `champion`.",
    params(("puuid" = String, Path, description = "Encrypted player UUID"),
           ("platform" = String, Query, description = "Platform routing value, e.g. `oc1`. Required"),
           ("start" = Option<i64>, Query, description = "0–10000, default 0"),
           ("count" = Option<i64>, Query, description = "1–20, default 10: every id is its own upstream call"),
           ("queue" = Option<i64>, Query, description = "Queue id, 0–5000"),
           ("type" = Option<String>, Query, description = "ranked, normal, tourney or tutorial; not with `champion`"),
           ("champion" = Option<i64>, Query, description = "Champion id, 1–10000: only that champion's archived games"),
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
        let platform = required_platform(&query)?;
        let start = validate::int_query("start", q(&query, "start"), 0, 10_000)?.unwrap_or(0);
        let count = validate::int_query("count", q(&query, "count"), 1, 20)?.unwrap_or(10);
        let queue = validate::int_query("queue", q(&query, "queue"), 0, 5000)?;
        let kind = validate::query_one_of(
            "type",
            q(&query, "type"),
            &["ranked", "normal", "tourney", "tutorial"],
        )?;
        let refresh = validate::bool_query("refresh", q(&query, "refresh"))?.unwrap_or(false);
        let champion = validate::int_query("champion", q(&query, "champion"), 1, 10_000)?;
        // The archive keeps the queue id; how Riot maps `type` to queues is
        // not in our sources, so the two don't combine.
        if champion.is_some() && kind.is_some() {
            return Err(validate::invalid(
                "querystring",
                "type",
                "must not be set with champion",
            ));
        }
        Ok::<_, ApiError>((platform, start, count, queue, kind, refresh, champion))
    })();
    let (platform, start, count, queue, kind, refresh, champion) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let region = platform.region();
    let window = state.refresh.window("matches", &puuid, refresh);

    // The id page cannot fail softly: no ids, no matches. It is also the only
    // part a refresh re-reads; the matches behind it are immutable (v1).
    // With a champion, it is Riot's newest page whatever page is asked for.
    let (riot_start, riot_count) = if champion.is_some() {
        (0, RECENT_IDS)
    } else {
        (start, count)
    };
    let id = "match.idsByPuuid";
    let target = Endpoint::by_id(id).and_then(|e| e.target_for_region(region));
    let ids_req = request(
        id,
        target,
        &[&puuid],
        &[
            ("start", Some(riot_start.to_string())),
            ("count", Some(riot_count.to_string())),
            ("queue", queue.map(|n| n.to_string())),
            ("type", kind.map(str::to_string)),
        ],
    );
    let ids = match fetch(&state, ids_req, window.refreshed).await {
        Ok(r) => r,
        Err(e) => return respond(Err(e)),
    };
    let riot_ids: Vec<String> = serde_json::from_slice(&ids.body).unwrap_or_default();
    let backfill = maybe_backfill(&state, &puuid, platform, start).await;
    let mut tally = Tally::default();
    tally.add(&ids);
    let mut warnings = Vec::new();

    let (match_ids, has_more, archive) = match champion {
        None => {
            let full = i64::try_from(riot_ids.len()).is_ok_and(|n| n == count);
            (riot_ids, full, None)
        }
        Some(champion) => {
            let filtered = champion_page(
                &state,
                &puuid,
                region,
                ChampionAsk {
                    champion,
                    queue,
                    start,
                    count,
                },
                &riot_ids,
                &mut tally,
                &mut warnings,
            )
            .await;
            match filtered {
                Ok(page) => page,
                Err(e) => return e.into_response(),
            }
        }
    };

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

    let versions = state.ddragon.versions().await;
    let mut summaries = Vec::with_capacity(match_ids.len());
    for m in &match_ids {
        let body = if let Some(b) = archived.get(m) {
            b.clone()
        } else {
            match fetched.remove(m) {
                Some(Ok(r)) => {
                    tally.add(&r);
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
            Some(mut s) => {
                s.ddragon_version = s
                    .game_version
                    .as_deref()
                    .and_then(|b| crate::r#static::ddragon_version_for(b, &versions));
                summaries.push(s);
            }
            None => warnings.push(format!("match {m} unavailable (no participant for this player)")),
        }
    }

    let body = MatchPage {
        puuid,
        platform: platform.as_str(),
        region: region.as_str(),
        start,
        count,
        has_more,
        champion,
        archive,
        match_ids,
        matches: summaries,
        match_ids_age_seconds: secs(ids.cache_age),
        // The id list is never archived, so it always has a fetch time.
        match_ids_fetched_age_seconds: ids.fetched_age.map_or(0, secs),
        backfill,
        refreshed: window.refreshed,
        refresh_available_in: window.available_in,
        warnings,
    };
    document(&body, tally)
}

struct ChampionAsk {
    champion: i64,
    queue: Option<i64>,
    start: i64,
    count: i64,
}

/// The champion filter (SITE-02): archive Riot's newest ids that aren't yet,
/// then page the archive for the champion. Returns the page's ids, whether
/// another game is behind them, and how complete the archive is.
async fn champion_page(
    state: &AppState,
    puuid: &str,
    region: crate::riot::routing::Region,
    ask: ChampionAsk,
    recent: &[String],
    tally: &mut Tally,
    warnings: &mut Vec<String>,
) -> Result<(Vec<String>, bool, Option<ArchiveCoverage>), ApiError> {
    let missing = matches::filter_unarchived(&state.db, recent)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "archive check failed; fetching every recent id");
            recent.to_vec()
        });
    // The fetcher archives each match it reads (immutable endpoint).
    let fetched = join_all(missing.iter().map(|m| async move {
        let id = "match.byId";
        let target = Endpoint::by_id(id).and_then(|e| e.target_for_region(region));
        (m, fetch(state, request(id, target, &[m], &[]), false).await)
    }))
    .await;
    for (m, r) in fetched {
        match r {
            Ok(r) => tally.add(&r),
            // It may not be this champion's game; say so rather than guess.
            Err(e) => warnings.push(format!(
                "recent match {m} not archived ({}: {}); it may be missing from this page",
                e.api.code.as_str(),
                e.api.message
            )),
        }
    }

    let scope = state.fetcher.key_scope().as_str();
    let mut page = crate::archive::player::champion_match_ids(
        &state.db,
        scope,
        puuid,
        ask.champion,
        region.as_str(),
        ask.queue,
        ask.start,
        ask.count + 1,
    )
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "champion page read failed");
        ApiError::internal()
    })?;
    let has_more = i64::try_from(page.len()).is_ok_and(|n| n > ask.count);
    page.truncate(usize::try_from(ask.count).unwrap_or(0));

    let complete = match crate::players::get(&state.db, scope, puuid).await {
        Ok(p) => p
            .and_then(|p| crate::jobs::archive::BackfillState::parse(p.backfill_state.as_deref()))
            .is_some_and(|s| s.done_at.is_some()),
        Err(e) => {
            tracing::warn!(error = %e, "could not read the player's backfill state");
            false
        }
    };
    Ok((page, has_more, Some(ArchiveCoverage { complete })))
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
pub(crate) fn js_number<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
    let whole = v.fract() == 0.0 && v.abs() < 9.0e15;
    if whole {
        #[allow(clippy::cast_possible_truncation)]
        return s.serialize_i64(*v as i64);
    }
    s.serialize_f64(*v)
}

#[allow(clippy::ref_option)]
pub(crate) fn js_opt_number<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(n) => js_number(n, s),
        None => s.serialize_none(),
    }
}

/// Four decimal places, as v1's analytics published.
pub(crate) fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn pool_body(
    puuid: &str,
    f: &pool::Filter,
    rows: Vec<pool::ChampionRow>,
    names: &HashMap<i64, String>,
) -> PlayerChampions {
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
                    // From the Data Dragon mirror; absent when it does not know the id (v1).
                    champion_name: names.get(&r.champion_id).cloned(),
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
           ("limit" = Option<i64>, Query, description = "1–500, default 200"),
           ("remakes" = Option<String>, Query, description = "`exclude` (default) or `include` games Riot flagged as remakes")),
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
        let remakes = validate::remakes_query(q(&query, "remakes"))?;
        Ok::<_, ApiError>(pool::Filter {
            platform: platform.map(|p| p.as_str().to_string()),
            queue_id: queue,
            patch: patch.map(str::to_string),
            limit: u32::try_from(limit).unwrap_or(200),
            remakes,
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
            ("remakes", if filter.remakes { "include" } else { "" }.to_string()),
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
    let ids: Vec<i64> = rows.iter().map(|r| r.champion_id).collect();
    let names = state.ddragon.champion_names(&ids).await;
    let body = pool_body(&puuid, &filter, rows, &names);
    if let Ok(bytes) = serde_json::to_vec(&body) {
        let ttls = Ttls {
            soft: Some(POOL_TTL),
            hard: Some(POOL_TTL),
            negative: None,
        };
        l1.put(&key, bytes.into(), &ttls).await;
    }
    // Built from the archive, not read from Riot: no fetch age to report.
    let built = Tally {
        upstream: true,
        ..Tally::default()
    };
    document(&body, built)
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
        let b = pool_body("p", &f, vec![row(2)], &HashMap::new());
        let e = &b.champions[0];
        assert_eq!(
            (e.win_rate, e.avg_kda, e.cs_per_min),
            (0.6667, Some(16.0), Some(7.8))
        );
        assert_eq!(b.archived_games, 3);
        let unswept = pool_body("p", &f, vec![row(0)], &HashMap::new());
        assert_eq!(unswept.champions[0].avg_kda, None, "absent, not zero");
        let no_length = pool_body(
            "p",
            &f,
            vec![pool::ChampionRow {
                cs_seconds: 0,
                ..row(2)
            }],
            &HashMap::new(),
        );
        assert_eq!(no_length.champions[0].cs_per_min, None);
    }
}

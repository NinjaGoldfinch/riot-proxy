//! The Data Dragon mirror over HTTP (plan P7-01): v1's `/v1/static/*` JSON
//! routes (`routes/static.ts`), which need a read key, and the raw files at
//! `/ddragon/*`, which v1's Caddy served without one (design/07 §Option B).
//! None of these calls Riot's API or touches the limiter.

use std::collections::HashMap;
use std::path::Path;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower_http::services::ServeDir;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::http::{ApiError, validate};
use crate::routes::passthrough::{JSON, LocalErrors};
use crate::r#static::{DATA_FILES, FILE_ALIASES, VERSIONS_FILE, resolve_file};

/// v1's Caddyfile header for `/ddragon/*`: a patch's files never change.
pub const IMMUTABLE: &str = "public, max-age=604800, immutable";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(static_versions))
        .routes(routes!(static_queues))
        .routes(routes!(static_file))
}

/// `/ddragon/<version>/<file>.json` straight from `dir`. Only a found file is
/// marked immutable: a 404 for a patch not synced yet must not be cached for
/// a week.
pub fn files(dir: &Path) -> Router {
    Router::new()
        .nest_service("/ddragon", ServeDir::new(dir))
        .layer(axum::middleware::map_response(|mut res: Response| async move {
            if res.status().is_success() {
                res.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
            }
            res
        }))
}

/// A local JSON document with v1's `applyCacheHeaders(reply, state, 0)`.
fn local(bytes: Vec<u8>, x_cache: &'static str) -> Response {
    let mut res = (StatusCode::OK, [(header::CONTENT_TYPE, JSON)], bytes).into_response();
    let h = res.headers_mut();
    h.insert("x-cache", HeaderValue::from_static(x_cache));
    h.insert("x-cache-age", HeaderValue::from(0));
    res
}

#[utoipa::path(
    get, path = "/v1/static/versions", tag = "static",
    summary = "Data Dragon patches",
    description = "`{current, versions}`: the mirrored patch (null before the first `ddragon:sync`) and \
        Riot's patch list, newest first. `X-Cache: HIT` from the mirror; before the first sync the \
        list is fetched live from Data Dragon (`MISS`).",
    responses((status = 200, description = "`{current, versions}`", body = serde_json::Value), LocalErrors),
)]
async fn static_versions(State(state): State<AppState>) -> Response {
    let mirror = &state.ddragon;
    let current = mirror.current_version().await;
    let mirrored = match &current {
        Some(v) => mirror.read(VERSIONS_FILE, Some(v)).await,
        None => None,
    };
    let (list, x_cache) = match mirrored {
        Some(bytes) => (bytes, "HIT"),
        // A fresh deployment is usable before its first sync (v1).
        None => match mirror.cdn().versions().await {
            Ok((bytes, _)) => (bytes, "MISS"),
            Err(e) => {
                tracing::error!(error = %e, "Data Dragon version list unavailable");
                return ApiError::internal().into_response();
            }
        },
    };
    let current = serde_json::to_string(&current).unwrap_or_else(|_| "null".into());
    let mut body = format!(r#"{{"current":{current},"versions":"#).into_bytes();
    body.extend_from_slice(&list);
    body.push(b'}');
    local(body, x_cache)
}

#[utoipa::path(
    get, path = "/v1/static/queues", tag = "static",
    summary = "Riot's queue id table",
    description = "Maps `queueId` to a map and a description — what `420` means where the archive and \
        the composite routes report one. Refreshed on every `ddragon:sync`, not only on a new patch: \
        Riot adds queue ids when a game mode ships, which is not a patch event.",
    responses((status = 200, description = "Riot's `queues.json`", body = serde_json::Value), LocalErrors),
)]
async fn static_queues(State(state): State<AppState>) -> Response {
    match state.ddragon.read_meta("queues").await {
        Some(bytes) => local(bytes, "HIT"),
        None => ApiError::not_found("The queue table has not been synced yet. Run the ddragon:sync job.")
            .into_response(),
    }
}

/// `/v1/static/{file}`'s names, for the document (v1's enum, in its order).
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
enum StaticFile {
    #[schema(rename = "champion")]
    Champion,
    #[schema(rename = "item")]
    Item,
    #[schema(rename = "runesReforged")]
    RunesReforged,
    #[schema(rename = "summoner")]
    Summoner,
    #[schema(rename = "profileicon")]
    Profileicon,
    #[schema(rename = "map")]
    Map,
    #[schema(rename = "champions")]
    Champions,
    #[schema(rename = "items")]
    Items,
    #[schema(rename = "runes")]
    Runes,
    #[schema(rename = "summoner-spells")]
    SummonerSpells,
    #[schema(rename = "profile-icons")]
    ProfileIcons,
    #[schema(rename = "maps")]
    Maps,
}

/// Every name `/v1/static/{file}` accepts: the Data Dragon names, then v1's aliases.
fn file_names() -> Vec<&'static str> {
    DATA_FILES
        .iter()
        .copied()
        .chain(FILE_ALIASES.iter().map(|(alias, _)| *alias))
        .collect()
}

#[utoipa::path(
    get, path = "/v1/static/{file}", tag = "static",
    summary = "A mirrored Data Dragon file",
    description = "Data Dragon's JSON for the mirrored patch, or for `?version=` when that patch was \
        mirrored. Plural aliases name the same files (`champions` is `champion`).",
    params(
        ("file" = inline(StaticFile), Path, description = "Data Dragon name or alias"),
        ("version" = Option<String>, Query, description = "A mirrored patch; default the current one",
            max_length = 20, pattern = r"^[0-9]+(\.[0-9]+)*$", example = "16.17.1"),
    ),
    responses((status = 200, description = "Data Dragon's JSON, untouched", body = serde_json::Value), LocalErrors),
)]
async fn static_file(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let checked = (|| {
        let resolved = resolve_file(&name).ok_or_else(|| {
            validate::one_of_str("file", &name, &file_names())
                .err()
                .unwrap_or_else(ApiError::internal)
        })?;
        let version = validate::version_query(query.get("version").map(String::as_str))?;
        Ok::<_, ApiError>((resolved, version.map(str::to_string)))
    })();
    let (resolved, version) = match checked {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    match state.ddragon.read(resolved, version.as_deref()).await {
        Some(bytes) => local(bytes, "HIT"),
        None => ApiError::not_found(format!(
            "Static data '{name}' has not been synced yet. Run the ddragon:sync job."
        ))
        .into_response(),
    }
}

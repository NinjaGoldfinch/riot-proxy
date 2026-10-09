//! The Data Dragon mirror over HTTP (plan P7-01): v1's `/v1/static/*` JSON
//! routes (`routes/static.ts`), which need a read key, and the raw files at
//! `/ddragon/*`, which v1's Caddy served without one (design/07 §Option B).
//! None of these calls Riot's API or touches the limiter.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::services::ServeDir;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::http::{ApiError, validate};
use crate::routes::passthrough::{JSON, LocalErrors};
use crate::r#static::images::{ImageError, RUNE_DIR};
use crate::r#static::{DATA_FILES, FILE_ALIASES, Mirror, VERSIONS_FILE, resolve_file};

/// `/ddragon/*`'s header: a patch's files never change once written, so a
/// year (SITE-07; v1's Caddyfile said a week).
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The image kinds `/ddragon` serves, as the OpenAPI document names them:
/// Data Dragon's own directory names, `spell` being summoner spells. The
/// handler matches the path segment against `IMAGE_KINDS`; a test keeps the
/// two lists the same.
#[derive(utoipa::ToSchema)]
#[schema(rename_all = "lowercase")]
#[allow(dead_code)] // documentation only
pub enum ImageKind {
    Champion,
    Profileicon,
    Item,
    Spell,
}

/// A PNG's bytes, for the OpenAPI document.
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
#[allow(dead_code)] // documentation only
pub struct Png(Vec<u8>);

/// What both image operations say about versions, immutability and load.
const IMAGE_NOTES: &str = "No key. The image is Data Dragon's own, fetched from Riot's CDN on its first request and served from disk after that.\n\n\
**Versions.** Any version in Riot's version list (`/v1/static/versions`) works, not only the current one, so a match can draw its own patch's icons (`ddragonVersion` on the match page). For a version the mirror hasn't synced, the one data file that lists the image (that patch's `item.json` for an item) is fetched first and kept. A version Riot doesn't list is a 404 with no fetch.\n\n\
**Immutable.** The bytes at a given version and path never change once served, so they are sent with `Cache-Control: public, max-age=31536000, immutable`. A response that isn't a 200 has no `Cache-Control`.\n\n\
**What can be fetched.** Only a file the version's own data lists, so a request can't make the proxy fetch anything else. Concurrent first requests for one file share one fetch, and a data file Riot has no copy of isn't asked for twice.\n\n\
**Errors** have an empty body, not the JSON error envelope.";

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(static_versions))
        .routes(routes!(static_queues))
        .routes(routes!(static_file))
}

/// `/ddragon/<version>/<file>.json` straight from the mirror's directory, and
/// `/ddragon/<version>/img/<kind>/<file>` and rune icons at
/// `/ddragon/<version>/img/perk-images/…` filled on first request
/// (`static::images`). Only a found file is marked immutable: a 404 for a
/// patch not synced yet must not be cached for a week.
pub fn files(mirror: Arc<Mirror>) -> Router {
    // Only what is not on disk yet reaches the fill route.
    let fill = Router::new()
        .route("/{version}/img/{kind}/{file}", get(ddragon_image))
        .route("/{version}/img/perk-images/{*icon}", get(ddragon_rune_image))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(Arc::clone(&mirror));
    Router::new()
        .nest_service("/ddragon", ServeDir::new(mirror.dir()).fallback(fill))
        .layer(axum::middleware::map_response(|mut res: Response| async move {
            if res.status().is_success() {
                res.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
            }
            res
        }))
}

#[utoipa::path(
    get, path = "/ddragon/{version}/img/{kind}/{file}", tag = "static",
    summary = "A champion, item, summoner spell or profile icon image",
    description = IMAGE_NOTES,
    security(()),
    params(
        ("version" = String, Path, description = "A Data Dragon version from Riot's list, e.g. `16.19.1`.",
            pattern = r"^[0-9]+(\.[0-9]+)*$", example = "16.19.1"),
        ("kind" = ImageKind, Path, description = "Data Dragon's image directory: `spell` is summoner spells."),
        ("file" = String, Path, description = "The image's file name as that version's data lists it: \
            `image.full` in champion.json, item.json, summoner.json or profileicon.json.",
            pattern = r"^[A-Za-z0-9_.]+\.png$", example = "3078.png"),
    ),
    responses(
        (status = 200, description = "The PNG.", content_type = "image/png", body = Png),
        (status = 304, description = "Not modified since `If-Modified-Since`, for a copy already on disk."),
        (status = 404, description = "Not a version Riot lists, not a kind served, not a file that version's \
            data lists, or a file Riot doesn't have. Empty body."),
        (status = 502, description = "Riot's CDN failed on the first fetch (unreachable, an error status, \
            or not a PNG). Nothing was kept, so the next request tries again. Empty body."),
    ),
)]
pub async fn ddragon_image(
    State(mirror): State<Arc<Mirror>>,
    UrlPath((version, kind, file)): UrlPath<(String, String, String)>,
) -> Response {
    png(
        mirror.image(&version, &kind, &file).await,
        &version,
        &format!("{kind}/{file}"),
    )
}

#[utoipa::path(
    get, path = "/ddragon/{version}/img/perk-images/{icon}", tag = "static",
    summary = "A rune or rune style icon",
    description = IMAGE_NOTES,
    security(()),
    params(
        ("version" = String, Path, description = "A Data Dragon version from Riot's list, e.g. `16.19.1`.",
            pattern = r"^[0-9]+(\.[0-9]+)*$", example = "16.19.1"),
        ("icon" = String, Path, description = "runesReforged.json's `icon` without its leading `perk-images/`, \
            e.g. `Styles/Domination/Electrocute/Electrocute.png`. It spans several path segments: send its \
            slashes as they are, or percent-encoded (`%2F`, as a generated client does). Both are served.",
            example = "Styles/Domination/Electrocute/Electrocute.png"),
    ),
    responses(
        (status = 200, description = "The PNG.", content_type = "image/png", body = Png),
        (status = 304, description = "Not modified since `If-Modified-Since`, for a copy already on disk."),
        (status = 404, description = "Not a version Riot lists, not an icon that version's runesReforged.json \
            lists, or an icon Riot doesn't have. Empty body."),
        (status = 502, description = "Riot's CDN failed on the first fetch (unreachable, an error status, \
            or not a PNG). Nothing was kept, so the next request tries again. Empty body."),
    ),
)]
pub async fn ddragon_rune_image(
    State(mirror): State<Arc<Mirror>>,
    UrlPath((version, icon)): UrlPath<(String, String)>,
) -> Response {
    let icon = format!("{RUNE_DIR}/{icon}");
    png(mirror.rune_image(&version, &icon).await, &version, &icon)
}

fn png(image: Result<Vec<u8>, ImageError>, version: &str, file: &str) -> Response {
    match image {
        Ok(bytes) => (StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        // Bare, like ServeDir's 404 for the JSON files beside it.
        Err(ImageError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Upstream(e)) => {
            tracing::warn!(error = %e, %version, %file, "Data Dragon image unavailable");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
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

//! The OpenAPI document and the API reference (plan P4-03, design/03 `routes/docs.rs`).
//!
//! Routes register through `utoipa_axum::OpenApiRouter`, so the document is built
//! from the same handlers that serve traffic. `/openapi.json`, `/openapi.yaml` and
//! `/docs` (Scalar) are served together under `DOCS_UI` and need no key (v1).
//! Servers, security schemes, tags and tag groups mirror v1's document.

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use utoipa::openapi::OpenApi as Document;
use utoipa::openapi::extensions::Extensions;
use utoipa::openapi::header::Header;
use utoipa::openapi::path::Operation;
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};

use crate::app::AppState;
use crate::routes;

const DESCRIPTION: &str = "\
A self-hosted proxy between [Riot's public API](https://developer.riotgames.com/) and \
everything downstream of it. It holds one Riot key, spends it carefully, and gives its \
consumers their own keys and quotas.

- **Rate limits:** read from Riot's own response headers. Requests a user is waiting on \
go ahead of background work.
- **Caching and archive:** repeated reads are served from cache; matches are immutable \
and archived.
- **One error envelope:** `{\"error\":{\"code\",\"message\",\"requestId\",\"retryAfter?\"}}`.

### Response headers

| Header | Meaning |
|---|---|
| `X-Cache` | `HIT`, `MISS`, `STALE`, `HIT-NEG` (a cached 404), `ARCHIVE` (from the match archive), `BYPASS` (`?refresh=true`) |
| `X-Cache-Age` | Age of the **content** in seconds, not of the last fetch |
| `X-Cache-Fetched-Age` | Seconds since the proxy last read it from Riot, changed or not. Not sent for `ARCHIVE` |
| `X-RateLimit-Limit` / `-Remaining` / `-Reset` | Your consumer quota |
| `X-Request-Id` | Quote this when reporting a problem |
| `Retry-After` | On `QUOTA_EXCEEDED` (429) and `RATE_LIMITED` (503): seconds to wait |

Each response in this document declares the headers it sends.

### Images

`/v1/static/*` serves Data Dragon's JSON. Its images are mirrored too, at `/ddragon/<version>/img/<kind>/<file>`: Data Dragon's own `/cdn/<version>/img/<kind>/<file>` layout, for `champion`, `profileicon`, `item` and `spell` (summoner spells). Rune icons are at `/ddragon/<version>/img/<icon>`, `icon` being `runesReforged.json`'s `perk-images/…` path. `<version>` must be a patch the mirror holds (`/v1/static/versions`), and the file one that patch's data lists. An image is fetched from Riot's CDN on first request, then served from disk. `/ddragon` needs no key. CommunityDragon assets are not mirrored.
";

#[derive(OpenApi)]
#[openapi(
    info(title = "riot-proxy", description = DESCRIPTION),
    servers(
        (url = "http://localhost:8080", description = "Local development"),
        (url = "{scheme}://{host}", description = "Your deployment", variables(
            ("scheme" = (default = "https", enum_values("https", "http"))),
            ("host" = (default = "riot-proxy.example.com"))
        ))
    ),
    tags(
        (name = "players", description = "Composite player reads"),
        (name = "riot", description = "Riot account-v1, passed through"),
        (name = "lol", description = "League of Legends endpoints"),
        (name = "static", description = "Data Dragon"),
        (name = "ws", description = "Realtime events"),
        (name = "ops", description = "Health and metrics; no key needed"),
        (name = "admin", description = "Administration; admin scope and IP allowlist"),
    ),
    security(("bearerAuth" = [])),
    modifiers(&SecuritySchemes),
    paths(crate::telemetry::render_metrics),
    components(schemas(crate::http::error::ErrorResponse)),
)]
struct ApiDoc;

/// v1's two schemes: a bearer key, and `?token=` for WebSocket handshakes.
struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, doc: &mut Document) {
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A consumer key, `rpx_…`, issued by `riot-proxy key create` or `POST /v1/admin/consumers` \
                         and shown exactly once. Only its SHA-256 is stored.",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "tokenQuery",
            SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::with_description(
                "token",
                "**WebSocket handshake only.** A browser `WebSocket` cannot set an `Authorization` header, \
                 so `/v1/ws` takes the same key as `?token=`. Avoid it on HTTP routes: a key in a query \
                 string ends up in access logs.",
            ))),
        );
    }
}

/// Every documented route. With `Some(state)`, protected routes get the auth guard
/// (`serve`); with `None` they don't, which is all `spec` needs.
pub fn api_router(auth: Option<AppState>) -> OpenApiRouter<AppState> {
    let mut read = OpenApiRouter::new()
        .merge(routes::players::router())
        .merge(routes::riot::router())
        .merge(routes::lol::router())
        .merge(routes::statics::router())
        .merge(routes::analytics::router());
    let mut admin = routes::admin::router();
    if let Some(state) = auth {
        read = read.route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::http::auth::require_read,
        ));
        admin = admin.route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::http::auth::require_admin,
        ));
    }
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(routes::health::router())
        .merge(routes::ws::router())
        .merge(read)
        .merge(admin)
}

/// The response headers (SITE-04): name, schema, description.
const HEADERS: [(&str, Type, &str); 8] = [
    (
        "X-Request-Id",
        Type::String,
        "This request's id; quote it when reporting a problem.",
    ),
    (
        "X-Cache",
        Type::String,
        "Where the body came from: `HIT`, `MISS`, `STALE`, `HIT-NEG` (a cached 404), \
         `ARCHIVE` (the match archive) or `BYPASS` (`?refresh=true`).",
    ),
    (
        "X-Cache-Age",
        Type::Integer,
        "Seconds the content has been unchanged, not since the last fetch.",
    ),
    (
        "X-Cache-Fetched-Age",
        Type::Integer,
        "Seconds since the proxy last read it from Riot, changed or not. Not sent for `ARCHIVE`.",
    ),
    (
        "X-RateLimit-Limit",
        Type::Integer,
        "Your consumer quota per minute.",
    ),
    (
        "X-RateLimit-Remaining",
        Type::Integer,
        "Requests left in the current minute.",
    ),
    (
        "X-RateLimit-Reset",
        Type::Integer,
        "Seconds until a request slot frees up.",
    ),
    (
        "Retry-After",
        Type::Integer,
        "Seconds to wait before trying again.",
    ),
];

/// Read routes, behind the consumer key and quota.
const READ_TAGS: [&str; 4] = ["players", "riot", "lol", "static"];

/// Which headers `path`'s `op` sends on `status`, as the handlers and middleware
/// set them. Every response carries `X-Request-Id` (the outermost layer); a
/// keyed route meters every request it authenticated; `Retry-After` comes with
/// the two rate-limit errors.
fn headers_for(path: &str, op: &Operation, status: &str) -> Vec<&'static str> {
    let tagged = |tags: &[&str]| op.tags.iter().flatten().any(|t| tags.contains(&t.as_str()));
    let mut out = vec!["X-Request-Id"];
    if tagged(&[READ_TAGS.as_slice(), &["admin"]].concat()) && !matches!(status, "401" | "403") {
        out.extend(["X-RateLimit-Limit", "X-RateLimit-Remaining", "X-RateLimit-Reset"]);
    }
    if matches!(status, "429" | "503") {
        out.push("Retry-After");
    }
    // The analytics routes revalidate with `ETag` instead of a cache tier.
    let cached = tagged(&READ_TAGS) && !path.starts_with("/v1/lol/analytics");
    let ok = status.starts_with('2');
    if cached && ok {
        out.extend(["X-Cache", "X-Cache-Age"]);
        // Static files and the archive-built champion pool are never read from Riot.
        if !tagged(&["static"]) && !path.ends_with("/champions") {
            out.push("X-Cache-Fetched-Age");
        }
    }
    // A cached 404 says so (`HIT-NEG`).
    if cached && status == "404" && tagged(&["riot", "lol"]) {
        out.push("X-Cache");
    }
    out
}

/// Declare [`HEADERS`] once under `components.headers` and reference them
/// from every response that sends them, so generated clients type them.
fn declare_headers(doc: &mut Document) {
    let components = doc.components.get_or_insert_with(Default::default);
    for (name, kind, description) in HEADERS {
        let schema = ObjectBuilder::new().schema_type(kind).build();
        let mut header = Header::new(schema);
        header.description = Some(description.to_string());
        components.headers.insert(name.to_string(), RefOr::T(header));
    }
    for (path, item) in &mut doc.paths.paths {
        let ops = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.patch,
        ];
        for op in ops.into_iter().flatten() {
            let wanted: Vec<(String, Vec<&str>)> = op
                .responses
                .responses
                .keys()
                .map(|status| (status.clone(), headers_for(path, op, status)))
                .collect();
            for (status, names) in wanted {
                if let Some(RefOr::T(res)) = op.responses.responses.get_mut(&status) {
                    for name in names {
                        res.headers.insert(
                            name.to_string(),
                            RefOr::Ref(Ref::new(format!("#/components/headers/{name}"))),
                        );
                    }
                }
            }
        }
    }
}

/// Final touches: the crate version, v1's tag groups and the response headers.
pub fn finish(mut doc: Document) -> Document {
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
    declare_headers(&mut doc);
    let groups = serde_json::json!([
        {"name": "Player data", "tags": ["players", "riot", "lol"]},
        {"name": "Static data", "tags": ["static"]},
        {"name": "Realtime", "tags": ["ws"]},
        {"name": "Operations", "tags": ["ops"]},
        {"name": "Administration", "tags": ["admin"]},
    ]);
    doc.extensions
        .get_or_insert_with(Extensions::default)
        .merge(Extensions::builder().add("x-tagGroups", groups).build());
    doc
}

/// The document `serve` publishes and `riot-proxy spec` prints.
pub fn spec() -> Document {
    let (_, doc) = api_router(None).split_for_parts();
    finish(doc)
}

/// `/openapi.json`, `/openapi.yaml` and `/docs`.
pub fn docs_router(doc: Document) -> Router {
    let json = doc.to_pretty_json().unwrap_or_else(|_| "{}".into());
    let yaml = doc.to_yaml().unwrap_or_default();
    Router::new()
        .route(
            "/openapi.json",
            get(move || async move { ([(header::CONTENT_TYPE, "application/json")], json).into_response() }),
        )
        .route(
            "/openapi.yaml",
            get(move || async move { ([(header::CONTENT_TYPE, "application/yaml")], yaml).into_response() }),
        )
        .merge(Scalar::with_url("/docs", doc))
}

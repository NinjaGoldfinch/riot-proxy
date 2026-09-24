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
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
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
| `X-RateLimit-Limit` / `-Remaining` / `-Reset` | Your consumer quota |
| `X-Request-Id` | Quote this when reporting a problem |
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
    let mut read = OpenApiRouter::new().merge(routes::riot::router());
    if let Some(state) = auth {
        read = read.route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::http::auth::require_read,
        ));
    }
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(routes::health::router())
        .merge(read)
}

/// Final touches: the crate version and v1's tag groups.
pub fn finish(mut doc: Document) -> Document {
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
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

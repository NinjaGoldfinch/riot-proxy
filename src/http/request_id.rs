//! Request ids: a ULID per request, or the caller's own `X-Request-Id` when it is
//! well-formed. The id is put in the request extensions, attached to a tracing span
//! wrapping the rest of the stack, and returned on the response (ADR-009).

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Longest inbound id honoured; anything longer is replaced with a fresh ULID.
pub const MAX_INBOUND_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    fn from_inbound(value: &HeaderValue) -> Option<Self> {
        let s = value.to_str().ok()?;
        let ok = !s.is_empty()
            && s.len() <= MAX_INBOUND_LEN
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
        ok.then(|| Self(s.to_string()))
    }

    fn generate() -> Self {
        Self(ulid::Ulid::generate().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `axum::middleware::from_fn(request_id)`. Outermost layer, so every log line
/// below it carries `request_id`.
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(RequestId::from_inbound)
        .unwrap_or_else(RequestId::generate);
    req.extensions_mut().insert(id.clone());

    let span = tracing::info_span!("request", request_id = %id.as_str());
    let mut res = next.run(req).instrument(span).await;

    // Only [A-Za-z0-9-_.:] reaches here, so the header value is always valid.
    if let Ok(value) = HeaderValue::from_str(id.as_str()) {
        res.headers_mut().insert(X_REQUEST_ID.clone(), value);
    }
    res
}

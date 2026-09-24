//! The one error envelope every route returns: `{error:{code,message,requestId,retryAfter?}}`.
//! Codes and their default statuses are v1's (`src/errors.ts`). `requestId` is a v2
//! addition, approved by the owner (ADR-011).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::http::request_id;

/// v1's closed set of error codes, serialised verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    QuotaExceeded,
    NotFound,
    UpstreamError,
    RateLimited,
    BadRegion,
    Validation,
    Internal,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 9] = [
        Self::Unauthorized,
        Self::Forbidden,
        Self::QuotaExceeded,
        Self::NotFound,
        Self::UpstreamError,
        Self::RateLimited,
        Self::BadRegion,
        Self::Validation,
        Self::Internal,
    ];

    /// v1 `DEFAULT_STATUS`.
    pub fn default_status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::UpstreamError => StatusCode::BAD_GATEWAY,
            Self::RateLimited => StatusCode::SERVICE_UNAVAILABLE,
            Self::BadRegion | Self::Validation => StatusCode::BAD_REQUEST,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub status: StatusCode,
    /// Seconds. Sent in the body and as a `Retry-After` header.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status: code.default_status(),
            retry_after: None,
        }
    }

    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    // Constructors with v1's default messages.

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(ErrorCode::Unauthorized, "Missing or invalid API key")
    }

    pub fn bad_region(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRegion, message)
    }

    pub fn rate_limited(retry_after_seconds: u64) -> Self {
        Self::new(ErrorCode::RateLimited, "Upstream rate limit budget exceeded")
            .with_retry_after(retry_after_seconds)
    }

    pub fn upstream() -> Self {
        Self::new(ErrorCode::UpstreamError, "Upstream request failed")
    }

    pub fn internal() -> Self {
        Self::new(ErrorCode::Internal, "Internal server error")
    }

    /// The JSON envelope, with the current request's id when one is in scope.
    pub fn envelope(&self) -> Envelope<'_> {
        Envelope {
            error: Body {
                code: self.code,
                message: &self.message,
                request_id: request_id::current().map(|id| id.0),
                retry_after: self.retry_after,
            },
        }
    }
}

/// The error envelope as documented in OpenAPI (v1 `ErrorResponse`, plus `requestId`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ErrorResponse)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    /// The request's `X-Request-Id`.
    pub request_id: Option<String>,
    /// Seconds to wait before retrying; also sent as `Retry-After`.
    pub retry_after: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Envelope<'a> {
    pub error: Body<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Body<'a> {
    pub code: ErrorCode,
    pub message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 5xx is our bug; 4xx is the caller's (v1's log levels).
        if self.status.is_server_error() {
            tracing::error!(code = ?self.code, status = self.status.as_u16(), message = %self.message, "request failed");
        } else {
            tracing::warn!(code = ?self.code, status = self.status.as_u16(), message = %self.message, "request failed");
        }
        let mut res = (self.status, Json(self.envelope())).into_response();
        if let Some(seconds) = self.retry_after {
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::request_id::{CURRENT, RequestId};

    fn json(err: &ApiError) -> serde_json::Value {
        serde_json::to_value(err.envelope()).unwrap()
    }

    /// v1 test/errors.test.ts "maps each code to its documented status".
    #[test]
    fn maps_each_code_to_its_documented_status() {
        let cases = [
            (ErrorCode::Unauthorized, 401),
            (ErrorCode::Forbidden, 403),
            (ErrorCode::QuotaExceeded, 429),
            (ErrorCode::NotFound, 404),
            (ErrorCode::UpstreamError, 502),
            (ErrorCode::RateLimited, 503),
            (ErrorCode::BadRegion, 400),
            (ErrorCode::Validation, 400),
            (ErrorCode::Internal, 500),
        ];
        for (code, status) in cases {
            assert_eq!(ApiError::new(code, "x").status.as_u16(), status, "{code:?}");
        }
        assert_eq!(cases.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn codes_serialise_as_v1_strings() {
        let names: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "UNAUTHORIZED",
                "FORBIDDEN",
                "QUOTA_EXCEEDED",
                "NOT_FOUND",
                "UPSTREAM_ERROR",
                "RATE_LIMITED",
                "BAD_REGION",
                "VALIDATION",
                "INTERNAL"
            ]
        );
    }

    /// v1 test/errors.test.ts "serialises to the single envelope shape, omitting
    /// retryAfter when absent". v2 also omits requestId outside a request.
    #[test]
    fn serialises_to_the_single_envelope_shape() {
        assert_eq!(
            json(&ApiError::not_found("gone")),
            serde_json::json!({"error": {"code": "NOT_FOUND", "message": "gone"}})
        );
        assert_eq!(
            json(&ApiError::rate_limited(3)),
            serde_json::json!({"error": {
                "code": "RATE_LIMITED",
                "message": "Upstream rate limit budget exceeded",
                "retryAfter": 3
            }})
        );
    }

    #[tokio::test]
    async fn includes_the_request_id_in_scope() {
        let body = CURRENT
            .scope(RequestId("01TESTREQUEST".into()), async {
                json(&ApiError::rate_limited(2))
            })
            .await;
        assert_eq!(
            body,
            serde_json::json!({"error": {
                "code": "RATE_LIMITED",
                "message": "Upstream rate limit budget exceeded",
                "requestId": "01TESTREQUEST",
                "retryAfter": 2
            }})
        );
    }

    #[test]
    fn response_has_status_and_retry_after_header() {
        let res = ApiError::rate_limited(7).into_response();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers()[header::RETRY_AFTER], "7");
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/json");

        let res = ApiError::internal()
            .with_status(StatusCode::NOT_IMPLEMENTED)
            .into_response();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(!res.headers().contains_key(header::RETRY_AFTER));
    }
}

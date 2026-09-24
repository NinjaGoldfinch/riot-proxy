//! HTTP-layer building blocks shared by every route (docs/design/03 §Module layout).

pub mod auth;
pub mod error;
pub mod quota;
pub mod request_id;
pub mod validate;

pub use error::{ApiError, ErrorCode};

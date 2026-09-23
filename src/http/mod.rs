//! HTTP-layer building blocks shared by every route (docs/design/03 §Module layout).

pub mod error;
pub mod request_id;

pub use error::{ApiError, ErrorCode};

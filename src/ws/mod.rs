//! Realtime (docs/design/06 §Realtime): a topic hub of `broadcast` channels and
//! one task per socket speaking v1's protocol (ADR-045). Auth and the `/v1/ws`
//! route arrive in P6-07; events in P6-02.

pub mod hub;
pub mod metrics;
pub mod protocol;
pub mod socket;

pub use hub::Hub;
pub use protocol::Topic;
pub use socket::{Client, serve_socket};

//! The header-driven rate limiter (docs/design/05). Header parsing lands first
//! (P1-04); buckets, acquire and observe arrive in P2.

pub mod headers;

//! riot-proxy v2 — a single-binary proxy in front of the Riot Games API.
//! Module layout follows docs/design/03-architecture.md#module-layout.

pub mod app;
pub mod archive;
pub mod cache;
pub mod cli;
pub mod clock;
pub mod config;
pub mod consumers;
pub mod db;
pub mod events;
pub mod fetcher;
pub mod http;
pub mod jobs;
pub mod metrics;
pub mod players;
pub mod riot;
pub mod routes;
pub mod singleflight;
pub mod r#static;
pub mod telemetry;
pub mod ws;

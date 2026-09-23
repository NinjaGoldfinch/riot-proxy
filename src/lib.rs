//! riot-proxy v2 — a single-binary proxy in front of the Riot Games API.
//! Module layout follows docs/design/03-architecture.md#module-layout.

pub mod app;
pub mod cli;
pub mod config;
pub mod consumers;
pub mod db;
pub mod http;
pub mod metrics;
pub mod routes;
pub mod telemetry;

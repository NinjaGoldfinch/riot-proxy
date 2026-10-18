//! The integration tests, one binary (CI-04, ADR-176). Each `tests/*.rs` is
//! a module here, so `tests/common` compiles once and one test binary links,
//! not one per file. `Cargo.toml` sets `autotests = false`: a new test file
//! must be listed below, and `every_test_file_is_a_module` fails until it is.
//! A file that needs a process of its own (`telemetry.rs` reads the global
//! metrics recorder, which every other test writes to) is a `[[test]]` target
//! in `Cargo.toml` instead.
//! Run one file's tests with `cargo test --test it <file>::`.

mod common;

mod admin_jobs;
mod admin_routes;
mod analytics;
mod archive;
mod archive_jobs;
mod auth;
mod builds;
mod cache_l2;
mod cli;
mod cors;
mod dashboard;
mod ddragon;
mod dev_reset;
mod fetcher_states;
mod http;
mod job_activity;
mod jobs;
mod ladder;
mod ladder_probe;
mod lanes;
mod limiter_restart;
mod limiter_soak;
mod lol_routes;
mod maintenance;
mod migrate_v1;
mod offload;
mod openapi;
mod p6_exit;
mod p7_exit;
mod players_routes;
mod poll;
mod replay;
mod riot_client;
mod riot_routes;
mod shutdown;
mod timelines;
mod ui;
mod ws_hub;
mod ws_route;
mod ws_soak;

/// A `tests/*.rs` file that is neither a module above nor a target of its own
/// would never compile or run.
#[test]
fn every_test_file_is_a_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests");
    let mut listed: std::collections::BTreeSet<String> = include_str!("it.rs")
        .lines()
        .filter_map(|l| l.strip_prefix("mod ")?.strip_suffix(';').map(String::from))
        .collect();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    listed.extend(manifest.lines().filter_map(|l| {
        let path = l.strip_prefix("path = \"tests/")?.strip_suffix(".rs\"")?;
        Some(path.to_string())
    }));
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    files.sort();
    let missing: Vec<&String> = files.iter().filter(|m| !listed.contains(*m)).collect();
    assert!(missing.is_empty(), "add to tests/it.rs: {missing:?}");
    assert!(files.len() > 40, "{files:?}");
}

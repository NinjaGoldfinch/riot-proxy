//! Build identity for `GET /v1/admin/info` (ADM-03): the commit, the compiler
//! and the build time, as compile-time environment variables.
//!
//! - `GIT_SHA`: the environment's `GIT_SHA` (CI and the Docker build pass it,
//!   since the image's context has no `.git`), else `git rev-parse`, else
//!   `unknown`. Shortened to 12 characters.
//! - `RUSTC_VERSION`: `rustc --version` of the compiler cargo runs.
//! - `BUILD_UNIX_MS`: `SOURCE_DATE_EPOCH` (seconds) when set, else now.
//!
//! Nothing here can fail a build: every probe falls back.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const SHA_LEN: usize = 12;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let sha = std::env::var("GIT_SHA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(git_sha)
        .map_or_else(|| "unknown".to_string(), |s| s.chars().take(SHA_LEN).collect());
    println!("cargo:rustc-env=GIT_SHA={sha}");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = run(Command::new(rustc).arg("--version")).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RUSTC_VERSION={version}");

    let built_ms = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|s| s.saturating_mul(1000))
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok())
        })
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_UNIX_MS={built_ms}");
}

/// The checked-out commit, and the files that change when it does, so a local
/// build after a commit reruns this script.
fn git_sha() -> Option<String> {
    let git_dir = run(Command::new("git").args(["rev-parse", "--git-dir"])).map(PathBuf::from)?;
    watch(&git_dir.join("HEAD"));
    if let Some(common) = run(Command::new("git").args(["rev-parse", "--git-common-dir"])) {
        let common = PathBuf::from(common);
        watch(&common.join("packed-refs"));
        if let Some(head) = run(Command::new("git").args(["symbolic-ref", "-q", "HEAD"])) {
            watch(&common.join(head));
        }
    }
    run(Command::new("git").args(["rev-parse", "HEAD"]))
}

fn watch(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

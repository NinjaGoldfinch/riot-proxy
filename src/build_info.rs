//! What this binary is (ADM-03): set at compile time by `build.rs`.

use serde::Serialize;

/// `Cargo.toml`'s version, as `--version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit, 12 characters; `unknown` when the build had neither `GIT_SHA`
/// nor a git checkout.
pub const GIT_SHA: &str = env!("GIT_SHA");
/// `rustc --version` of the compiler that built it.
pub const RUSTC_VERSION: &str = env!("RUSTC_VERSION");
const BUILD_UNIX_MS: &str = env!("BUILD_UNIX_MS");
/// The release tag's version without `v`; empty unless `release.yml` built it
/// from a tag (STS-01).
const RELEASE_VERSION: &str = env!("RELEASE_VERSION");

/// When it was built, unix ms (`SOURCE_DATE_EPOCH` when the build set it).
pub fn built_at_ms() -> i64 {
    BUILD_UNIX_MS.parse().unwrap_or(0)
}

/// The build as the shared status contract names it (design/14 §Build
/// identity): `riot-proxy build-info` prints it, `serve` logs it at start and
/// `GET /status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Identity {
    /// The release's version, or `dev` for any build that is not a release.
    pub version: &'static str,
    /// The commit, 7 characters.
    pub commit: &'static str,
    /// RFC 3339 UTC, to the second.
    pub built_at: String,
}

impl Identity {
    pub fn current() -> Self {
        Self::of(RELEASE_VERSION, GIT_SHA, built_at_ms())
    }

    fn of(release: &'static str, sha: &'static str, built_ms: i64) -> Self {
        Self {
            version: if release.is_empty() { "dev" } else { release },
            commit: sha.get(..7).unwrap_or(sha),
            built_at: crate::clock::rfc3339_s(built_ms).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_script_set_every_value() {
        assert_eq!(GIT_SHA.len().min(12), GIT_SHA.len(), "at most 12 characters");
        assert!(!GIT_SHA.is_empty());
        assert!(RUSTC_VERSION.starts_with("rustc ") || RUSTC_VERSION == "unknown");
        assert!(built_at_ms() > 1_700_000_000_000, "{BUILD_UNIX_MS}");
        assert!(!RELEASE_VERSION.starts_with('v'), "{RELEASE_VERSION}");
    }

    #[test]
    fn a_build_that_is_not_a_release_is_dev() {
        let id = Identity::of("", "0123456789ab", 1_762_247_520_000);
        assert_eq!(id.version, "dev");
        assert_eq!(id.commit, "0123456");
        assert_eq!(id.built_at, "2025-11-04T09:12:00Z");
    }

    #[test]
    fn a_release_reports_its_version() {
        let id = Identity::of("2.11.0", "a1b2c3d4e5f6", 0);
        assert_eq!(id.version, "2.11.0");
        assert_eq!(id.built_at, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn an_unknown_commit_stays_unknown() {
        assert_eq!(Identity::of("", "unknown", 0).commit, "unknown");
        assert_eq!(Identity::of("", "abc", 0).commit, "abc");
    }

    #[test]
    fn it_serialises_in_the_contract_names() {
        let json = serde_json::to_value(Identity::of("2.11.0", "a1b2c3d4e5f6", 0)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"version": "2.11.0", "commit": "a1b2c3d", "built_at": "1970-01-01T00:00:00Z"})
        );
    }
}

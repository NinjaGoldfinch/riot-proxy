//! What this binary is (ADM-03): set at compile time by `build.rs`.

/// `Cargo.toml`'s version, as `--version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit, 12 characters; `unknown` when the build had neither `GIT_SHA`
/// nor a git checkout.
pub const GIT_SHA: &str = env!("GIT_SHA");
/// `rustc --version` of the compiler that built it.
pub const RUSTC_VERSION: &str = env!("RUSTC_VERSION");
const BUILD_UNIX_MS: &str = env!("BUILD_UNIX_MS");
/// The tag's version on a release build, empty on any other (STS-01).
const RELEASE_VERSION: &str = env!("RELEASE_VERSION");
/// The contract's `commit` is 7 characters; `/v1/admin/info` keeps 12 (ADR-203).
const SHORT_SHA_LEN: usize = 7;

/// When it was built, unix ms (`SOURCE_DATE_EPOCH` when the build set it).
pub fn built_at_ms() -> i64 {
    BUILD_UNIX_MS.parse().unwrap_or(0)
}

/// `/status`'s `version`: the release tag without `v`, or `dev` for a build
/// that is not a release (design/14 §Build identity). `--version`, `/readyz`
/// and `/v1/admin/info` keep [`VERSION`].
pub fn status_version() -> &'static str {
    release_or_dev(RELEASE_VERSION)
}

/// `/status`'s `commit`: the first 7 characters of [`GIT_SHA`].
pub fn short_commit() -> &'static str {
    GIT_SHA.get(..SHORT_SHA_LEN).unwrap_or(GIT_SHA)
}

/// `/status`'s `built_at`: RFC 3339, UTC, to the second.
pub fn built_at_rfc3339() -> String {
    rfc3339(built_at_ms())
}

fn release_or_dev(release: &str) -> &str {
    if release.is_empty() { "dev" } else { release }
}

/// Unix ms as RFC 3339, UTC, to the second: every `/status` timestamp.
pub fn rfc3339(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
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
    }

    #[test]
    fn a_build_without_release_version_is_dev() {
        assert_eq!(release_or_dev(""), "dev");
        assert_eq!(release_or_dev("2.11.0"), "2.11.0");
        assert_eq!(status_version(), release_or_dev(RELEASE_VERSION));
    }

    #[test]
    fn the_commit_is_seven_characters_of_the_sha() {
        assert!(GIT_SHA.starts_with(short_commit()));
        assert_eq!(short_commit().len(), GIT_SHA.len().min(7));
    }

    #[test]
    fn built_at_is_rfc3339_utc_to_the_second() {
        assert_eq!(rfc3339(1_793_783_520_000), "2026-11-04T09:12:00Z");
        assert_eq!(rfc3339(1_793_783_520_999), "2026-11-04T09:12:00Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert!(built_at_rfc3339().parse::<jiff::Timestamp>().is_ok());
    }
}

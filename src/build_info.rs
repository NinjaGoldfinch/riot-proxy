//! What this binary is (ADM-03): set at compile time by `build.rs`.

/// `Cargo.toml`'s version, as `--version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit, 12 characters; `unknown` when the build had neither `GIT_SHA`
/// nor a git checkout.
pub const GIT_SHA: &str = env!("GIT_SHA");
/// `rustc --version` of the compiler that built it.
pub const RUSTC_VERSION: &str = env!("RUSTC_VERSION");
const BUILD_UNIX_MS: &str = env!("BUILD_UNIX_MS");

/// When it was built, unix ms (`SOURCE_DATE_EPOCH` when the build set it).
pub fn built_at_ms() -> i64 {
    BUILD_UNIX_MS.parse().unwrap_or(0)
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
}

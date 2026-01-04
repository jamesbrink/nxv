//! Version information for the nxv binary.
//!
//! This module provides version strings that include git revision information
//! when built via Nix flake (which sets the NXV_GIT_REV environment variable).

use std::sync::LazyLock;

/// The package version from Cargo.toml.
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Git revision from Nix build (empty string if not set).
pub const GIT_REV: &str = match option_env!("NXV_GIT_REV") {
    Some(rev) => rev,
    None => "",
};

/// Static full version string for clap compatibility.
static FULL_VERSION: LazyLock<String> = LazyLock::new(|| {
    if GIT_REV.is_empty() {
        PKG_VERSION.to_string()
    } else {
        format!("{} ({})", PKG_VERSION, GIT_REV)
    }
});

/// Static long version string for clap compatibility.
static LONG_VERSION: LazyLock<String> = LazyLock::new(|| {
    let mut version = full_version();
    if !GIT_REV.is_empty() {
        version.push_str("\nBuilt from git revision");
    }
    version
});

/// Returns the full version string for display.
///
/// If built with a git revision (via Nix flake), returns `"X.Y.Z (abcdef0)"`.
/// Otherwise, returns just `"X.Y.Z"`.
///
/// # Examples
///
/// ```
/// use nxv::version::full_version;
///
/// let version = full_version();
/// assert!(version.starts_with(env!("CARGO_PKG_VERSION")));
/// ```
pub fn full_version() -> String {
    FULL_VERSION.clone()
}

/// Returns the version string for clap's version flag.
///
/// This is a static string suitable for clap's `#[command(version = ...)]`.
pub fn clap_version() -> &'static str {
    PKG_VERSION
}

/// Returns the long version string for clap's long_version flag.
///
/// This includes the git revision if available, and is suitable for
/// `--version` output. Returns a static reference for clap compatibility.
pub fn long_version() -> &'static str {
    // LazyLock ensures this is initialized only once and returns &'static str
    LONG_VERSION.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkg_version_matches_cargo() {
        assert_eq!(PKG_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_full_version_contains_pkg_version() {
        let version = full_version();
        assert!(version.contains(PKG_VERSION));
    }

    #[test]
    fn test_full_version_format() {
        let version = full_version();
        if GIT_REV.is_empty() {
            // When no git rev, should just be the version
            assert_eq!(version, PKG_VERSION);
        } else {
            // When git rev present, should be "version (rev)"
            assert!(version.contains('('));
            assert!(version.contains(')'));
            assert!(version.contains(GIT_REV));
        }
    }

    #[test]
    fn test_clap_version_is_static() {
        let version: &'static str = clap_version();
        assert_eq!(version, PKG_VERSION);
    }

    #[test]
    fn test_long_version_contains_full_version() {
        let long = long_version();
        let full = full_version();
        assert!(long.starts_with(&full));
    }
}

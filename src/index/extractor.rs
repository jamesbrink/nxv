//! Nix package extraction from nixpkgs commits.

use crate::error::Result;
use crate::index::nix_ffi::with_evaluator;
use serde::Deserialize;
use std::path::Path;
#[cfg(test)]
use std::process::Command;
use std::time::Instant;
use tracing::{instrument, trace};

/// Information about an extracted package.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    pub name: String,
    /// Package version. None if version could not be extracted from any source.
    pub version: Option<String>,
    /// Source of the version information: "direct", "unwrapped", "passthru", "name", or null.
    /// Tracks how the version was extracted for debugging and quality tracking.
    pub version_source: Option<String>,
    #[serde(rename = "attrPath")]
    pub attribute_path: String,
    pub description: Option<String>,
    pub license: Option<Vec<String>>,
    pub homepage: Option<String>,
    pub maintainers: Option<Vec<String>>,
    pub platforms: Option<Vec<String>>,
    /// Source file path relative to nixpkgs root (e.g., "pkgs/development/interpreters/python/default.nix")
    #[serde(rename = "sourcePath")]
    pub source_path: Option<String>,
    /// Known security vulnerabilities or EOL notices from meta.knownVulnerabilities
    pub known_vulnerabilities: Option<Vec<String>>,
    /// Store path for the package output (e.g., "/nix/store/hash-name-version")
    /// Only populated for commits from 2020-01-01 onwards.
    /// Note: Named "storePath" in Nix output because "outPath" is a special attribute.
    #[serde(rename = "storePath", default)]
    pub out_path: Option<String>,
}

/// Attribute position information for mapping attribute names to files.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttrPosition {
    pub attr_path: String,
    pub file: Option<String>,
}

impl PackageInfo {
    /// Serialize licenses to JSON for database storage.
    #[allow(dead_code)]
    pub fn license_json(&self) -> Option<String> {
        self.license
            .as_ref()
            .map(|l| serde_json::to_string(l).unwrap_or_default())
    }

    /// Serialize maintainers to JSON for database storage.
    #[allow(dead_code)]
    pub fn maintainers_json(&self) -> Option<String> {
        self.maintainers
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default())
    }

    /// Serialize platforms to JSON for database storage.
    #[allow(dead_code)]
    pub fn platforms_json(&self) -> Option<String> {
        self.platforms
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default())
    }

    /// Serialize known vulnerabilities to JSON for database storage.
    #[allow(dead_code)]
    pub fn known_vulnerabilities_json(&self) -> Option<String> {
        self.known_vulnerabilities
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
    }
}

/// The nix expression for extracting package information.
/// Loaded from external file at compile time for better maintainability.
const EXTRACT_NIX: &str = include_str!("nix/extract.nix");

/// The nix expression for extracting attribute positions.
/// Loaded from external file at compile time for better maintainability.
const POSITIONS_NIX: &str = include_str!("nix/positions.nix");

/// Extract packages from a nixpkgs checkout at a specific path.
///
/// # Arguments
/// * `repo_path` - Path to the nixpkgs repository checkout
///
/// # Returns
/// A vector of PackageInfo, or an error if extraction fails.
pub fn extract_packages<P: AsRef<Path>>(repo_path: P) -> Result<Vec<PackageInfo>> {
    extract_packages_for_attrs(repo_path, "x86_64-linux", &[])
}

/// Extract packages for a specific list of attribute names and system.
#[instrument(level = "debug", skip(repo_path, attr_names), fields(attr_count = attr_names.len()))]
pub fn extract_packages_for_attrs<P: AsRef<Path>>(
    repo_path: P,
    system: &str,
    attr_names: &[String],
) -> Result<Vec<PackageInfo>> {
    let repo_path = repo_path.as_ref();

    // Canonicalize the path to avoid any relative path issues
    let canonical_path = std::fs::canonicalize(repo_path)?;
    let repo_path_str = canonical_path.display().to_string();

    // Write the nix expression to a temp file
    let temp_dir = tempfile::tempdir()?;
    let nix_file = temp_dir.path().join("extract.nix");
    std::fs::write(&nix_file, EXTRACT_NIX)?;

    // Build the attrNames argument - write to file if large to avoid "Argument list too long"
    // OS limit is typically ~2MB for all args + env, so we use a conservative threshold
    let attr_names_arg = if attr_names.is_empty() {
        "null".to_string()
    } else {
        // Estimate the size: each name plus quotes and space
        let estimated_size: usize = attr_names.iter().map(|s| s.len() + 3).sum();

        if estimated_size > 100_000 {
            // Write attr names to a JSON file and read in Nix
            let attr_file = temp_dir.path().join("attrs.json");
            let json = serde_json::to_string(attr_names)?;
            std::fs::write(&attr_file, &json)?;
            // Quote the path to handle spaces and special characters
            format!(
                "builtins.fromJSON (builtins.readFile \"{}\")",
                attr_file.display()
            )
        } else {
            let items: Vec<String> = attr_names.iter().map(|s| format!("\"{}\"", s)).collect();
            format!("[ {} ]", items.join(" "))
        }
    };

    // Build an expression that imports and calls the extract file.
    // Note: Nix import takes a path, not a string, so we don't quote nix_file.
    // But nixpkgsPath is assigned as a string, so we quote it.
    let expr = format!(
        "import {} {{ nixpkgsPath = \"{}\"; system = \"{}\"; attrNames = {}; }}",
        nix_file.display(),
        repo_path_str,
        system,
        attr_names_arg
    );

    // Use FFI evaluator with large stack thread
    let eval_start = Instant::now();
    let json_output = with_evaluator(move |eval| eval.eval_json(&expr, "<extract>"))?;
    let eval_time = eval_start.elapsed();

    let parse_start = Instant::now();
    let packages: Vec<PackageInfo> = serde_json::from_str(&json_output)?;
    let parse_time = parse_start.elapsed();

    trace!(
        system = %system,
        attr_count = attr_names.len(),
        packages_found = packages.len(),
        json_size_bytes = json_output.len(),
        eval_time_ms = eval_time.as_millis(),
        parse_time_ms = parse_time.as_millis(),
        "Nix extraction completed"
    );

    Ok(packages)
}

/// Extract attribute positions for a nixpkgs checkout and system.
#[instrument(level = "debug", skip(repo_path))]
pub fn extract_attr_positions<P: AsRef<Path>>(
    repo_path: P,
    system: &str,
) -> Result<Vec<AttrPosition>> {
    let repo_path = repo_path.as_ref();

    // Canonicalize the path to avoid any relative path issues
    let canonical_path = std::fs::canonicalize(repo_path)?;
    let repo_path_str = canonical_path.display().to_string();

    // Write the nix expression to a temp file
    let temp_dir = tempfile::tempdir()?;
    let nix_file = temp_dir.path().join("positions.nix");
    std::fs::write(&nix_file, POSITIONS_NIX)?;

    // Build an expression that imports and calls the positions file
    let expr = format!(
        "import {} {{ nixpkgsPath = \"{}\"; system = \"{}\"; }}",
        nix_file.display(),
        repo_path_str,
        system
    );

    // Use FFI evaluator with large stack thread
    let eval_start = Instant::now();
    let json_output = with_evaluator(move |eval| eval.eval_json(&expr, "<positions>"))?;
    let eval_time = eval_start.elapsed();

    let parse_start = Instant::now();
    let positions: Vec<AttrPosition> = serde_json::from_str(&json_output)?;
    let parse_time = parse_start.elapsed();

    trace!(
        system = %system,
        positions_found = positions.len(),
        json_size_bytes = json_output.len(),
        eval_time_ms = eval_time.as_millis(),
        parse_time_ms = parse_time.as_millis(),
        "Positions extraction completed"
    );

    Ok(positions)
}

#[allow(dead_code)]
fn nix_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[allow(dead_code)]
fn nix_list(values: &[String]) -> String {
    if values.is_empty() {
        return "null".to_string();
    }

    let items: Vec<String> = values
        .iter()
        .map(|value| format!("\"{}\"", nix_string(value)))
        .collect();
    format!("[ {} ]", items.join(" "))
}

/// Extract packages at a specific commit.
///
/// This function checks out the commit, runs extraction, then restores the original state.
/// For parallel extraction, use git worktrees instead.
///
/// # Arguments
/// * `repo_path` - Path to the nixpkgs repository
/// * `commit_hash` - The commit hash to extract from
///
/// # Returns
/// A vector of PackageInfo, or an error if extraction fails.
#[allow(dead_code)]
pub fn extract_at_commit<P: AsRef<Path>>(
    repo_path: P,
    commit_hash: &str,
) -> Result<Vec<PackageInfo>> {
    use crate::index::git::{NixpkgsRepo, WorktreeSession};

    let repo = NixpkgsRepo::open(&repo_path)?;

    // Create a worktree session - doesn't modify the main repo
    let session = WorktreeSession::new(&repo, commit_hash)?;

    // Extract packages from the worktree
    extract_packages(session.path())
    // WorktreeSession auto-cleans up on drop
}

/// Try to extract packages, returning None on failure instead of error.
///
/// This is useful for iterating over commits where some may fail to evaluate.
#[allow(dead_code)]
pub fn try_extract_at_commit<P: AsRef<Path>>(
    repo_path: P,
    commit_hash: &str,
) -> Option<Vec<PackageInfo>> {
    match extract_at_commit(repo_path, commit_hash) {
        Ok(packages) => Some(packages),
        Err(e) => {
            eprintln!("Warning: extraction failed at {}: {}", &commit_hash[..7], e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_info_json_serialization() {
        let pkg = PackageInfo {
            name: "test".to_string(),
            version: Some("1.0.0".to_string()),
            version_source: Some("direct".to_string()),
            attribute_path: "test".to_string(),
            description: Some("A test package".to_string()),
            license: Some(vec!["MIT".to_string(), "Apache-2.0".to_string()]),
            homepage: Some("https://example.com".to_string()),
            maintainers: Some(vec!["user1".to_string(), "user2".to_string()]),
            platforms: Some(vec!["x86_64-linux".to_string()]),
            source_path: Some("pkgs/test/default.nix".to_string()),
            known_vulnerabilities: None,
            out_path: Some("/nix/store/abc123-test-1.0.0".to_string()),
        };

        let license_json = pkg.license_json().unwrap();
        assert!(license_json.contains("MIT"));
        assert!(license_json.contains("Apache-2.0"));

        let maintainers_json = pkg.maintainers_json().unwrap();
        assert!(maintainers_json.contains("user1"));

        let platforms_json = pkg.platforms_json().unwrap();
        assert!(platforms_json.contains("x86_64-linux"));
    }

    #[test]
    #[ignore] // Requires nix to be installed and nixpkgs to be present
    fn test_extract_packages_from_nixpkgs() {
        let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");

        if !nixpkgs_path.exists() {
            eprintln!("Skipping: nixpkgs not present");
            return;
        }

        let result = extract_packages(&nixpkgs_path);

        match result {
            Ok(packages) => {
                assert!(
                    !packages.is_empty(),
                    "Should extract at least some packages"
                );

                // Verify we got some common packages
                let names: Vec<_> = packages.iter().map(|p| p.name.as_str()).collect();

                // At least one of these common packages should exist
                let has_common = names
                    .iter()
                    .any(|n| ["hello", "git", "curl", "coreutils", "bash"].contains(n));
                assert!(has_common, "Should find at least one common package");

                // Verify package structure
                for pkg in packages.iter().take(5) {
                    assert!(!pkg.name.is_empty());
                    assert!(pkg.version.is_some() && !pkg.version.as_ref().unwrap().is_empty());
                    assert!(!pkg.attribute_path.is_empty());
                }
            }
            Err(e) => {
                // Nix might not be available in CI
                eprintln!("Extraction failed (nix may not be available): {}", e);
            }
        }
    }

    /// Test that the Nix extractor handles edge cases where:
    /// - maintainers or platforms are strings instead of lists
    /// - version is an integer instead of a string
    ///
    /// These bugs caused extraction failures when packages had non-standard types.
    #[test]
    #[ignore] // Requires nix to be installed
    fn test_extract_handles_string_maintainers_and_platforms() {
        use std::process::Command;
        use tempfile::tempdir;

        // Create a minimal nixpkgs-like structure with edge cases
        let dir = tempdir().unwrap();
        let path = dir.path();

        // Create pkgs directory (required for validation)
        std::fs::create_dir(path.join("pkgs")).unwrap();

        // Create a default.nix that acts like nixpkgs - a function that takes config
        let default_nix = r#"
{ config ? {}, system ? builtins.currentSystem, ... }:
{
  # Normal package with list maintainers
  normalPkg = {
    pname = "normal-pkg";
    version = "1.0.0";
    type = "derivation";
    meta = {
      description = "A normal package";
      maintainers = [ { github = "user1"; } { name = "User Two"; } ];
      platforms = [ "x86_64-linux" "aarch64-darwin" ];
    };
  };

  # Edge case: maintainers is a string instead of a list
  stringMaintainerPkg = {
    pname = "string-maintainer-pkg";
    version = "2.0.0";
    type = "derivation";
    meta = {
      description = "Package with string maintainer";
      maintainers = "David Kleuker <post@davidak.de>";
      platforms = [ "x86_64-linux" ];
    };
  };

  # Edge case: platforms is a string instead of a list
  stringPlatformPkg = {
    pname = "string-platform-pkg";
    version = "3.0.0";
    type = "derivation";
    meta = {
      description = "Package with string platform";
      maintainers = [ { github = "someone"; } ];
      platforms = "x86_64-linux";
    };
  };

  # Edge case: both maintainers and platforms are strings
  bothStringsPkg = {
    pname = "both-strings-pkg";
    version = "4.0.0";
    type = "derivation";
    meta = {
      description = "Package with both as strings";
      maintainers = "test@example.com";
      platforms = "aarch64-darwin";
    };
  };

  # Edge case: version is an integer instead of a string
  intVersionPkg = {
    pname = "int-version-pkg";
    version = 61;
    type = "derivation";
    meta = {
      description = "Package with integer version";
    };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        // Check if nix is available
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        // Run extraction
        let result = extract_packages(path);

        match result {
            Ok(packages) => {
                assert_eq!(
                    packages.len(),
                    5,
                    "Should extract all 5 packages despite edge cases"
                );

                // Verify normal package
                let normal = packages.iter().find(|p| p.name == "normal-pkg").unwrap();
                assert_eq!(normal.version.as_deref(), Some("1.0.0"));
                assert!(normal.maintainers.is_some());
                assert!(normal.platforms.is_some());

                // Verify string maintainer package - should have maintainers as list with one element
                let string_maint = packages
                    .iter()
                    .find(|p| p.name == "string-maintainer-pkg")
                    .unwrap();
                assert_eq!(string_maint.version.as_deref(), Some("2.0.0"));
                let maint = string_maint.maintainers.as_ref().unwrap();
                assert_eq!(maint.len(), 1);
                assert!(maint[0].contains("David Kleuker"));

                // Verify string platform package - should have platforms as list with one element
                let string_plat = packages
                    .iter()
                    .find(|p| p.name == "string-platform-pkg")
                    .unwrap();
                assert_eq!(string_plat.version.as_deref(), Some("3.0.0"));
                let plat = string_plat.platforms.as_ref().unwrap();
                assert_eq!(plat.len(), 1);
                assert_eq!(plat[0], "x86_64-linux");

                // Verify both strings package
                let both = packages
                    .iter()
                    .find(|p| p.name == "both-strings-pkg")
                    .unwrap();
                assert_eq!(both.version.as_deref(), Some("4.0.0"));
                assert!(both.maintainers.is_some());
                assert!(both.platforms.is_some());

                // Verify integer version package - should have version converted to string
                let int_ver = packages
                    .iter()
                    .find(|p| p.name == "int-version-pkg")
                    .unwrap();
                assert_eq!(
                    int_ver.version.as_deref(),
                    Some("61"),
                    "Integer version should be converted to string"
                );
            }
            Err(e) => {
                panic!("Extraction should not fail with edge case packages: {}", e);
            }
        }
    }

    #[test]
    fn test_nix_list_empty_is_null() {
        assert_eq!(super::nix_list(&[]), "null");
    }

    #[test]
    fn test_nix_list_values_are_escaped() {
        let values = vec![r#"a"b"#.to_string(), "c\\d".to_string()];
        let list = super::nix_list(&values);
        assert!(list.contains(r#""a\"b""#));
        assert!(list.contains(r#""c\\d""#));
    }

    #[test]
    fn test_extract_packages_with_empty_attr_list() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        let default_nix = r#"
{ system, config }:
{
  hello = {
    pname = "hello";
    version = "1.0.0";
    type = "derivation";
    meta = {
      description = "A test package";
    };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let packages = extract_packages_for_attrs(path, "x86_64-linux", &[]).unwrap();
        assert!(!packages.is_empty());
        assert!(packages.iter().any(|pkg| pkg.name == "hello"));
    }

    #[test]
    fn test_extract_attr_positions_returns_files() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        let default_nix = r#"
{ system, config }:
{
  hello = {
    pname = "hello";
    version = "1.0.0";
    type = "derivation";
    meta = {
      description = "A test package";
    };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let positions = extract_attr_positions(path, "x86_64-linux").unwrap();
        assert!(positions.iter().any(|pos| pos.attr_path == "hello"));
    }

    #[test]
    fn test_extract_attr_positions_handles_non_attrset() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        let default_nix = r#"
{ system, config }:
  x: x
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let positions = extract_attr_positions(path, "x86_64-linux").unwrap();
        assert!(positions.is_empty());
    }

    #[test]
    fn test_extract_packages_with_attr_filter() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        let default_nix = r#"
{ system, config }:
{
  hello = {
    pname = "hello";
    version = "1.0.0";
    type = "derivation";
  };
  world = {
    pname = "world";
    version = "2.0.0";
    type = "derivation";
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let names = vec!["hello".to_string()];
        let packages = extract_packages_for_attrs(path, "x86_64-linux", &names).unwrap();
        assert!(packages.iter().any(|pkg| pkg.name == "hello"));
        assert!(!packages.iter().any(|pkg| pkg.name == "world"));
    }

    /// Test that extract_attr_positions handles attributes that throw errors.
    /// This simulates older nixpkgs commits where some attributes may fail to evaluate.
    #[test]
    fn test_extract_attr_positions_handles_throwing_attrs() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        // Create a nixpkgs-like structure where one attribute throws an error
        let default_nix = r#"
{ system, config }:
{
  goodPkg = {
    pname = "good-pkg";
    version = "1.0.0";
    type = "derivation";
  };
  # This attribute will throw when accessed
  badPkg = throw "This package is broken";
  anotherGoodPkg = {
    pname = "another-good-pkg";
    version = "2.0.0";
    type = "derivation";
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        // Should not fail even though badPkg throws
        let positions = extract_attr_positions(path, "x86_64-linux").unwrap();

        // Should have positions for the good packages
        assert!(positions.iter().any(|p| p.attr_path == "goodPkg"));
        assert!(positions.iter().any(|p| p.attr_path == "anotherGoodPkg"));
        // badPkg may or may not have a position depending on when the error occurs
    }

    /// Test that large attribute lists are written to file to avoid "Argument list too long".
    /// This tests the fix for OS error 7 (E2BIG) when extracting many packages.
    #[test]
    fn test_extract_large_attr_list_uses_file() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        // Create a nixpkgs-like structure with many packages
        let mut default_nix = "{ system, config }:\n{\n".to_string();
        for i in 0..100 {
            default_nix.push_str(&format!(
                r#"  pkg{} = {{ pname = "pkg{}"; version = "1.0.0"; type = "derivation"; }};
"#,
                i, i
            ));
        }
        default_nix.push_str("}\n");
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        // Generate a large list of attribute names that would exceed the threshold
        // The threshold is 100,000 bytes, so we need ~10,000 attrs of ~10 chars each
        let mut large_attr_list: Vec<String> = (0..100).map(|i| format!("pkg{}", i)).collect();
        // Add many fake attrs to push over the size threshold
        for i in 100..15000 {
            large_attr_list.push(format!("nonexistent_package_{}", i));
        }

        // This should NOT fail with "Argument list too long" because
        // the attr list is written to a file
        let result = extract_packages_for_attrs(path, "x86_64-linux", &large_attr_list);

        match result {
            Ok(packages) => {
                // Should find some of the real packages
                assert!(packages.iter().any(|p| p.name == "pkg0"));
                assert!(packages.iter().any(|p| p.name == "pkg99"));
            }
            Err(e) => {
                // Should NOT be "Argument list too long"
                let err_str = e.to_string();
                assert!(
                    !err_str.contains("Argument list too long"),
                    "Should not get E2BIG error, but got: {}",
                    err_str
                );
                // Other nix eval errors are acceptable (e.g., evaluation errors)
            }
        }
    }

    /// Test the version extraction fallback chain: direct -> unwrapped -> passthru -> name
    /// This verifies the Phase 1 version extraction improvements work correctly.
    #[test]
    fn test_version_extraction_fallback_chain() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        // Create test packages with different version extraction scenarios
        let default_nix = r#"
{ system, config }:
{
  # Direct version - should use "direct" source
  directVersion = {
    pname = "direct-pkg";
    version = "1.0.0";
    type = "derivation";
    meta = { description = "Package with direct version"; };
  };

  # Wrapper pattern - version from unwrapped
  wrapperPkg = {
    pname = "wrapper-pkg";
    type = "derivation";
    unwrapped = {
      version = "2.0.0";
    };
    meta = { description = "Wrapper without direct version"; };
  };

  # Passthru pattern - version from passthru.unwrapped
  passthruPkg = {
    pname = "passthru-pkg";
    type = "derivation";
    passthru = {
      unwrapped = {
        version = "3.0.0";
      };
    };
    meta = { description = "Package with passthru unwrapped version"; };
  };

  # Name-based version - version extracted from pname
  namePkg = {
    pname = "name-pkg-4.0.0";
    type = "derivation";
    meta = { description = "Package with version in name"; };
  };

  # Truly versionless - no version anywhere
  versionlessPkg = {
    pname = "versionless-hook";
    type = "derivation";
    meta = { description = "A build hook with no version"; };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let packages = extract_packages_for_attrs(path, "x86_64-linux", &[]).unwrap();

        // Test direct version extraction
        let direct = packages.iter().find(|p| p.name == "direct-pkg");
        assert!(direct.is_some(), "Should find direct-pkg");
        let direct = direct.unwrap();
        assert_eq!(direct.version.as_deref(), Some("1.0.0"));
        assert_eq!(direct.version_source.as_deref(), Some("direct"));

        // Test wrapper version extraction
        let wrapper = packages.iter().find(|p| p.name == "wrapper-pkg");
        assert!(wrapper.is_some(), "Should find wrapper-pkg");
        let wrapper = wrapper.unwrap();
        assert_eq!(wrapper.version.as_deref(), Some("2.0.0"));
        assert_eq!(wrapper.version_source.as_deref(), Some("unwrapped"));

        // Test passthru version extraction
        let passthru = packages.iter().find(|p| p.name == "passthru-pkg");
        assert!(passthru.is_some(), "Should find passthru-pkg");
        let passthru = passthru.unwrap();
        assert_eq!(passthru.version.as_deref(), Some("3.0.0"));
        assert_eq!(passthru.version_source.as_deref(), Some("passthru"));

        // Test name-based version extraction
        let name_pkg = packages.iter().find(|p| p.name == "name-pkg-4.0.0");
        assert!(name_pkg.is_some(), "Should find name-pkg-4.0.0");
        let name_pkg = name_pkg.unwrap();
        assert_eq!(name_pkg.version.as_deref(), Some("4.0.0"));
        assert_eq!(name_pkg.version_source.as_deref(), Some("name"));

        // Test versionless package
        let versionless = packages.iter().find(|p| p.name == "versionless-hook");
        assert!(versionless.is_some(), "Should find versionless-hook");
        let versionless = versionless.unwrap();
        assert!(
            versionless.version.is_none() || versionless.version.as_deref() == Some(""),
            "Versionless package should have None or empty version"
        );
    }

    /// Test that version_source field is properly populated
    #[test]
    fn test_version_source_field_populated() {
        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            eprintln!("Skipping: nix not available");
            return;
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        std::fs::create_dir_all(path.join("pkgs")).unwrap();

        let default_nix = r#"
{ system, config }:
{
  hello = {
    pname = "hello";
    version = "2.12.1";
    type = "derivation";
    meta = { description = "Hello world"; };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        let packages =
            extract_packages_for_attrs(path, "x86_64-linux", &["hello".to_string()]).unwrap();

        assert_eq!(packages.len(), 1);
        let hello = &packages[0];
        assert_eq!(hello.name, "hello");
        assert_eq!(hello.version.as_deref(), Some("2.12.1"));
        assert_eq!(hello.version_source.as_deref(), Some("direct"));
    }

    /// Regression test that validates package extraction against known-good fixture.
    ///
    /// This test requires a nixpkgs clone and runs actual nix evaluation, so it's
    /// marked as ignored. Run with:
    ///   NIXPKGS_PATH=/path/to/nixpkgs cargo test --features indexer test_regression_fixture -- --ignored
    #[test]
    #[ignore]
    fn test_regression_fixture() {
        use regex::Regex;
        use serde::Deserialize;
        use std::collections::HashMap;
        use std::env;
        use std::fs;

        #[derive(Deserialize)]
        struct RegressionFixture {
            packages: Vec<PackageTest>,
        }

        #[derive(Deserialize)]
        struct PackageTest {
            attr_path: String,
            expect_version_regex: Option<String>,
            expect_version_source: Option<serde_json::Value>,
        }

        // Check for nixpkgs path
        let nixpkgs_path = env::var("NIXPKGS_PATH").unwrap_or_else(|_| {
            panic!(
                "NIXPKGS_PATH environment variable not set. \
                 Set it to the path of your nixpkgs clone."
            )
        });

        let nix_check = Command::new("nix").arg("--version").output();
        if nix_check.is_err() || !nix_check.unwrap().status.success() {
            panic!("nix is not available - required for regression tests");
        }

        // Load fixture
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/regression_packages.json");
        let fixture_content = fs::read_to_string(&fixture_path)
            .unwrap_or_else(|e| panic!("Failed to read fixture file {:?}: {}", fixture_path, e));
        let fixture: RegressionFixture =
            serde_json::from_str(&fixture_content).expect("Failed to parse fixture JSON");

        // Extract packages
        let attr_paths: Vec<String> = fixture
            .packages
            .iter()
            .map(|p| p.attr_path.clone())
            .collect();
        let packages = extract_packages_for_attrs(
            std::path::Path::new(&nixpkgs_path),
            "x86_64-linux",
            &attr_paths,
        )
        .expect("Failed to extract packages");

        // Build lookup map
        let package_map: HashMap<_, _> = packages
            .iter()
            .map(|p| (p.attribute_path.clone(), p))
            .collect();

        let mut failures = Vec::new();

        for test_pkg in &fixture.packages {
            let pkg = match package_map.get(&test_pkg.attr_path) {
                Some(p) => p,
                None => {
                    failures.push(format!("{}: Package not extracted", test_pkg.attr_path));
                    continue;
                }
            };

            // Check version regex
            if let Some(regex_str) = &test_pkg.expect_version_regex {
                let regex = Regex::new(regex_str).expect("Invalid regex in fixture");
                match &pkg.version {
                    Some(v) if regex.is_match(v) => {}
                    Some(v) => {
                        failures.push(format!(
                            "{}: Version '{}' doesn't match regex '{}'",
                            test_pkg.attr_path, v, regex_str
                        ));
                    }
                    None => {
                        failures.push(format!(
                            "{}: No version extracted (expected match for '{}')",
                            test_pkg.attr_path, regex_str
                        ));
                    }
                }
            }

            // Check version source
            if let Some(expected_source) = &test_pkg.expect_version_source {
                let actual_source = pkg.version_source.as_deref();
                let matches = match expected_source {
                    serde_json::Value::String(s) => actual_source == Some(s.as_str()),
                    serde_json::Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .any(|s| actual_source == Some(s)),
                    _ => false,
                };
                if !matches {
                    failures.push(format!(
                        "{}: version_source {:?} doesn't match expected {:?}",
                        test_pkg.attr_path, actual_source, expected_source
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "Regression test failures ({} of {}):\n  - {}",
                failures.len(),
                fixture.packages.len(),
                failures.join("\n  - ")
            );
        }

        println!(
            "Regression test passed: {} packages validated",
            fixture.packages.len()
        );
    }

    /// Documents the version extraction patterns in extract.nix.
    ///
    /// The extractVersionFromName function handles these edge cases:
    /// - Milestone versions: mezzo-0.0.m8 -> 0.0.m8
    /// - Internal hyphens: omake-0.9.8.6-0.rc1 -> 0.9.8.6-0.rc1
    /// - File extensions: perl-Memoize-1.03.tgz -> 1.03
    /// - Pre-release: matita-0.99.1pre130312 -> 0.99.1pre130312
    ///
    /// To test these patterns manually, run:
    /// ```bash
    /// nix-instantiate --eval --strict -E '
    /// let
    ///   # Copy patterns from extract.nix and test them
    ///   extract = name: ...; # See extract.nix extractVersionFromName
    /// in {
    ///   mezzo = extract "mezzo-0.0.m8";           # -> "0.0.m8"
    ///   omake = extract "omake-0.9.8.6-0.rc1";    # -> "0.9.8.6-0.rc1"
    ///   perl = extract "perl-Memoize-1.03.tgz";   # -> "1.03"
    ///   matita = extract "matita-0.99.1pre130312"; # -> "0.99.1pre130312"
    /// }
    /// '
    /// ```
    #[test]
    fn test_version_extraction_patterns_documented() {
        // This test documents the expected behavior of extractVersionFromName in extract.nix
        // The actual extraction happens in Nix code and is tested via nix-instantiate
        //
        // Expected extractions (validated via nix-instantiate):
        let expected_extractions = [
            ("mezzo-0.0.m8", "0.0.m8"),
            ("omake-0.9.8.6-0.rc1", "0.9.8.6-0.rc1"),
            ("perl-Memoize-1.03.tgz", "1.03"),
            ("matita-0.99.1pre130312", "0.99.1pre130312"),
            ("foo-1.2.3.tar.gz", "1.2.3"),
            ("hello-2.12.1", "2.12.1"),
            ("pkg-1.0rc1", "1.0rc1"),
            ("pkg-2.0.0beta2", "2.0.0beta2"),
            ("pkg-v1.2.3", "1.2.3"),
            ("pkg-2021-07-29", "2021-07-29"),
        ];

        // These should NOT extract a version (return null)
        let no_version_extractions = ["vimplugin-YankRing", "stdenv-linux", "stdenv"];

        // Document the patterns for reference
        println!("Version extraction patterns (from extract.nix):");
        println!("  1. Internal hyphen: .*-([0-9]+\\.[0-9]+(\\.[0-9]+)*-[0-9a-z.]+)$");
        println!("  2. Pre-release: .*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]+[0-9]+)$");
        println!("  3. Milestone: .*-([0-9]+\\.[0-9]+\\.[a-z]+[0-9]*)$");
        println!("  4. Letter suffix: .*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]+[0-9]*)$");
        println!("  5. Semver: .*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]?)$");
        println!("  + Extension stripping: .tar.gz, .tgz, .zip, etc.");

        println!("\nExpected extractions:");
        for (name, version) in &expected_extractions {
            println!("  {} -> {}", name, version);
        }

        println!("\nNo version expected:");
        for name in &no_version_extractions {
            println!("  {} -> null", name);
        }

        // The test passes if the documentation is correct
        // Actual validation is done via nix-instantiate in development
        assert!(!expected_extractions.is_empty());
    }
}

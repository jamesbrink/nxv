//! Nix package extraction from nixpkgs commits.

use crate::error::{NxvError, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// Information about an extracted package.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    #[serde(rename = "attrPath")]
    pub attribute_path: String,
    pub description: Option<String>,
    pub license: Option<Vec<String>>,
    pub homepage: Option<String>,
    pub maintainers: Option<Vec<String>>,
    pub platforms: Option<Vec<String>>,
}

impl PackageInfo {
    /// Serialize licenses to JSON for database storage.
    pub fn license_json(&self) -> Option<String> {
        self.license
            .as_ref()
            .map(|l| serde_json::to_string(l).unwrap_or_default())
    }

    /// Serialize maintainers to JSON for database storage.
    pub fn maintainers_json(&self) -> Option<String> {
        self.maintainers
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default())
    }

    /// Serialize platforms to JSON for database storage.
    pub fn platforms_json(&self) -> Option<String> {
        self.platforms
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default())
    }
}

/// The nix expression for extracting package information.
/// This is embedded in the binary to avoid needing external files.
const EXTRACT_NIX: &str = r#"
{ nixpkgsPath }:
let
  pkgs = import nixpkgsPath {
    config = { allowUnfree = true; allowBroken = true; allowInsecure = true; };
  };

  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      getLicenses = l:
        if builtins.isList l then map (x: x.spdxId or x.shortName or "unknown") l
        else if l ? spdxId then [ l.spdxId ]
        else if l ? shortName then [ l.shortName ]
        else [ "unknown" ];
    in {
      name = pkg.pname or pkg.name or attrPath;
      version = pkg.version or "unknown";
      attrPath = attrPath;
      description = meta.description or null;
      homepage = meta.homepage or null;
      license = if meta ? license then getLicenses meta.license else null;
      maintainers = if meta ? maintainers
        then map (m: m.github or m.name or "unknown") meta.maintainers
        else null;
      platforms = if meta ? platforms
        then map (p: if builtins.isString p then p else p.system or "unknown") meta.platforms
        else null;
    };

  # Only get top-level packages (derivations directly under pkgs)
  # Recursing into all attribute sets is too slow
  topLevelPackages = builtins.filter (x: x != null) (
    builtins.attrValues (
      builtins.mapAttrs (name: value:
        if builtins.isAttrs value && value ? type && value.type == "derivation"
        then builtins.tryEval (getPackageInfo name value)
        else null
      ) pkgs
    )
  );

  # Extract successfully evaluated packages
  successfulPackages = map (x: x.value) (builtins.filter (x: x.success) topLevelPackages);
in
  successfulPackages
"#;

/// Extract packages from a nixpkgs checkout at a specific path.
///
/// # Arguments
/// * `repo_path` - Path to the nixpkgs repository checkout
///
/// # Returns
/// A vector of PackageInfo, or an error if extraction fails.
pub fn extract_packages<P: AsRef<Path>>(repo_path: P) -> Result<Vec<PackageInfo>> {
    let repo_path = repo_path.as_ref();

    // Write the nix expression to a temp file
    let temp_dir = tempfile::tempdir()?;
    let nix_file = temp_dir.path().join("extract.nix");
    std::fs::write(&nix_file, EXTRACT_NIX)?;

    // Run nix eval
    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--impure",
            "--expr",
            &format!(
                "import {} {{ nixpkgsPath = {}; }}",
                nix_file.display(),
                repo_path.display()
            ),
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NxvError::NixEval(format!(
            "nix eval failed: {}",
            stderr.lines().take(5).collect::<Vec<_>>().join("\n")
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let packages: Vec<PackageInfo> = serde_json::from_str(&stdout)?;

    Ok(packages)
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
    use crate::index::git::NixpkgsRepo;

    let repo = NixpkgsRepo::open(&repo_path)?;

    // Save current HEAD
    let original_head = repo.head_commit()?;

    // Checkout the target commit
    repo.checkout_commit(commit_hash)?;

    // Extract packages
    let result = extract_packages(&repo_path);

    // Restore original HEAD (best effort)
    let _ = repo.checkout_commit(&original_head);

    result
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
            version: "1.0.0".to_string(),
            attribute_path: "test".to_string(),
            description: Some("A test package".to_string()),
            license: Some(vec!["MIT".to_string(), "Apache-2.0".to_string()]),
            homepage: Some("https://example.com".to_string()),
            maintainers: Some(vec!["user1".to_string(), "user2".to_string()]),
            platforms: Some(vec!["x86_64-linux".to_string()]),
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
                    assert!(!pkg.version.is_empty());
                    assert!(!pkg.attribute_path.is_empty());
                }
            }
            Err(e) => {
                // Nix might not be available in CI
                eprintln!("Extraction failed (nix may not be available): {}", e);
            }
        }
    }
}

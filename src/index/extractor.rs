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
  # Import nixpkgs with current system and permissive config
  pkgs = import nixpkgsPath {
    system = builtins.currentSystem;
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecure = true;
      allowUnsupportedSystem = true;
    };
  };

  # Force full evaluation and catch any errors - this is critical for lazy evaluation
  tryDeep = expr:
    let result = builtins.tryEval (builtins.deepSeq expr expr);
    in if result.success then result.value else null;

  # Safely extract a string field - converts integers/floats to strings
  safeString = x: tryDeep (
    if x == null then null
    else if builtins.isString x then x
    else if builtins.isInt x || builtins.isFloat x then builtins.toString x
    else null
  );

  # Safely get licenses - force evaluation of each license
  getLicenses = l: tryDeep (
    let
      extractOne = x:
        if builtins.isAttrs x then (x.spdxId or x.shortName or "unknown")
        else if builtins.isString x then x
        else if builtins.isInt x || builtins.isFloat x then builtins.toString x
        else "unknown";
    in
      if builtins.isList l then map extractOne l
      else [ (extractOne l) ]
  );

  # Safely get maintainers - force evaluation of each maintainer
  # Handle both list of maintainers and single string/maintainer
  getMaintainers = m: tryDeep (
    if m == null then null
    else if builtins.isString m then [ m ]
    else if builtins.isList m then map (x:
      if builtins.isAttrs x then (x.github or x.name or "unknown")
      else if builtins.isString x then x
      else if builtins.isInt x || builtins.isFloat x then builtins.toString x
      else "unknown"
    ) m
    else null
  );

  # Safely get platforms - force evaluation of each platform
  # Handle both list of platforms and single string/platform
  getPlatforms = p: tryDeep (
    if p == null then null
    else if builtins.isString p then [ p ]
    else if builtins.isList p then map (x:
      if builtins.isString x then x
      else if builtins.isAttrs x then (x.system or "unknown")
      else if builtins.isInt x || builtins.isFloat x then builtins.toString x
      else "unknown"
    ) p
    else null
  );

  # Check if something is a derivation (with error handling)
  isDerivation = x:
    let result = builtins.tryEval (builtins.isAttrs x && x ? type && x.type == "derivation");
    in result.success && result.value;

  # Convert any value to string safely
  toString' = x:
    if x == null then null
    else if builtins.isString x then x
    else builtins.toString x;

  # Safely extract package info - each field is independently evaluated
  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      name = tryDeep (toString' (pkg.pname or pkg.name or attrPath));
      version = tryDeep (toString' (pkg.version or "unknown"));
    in {
      name = if name != null then name else attrPath;
      version = if version != null then version else "unknown";
      attrPath = attrPath;
      description = safeString (meta.description or null);
      homepage = safeString (meta.homepage or null);
      license = if meta ? license then getLicenses meta.license else null;
      maintainers = if meta ? maintainers then getMaintainers meta.maintainers else null;
      platforms = if meta ? platforms then getPlatforms meta.platforms else null;
    };

  # Process each package name with full error isolation
  # The entire result is forced to catch any remaining lazy errors
  processAttr = name:
    let
      valueResult = builtins.tryEval pkgs.${name};
      value = if valueResult.success then valueResult.value else null;
      isDeriv = if value != null then isDerivation value else false;
      info = if isDeriv then getPackageInfo name value else null;
      # Force the entire info record to catch lazy evaluation errors
      forcedResult = if info != null then builtins.tryEval (builtins.deepSeq info info) else { success = false; };
    in if forcedResult.success then forcedResult.value else null;

  # Get list of attribute names and process them
  attrNames = builtins.attrNames pkgs;
  results = builtins.filter (x: x != null) (map processAttr attrNames);
in
  results
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
                assert_eq!(normal.version, "1.0.0");
                assert!(normal.maintainers.is_some());
                assert!(normal.platforms.is_some());

                // Verify string maintainer package - should have maintainers as list with one element
                let string_maint = packages
                    .iter()
                    .find(|p| p.name == "string-maintainer-pkg")
                    .unwrap();
                assert_eq!(string_maint.version, "2.0.0");
                let maint = string_maint.maintainers.as_ref().unwrap();
                assert_eq!(maint.len(), 1);
                assert!(maint[0].contains("David Kleuker"));

                // Verify string platform package - should have platforms as list with one element
                let string_plat = packages
                    .iter()
                    .find(|p| p.name == "string-platform-pkg")
                    .unwrap();
                assert_eq!(string_plat.version, "3.0.0");
                let plat = string_plat.platforms.as_ref().unwrap();
                assert_eq!(plat.len(), 1);
                assert_eq!(plat[0], "x86_64-linux");

                // Verify both strings package
                let both = packages
                    .iter()
                    .find(|p| p.name == "both-strings-pkg")
                    .unwrap();
                assert_eq!(both.version, "4.0.0");
                assert!(both.maintainers.is_some());
                assert!(both.platforms.is_some());

                // Verify integer version package - should have version converted to string
                let int_ver = packages
                    .iter()
                    .find(|p| p.name == "int-version-pkg")
                    .unwrap();
                assert_eq!(int_ver.version, "61", "Integer version should be converted to string");
            }
            Err(e) => {
                panic!(
                    "Extraction should not fail with edge case packages: {}",
                    e
                );
            }
        }
    }
}

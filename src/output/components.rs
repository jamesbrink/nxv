//! Reusable output components for package display.
//!
//! This module provides functions to render common output sections
//! that are shared between commands (info, history, etc.).

use crate::db::queries::PackageVersion;
use crate::theme::Themed;

/// Detect the current system architecture in Nix format.
///
/// Returns a string like "x86_64-linux" or "aarch64-darwin".
pub fn detect_current_system() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if std::env::consts::OS == "macos" {
            "darwin"
        } else {
            std::env::consts::OS
        }
    )
}

/// Print a package header: "Package: python 3.11.0"
pub fn print_package_header(pkg: &PackageVersion) {
    println!(
        "{}: {} {}",
        "Package".bold_text(),
        pkg.name.attr_path(),
        pkg.version.version()
    );
    println!();
}

/// Print a package header for specific version query: "Package: python 2.7.13"
pub fn print_package_header_with_attr(attr_path: &str, version: &str) {
    println!("Package: {} {}", attr_path, version);
    println!();
}

/// Print the details section (attribute, description, homepage, license).
pub fn print_details(pkg: &PackageVersion) {
    println!("{}", "Details".section_header());
    println!(
        "  {:<16} {}",
        "Attribute:".label(),
        pkg.attribute_path.attr_path()
    );
    println!(
        "  {:<16} {}",
        "Description:",
        pkg.description.as_deref().unwrap_or("-")
    );
    println!(
        "  {:<16} {}",
        "Homepage:",
        pkg.homepage.as_deref().unwrap_or("-")
    );
    println!(
        "  {:<16} {}",
        "License:",
        pkg.license.as_deref().unwrap_or("-")
    );
    println!();
}

/// Print the availability section (first/last seen with commits).
pub fn print_availability(pkg: &PackageVersion) {
    println!("{}", "Availability".section_header());
    println!(
        "  {:<16} {} ({})",
        "First seen:".label(),
        pkg.first_commit_short().commit(),
        pkg.first_commit_date.format("%Y-%m-%d")
    );
    println!(
        "  {:<16} {} ({})",
        "Last seen:".label(),
        pkg.last_commit_short().commit(),
        pkg.last_commit_date.format("%Y-%m-%d")
    );
    println!();
}

/// Print availability in compact format (for history command).
pub fn print_availability_compact(pkg: &PackageVersion) {
    println!(
        "{} {} ({})",
        "First appeared:".label(),
        pkg.first_commit_short().commit(),
        pkg.first_commit_date.format("%Y-%m-%d")
    );
    println!(
        "{} {} ({})",
        "Last seen:".label(),
        pkg.last_commit_short().commit(),
        pkg.last_commit_date.format("%Y-%m-%d")
    );
    println!();
}

/// Print maintainers section if available.
pub fn print_maintainers(pkg: &PackageVersion) {
    if let Some(ref maintainers) = pkg.maintainers {
        println!("{}", "Maintainers".section_header());
        // Parse JSON array and display
        if let Ok(list) = serde_json::from_str::<Vec<String>>(maintainers) {
            for m in list {
                println!("  \u{2022} {}", m);
            }
        } else {
            println!("  {}", maintainers);
        }
        println!();
    }
}

/// Print platforms section with current platform highlighting.
pub fn print_platforms(pkg: &PackageVersion) {
    if let Some(ref platforms) = pkg.platforms {
        println!("{}", "Platforms".section_header());
        if let Ok(list) = serde_json::from_str::<Vec<String>>(platforms) {
            let current_platform = detect_current_system();

            // Helper to format platform with highlighting
            let format_platform = |p: &str| -> String {
                if p == current_platform {
                    p.highlight()
                } else {
                    p.to_string()
                }
            };

            // Group by OS
            let mut linux: Vec<&str> = Vec::new();
            let mut darwin: Vec<&str> = Vec::new();
            let mut other: Vec<&str> = Vec::new();

            for p in &list {
                if p.contains("linux") {
                    linux.push(p);
                } else if p.contains("darwin") {
                    darwin.push(p);
                } else {
                    other.push(p);
                }
            }

            if !linux.is_empty() {
                let formatted: Vec<_> = linux.iter().map(|p| format_platform(p)).collect();
                println!("  Linux:  {}", formatted.join(", "));
            }
            if !darwin.is_empty() {
                let formatted: Vec<_> = darwin.iter().map(|p| format_platform(p)).collect();
                println!("  Darwin: {}", formatted.join(", "));
            }
            if !other.is_empty() {
                let formatted: Vec<_> = other.iter().map(|p| format_platform(p)).collect();
                println!("  Other:  {}", formatted.join(", "));
            }
        } else {
            println!("  {}", platforms);
        }
        println!();
    }
}

/// Print security warning if package is insecure.
///
/// This function does nothing if the package has no known vulnerabilities.
pub fn print_security_warning(pkg: &PackageVersion) {
    if !pkg.is_insecure() {
        return;
    }
    println!("{}", "Security Warning".section_header_error());
    println!(
        "  {}",
        "This package has known vulnerabilities!".error_style()
    );
    for vuln in pkg.vulnerabilities() {
        println!("  {} {}", "\u{2022}".error_style(), vuln);
    }
    println!();
}

/// Print usage examples (nix-shell, nix run).
pub fn print_usage(pkg: &PackageVersion) {
    println!("{}", "Usage".section_header());
    println!("  {}", pkg.nix_shell_cmd());
    println!("  {}", pkg.nix_run_cmd());
}

/// Print single usage example (for history compact view).
pub fn print_usage_compact(pkg: &PackageVersion) {
    println!("{}", "To use this version:".label());
    println!("  {}", pkg.nix_run_cmd());
}

/// Print fetchClosure / binary cache section.
pub fn print_store_paths(pkg: &PackageVersion) {
    if pkg.store_paths.is_empty() {
        return;
    }
    println!();
    println!("{}", "Binary Cache (fetchClosure)".section_header());

    let current_system = detect_current_system();

    // Sort architectures for consistent display, current system first
    let mut archs: Vec<_> = pkg.store_paths.keys().collect();
    archs.sort_by(|a, b| {
        let a_current = a.as_str() == current_system;
        let b_current = b.as_str() == current_system;
        b_current.cmp(&a_current).then(a.cmp(b))
    });

    for arch in archs {
        if let Some(store_path) = pkg.store_paths.get(arch.as_str()) {
            let is_current = arch.as_str() == current_system;
            let arch_label = if is_current {
                format!("  {} {}", "\u{25b6}".success(), arch.highlight())
            } else {
                format!("    {}", arch.warning())
            };
            println!("{}:", arch_label);
            println!("      {}: {}", "Path".label(), store_path.store_path());
            let expr = format!(
                "builtins.fetchClosure {{ fromStore = \"https://cache.nixos.org\"; fromPath = {}; inputAddressed = true; }}",
                store_path
            );
            println!("      {}: {}", "Expr".label(), expr.attr_path());
        }
    }
}

/// Print pre-flakes warning if package is from before flake.nix.
pub fn print_preflakes_warning(pkg: &PackageVersion) {
    if pkg.predates_flakes() {
        println!();
        println!(
            "{}",
            "Note: Very old nixpkgs (pre-2020) may not build with modern Nix.".warning()
        );
    }
}

/// Print other attribute paths if there are multiple matches.
pub fn print_other_attr_paths(packages: &[PackageVersion], primary: &PackageVersion) {
    if packages.len() <= 1 {
        return;
    }

    use std::collections::HashSet;
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    seen.insert((&primary.attribute_path, &primary.version));

    let others: Vec<_> = packages
        .iter()
        .filter(|p| seen.insert((&p.attribute_path, &p.version)))
        .collect();

    if !others.is_empty() {
        println!();
        println!("{}", "Other Attribute Paths".section_header());
        for other_pkg in others {
            println!(
                "  \u{2022} {} ({})",
                other_pkg.attribute_path.attr_path(),
                other_pkg.version.version()
            );
        }
    }
}

/// Print deprecation warning for history command with version.
pub fn print_history_deprecation_warning() {
    eprintln!(
        "{}: `nxv history <pkg> <version>` is deprecated.",
        "Note".warning()
    );
    eprintln!("      Use `nxv info <pkg> <version>` for detailed package information.");
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_test_package() -> PackageVersion {
        PackageVersion {
            id: 1,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            first_commit_hash: "abc1234567890".to_string(),
            first_commit_date: Utc::now(),
            last_commit_hash: "def1234567890".to_string(),
            last_commit_date: Utc::now(),
            attribute_path: "python311".to_string(),
            description: Some("Python interpreter".to_string()),
            license: Some("MIT".to_string()),
            homepage: Some("https://python.org".to_string()),
            maintainers: Some(r#"["alice", "bob"]"#.to_string()),
            platforms: Some(r#"["x86_64-linux", "aarch64-linux", "x86_64-darwin"]"#.to_string()),
            source_path: Some("pkgs/development/interpreters/python/default.nix".to_string()),
            known_vulnerabilities: None,
            store_paths: HashMap::new(),
        }
    }

    fn make_insecure_package() -> PackageVersion {
        let mut pkg = make_test_package();
        pkg.known_vulnerabilities = Some(r#"["CVE-2021-1234", "CVE-2022-5678"]"#.to_string());
        pkg
    }

    #[test]
    fn test_detect_current_system() {
        let system = detect_current_system();
        // Should contain a hyphen separating arch and OS
        assert!(system.contains('-'));
        // Should not be empty
        assert!(!system.is_empty());
    }

    #[test]
    fn test_print_package_header_does_not_panic() {
        let pkg = make_test_package();
        // Should not panic
        print_package_header(&pkg);
    }

    #[test]
    fn test_print_details_does_not_panic() {
        let pkg = make_test_package();
        print_details(&pkg);
    }

    #[test]
    fn test_print_availability_does_not_panic() {
        let pkg = make_test_package();
        print_availability(&pkg);
    }

    #[test]
    fn test_print_maintainers_with_json_array() {
        let pkg = make_test_package();
        // Should parse the JSON array and not panic
        print_maintainers(&pkg);
    }

    #[test]
    fn test_print_maintainers_with_none() {
        let mut pkg = make_test_package();
        pkg.maintainers = None;
        // Should silently skip
        print_maintainers(&pkg);
    }

    #[test]
    fn test_print_platforms_with_grouping() {
        let pkg = make_test_package();
        // Should group by OS and not panic
        print_platforms(&pkg);
    }

    #[test]
    fn test_print_security_warning_safe_package() {
        let pkg = make_test_package();
        // Should not print anything for safe package
        print_security_warning(&pkg);
    }

    #[test]
    fn test_print_security_warning_insecure_package() {
        let pkg = make_insecure_package();
        // Should print warning for insecure package
        print_security_warning(&pkg);
    }

    #[test]
    fn test_print_usage_does_not_panic() {
        let pkg = make_test_package();
        print_usage(&pkg);
    }

    #[test]
    fn test_print_store_paths_empty() {
        let pkg = make_test_package();
        // Should not print anything for empty store_paths
        print_store_paths(&pkg);
    }

    #[test]
    fn test_print_store_paths_with_data() {
        let mut pkg = make_test_package();
        pkg.store_paths.insert(
            "x86_64-linux".to_string(),
            "/nix/store/hash-python".to_string(),
        );
        // Should print the store path
        print_store_paths(&pkg);
    }

    #[test]
    fn test_print_other_attr_paths_single() {
        let pkg = make_test_package();
        // Should not print anything for single package
        print_other_attr_paths(std::slice::from_ref(&pkg), &pkg);
    }

    #[test]
    fn test_print_other_attr_paths_multiple() {
        let pkg1 = make_test_package();
        let mut pkg2 = make_test_package();
        pkg2.attribute_path = "python3".to_string();

        // Should print the other attribute path
        print_other_attr_paths(&[pkg1.clone(), pkg2], &pkg1);
    }

    #[test]
    fn test_print_preflakes_warning_modern_package() {
        let pkg = make_test_package();
        // Modern package should not print warning
        print_preflakes_warning(&pkg);
    }

    #[test]
    fn test_print_history_deprecation_warning() {
        // Should not panic
        print_history_deprecation_warning();
    }
}

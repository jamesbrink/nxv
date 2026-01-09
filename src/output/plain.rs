//! Plain text output for search results.

use crate::db::queries::PackageVersion;

use crate::output::TableOptions;

/// Print search results as plain tab-separated text.
///
/// Each `PackageVersion` in `results` is printed as one line with the columns:
/// `PACKAGE` (attribute path), `VERSION`, `COMMIT` (last commit hash), `DATE` (formatted `YYYY-MM-DD`), and `DESCRIPTION`.
/// If `options.show_platforms` is `true`, a `PLATFORMS` column is appended.
/// If `options.show_store_path` is `true`, a `STORE_PATH` column is appended.
/// For `description`, `platforms`, and `store_path`, `None` is rendered as `-`. If `results` is empty, prints `No packages found.`.
///
/// # Parameters
///
/// - `results`: slice of `PackageVersion` entries to print.
/// - `options`: display options controlling which optional columns to show.
///
/// # Examples
///
/// ```
/// // Print nothing but the "No packages found." message.
/// print_plain(&[], TableOptions::default());
/// ```
pub fn print_plain(results: &[PackageVersion], options: TableOptions) {
    if results.is_empty() {
        println!("No packages found.");
        return;
    }

    // Build header dynamically based on options
    let mut header = String::from("PACKAGE\tVERSION\tCOMMIT\tDATE\tDESCRIPTION");
    if options.show_platforms {
        header.push_str("\tPLATFORMS");
    }
    if options.show_store_path {
        header.push_str("\tSTORE_PATH");
    }
    println!("{}", header);

    for pkg in results {
        let date = pkg.last_commit_date.format("%Y-%m-%d").to_string();
        let description = pkg.description.as_deref().unwrap_or("-");

        // Build row dynamically
        let mut row = format!(
            "{}\t{}\t{}\t{}\t{}",
            pkg.attribute_path, pkg.version, pkg.last_commit_hash, date, description
        );

        if options.show_platforms {
            let platforms = pkg.platforms.as_deref().unwrap_or("-");
            row.push('\t');
            row.push_str(platforms);
        }

        if options.show_store_path {
            // Detect current system
            let current_system = format!(
                "{}-{}",
                std::env::consts::ARCH,
                if std::env::consts::OS == "macos" {
                    "darwin"
                } else {
                    std::env::consts::OS
                }
            );

            // Show available architectures
            let arch_count = pkg.store_paths.len();
            let display = if arch_count == 0 {
                "-".to_string()
            } else {
                // Prefer current system, fallback to x86_64-linux, then first available
                let (primary, is_current) = pkg
                    .store_paths
                    .get(&current_system)
                    .map(|p| (p.as_str(), true))
                    .or_else(|| {
                        pkg.store_paths
                            .get("x86_64-linux")
                            .map(|p| (p.as_str(), false))
                    })
                    .or_else(|| pkg.store_paths.values().next().map(|p| (p.as_str(), false)))
                    .unwrap_or(("-", false));
                let marker = if is_current { "* " } else { "" };
                if arch_count > 1 {
                    format!("{}{} (+{} arch)", marker, primary, arch_count - 1)
                } else {
                    format!("{}{}", marker, primary)
                }
            };
            row.push('\t');
            row.push_str(&display);
        }

        println!("{}", row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_print_plain_empty() {
        // Should not panic
        print_plain(&[], TableOptions::default());
    }

    #[test]
    fn test_print_plain_with_results() {
        let results = vec![PackageVersion {
            id: 1,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            version_source: None,
            first_commit_hash: "abc1234567890".to_string(),
            first_commit_date: Utc::now(),
            last_commit_hash: "def1234567890".to_string(),
            last_commit_date: Utc::now(),
            attribute_path: "python311".to_string(),
            description: Some("Python interpreter".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: std::collections::HashMap::new(),
        }];

        // Should not panic
        print_plain(&results, TableOptions::default());
        print_plain(
            &results,
            TableOptions {
                show_platforms: true,
                show_store_path: false,
                ascii: false,
            },
        );
        print_plain(
            &results,
            TableOptions {
                show_platforms: false,
                show_store_path: true,
                ascii: false,
            },
        );
    }
}

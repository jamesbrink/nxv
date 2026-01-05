//! Colored table output for search results.

use crate::db::queries::PackageVersion;
use crate::output::TableOptions;
use comfy_table::{
    Cell, Color, ContentArrangement, Table,
    presets::{ASCII_FULL, UTF8_FULL},
};

/// Render package search results as a colored table to stdout.
///
/// The table shows columns for Package (attribute path), Version, Commit, Date,
/// and Description. If `options.show_platforms` is true, a Platforms column is
/// appended. The ASCII/UTF-8 drawing preset is selected according to
/// `options.ascii`.
///
/// # Examples
///
/// ```
/// // Render an empty result set (no packages found).
/// let results: &[crate::db::queries::PackageVersion] = &[];
/// crate::output::print_table(results, crate::output::TableOptions::default());
/// ```
pub fn print_table(results: &[PackageVersion], options: TableOptions) {
    if results.is_empty() {
        println!("No packages found.");
        return;
    }

    let mut table = Table::new();

    // Choose preset based on ASCII option
    if options.ascii {
        table.load_preset(ASCII_FULL);
    } else {
        table.load_preset(UTF8_FULL);
    }

    table.set_content_arrangement(ContentArrangement::Dynamic);

    // Set headers - Package (attr path) is what users install with
    let mut headers = vec!["Package", "Version", "Commit", "Date", "Description"];
    if options.show_platforms {
        headers.push("Platforms");
    }
    if options.show_store_path {
        headers.push("Store Path");
    }
    table.set_header(headers);

    for pkg in results {
        let date = pkg.last_commit_date.format("%Y-%m-%d").to_string();
        let description = pkg.description.as_deref().unwrap_or("-");

        // Add warning indicator for insecure packages
        let version_display = if pkg.is_insecure() {
            format!("{} ⚠", pkg.version)
        } else {
            pkg.version.clone()
        };

        let mut row = vec![
            Cell::new(&pkg.attribute_path).fg(Color::Cyan),
            Cell::new(&version_display).fg(if pkg.is_insecure() {
                Color::Red
            } else {
                Color::Green
            }),
            Cell::new(&pkg.last_commit_hash).fg(Color::Yellow),
            Cell::new(&date).fg(Color::White),
            Cell::new(description).fg(Color::White),
        ];

        if options.show_platforms {
            let platforms = pkg.platforms.as_deref().unwrap_or("-");
            row.push(Cell::new(platforms));
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

            // Show available architectures count and primary path
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
                let marker = if is_current { "✓ " } else { "" };
                if arch_count > 1 {
                    format!("{}{} (+{} arch)", marker, primary, arch_count - 1)
                } else {
                    format!("{}{}", marker, primary)
                }
            };
            row.push(Cell::new(&display).fg(Color::Magenta));
        }

        table.add_row(row);
    }

    println!("{table}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_print_table_empty() {
        // Should not panic
        print_table(&[], TableOptions::default());
    }

    #[test]
    fn test_print_table_with_results() {
        let results = vec![PackageVersion {
            id: 1,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
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
        print_table(&results, TableOptions::default());
        print_table(
            &results,
            TableOptions {
                show_platforms: true,
                show_store_path: false,
                ascii: false,
            },
        );
        print_table(
            &results,
            TableOptions {
                show_platforms: false,
                show_store_path: true,
                ascii: true,
            },
        );
    }
}

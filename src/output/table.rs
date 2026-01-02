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
        }];

        // Should not panic
        print_table(&results, TableOptions::default());
        print_table(
            &results,
            TableOptions {
                show_platforms: true,
                ascii: false,
            },
        );
        print_table(
            &results,
            TableOptions {
                show_platforms: false,
                ascii: true,
            },
        );
    }
}

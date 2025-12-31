//! Colored table output for search results.

use crate::db::queries::PackageVersion;
use crate::output::TableOptions;
use comfy_table::{
    Cell, Color, ContentArrangement, Table,
    presets::{ASCII_FULL, UTF8_FULL},
};

/// Print search results as a colored table.
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

    // Set headers
    let mut headers = vec!["Name", "Version", "Commit", "Date", "Attr Path"];
    if options.show_platforms {
        headers.push("Platforms");
    }
    table.set_header(headers);

    for pkg in results {
        let date = pkg.last_commit_date.format("%Y-%m-%d").to_string();

        let mut row = vec![
            Cell::new(&pkg.name).fg(Color::Cyan),
            Cell::new(&pkg.version).fg(Color::Green),
            Cell::new(&pkg.last_commit_hash).fg(Color::Yellow),
            Cell::new(&date).fg(Color::White),
            Cell::new(&pkg.attribute_path),
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

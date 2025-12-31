//! Plain text output for search results.

use crate::db::queries::PackageVersion;

/// Print search results as plain text (tab-separated, no colors).
pub fn print_plain(results: &[PackageVersion], show_platforms: bool) {
    if results.is_empty() {
        println!("No packages found.");
        return;
    }

    // Print header - PACKAGE (attr path) is what users install with
    if show_platforms {
        println!("PACKAGE\tVERSION\tCOMMIT\tDATE\tDESCRIPTION\tPLATFORMS");
    } else {
        println!("PACKAGE\tVERSION\tCOMMIT\tDATE\tDESCRIPTION");
    }

    for pkg in results {
        let date = pkg.last_commit_date.format("%Y-%m-%d").to_string();
        let description = pkg.description.as_deref().unwrap_or("-");

        if show_platforms {
            let platforms = pkg.platforms.as_deref().unwrap_or("-");
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                pkg.attribute_path, pkg.version, pkg.last_commit_hash, date, description, platforms
            );
        } else {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                pkg.attribute_path, pkg.version, pkg.last_commit_hash, date, description
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_print_plain_empty() {
        // Should not panic
        print_plain(&[], false);
    }

    #[test]
    fn test_print_plain_with_results() {
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
        print_plain(&results, false);
        print_plain(&results, true);
    }
}

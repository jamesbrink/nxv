//! JSON output for search results.

use crate::db::queries::PackageVersion;

/// Print search results as JSON.
pub fn print_json(results: &[PackageVersion]) {
    let json = serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string());
    println!("{}", json);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_print_json_empty() {
        // Should not panic and produce valid JSON
        print_json(&[]);
    }

    #[test]
    fn test_print_json_with_results() {
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
            platforms: Some("[\"x86_64-linux\"]".to_string()),
            source_path: None,
        }];

        // Should not panic
        print_json(&results);
    }
}

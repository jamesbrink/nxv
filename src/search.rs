//! Shared search logic for CLI and API.
//!
//! This module provides common search functionality that can be reused
//! by both the CLI commands and the API server.

use crate::db::queries::{self, PackageVersion};
use crate::error::Result;
use clap::ValueEnum;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Sort order for search results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    /// Sort by date (newest first).
    #[default]
    Date,
    /// Sort by version (semver-aware).
    Version,
    /// Sort by name (alphabetical).
    Name,
}

/// Common search options shared between CLI and API.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Package name or attribute path to search for.
    pub query: String,
    /// Filter by version (prefix match).
    pub version: Option<String>,
    /// Perform exact name match only.
    pub exact: bool,
    /// Search in package descriptions (FTS).
    pub desc: bool,
    /// Filter by license (case-insensitive contains).
    pub license: Option<String>,
    /// Sort order for results.
    pub sort: SortOrder,
    /// Reverse the sort order.
    pub reverse: bool,
    /// Show all commits (skip deduplication).
    pub full: bool,
    /// Maximum number of results (0 for unlimited).
    pub limit: usize,
    /// Offset for pagination.
    pub offset: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            version: None,
            exact: false,
            desc: false,
            license: None,
            sort: SortOrder::Date,
            reverse: false,
            full: false,
            limit: 50,
            offset: 0,
        }
    }
}

/// Result of a search operation with pagination metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The matching packages.
    pub data: Vec<PackageVersion>,
    /// Total count before pagination.
    pub total: usize,
    /// Whether there are more results available.
    pub has_more: bool,
}

/// Execute a search with the given options.
///
/// This function encapsulates the full search pipeline:
/// 1. Query the database based on search type (name, version, description)
/// 2. Apply license filter if specified
/// 3. Sort results
/// 4. Deduplicate (unless `full` is true)
/// 5. Apply pagination (offset and limit)
pub fn execute_search(conn: &Connection, opts: &SearchOptions) -> Result<SearchResult> {
    // Step 1: Query database
    let results = if opts.desc {
        // FTS search on description
        queries::search_by_description(conn, &opts.query)?
    } else if let Some(ref version) = opts.version {
        // Search by name and version
        queries::search_by_name_version(conn, &opts.query, version)?
    } else if opts.exact {
        // Exact match on attribute_path
        queries::search_by_attr(conn, &opts.query)?
            .into_iter()
            .filter(|p| p.attribute_path == opts.query)
            .collect()
    } else {
        // Prefix search on attribute_path
        queries::search_by_attr(conn, &opts.query)?
    };

    // Step 2: Apply license filter
    let results = filter_by_license(results, opts.license.as_deref());

    // Step 3: Sort results
    let mut results = results;
    sort_results(&mut results, opts.sort, opts.reverse);

    // Step 4: Deduplicate (unless full mode)
    let results = if opts.full {
        results
    } else {
        deduplicate(results)
    };

    // Step 5: Apply pagination
    let (data, total) = paginate(results, opts.limit, opts.offset);
    let has_more = opts.limit > 0 && total > opts.offset + data.len();

    Ok(SearchResult {
        data,
        total,
        has_more,
    })
}

/// Filter results by license (case-insensitive contains).
pub fn filter_by_license(
    results: Vec<PackageVersion>,
    license: Option<&str>,
) -> Vec<PackageVersion> {
    match license {
        Some(license) => {
            let license_lower = license.to_lowercase();
            results
                .into_iter()
                .filter(|p| {
                    p.license
                        .as_ref()
                        .is_some_and(|l| l.to_lowercase().contains(&license_lower))
                })
                .collect()
        }
        None => results,
    }
}

/// Sort results based on sort order.
///
/// For `Version` sort, uses semver-aware comparison with fallback to string comparison.
pub fn sort_results(results: &mut [PackageVersion], order: SortOrder, reverse: bool) {
    match order {
        SortOrder::Date => {
            results.sort_by(|a, b| b.last_commit_date.cmp(&a.last_commit_date));
        }
        SortOrder::Version => {
            results.sort_by(|a, b| {
                // Semver-aware version comparison
                match (
                    semver::Version::parse(&a.version),
                    semver::Version::parse(&b.version),
                ) {
                    (Ok(va), Ok(vb)) => va.cmp(&vb),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less, // Valid semver sorts before invalid
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.version.cmp(&b.version), // Fall back to string comparison
                }
            });
        }
        SortOrder::Name => {
            results.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    if reverse {
        results.reverse();
    }
}

/// Deduplicate results by (attribute_path, version), keeping the most recent.
pub fn deduplicate(results: Vec<PackageVersion>) -> Vec<PackageVersion> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    results
        .into_iter()
        .filter(|p| seen.insert((p.attribute_path.clone(), p.version.clone())))
        .collect()
}

/// Apply pagination (offset and limit) to results.
///
/// Returns the paginated results and the total count before pagination.
pub fn paginate(
    results: Vec<PackageVersion>,
    limit: usize,
    offset: usize,
) -> (Vec<PackageVersion>, usize) {
    let total = results.len();

    let data: Vec<_> = if limit > 0 {
        results.into_iter().skip(offset).take(limit).collect()
    } else {
        results.into_iter().skip(offset).collect()
    };

    (data, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_package(name: &str, version: &str, attr: &str, date_offset: i64) -> PackageVersion {
        let now = Utc::now();
        PackageVersion {
            id: 1,
            name: name.to_string(),
            version: version.to_string(),
            first_commit_hash: "abc1234567890".to_string(),
            first_commit_date: now - chrono::Duration::days(date_offset + 10),
            last_commit_hash: "def1234567890".to_string(),
            last_commit_date: now - chrono::Duration::days(date_offset),
            attribute_path: attr.to_string(),
            description: Some(format!("{} package", name)),
            license: Some("MIT".to_string()),
            homepage: None,
            maintainers: None,
            platforms: None,
        }
    }

    #[test]
    fn test_filter_by_license() {
        let packages = vec![
            {
                let mut p = make_package("foo", "1.0", "foo", 0);
                p.license = Some("MIT".to_string());
                p
            },
            {
                let mut p = make_package("bar", "1.0", "bar", 1);
                p.license = Some("GPL-3.0".to_string());
                p
            },
            {
                let mut p = make_package("baz", "1.0", "baz", 2);
                p.license = None;
                p
            },
        ];

        let filtered = filter_by_license(packages.clone(), Some("mit"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "foo");

        let filtered = filter_by_license(packages.clone(), Some("GPL"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "bar");

        let filtered = filter_by_license(packages, None);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_sort_by_date() {
        let mut packages = vec![
            make_package("a", "1.0", "a", 10),
            make_package("b", "1.0", "b", 5),
            make_package("c", "1.0", "c", 15),
        ];

        sort_results(&mut packages, SortOrder::Date, false);
        assert_eq!(packages[0].name, "b"); // Most recent (5 days ago)
        assert_eq!(packages[1].name, "a");
        assert_eq!(packages[2].name, "c"); // Oldest (15 days ago)
    }

    #[test]
    fn test_sort_by_version() {
        let mut packages = vec![
            make_package("a", "1.10.0", "a", 0),
            make_package("a", "1.2.0", "a", 1),
            make_package("a", "1.9.0", "a", 2),
        ];

        sort_results(&mut packages, SortOrder::Version, false);
        assert_eq!(packages[0].version, "1.2.0");
        assert_eq!(packages[1].version, "1.9.0");
        assert_eq!(packages[2].version, "1.10.0");
    }

    #[test]
    fn test_sort_by_name() {
        let mut packages = vec![
            make_package("zsh", "1.0", "zsh", 0),
            make_package("bash", "1.0", "bash", 1),
            make_package("fish", "1.0", "fish", 2),
        ];

        sort_results(&mut packages, SortOrder::Name, false);
        assert_eq!(packages[0].name, "bash");
        assert_eq!(packages[1].name, "fish");
        assert_eq!(packages[2].name, "zsh");
    }

    #[test]
    fn test_sort_reverse() {
        let mut packages = vec![
            make_package("a", "1.0", "a", 0),
            make_package("b", "1.0", "b", 1),
            make_package("c", "1.0", "c", 2),
        ];

        sort_results(&mut packages, SortOrder::Name, true);
        assert_eq!(packages[0].name, "c");
        assert_eq!(packages[1].name, "b");
        assert_eq!(packages[2].name, "a");
    }

    #[test]
    fn test_deduplicate() {
        let packages = vec![
            make_package("python", "3.11.0", "python", 0),
            make_package("python", "3.11.0", "python", 5), // Duplicate
            make_package("python", "3.12.0", "python", 1),
        ];

        let deduped = deduplicate(packages);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_paginate() {
        let packages: Vec<_> = (0..10)
            .map(|i| make_package(&format!("pkg{}", i), "1.0", &format!("pkg{}", i), i))
            .collect();

        // Test limit
        let (data, total) = paginate(packages.clone(), 5, 0);
        assert_eq!(data.len(), 5);
        assert_eq!(total, 10);

        // Test offset
        let (data, total) = paginate(packages.clone(), 5, 5);
        assert_eq!(data.len(), 5);
        assert_eq!(total, 10);
        assert_eq!(data[0].name, "pkg5");

        // Test offset + limit exceeding total
        let (data, total) = paginate(packages.clone(), 5, 8);
        assert_eq!(data.len(), 2);
        assert_eq!(total, 10);

        // Test unlimited (limit = 0)
        let (data, total) = paginate(packages, 0, 0);
        assert_eq!(data.len(), 10);
        assert_eq!(total, 10);
    }

    #[test]
    fn test_search_options_default() {
        let opts = SearchOptions::default();
        assert_eq!(opts.limit, 50);
        assert_eq!(opts.offset, 0);
        assert!(!opts.exact);
        assert!(!opts.desc);
        assert!(!opts.full);
        assert!(!opts.reverse);
        assert_eq!(opts.sort, SortOrder::Date);
    }
}

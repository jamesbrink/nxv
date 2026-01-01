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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum, utoipa::ToSchema,
)]
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
    /// Creates the default set of search options used by the CLI and API.
    ///
    /// Defaults:
    /// - `query`: empty string
    /// - `version`: `None`
    /// - `exact`: `false`
    /// - `desc`: `false`
    /// - `license`: `None`
    /// - `sort`: `SortOrder::Date`
    /// - `reverse`: `false`
    /// - `full`: `false`
    /// - `limit`: `50`
    /// - `offset`: `0`
    ///
    /// # Examples
    ///
    /// ```
    /// let opts = crate::search::SearchOptions::default();
    /// assert_eq!(opts.query, "");
    /// assert!(opts.version.is_none());
    /// assert!(!opts.exact && !opts.desc && !opts.reverse && !opts.full);
    /// assert!(opts.license.is_none());
    /// assert_eq!(opts.sort, crate::search::SortOrder::Date);
    /// assert_eq!(opts.limit, 50);
    /// assert_eq!(opts.offset, 0);
    /// ```
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

/// Performs a package search using the provided options and returns paginated results.
///
/// Applies filters, sorting, deduplication (unless disabled), and pagination according to `opts`.
///
/// # Returns
///
/// `SearchResult` containing the matching `PackageVersion` entries, the total number of matches
/// before pagination, and a `has_more` flag indicating if additional results exist.
///
/// # Examples
///
/// ```no_run
/// use rusqlite::Connection;
/// let conn = Connection::open_in_memory().unwrap();
/// let opts = SearchOptions::default();
/// let res = execute_search(&conn, &opts).unwrap();
/// // `res.data` holds the matching package versions, `res.total` is the total matches.
/// assert!(res.total >= 0);
/// ```
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

/// Filter package versions by license using a case-insensitive substring match.
///
/// If `license` is `None`, the input `results` are returned unchanged. When `license` is
/// provided, only entries whose `license` field is present and contains the given string
/// (case-insensitive) are retained.
///
/// # Examples
///
/// ```
/// let empty: Vec<PackageVersion> = Vec::new();
/// let filtered = filter_by_license(empty, Some("mit"));
/// assert!(filtered.is_empty());
/// ```
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

/// Remove duplicate package versions identified by (attribute_path, version),
/// preserving the most recent occurrence according to the input order.
///
/// Duplicates are determined by the tuple `(attribute_path, version)`. The first
/// occurrence of each unique tuple in `results` is kept; subsequent duplicates
/// are discarded.
///
/// # Examples
///
/// ```
/// use crate::search::deduplicate;
/// use crate::db::PackageVersion;
///
/// let a1 = PackageVersion { attribute_path: "pkg::a".into(), version: "1.0".into(), ..Default::default() };
/// let a2 = PackageVersion { attribute_path: "pkg::a".into(), version: "1.0".into(), ..Default::default() };
/// let a3 = PackageVersion { attribute_path: "pkg::a".into(), version: "2.0".into(), ..Default::default() };
///
/// let out = deduplicate(vec![a1, a2, a3]);
/// assert_eq!(out.len(), 2);
/// ```
pub fn deduplicate(results: Vec<PackageVersion>) -> Vec<PackageVersion> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    results
        .into_iter()
        .filter(|p| seen.insert((p.attribute_path.clone(), p.version.clone())))
        .collect()
}

/// Paginate a list of `PackageVersion` by applying `offset` and an optional `limit`.
///
/// If `limit` is zero, all items after `offset` are returned. The second element of the
/// returned tuple is the total number of items before pagination.
///
/// # Examples
///
/// ```
/// let (page, total) = paginate(Vec::<_>::new(), 10, 0);
/// assert!(page.is_empty());
/// assert_eq!(total, 0);
/// ```
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

    /// Create a PackageVersion test instance with the given name, version, attribute path, and a last-commit date offset in days.
    ///
    /// The `date_offset` is subtracted from the current UTC time to set `last_commit_date` (and `first_commit_date` is set 10 days earlier).
    ///
    /// # Examples
    ///
    /// ```
    /// let pkg = make_package("foo", "1.0.0", "foo.attr", 5);
    /// assert_eq!(pkg.name, "foo");
    /// assert_eq!(pkg.version, "1.0.0");
    /// assert_eq!(pkg.attribute_path, "foo.attr");
    /// ```
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
            source_path: None,
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
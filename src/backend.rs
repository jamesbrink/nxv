//! Backend abstraction for local database or remote API access.
//!
//! This module provides a unified interface for querying package data,
//! regardless of whether the data comes from a local SQLite database
//! or a remote API server.
//!
//! Set the `NXV_API_URL` environment variable to use a remote server:
//! ```bash
//! export NXV_API_URL=http://localhost:8080
//! nxv search python
//! ```

use crate::client::ApiClient;
use crate::db::Database;
use crate::db::queries::{self, IndexStats, PackageVersion, VersionHistoryEntry};
use crate::error::Result;
use crate::search::{self, SearchOptions, SearchResult};

/// Backend for data access - either local database or remote API.
pub enum Backend {
    /// Local SQLite database.
    Local(Database),
    /// Remote API server.
    Remote(ApiClient),
}

impl Backend {
    /// Check if using remote API.
    pub fn is_remote(&self) -> bool {
        matches!(self, Backend::Remote(_))
    }

    /// Searches packages using the provided search options.
    ///
    /// Uses the backend's configured data source to perform the query according to `opts`.
    ///
    /// Returns a `SearchResult` containing matches and related metadata on success.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let backend: Backend = unimplemented!();
    /// let opts: SearchOptions = unimplemented!();
    /// let _result = backend.search(&opts).unwrap();
    /// ```
    pub fn search(&self, opts: &SearchOptions) -> Result<SearchResult> {
        match self {
            Backend::Local(db) => search::execute_search(db.connection(), opts),
            Backend::Remote(client) => client.search(opts),
        }
    }

    /// Retrieve package versions whose attribute path exactly matches the provided attribute.
    ///
    /// On success returns a `Vec<PackageVersion>` containing only entries whose `attribute_path` is
    /// exactly equal to `attr`.
    ///
    /// # Examples
    ///
    /// ```
    /// // assuming `backend` is a `Backend` instance and `pkg_attr` is the attribute path string
    /// let matches = backend.get_package("example/package").unwrap();
    /// for pv in &matches {
    ///     assert_eq!(pv.attribute_path, "example/package");
    /// }
    /// ```
    #[allow(dead_code)]
    pub fn get_package(&self, attr: &str) -> Result<Vec<PackageVersion>> {
        match self {
            Backend::Local(db) => {
                let results = queries::search_by_attr(db.connection(), attr)?;
                Ok(results
                    .into_iter()
                    .filter(|p| p.attribute_path == attr)
                    .collect())
            }
            Backend::Remote(client) => client.get_package(attr),
        }
    }

    /// Search for package versions by package name with an optional version filter.
    ///
    /// If `version` is provided, returns package versions that match the given package name and version.
    /// If `version` is `None`, attempts an exact attribute-path match first; if none are found, falls back
    /// to a name-prefix search.
    ///
    /// # Returns
    ///
    /// A `Vec<PackageVersion>` containing the matching package versions (empty if no matches).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use crate::backend::Backend;
    /// # use crate::models::PackageVersion;
    /// # fn example(backend: Backend) -> anyhow::Result<()> {
    /// let results: Vec<PackageVersion> = backend.search_by_name_version("example/pkg", None)?;
    /// println!("found {} versions", results.len());
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_by_name_version(
        &self,
        package: &str,
        version: Option<&str>,
    ) -> Result<Vec<PackageVersion>> {
        match self {
            Backend::Local(db) => {
                if let Some(v) = version {
                    queries::search_by_name_version(db.connection(), package, v)
                } else {
                    // Try exact attribute path first
                    let by_attr: Vec<_> = queries::search_by_attr(db.connection(), package)?
                        .into_iter()
                        .filter(|p| p.attribute_path == package)
                        .collect();

                    if !by_attr.is_empty() {
                        Ok(by_attr)
                    } else {
                        // Fall back to name prefix search
                        queries::search_by_name(db.connection(), package, false)
                    }
                }
            }
            Backend::Remote(client) => {
                if version.is_some() {
                    client.search_by_name_version(package, version, None)
                } else {
                    // Try exact attribute path first
                    let by_attr = client.get_package(package)?;
                    if !by_attr.is_empty() {
                        Ok(by_attr)
                    } else {
                        // Fall back to name prefix search
                        client.search_by_name(package, false)
                    }
                }
            }
        }
    }

    /// Search packages by name, using either an exact match or a prefix match.
    ///
    /// If `exact` is `true`, only packages whose attribute path exactly equals `name` are returned.
    /// If `exact` is `false`, packages whose names start with `name` are returned.
    ///
    /// # Returns
    ///
    /// A vector of `PackageVersion` entries that match the provided name and match mode.
    ///
    /// # Examples
    ///
    /// ```
    /// // Search for packages whose name starts with "libfoo"
    /// let results = backend.search_by_name("libfoo", false).unwrap();
    /// // Search for the package whose attribute path is exactly "libfoo/pkg"
    /// let exact = backend.search_by_name("libfoo/pkg", true).unwrap();
    /// ```
    pub fn search_by_name(&self, name: &str, exact: bool) -> Result<Vec<PackageVersion>> {
        match self {
            Backend::Local(db) => queries::search_by_name(db.connection(), name, exact),
            Backend::Remote(client) => client.search_by_name(name, exact),
        }
    }

    /// Get first occurrence of a specific version.
    ///
    /// This method is part of the public API for library consumers and mirrors
    /// the `/api/v1/packages/{attr}/versions/{version}/first` endpoint.
    /// Not currently used by the CLI but provided for API completeness.
    #[allow(dead_code)]
    pub fn get_first_occurrence(
        &self,
        attr: &str,
        version: &str,
    ) -> Result<Option<PackageVersion>> {
        match self {
            Backend::Local(db) => queries::get_first_occurrence(db.connection(), attr, version),
            Backend::Remote(client) => client.get_first_occurrence(attr, version),
        }
    }

    /// Retrieve the most recent `PackageVersion` entry for the given attribute path and version.
    ///
    /// This method is part of the public API for library consumers and mirrors
    /// the `/api/v1/packages/{attr}/versions/{version}/last` endpoint.
    /// Not currently used by the CLI but provided for API completeness.
    ///
    /// - `attr`: attribute path (attribute identifier) to search for.
    /// - `version`: package version to match.
    ///
    /// # Returns
    ///
    /// `Some(PackageVersion)` containing the most recent occurrence for that attribute and version, `None` if no match is found.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // Given a `Backend` instance `backend`, fetch the last occurrence:
    /// let last = backend.get_last_occurrence("example/pkg", "1.2.3")?;
    /// if let Some(pkg) = last {
    ///     println!("Found: {}", pkg.attribute_path);
    /// }
    /// ```
    #[allow(dead_code)]
    pub fn get_last_occurrence(&self, attr: &str, version: &str) -> Result<Option<PackageVersion>> {
        match self {
            Backend::Local(db) => queries::get_last_occurrence(db.connection(), attr, version),
            Backend::Remote(client) => client.get_last_occurrence(attr, version),
        }
    }

    /// Retrieve the version history for the package identified by `attr`.
    ///
    /// # Parameters
    ///
    /// - `attr`: The package attribute path (e.g., `"nxv:package/name"`) to fetch version history for.
    ///
    /// # Returns
    ///
    /// A vector of `VersionHistoryEntry` records for the package, or an error if the underlying
    /// data source query fails.
    ///
    /// # Examples
    ///
    /// ```
    /// // Assuming `backend` is an initialized `Backend` (Local or Remote):
    /// let history = backend.get_version_history("nxv:example/package").unwrap();
    /// assert!(history.len() >= 0);
    /// ```
    pub fn get_version_history(&self, attr: &str) -> Result<Vec<VersionHistoryEntry>> {
        match self {
            Backend::Local(db) => queries::get_version_history(db.connection(), attr),
            Backend::Remote(client) => client.get_version_history(attr),
        }
    }

    /// Fetches aggregated index statistics for this backend.
    ///
    /// # Returns
    ///
    /// `IndexStats` containing aggregated counts and metadata about the index.
    ///
    /// # Examples
    ///
    /// ```
    /// # use crate::backend::Backend;
    /// let backend: Backend = /* obtain Backend::Local or Backend::Remote */;
    /// let stats = backend.get_stats().unwrap();
    /// ```
    pub fn get_stats(&self) -> Result<IndexStats> {
        match self {
            Backend::Local(db) => queries::get_stats(db.connection()),
            Backend::Remote(client) => client.get_stats(),
        }
    }

    /// Fetches a metadata value for the given key from the backend.
    ///
    /// For a local backend this returns the stored metadata entry for `key`.
    /// For a remote backend this returns the remote health-derived value only for
    /// the `"last_indexed_commit"` key; other keys return `None`.
    ///
    /// # Arguments
    ///
    /// * `key` - Metadata key to retrieve. The remote backend recognizes `"last_indexed_commit"`.
    ///
    /// # Returns
    ///
    /// `Some(String)` with the metadata value if present, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // `backend` can be a Local or Remote Backend instance.
    /// let value = backend.get_meta("last_indexed_commit")?;
    /// match value {
    ///     Some(commit) => println!("Last indexed commit: {}", commit),
    ///     None => println!("No metadata available for that key"),
    /// }
    /// ```
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self {
            Backend::Local(db) => db.get_meta(key),
            Backend::Remote(client) => {
                // For remote, we can get some info from health endpoint
                if key == "last_indexed_commit" {
                    let health = client.get_health()?;
                    Ok(health.index_commit)
                } else {
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(all(test, feature = "indexer"))]
mod tests {
    use super::*;
    use crate::db::queries::PackageVersion;
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;
    use tempfile::tempdir;

    /// Create a test database with sample data
    fn setup_test_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        // Insert test packages using upsert_packages_batch
        let packages = vec![
            PackageVersion {
                id: 0,
                name: "hello".to_string(),
                version: "2.10".to_string(),
                version_source: Some("direct".to_string()),
                first_commit_hash: "abc123".to_string(),
                first_commit_date: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
                last_commit_hash: "def456".to_string(),
                last_commit_date: Utc.with_ymd_and_hms(2020, 6, 1, 0, 0, 0).unwrap(),
                attribute_path: "hello".to_string(),
                description: Some("A friendly greeting program".to_string()),
                license: Some("GPL-3.0".to_string()),
                homepage: Some("https://www.gnu.org/software/hello/".to_string()),
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            },
            PackageVersion {
                id: 0,
                name: "hello".to_string(),
                version: "2.12".to_string(),
                version_source: Some("direct".to_string()),
                first_commit_hash: "ghi789".to_string(),
                first_commit_date: Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap(),
                last_commit_hash: "jkl012".to_string(),
                last_commit_date: Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap(),
                attribute_path: "hello".to_string(),
                description: Some("A friendly greeting program".to_string()),
                license: Some("GPL-3.0".to_string()),
                homepage: Some("https://www.gnu.org/software/hello/".to_string()),
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            },
            PackageVersion {
                id: 0,
                name: "git".to_string(),
                version: "2.30.0".to_string(),
                version_source: Some("direct".to_string()),
                first_commit_hash: "mno345".to_string(),
                first_commit_date: Utc.with_ymd_and_hms(2021, 1, 15, 0, 0, 0).unwrap(),
                last_commit_hash: "pqr678".to_string(),
                last_commit_date: Utc.with_ymd_and_hms(2021, 3, 1, 0, 0, 0).unwrap(),
                attribute_path: "git".to_string(),
                description: Some("Distributed version control system".to_string()),
                license: Some("GPL-2.0".to_string()),
                homepage: Some("https://git-scm.com/".to_string()),
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            },
        ];

        db.upsert_packages_batch(&packages).unwrap();
        db.set_meta("last_indexed_commit", "abc123def456").unwrap();

        (dir, db)
    }

    #[test]
    fn test_local_backend_is_not_remote() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);
        assert!(!backend.is_remote());
    }

    #[test]
    fn test_local_backend_search() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let opts = SearchOptions {
            query: "hello".to_string(),
            ..Default::default()
        };

        let result = backend.search(&opts).unwrap();
        assert!(!result.data.is_empty());
        assert!(result.data.iter().any(|p| p.attribute_path == "hello"));
    }

    #[test]
    fn test_local_backend_search_no_results() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let opts = SearchOptions {
            query: "nonexistent_package_xyz".to_string(),
            ..Default::default()
        };

        let result = backend.search(&opts).unwrap();
        assert!(result.data.is_empty());
    }

    #[test]
    fn test_local_backend_get_package() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let results = backend.get_package("hello").unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|p| p.attribute_path == "hello"));
    }

    #[test]
    fn test_local_backend_get_package_not_found() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let results = backend.get_package("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_local_backend_search_by_name_version() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        // Search with specific version
        let results = backend
            .search_by_name_version("hello", Some("2.10"))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].version, "2.10");

        // Search without version (should return all versions)
        let results = backend.search_by_name_version("hello", None).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_local_backend_search_by_name() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        // Exact match
        let results = backend.search_by_name("hello", true).unwrap();
        assert!(!results.is_empty());

        // Prefix match
        let results = backend.search_by_name("hel", false).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_local_backend_get_first_occurrence() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let result = backend.get_first_occurrence("hello", "2.10").unwrap();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.version, "2.10");
        assert_eq!(pkg.first_commit_hash, "abc123");
    }

    #[test]
    fn test_local_backend_get_first_occurrence_not_found() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let result = backend.get_first_occurrence("hello", "9.99.99").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_local_backend_get_last_occurrence() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let result = backend.get_last_occurrence("hello", "2.10").unwrap();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.version, "2.10");
        assert_eq!(pkg.last_commit_hash, "def456");
    }

    #[test]
    fn test_local_backend_get_version_history() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let history = backend.get_version_history("hello").unwrap();
        assert_eq!(history.len(), 2);
        // History should be ordered by date (newest first typically)
    }

    #[test]
    fn test_local_backend_get_version_history_not_found() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let history = backend.get_version_history("nonexistent").unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_local_backend_get_stats() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let stats = backend.get_stats().unwrap();
        assert_eq!(stats.total_ranges, 3);
        assert_eq!(stats.unique_names, 2); // hello and git
    }

    #[test]
    fn test_local_backend_get_meta() {
        let (_dir, db) = setup_test_db();
        let backend = Backend::Local(db);

        let commit = backend.get_meta("last_indexed_commit").unwrap();
        assert!(commit.is_some());
        assert_eq!(commit.unwrap(), "abc123def456");

        let missing = backend.get_meta("nonexistent_key").unwrap();
        assert!(missing.is_none());
    }
}

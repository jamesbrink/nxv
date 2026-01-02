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

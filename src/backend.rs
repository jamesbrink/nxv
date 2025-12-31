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

    /// Search for packages using shared search options.
    pub fn search(&self, opts: &SearchOptions) -> Result<SearchResult> {
        match self {
            Backend::Local(db) => search::execute_search(db.connection(), opts),
            Backend::Remote(client) => client.search(opts),
        }
    }

    /// Get package by exact attribute path.
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

    /// Search by name with optional version filter (for info command).
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
                    client.search_by_name_version(package, version)
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

    /// Search by name (prefix or exact).
    pub fn search_by_name(&self, name: &str, exact: bool) -> Result<Vec<PackageVersion>> {
        match self {
            Backend::Local(db) => queries::search_by_name(db.connection(), name, exact),
            Backend::Remote(client) => client.search_by_name(name, exact),
        }
    }

    /// Get first occurrence of a specific version.
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

    /// Get last occurrence of a specific version.
    pub fn get_last_occurrence(&self, attr: &str, version: &str) -> Result<Option<PackageVersion>> {
        match self {
            Backend::Local(db) => queries::get_last_occurrence(db.connection(), attr, version),
            Backend::Remote(client) => client.get_last_occurrence(attr, version),
        }
    }

    /// Get version history for a package.
    pub fn get_version_history(&self, attr: &str) -> Result<Vec<VersionHistoryEntry>> {
        match self {
            Backend::Local(db) => queries::get_version_history(db.connection(), attr),
            Backend::Remote(client) => client.get_version_history(attr),
        }
    }

    /// Get index statistics.
    pub fn get_stats(&self) -> Result<IndexStats> {
        match self {
            Backend::Local(db) => queries::get_stats(db.connection()),
            Backend::Remote(client) => client.get_stats(),
        }
    }

    /// Get metadata value (only available for local backend).
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

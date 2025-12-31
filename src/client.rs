//! HTTP client for remote API access.
//!
//! This module provides an API client that can query a remote nxv server
//! instead of the local SQLite database. Enable by setting `NXV_API_URL`.

use crate::db::queries::{IndexStats, PackageVersion, VersionHistoryEntry};
use crate::error::{NxvError, Result};
use crate::search::{SearchOptions, SearchResult, SortOrder};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

/// Response wrapper matching the API format.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    data: T,
    meta: Option<PaginationMeta>,
}

#[derive(Debug, Deserialize)]
struct PaginationMeta {
    total: usize,
    #[allow(dead_code)]
    limit: usize,
    #[allow(dead_code)]
    offset: usize,
    has_more: bool,
}

/// Version history entry from API (for deserialization).
#[derive(Debug, Deserialize)]
struct ApiVersionHistoryEntry {
    version: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

/// HTTP client for the nxv API.
pub struct ApiClient {
    base_url: String,
    client: Client,
}

impl ApiClient {
    /// Create a new API client.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(NxvError::Network)?;

        Ok(Self { base_url, client })
    }

    /// Search packages using the shared search options.
    pub fn search(&self, opts: &SearchOptions) -> Result<SearchResult> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(&opts.query)
        );

        if let Some(ref version) = opts.version {
            url.push_str(&format!("&version={}", urlencoding::encode(version)));
        }
        if opts.exact {
            url.push_str("&exact=true");
        }
        if let Some(ref license) = opts.license {
            url.push_str(&format!("&license={}", urlencoding::encode(license)));
        }

        let sort_str = match opts.sort {
            SortOrder::Date => "date",
            SortOrder::Version => "version",
            SortOrder::Name => "name",
        };
        url.push_str(&format!("&sort={}", sort_str));

        if opts.reverse {
            url.push_str("&reverse=true");
        }
        if opts.limit > 0 {
            url.push_str(&format!("&limit={}", opts.limit));
        }
        if opts.offset > 0 {
            url.push_str(&format!("&offset={}", opts.offset));
        }

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;

        let (total, has_more) = match response.meta {
            Some(meta) => (meta.total, meta.has_more),
            None => (response.data.len(), false),
        };

        Ok(SearchResult {
            data: response.data,
            total,
            has_more,
        })
    }

    /// Get package by attribute path (exact match).
    pub fn get_package(&self, attr: &str) -> Result<Vec<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}",
            self.base_url,
            urlencoding::encode(attr)
        );

        match self.get::<ApiResponse<Vec<PackageVersion>>>(&url) {
            Ok(response) => Ok(response.data),
            Err(NxvError::PackageNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Search by name with optional version filter (for info command).
    pub fn search_by_name_version(
        &self,
        package: &str,
        version: Option<&str>,
    ) -> Result<Vec<PackageVersion>> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(package)
        );

        if let Some(v) = version {
            url.push_str(&format!("&version={}", urlencoding::encode(v)));
        }

        // Use high limit to get all relevant results
        url.push_str("&limit=1000");

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;
        Ok(response.data)
    }

    /// Get first occurrence of a specific version.
    pub fn get_first_occurrence(
        &self,
        attr: &str,
        version: &str,
    ) -> Result<Option<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}/versions/{}/first",
            self.base_url,
            urlencoding::encode(attr),
            urlencoding::encode(version)
        );

        match self.get::<ApiResponse<PackageVersion>>(&url) {
            Ok(response) => Ok(Some(response.data)),
            Err(NxvError::PackageNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get last occurrence of a specific version.
    pub fn get_last_occurrence(&self, attr: &str, version: &str) -> Result<Option<PackageVersion>> {
        let url = format!(
            "{}/api/v1/packages/{}/versions/{}/last",
            self.base_url,
            urlencoding::encode(attr),
            urlencoding::encode(version)
        );

        match self.get::<ApiResponse<PackageVersion>>(&url) {
            Ok(response) => Ok(Some(response.data)),
            Err(NxvError::PackageNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get version history for a package.
    pub fn get_version_history(&self, attr: &str) -> Result<Vec<VersionHistoryEntry>> {
        let url = format!(
            "{}/api/v1/packages/{}/history",
            self.base_url,
            urlencoding::encode(attr)
        );

        match self.get::<ApiResponse<Vec<ApiVersionHistoryEntry>>>(&url) {
            Ok(response) => Ok(response
                .data
                .into_iter()
                .map(|e| (e.version, e.first_seen, e.last_seen))
                .collect()),
            Err(NxvError::PackageNotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Get index statistics.
    pub fn get_stats(&self) -> Result<IndexStats> {
        let url = format!("{}/api/v1/stats", self.base_url);
        let response: ApiResponse<IndexStats> = self.get(&url)?;
        Ok(response.data)
    }

    /// Get health info including index commit.
    pub fn get_health(&self) -> Result<HealthInfo> {
        let url = format!("{}/api/v1/health", self.base_url);
        self.get(&url)
    }

    /// Search packages by name (prefix search, for info command fallback).
    pub fn search_by_name(&self, name: &str, exact: bool) -> Result<Vec<PackageVersion>> {
        let mut url = format!(
            "{}/api/v1/search?q={}",
            self.base_url,
            urlencoding::encode(name)
        );

        if exact {
            url.push_str("&exact=true");
        }
        url.push_str("&limit=1000");

        let response: ApiResponse<Vec<PackageVersion>> = self.get(&url)?;
        Ok(response.data)
    }

    /// Perform a GET request and parse JSON response.
    fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self.client.get(url).send().map_err(NxvError::Network)?;

        let status = response.status();
        if !status.is_success() {
            return Err(self.map_status_error(status, url));
        }

        response.json().map_err(NxvError::Network)
    }

    /// Map HTTP status codes to NxvError.
    fn map_status_error(&self, status: StatusCode, _url: &str) -> NxvError {
        match status {
            StatusCode::NOT_FOUND => NxvError::PackageNotFound("Resource not found".to_string()),
            StatusCode::SERVICE_UNAVAILABLE => NxvError::NoIndex,
            _ => NxvError::ApiError(format!("HTTP error: {}", status)),
        }
    }
}

/// Health response from the API.
#[derive(Debug, Deserialize)]
pub struct HealthInfo {
    #[allow(dead_code)]
    pub status: String,
    #[allow(dead_code)]
    pub version: String,
    pub index_commit: Option<String>,
}

// URL encoding helper
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut result = String::with_capacity(s.len() * 3);
        for c in s.chars() {
            match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
                _ => {
                    for b in c.to_string().as_bytes() {
                        result.push_str(&format!("%{:02X}", b));
                    }
                }
            }
        }
        result
    }
}

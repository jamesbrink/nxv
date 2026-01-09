//! API request and response types.

use crate::db::queries::{IndexStats, PackageVersion};
use crate::search::SortOrder;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Wrapper for paginated API responses.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse<T: Serialize> {
    /// The response data.
    pub data: T,
    /// Pagination metadata (present for list responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<PaginationMeta>,
}

impl<T: Serialize> ApiResponse<T> {
    /// Creates an ApiResponse containing the provided data and no pagination metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// let resp = ApiResponse::new("payload");
    /// assert_eq!(resp.data, "payload");
    /// assert!(resp.meta.is_none());
    /// ```
    pub fn new(data: T) -> Self {
        Self { data, meta: None }
    }

    /// Wraps `data` in an `ApiResponse` and attaches pagination metadata.
    ///
    /// The `has_more` parameter should be pre-computed by the caller using the actual
    /// number of items returned: `limit > 0 && total > offset + data.len()`.
    ///
    /// # Returns
    ///
    /// An `ApiResponse` containing the provided `data` and a `meta` value with the
    /// specified pagination fields.
    ///
    /// # Examples
    ///
    /// ```
    /// let resp = ApiResponse::with_pagination(vec![1, 2, 3], 100, 10, 0, true);
    /// assert_eq!(resp.data.len(), 3);
    /// let meta = resp.meta.unwrap();
    /// assert_eq!(meta.total, 100);
    /// assert_eq!(meta.limit, 10);
    /// assert_eq!(meta.offset, 0);
    /// assert!(meta.has_more);
    /// ```
    pub fn with_pagination(
        data: T,
        total: usize,
        limit: usize,
        offset: usize,
        has_more: bool,
    ) -> Self {
        Self {
            data,
            meta: Some(PaginationMeta {
                total,
                limit,
                offset,
                has_more,
            }),
        }
    }
}

/// Pagination metadata for list responses.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaginationMeta {
    /// Total number of results before pagination.
    pub total: usize,
    /// Maximum results per page.
    pub limit: usize,
    /// Number of results skipped.
    pub offset: usize,
    /// Whether more results are available.
    pub has_more: bool,
}

/// Search query parameters.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchParams {
    /// Package name or attribute path to search for.
    pub q: String,
    /// Filter by version prefix.
    #[serde(default)]
    pub version: Option<String>,
    /// Exact match only (default: false).
    #[serde(default)]
    pub exact: bool,
    /// Search in package descriptions using FTS (default: false).
    #[serde(default)]
    pub desc: bool,
    /// Filter by license (case-insensitive contains).
    #[serde(default)]
    pub license: Option<String>,
    /// Filter by platform (e.g., "x86_64-linux", "aarch64-darwin").
    #[serde(default)]
    pub platform: Option<String>,
    /// Sort order: relevance, date, version, or name.
    #[serde(default)]
    pub sort: SortOrder,
    /// Reverse sort order (default: false).
    #[serde(default)]
    pub reverse: bool,
    /// Show all commits (skip deduplication, default: false).
    #[serde(default)]
    pub full: bool,
    /// Maximum number of results (default: 50, 0 for unlimited).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of results to skip (default: 0).
    #[serde(default)]
    pub offset: usize,
}

/// Maximum allowed limit for any query to prevent memory exhaustion.
/// Requests with higher limits will be capped to this value.
pub const MAX_LIMIT: usize = 100;

/// Default limit for search queries.
const DEFAULT_LIMIT: usize = 50;

/// # Examples
///
/// ```
/// assert_eq!(crate::default_limit(), 50);
/// ```
fn default_limit() -> usize {
    DEFAULT_LIMIT
}

/// Description search query parameters.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DescriptionSearchParams {
    /// Search query for FTS.
    pub q: String,
    /// Maximum number of results.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of results to skip.
    #[serde(default)]
    pub offset: usize,
}

/// Health check response.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Service status.
    pub status: String,
    /// nxv version.
    pub version: String,
    /// Last indexed commit hash (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_commit: Option<String>,
}

/// Server metrics response for monitoring.
#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsResponse {
    /// Server uptime information.
    pub server: ServerMetrics,
    /// Database connection pool metrics.
    pub database: DatabaseMetrics,
    /// Rate limiting metrics (if enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitMetrics>,
}

/// Server-level metrics.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServerMetrics {
    /// nxv version.
    pub version: String,
    /// Server status.
    pub status: String,
}

/// Database connection pool metrics.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatabaseMetrics {
    /// Maximum concurrent database connections allowed.
    pub max_connections: usize,
    /// Currently available connection permits.
    pub available_permits: usize,
    /// Permits currently in use.
    pub in_use: usize,
    /// Database operation timeout in seconds.
    pub timeout_seconds: u64,
}

/// Rate limiting metrics.
#[derive(Debug, Serialize, ToSchema)]
pub struct RateLimitMetrics {
    /// Configured requests per second per IP.
    pub requests_per_second: u64,
    /// Configured burst size.
    pub burst_size: u32,
    /// Whether rate limiting is enabled.
    pub enabled: bool,
}

/// Version history entry for API responses.
#[derive(Debug, Serialize, ToSchema)]
pub struct VersionHistorySchema {
    /// Package version string.
    pub version: String,
    /// First time this version was seen.
    pub first_seen: DateTime<Utc>,
    /// Last time this version was seen.
    pub last_seen: DateTime<Utc>,
    /// Whether this version has known vulnerabilities.
    pub is_insecure: bool,
}

/// Package version info (re-export with ToSchema).
/// This wrapper is needed because PackageVersion is defined elsewhere.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PackageVersionSchema)]
pub struct PackageVersionSchema {
    pub id: i64,
    pub name: String,
    pub version: String,
    pub first_commit_hash: String,
    pub first_commit_date: DateTime<Utc>,
    pub last_commit_hash: String,
    pub last_commit_date: DateTime<Utc>,
    pub attribute_path: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub maintainers: Option<String>,
    pub platforms: Option<String>,
    /// Source file path relative to nixpkgs root (may be null for older packages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Known security vulnerabilities (JSON array, may be null for secure packages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_vulnerabilities: Option<String>,
    /// Store paths per architecture. Keys are system triples (x86_64-linux, aarch64-linux,
    /// x86_64-darwin, aarch64-darwin), values are Nix store paths.
    /// Only populated for packages from 2020-01-01 onwards.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    #[schema(
        value_type = Object,
        example = json!({"x86_64-linux": "/nix/store/abc123-hello-2.10", "aarch64-linux": "/nix/store/def456-hello-2.10"})
    )]
    pub store_paths: std::collections::HashMap<String, String>,
}

impl From<PackageVersion> for PackageVersionSchema {
    /// Convert a `PackageVersion` into a `PackageVersionSchema` for API responses.
    ///
    /// The resulting schema preserves all public metadata fields (ids, version and
    /// commit timestamps, paths, description, license, homepage, maintainers,
    /// platforms, and optional `source_path`).
    ///
    /// # Examples
    ///
    /// ```
    /// // given a PackageVersion value `pv`:
    /// // let pv: PackageVersion = ...;
    /// let schema = PackageVersionSchema::from(pv);
    /// ```
    fn from(p: PackageVersion) -> Self {
        Self {
            id: p.id,
            name: p.name,
            version: p.version,
            first_commit_hash: p.first_commit_hash,
            first_commit_date: p.first_commit_date,
            last_commit_hash: p.last_commit_hash,
            last_commit_date: p.last_commit_date,
            attribute_path: p.attribute_path,
            description: p.description,
            license: p.license,
            homepage: p.homepage,
            maintainers: p.maintainers,
            platforms: p.platforms,
            source_path: p.source_path,
            known_vulnerabilities: p.known_vulnerabilities,
            store_paths: p.store_paths,
        }
    }
}

/// Fetch closure query parameters.
#[derive(Debug, Deserialize, ToSchema)]
pub struct FetchClosureParams {
    /// Package attribute path.
    pub attr: String,
    /// Package version.
    pub version: String,
    /// Nix cache URL (default: https://cache.nixos.org).
    #[serde(default = "default_cache_url")]
    pub cache_url: String,
    /// Target system (default: x86_64-linux).
    /// Supported systems: x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin.
    #[serde(default = "default_system")]
    pub system: String,
}

fn default_cache_url() -> String {
    "https://cache.nixos.org".to_string()
}

fn default_system() -> String {
    "x86_64-linux".to_string()
}

/// Fetch closure response containing Nix expression.
#[derive(Debug, Serialize, ToSchema)]
pub struct FetchClosureResponse {
    /// The package attribute path.
    pub attr: String,
    /// The package version.
    pub version: String,
    /// Target system for the store path.
    pub system: String,
    /// The store path (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_path: Option<String>,
    /// The nixpkgs commit hash where this version was found.
    pub commit: String,
    /// Ready-to-use fetchClosure Nix expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nix_expr: Option<String>,
    /// Error message if store path is not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Index statistics schema.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = IndexStatsSchema)]
pub struct IndexStatsSchema {
    pub total_ranges: i64,
    pub unique_names: i64,
    pub unique_versions: i64,
    pub oldest_commit_date: Option<DateTime<Utc>>,
    pub newest_commit_date: Option<DateTime<Utc>>,
    /// The commit hash that was last indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_commit: Option<String>,
    /// When the index was last updated (RFC3339 format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_date: Option<String>,
}

impl From<IndexStats> for IndexStatsSchema {
    /// Converts an `IndexStats` into an `IndexStatsSchema` by copying all fields.
    fn from(s: IndexStats) -> Self {
        Self {
            total_ranges: s.total_ranges,
            unique_names: s.unique_names,
            unique_versions: s.unique_versions,
            oldest_commit_date: s.oldest_commit_date,
            newest_commit_date: s.newest_commit_date,
            last_indexed_commit: s.last_indexed_commit,
            last_indexed_date: s.last_indexed_date,
        }
    }
}

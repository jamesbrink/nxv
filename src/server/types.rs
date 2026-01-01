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
    /// Create a response without pagination.
    pub fn new(data: T) -> Self {
        Self { data, meta: None }
    }

    /// Create a response with pagination metadata.
    pub fn with_pagination(data: T, total: usize, limit: usize, offset: usize) -> Self {
        Self {
            data,
            meta: Some(PaginationMeta {
                total,
                limit,
                offset,
                has_more: limit > 0 && total > offset + limit,
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
    /// Filter by license (case-insensitive contains).
    #[serde(default)]
    pub license: Option<String>,
    /// Sort order: date, version, or name.
    #[serde(default)]
    pub sort: SortOrder,
    /// Reverse sort order (default: false).
    #[serde(default)]
    pub reverse: bool,
    /// Maximum number of results (default: 50, 0 for unlimited).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Number of results to skip (default: 0).
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    50
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

/// Version history entry for API responses.
#[derive(Debug, Serialize, ToSchema)]
pub struct VersionHistorySchema {
    /// Package version string.
    pub version: String,
    /// First time this version was seen.
    pub first_seen: DateTime<Utc>,
    /// Last time this version was seen.
    pub last_seen: DateTime<Utc>,
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
}

impl From<PackageVersion> for PackageVersionSchema {
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
        }
    }
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
}

impl From<IndexStats> for IndexStatsSchema {
    fn from(s: IndexStats) -> Self {
        Self {
            total_ranges: s.total_ranges,
            unique_names: s.unique_names,
            unique_versions: s.unique_versions,
            oldest_commit_date: s.oldest_commit_date,
            newest_commit_date: s.newest_commit_date,
        }
    }
}

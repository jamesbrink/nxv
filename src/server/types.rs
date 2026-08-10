//! API request and response types.

use crate::db::queries::{IndexStats, PackageVersion};
use crate::search::{SearchResolution, SortOrder};
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
                resolution: None,
            }),
        }
    }

    /// Attach pagination and search-resolution metadata.
    pub fn with_search_pagination(
        data: T,
        total: usize,
        limit: usize,
        offset: usize,
        has_more: bool,
        resolution: Option<SearchResolution>,
    ) -> Self {
        Self {
            data,
            meta: Some(PaginationMeta {
                total,
                limit,
                offset,
                has_more,
                resolution,
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
    /// How a version-qualified search resolved the attribute scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<SearchResolution>,
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
    /// Include all matching attribute-path depths (requires a version).
    #[serde(default)]
    pub all_depths: bool,
    /// Filter by license (case-insensitive contains).
    #[serde(default)]
    pub license: Option<String>,
    /// Sort order: relevance, date, version, or name.
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
    /// Process runtime information (start time, uptime, total requests).
    pub runtime: RuntimeMetrics,
    /// Request latency percentiles over a rolling window of recent API requests.
    pub latency: LatencyMetrics,
    /// Request activity bucketed per minute for the last 30 minutes (oldest first).
    pub activity: Vec<ActivityBucketSchema>,
}

/// Runtime metrics: process start time and request count.
#[derive(Debug, Serialize, ToSchema)]
pub struct RuntimeMetrics {
    /// When the server process started.
    pub started_at: DateTime<Utc>,
    /// Seconds since the process started.
    pub uptime_seconds: u64,
    /// Total HTTP requests served since start (all routes).
    pub total_requests: u64,
}

/// Latency percentile snapshot in milliseconds.
#[derive(Debug, Serialize, ToSchema)]
pub struct LatencyMetrics {
    /// 50th-percentile request latency, milliseconds.
    pub p50_ms: f64,
    /// 95th-percentile request latency, milliseconds.
    pub p95_ms: f64,
    /// 99th-percentile request latency, milliseconds.
    pub p99_ms: f64,
    /// Number of samples that back these percentiles.
    pub samples: u64,
}

/// Per-minute request count.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityBucketSchema {
    /// Start of the minute (UTC, truncated to :00 seconds).
    pub minute: DateTime<Utc>,
    /// Number of requests served during that minute.
    pub count: u64,
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
    /// Whether nixpkgs reports a known advisory for this software version.
    ///
    /// Scoped by package name and version, so every attribute packaging the same
    /// build agrees (`emacs` and `emacs28` at 28.2 are both flagged). This is not
    /// the same question as "will nix refuse to build this attribute at this
    /// revision" — for that, read `known_vulnerabilities` off the version itself
    /// via `/packages/{attr}/versions/{version}/first`.
    pub is_insecure: bool,
    /// The advisory text as a JSON array, omitted when the version is clean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vulnerabilities: Option<String>,
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
    /// SPDX license identifiers (may be null when the package declares none).
    #[serde(serialize_with = "crate::db::json_array::serialize_opt")]
    #[schema(value_type = Option<Vec<String>>)]
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// Maintainer handles (may be null when the package declares none).
    #[serde(serialize_with = "crate::db::json_array::serialize_opt")]
    #[schema(value_type = Option<Vec<String>>)]
    pub maintainers: Option<String>,
    /// Systems the package declares support for.
    #[serde(serialize_with = "crate::db::json_array::serialize_opt")]
    #[schema(value_type = Option<Vec<String>>)]
    pub platforms: Option<String>,
    /// Source file path relative to nixpkgs root (null for older packages).
    ///
    /// Serialized even when absent: the CLI emits `null` here, and omitting it
    /// would make `/docs` and the API diverge from the row shape SKILL.md
    /// documents for both backends.
    pub source_path: Option<String>,
    /// Known security vulnerabilities (null for secure packages).
    ///
    /// Serialized even when absent, for the same reason as `source_path`.
    #[serde(serialize_with = "crate::db::json_array::serialize_opt")]
    #[schema(value_type = Option<Vec<String>>)]
    pub known_vulnerabilities: Option<String>,
}

impl From<PackageVersion> for PackageVersionSchema {
    /// Convert a `PackageVersion` into a `PackageVersionSchema` for API responses.
    ///
    /// The resulting schema preserves all public metadata fields (ids, version and
    /// commit timestamps, paths, description, license, homepage, maintainers,
    /// platforms, `source_path` and `known_vulnerabilities`).
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
        }
    }
}

/// Per-channel snapshot ingestion coverage (schema v4 indexes).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ChannelCoverageSchema)]
pub struct ChannelCoverageSchema {
    pub channel: String,
    pub releases_ingested: i64,
    pub releases_pending: i64,
    /// Of `releases_pending`, the pre-2020 nix-env-era releases. Scheduled runs
    /// never retry these; only `nxv index --backfill-evals` ingests them.
    pub releases_pending_eval_era: i64,
    pub releases_failed: i64,
    pub releases_skipped: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_release_date: Option<DateTime<Utc>>,
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
    /// Per-channel release coverage (empty for pre-v4 indexes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelCoverageSchema>,
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
            channels: s
                .channels
                .into_iter()
                .map(|c| ChannelCoverageSchema {
                    channel: c.channel,
                    releases_ingested: c.releases_ingested,
                    releases_pending: c.releases_pending,
                    releases_pending_eval_era: c.releases_pending_eval_era,
                    releases_failed: c.releases_failed,
                    releases_skipped: c.releases_skipped,
                    newest_release: c.newest_release,
                    newest_release_date: c.newest_release_date,
                })
                .collect(),
        }
    }
}

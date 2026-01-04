//! API request handlers.
//!
//! All database operations are wrapped in `tokio::task::spawn_blocking()` to prevent
//! blocking the async runtime. This is critical for server stability under load, as
//! rusqlite operations are synchronous and would otherwise block Tokio's worker threads.
//!
//! Additionally, all database operations:
//! - Acquire a semaphore permit to limit concurrent connections (prevents file descriptor exhaustion)
//! - Are wrapped in a timeout to prevent indefinite blocking (returns 504 on timeout)
//!
//! Each handler is instrumented with `tracing` to provide structured logging of requests,
//! parameters, and timing information.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use std::sync::Arc;
use tracing::instrument;

use crate::db::Database;
use crate::db::queries::{self, PackageVersion};
use crate::search::{self, SearchOptions};
use crate::version;

use super::AppState;
use super::error::ApiError;
use super::types::{self, *};

/// Execute a database operation with concurrency limiting and timeout.
///
/// This helper:
/// 1. Acquires a semaphore permit to limit concurrent DB connections
/// 2. Wraps the spawn_blocking call in a timeout
/// 3. Provides appropriate error responses for capacity/timeout issues
///
/// # Arguments
///
/// * `state` - The application state containing semaphore and timeout config
/// * `operation` - A closure that performs the database operation
///
/// # Returns
///
/// The result of the database operation, or an ApiError for timeout/capacity issues.
async fn run_db_operation<T, F>(state: &AppState, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, crate::error::NxvError> + Send + 'static,
{
    // Try to acquire a semaphore permit with a short timeout
    // If we can't get a permit quickly, the server is overloaded
    let permit = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.db_semaphore.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => {
            // Semaphore closed (shouldn't happen)
            return Err(ApiError::internal("Database semaphore closed"));
        }
        Err(_) => {
            // Timeout waiting for permit - server at capacity
            tracing::warn!("Database semaphore acquisition timed out - server at capacity");
            return Err(ApiError::overloaded());
        }
    };

    // Run the blocking operation with a timeout
    let timeout = state.db_timeout;
    let result = tokio::time::timeout(timeout, tokio::task::spawn_blocking(operation)).await;

    // Release the permit (done automatically when dropped, but be explicit)
    drop(permit);

    match result {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(e))) => {
            // Database error
            Err(e.into())
        }
        Ok(Err(e)) => {
            // spawn_blocking panicked
            tracing::error!(error = %e, "Database task panicked");
            Err(ApiError::internal(format!("Task join error: {}", e)))
        }
        Err(_) => {
            // Timeout
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                "Database operation timed out"
            );
            Err(ApiError::timeout(format!(
                "Database operation timed out after {} seconds",
                timeout.as_secs()
            )))
        }
    }
}

/// Search packages by name or attribute path and return paginated package versions.
///
/// Builds search options from query parameters, executes the index search, and wraps
/// the matching `PackageVersion` records along with pagination metadata in an `ApiResponse`.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use axum::extract::Query;
/// use crate::server::{AppState, SearchParams};
/// // Construct query params (as if received from a request)
/// let params = SearchParams {
///     q: "serde".to_string(),
///     version: None,
///     exact: None,
///     license: None,
///     sort: None,
///     reverse: None,
///     limit: Some(10),
///     offset: Some(0),
/// };
/// // In an application handler you'd call `search_packages(State(state), Query(params)).await`.
/// // This example demonstrates the intended parameters; actual invocation requires an Axum runtime and AppState.
/// ```
#[utoipa::path(
get,
path = "/api/v1/search",
params(
("q" = String, Query, description = "Package name or attribute path to search"),
("version" = Option<String>, Query, description = "Filter by version prefix"),
("exact" = Option<bool>, Query, description = "Exact match only"),
("license" = Option<String>, Query, description = "Filter by license"),
("sort" = Option<String>, Query, description = "Sort order: date, version, or name"),
("reverse" = Option<bool>, Query, description = "Reverse sort order"),
("limit" = Option<usize>, Query, description = "Maximum results (default: 50)"),
("offset" = Option<usize>, Query, description = "Results to skip"),
),
responses(
(status = 200, description = "Search results", body = ApiResponse<Vec<PackageVersionSchema>>),
(status = 400, description = "Invalid parameters"),
(status = 503, description = "Index not available"),
),
tag = "packages"
)]
#[instrument(skip(state), fields(query = %params.q, version = ?params.version, exact = ?params.exact))]
pub async fn search_packages(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db_path = state.db_path.clone();

    // Cap limit to prevent memory exhaustion from malicious requests
    let capped_limit = params.limit.min(types::MAX_LIMIT);

    let opts = SearchOptions {
        query: params.q,
        version: params.version,
        exact: params.exact,
        desc: false,
        license: params.license,
        sort: params.sort,
        reverse: params.reverse,
        full: false,
        limit: capped_limit,
        offset: params.offset,
    };

    // Clone opts values needed for response before moving opts into spawn_blocking
    let limit = opts.limit;
    let offset = opts.offset;

    let result = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_search").entered();
        let db = Database::open_readonly(&db_path)?;
        search::execute_search(db.connection(), &opts)
    })
    .await?;

    tracing::debug!(
        total = result.total,
        returned = result.data.len(),
        "Search completed"
    );

    Ok(Json(ApiResponse::with_pagination(
        result.data,
        result.total,
        limit,
        offset,
        result.has_more,
    )))
}

/// Search packages by their description using full-text search.
///
/// Performs a description-based search and returns a paginated list of matching package versions.
///
/// # Examples
///
/// ```
/// // Build a query URL for the description search endpoint.
/// let query = "serde features";
/// let limit = 10;
/// let offset = 0;
/// let url = format!("/api/v1/search/description?q={}&limit={}&offset={}", query, limit, offset);
/// assert!(url.starts_with("/api/v1/search/description"));
/// ```
#[utoipa::path(
get,
path = "/api/v1/search/description",
params(
("q" = String, Query, description = "Search query for descriptions"),
("limit" = Option<usize>, Query, description = "Maximum results (default: 50)"),
("offset" = Option<usize>, Query, description = "Results to skip"),
),
responses(
(status = 200, description = "Search results", body = ApiResponse<Vec<PackageVersionSchema>>),
(status = 400, description = "Invalid parameters"),
(status = 503, description = "Index not available"),
),
tag = "packages"
)]
#[instrument(skip(state), fields(query = %params.q))]
pub async fn search_description(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DescriptionSearchParams>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db_path = state.db_path.clone();
    let query = params.q.clone();
    // Cap limit to prevent memory exhaustion from malicious requests
    let limit = params.limit.min(types::MAX_LIMIT);
    let offset = params.offset;

    let results = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_fts_search").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::search_by_description(db.connection(), &query)
    })
    .await?;

    let total = results.len();
    tracing::debug!(total, "Description search completed");

    // Apply pagination
    let data: Vec<_> = if limit > 0 {
        results.into_iter().skip(offset).take(limit).collect()
    } else {
        results.into_iter().skip(offset).collect()
    };

    let has_more = limit > 0 && total > offset + data.len();

    Ok(Json(ApiResponse::with_pagination(
        data, total, limit, offset, has_more,
    )))
}

/// Retrieve all package versions that match the given attribute path.
///
/// Returns a list of package version records for the exact attribute path. If no matching
/// package versions are found, the handler returns a 404 Not Found error.
///
/// # Examples
///
/// ```rust,ignore
/// // Example: the handler returns an ApiResponse wrapping the matching package versions.
/// // In handler tests you would call `get_package` with a test AppState and assert the result.
/// use axum::Json;
/// use dpkg_indexer_common::ApiResponse;
///
/// let resp = ApiResponse::new(Vec::<()>::new());
/// assert!(matches!(resp.data.len(), 0));
/// ```
#[utoipa::path(
get,
path = "/api/v1/packages/{attr}",
params(
("attr" = String, Path, description = "Package attribute path"),
),
responses(
(status = 200, description = "Package info", body = ApiResponse<Vec<PackageVersionSchema>>),
(status = 404, description = "Package not found"),
(status = 503, description = "Index not available"),
),
tag = "packages"
)]
#[instrument(skip(state), fields(attr = %attr))]
pub async fn get_package(
    State(state): State<Arc<AppState>>,
    Path(attr): Path<String>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db_path = state.db_path.clone();
    let attr_clone = attr.clone();

    let packages = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_package").entered();
        let db = Database::open_readonly(&db_path)?;
        let results: Vec<_> = queries::search_by_attr(db.connection(), &attr_clone)?
            .into_iter()
            .filter(|p| p.attribute_path == attr_clone)
            .collect();
        Ok::<_, crate::error::NxvError>(results)
    })
    .await?;

    if packages.is_empty() {
        tracing::trace!("Package not found");
        return Err(ApiError::not_found(format!("Package '{}' not found", attr)));
    }

    tracing::debug!(versions = packages.len(), "Package found");

    Ok(Json(ApiResponse::new(packages)))
}

/// Get version history for a package.
#[utoipa::path(
    get,
    path = "/api/v1/packages/{attr}/history",
    params(
        ("attr" = String, Path, description = "Package attribute path"),
    ),
    responses(
        (status = 200, description = "Version history", body = ApiResponse<Vec<VersionHistorySchema>>),
        (status = 404, description = "Package not found"),
        (status = 503, description = "Index not available"),
    ),
    tag = "packages"
)]
#[instrument(skip(state), fields(attr = %attr))]
pub async fn get_version_history(
    State(state): State<Arc<AppState>>,
    Path(attr): Path<String>,
) -> Result<Json<ApiResponse<Vec<VersionHistorySchema>>>, ApiError> {
    let db_path = state.db_path.clone();
    let attr_clone = attr.clone();

    let history = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_history").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::get_version_history(db.connection(), &attr_clone)
    })
    .await?;

    if history.is_empty() {
        tracing::trace!("Package not found");
        return Err(ApiError::not_found(format!("Package '{}' not found", attr)));
    }

    tracing::debug!(versions = history.len(), "History retrieved");

    let entries: Vec<_> = history
        .into_iter()
        .map(|(version, first, last, is_insecure)| VersionHistorySchema {
            version,
            first_seen: first,
            last_seen: last,
            is_insecure,
        })
        .collect();

    Ok(Json(ApiResponse::new(entries)))
}

/// Retrieve information for a specific package version.
///
/// If the package version exists, the response payload contains the package record.
/// If the version is not found, the handler returns an `ApiError::not_found`.
///
/// # Examples
///
/// ```no_run
/// use axum::extract::{State, Path};
/// use std::sync::Arc;
/// // Assume `state` is an Arc<AppState> and `attr`, `version` are Strings.
/// // let resp = get_version_info(State(state), Path((attr, version))).await;
/// ```
#[utoipa::path(
get,
path = "/api/v1/packages/{attr}/versions/{version}",
params(
("attr" = String, Path, description = "Package attribute path"),
("version" = String, Path, description = "Package version"),
),
responses(
(status = 200, description = "Version info", body = ApiResponse<PackageVersionSchema>),
(status = 404, description = "Version not found"),
(status = 503, description = "Index not available"),
),
tag = "packages"
)]
#[instrument(skip(state), fields(attr = %attr, version = %version))]
pub async fn get_version_info(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db_path = state.db_path.clone();
    let attr_clone = attr.clone();
    let version_clone = version.clone();

    // Get the most recent occurrence of this version
    let pkg = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_version").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::get_last_occurrence(db.connection(), &attr_clone, &version_clone)
    })
    .await?;

    match pkg {
        Some(p) => {
            tracing::debug!("Version found");
            Ok(Json(ApiResponse::new(p)))
        }
        None => {
            tracing::trace!("Version not found");
            Err(ApiError::not_found(format!(
                "Version '{}' of '{}' not found",
                version, attr
            )))
        }
    }
}

/// Returns the first recorded occurrence of a specific package version.
///
/// Looks up the earliest stored PackageVersion for the given package attribute path (`attr`)
/// and version string (`version`). If found, the package version is returned wrapped in an
/// `ApiResponse`; if not found, a 404 ApiError is returned.
///
/// # Parameters
///
/// - `attr`: Package attribute path to look up.
/// - `version`: Specific package version to find.
///
/// # Returns
///
/// `ApiResponse<PackageVersion>` containing the first recorded occurrence of the requested version.
///
/// # Examples
///
/// ```no_run
/// // HTTP GET /api/v1/packages/my.package/versions/1.2.3/first
/// ```
#[utoipa::path(
get,
path = "/api/v1/packages/{attr}/versions/{version}/first",
params(
("attr" = String, Path, description = "Package attribute path"),
("version" = String, Path, description = "Package version"),
),
responses(
(status = 200, description = "First occurrence", body = ApiResponse<PackageVersionSchema>),
(status = 404, description = "Version not found"),
(status = 503, description = "Index not available"),
),
tag = "packages"
)]
#[instrument(skip(state), fields(attr = %attr, version = %version))]
pub async fn get_first_occurrence(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db_path = state.db_path.clone();
    let attr_clone = attr.clone();
    let version_clone = version.clone();

    let pkg = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_first_occurrence").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::get_first_occurrence(db.connection(), &attr_clone, &version_clone)
    })
    .await?;

    match pkg {
        Some(p) => {
            tracing::debug!("First occurrence found");
            Ok(Json(ApiResponse::new(p)))
        }
        None => {
            tracing::trace!("Version not found");
            Err(ApiError::not_found(format!(
                "Version '{}' of '{}' not found",
                version, attr
            )))
        }
    }
}

/// Get last occurrence of a specific version.
#[utoipa::path(
    get,
    path = "/api/v1/packages/{attr}/versions/{version}/last",
    params(
        ("attr" = String, Path, description = "Package attribute path"),
        ("version" = String, Path, description = "Package version"),
    ),
    responses(
        (status = 200, description = "Last occurrence", body = ApiResponse<PackageVersionSchema>),
        (status = 404, description = "Version not found"),
        (status = 503, description = "Index not available"),
    ),
    tag = "packages"
)]
#[instrument(skip(state), fields(attr = %attr, version = %version))]
pub async fn get_last_occurrence(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db_path = state.db_path.clone();
    let attr_clone = attr.clone();
    let version_clone = version.clone();

    let pkg = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_last_occurrence").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::get_last_occurrence(db.connection(), &attr_clone, &version_clone)
    })
    .await?;

    match pkg {
        Some(p) => {
            tracing::debug!("Last occurrence found");
            Ok(Json(ApiResponse::new(p)))
        }
        None => {
            tracing::trace!("Version not found");
            Err(ApiError::not_found(format!(
                "Version '{}' of '{}' not found",
                version, attr
            )))
        }
    }
}

/// Return index statistics for the server's package index.
///
/// On success, returns `Ok(Json(ApiResponse<IndexStatsSchema>))` containing the index statistics; on failure, returns `Err(ApiError)`.
///
/// # Examples
///
/// ```
/// # async fn doc_example() {
/// # use std::sync::Arc;
/// # use axum::extract::State;
/// # use crate::server::AppState;
/// # use crate::server::handlers::get_stats;
/// // `state` would normally be provided by the application runtime.
/// // let state: Arc<AppState> = ...;
/// // let result = get_stats(State(state)).await;
/// // match result {
/// //     Ok(json_resp) => println!("stats: {:?}", json_resp),
/// //     Err(err) => eprintln!("error: {:?}", err),
/// // }
/// # }
/// ```
#[utoipa::path(
get,
path = "/api/v1/stats",
responses(
(status = 200, description = "Index statistics", body = ApiResponse<IndexStatsSchema>),
(status = 503, description = "Index not available"),
),
tag = "stats"
)]
#[instrument(skip(state))]
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ApiResponse<IndexStatsSchema>>, ApiError> {
    let db_path = state.db_path.clone();

    let stats = run_db_operation(&state, move || {
        let _span = tracing::info_span!("db_get_stats").entered();
        let db = Database::open_readonly(&db_path)?;
        queries::get_stats(db.connection())
    })
    .await?;

    tracing::debug!(
        total_ranges = stats.total_ranges,
        unique_names = stats.unique_names,
        "Stats retrieved"
    );

    Ok(Json(ApiResponse::new(stats.into())))
}

/// Health check endpoint.
///
/// Note: The health check uses relaxed concurrency controls to ensure it can
/// respond even when the server is under heavy load. It has a shorter timeout
/// and doesn't wait for a semaphore permit.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    ),
    tag = "health"
)]
#[instrument(skip(state))]
pub async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let db_path = state.db_path.clone();

    // Health check uses a shorter timeout and doesn't require a semaphore permit
    // to ensure it can respond even under heavy load
    let index_commit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            Database::open_readonly(&db_path)
                .ok()
                .and_then(|db| db.get_meta("last_indexed_commit").ok().flatten())
        }),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .flatten();

    Json(HealthResponse {
        status: "ok".to_string(),
        version: version::full_version(),
        index_commit,
    })
}

/// Get server metrics for monitoring.
///
/// Returns metrics about the server including database connection pool utilization
/// and rate limiting configuration. Useful for monitoring dashboards and alerting.
///
/// # Returns
///
/// JSON response with server, database, and rate limit metrics.
#[utoipa::path(
    get,
    path = "/api/v1/metrics",
    responses(
        (status = 200, description = "Server metrics", body = MetricsResponse)
    ),
    tag = "monitoring"
)]
#[instrument(skip(state))]
pub async fn get_metrics(State(state): State<Arc<AppState>>) -> Json<types::MetricsResponse> {
    let available_permits = state.db_semaphore.available_permits();
    let in_use = state.max_db_connections.saturating_sub(available_permits);

    let rate_limit = state
        .rate_limit_config
        .as_ref()
        .map(|config| types::RateLimitMetrics {
            requests_per_second: config.requests_per_second,
            burst_size: config.burst_size,
            enabled: true,
        });

    Json(types::MetricsResponse {
        server: types::ServerMetrics {
            version: version::full_version(),
            status: "ok".to_string(),
        },
        database: types::DatabaseMetrics {
            max_connections: state.max_db_connections,
            available_permits,
            in_use,
            timeout_seconds: state.db_timeout.as_secs(),
        },
        rate_limit,
    })
}

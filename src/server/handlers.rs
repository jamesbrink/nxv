//! API request handlers.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use std::sync::Arc;

use crate::db::queries::{self, PackageVersion};
use crate::search::{self, SearchOptions};

use super::AppState;
use super::error::ApiError;
use super::types::*;

/// Search packages by name/attribute path.
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
pub async fn search_packages(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db = state.get_db()?;

    let opts = SearchOptions {
        query: params.q,
        version: params.version,
        exact: params.exact,
        desc: false,
        license: params.license,
        sort: params.sort,
        reverse: params.reverse,
        full: false,
        limit: params.limit,
        offset: params.offset,
    };

    let result = search::execute_search(db.connection(), &opts)?;

    Ok(Json(ApiResponse::with_pagination(
        result.data,
        result.total,
        opts.limit,
        opts.offset,
    )))
}

/// Search packages by description (FTS).
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
pub async fn search_description(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DescriptionSearchParams>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db = state.get_db()?;

    let results = queries::search_by_description(db.connection(), &params.q)?;
    let total = results.len();

    // Apply pagination
    let data: Vec<_> = if params.limit > 0 {
        results
            .into_iter()
            .skip(params.offset)
            .take(params.limit)
            .collect()
    } else {
        results.into_iter().skip(params.offset).collect()
    };

    Ok(Json(ApiResponse::with_pagination(
        data,
        total,
        params.limit,
        params.offset,
    )))
}

/// Get package info by attribute path.
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
pub async fn get_package(
    State(state): State<Arc<AppState>>,
    Path(attr): Path<String>,
) -> Result<Json<ApiResponse<Vec<PackageVersion>>>, ApiError> {
    let db = state.get_db()?;

    let packages: Vec<_> = queries::search_by_attr(db.connection(), &attr)?
        .into_iter()
        .filter(|p| p.attribute_path == attr)
        .collect();

    if packages.is_empty() {
        return Err(ApiError::not_found(format!("Package '{}' not found", attr)));
    }

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
        (status = 200, description = "Version history", body = ApiResponse<Vec<VersionHistoryEntry>>),
        (status = 404, description = "Package not found"),
        (status = 503, description = "Index not available"),
    ),
    tag = "packages"
)]
pub async fn get_version_history(
    State(state): State<Arc<AppState>>,
    Path(attr): Path<String>,
) -> Result<Json<ApiResponse<Vec<VersionHistoryEntry>>>, ApiError> {
    let db = state.get_db()?;

    let history = queries::get_version_history(db.connection(), &attr)?;

    if history.is_empty() {
        return Err(ApiError::not_found(format!("Package '{}' not found", attr)));
    }

    let entries: Vec<_> = history
        .into_iter()
        .map(|(version, first, last)| VersionHistoryEntry {
            version,
            first_seen: first,
            last_seen: last,
        })
        .collect();

    Ok(Json(ApiResponse::new(entries)))
}

/// Get specific version info.
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
pub async fn get_version_info(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db = state.get_db()?;

    // Get the most recent occurrence of this version
    let pkg = queries::get_last_occurrence(db.connection(), &attr, &version)?;

    match pkg {
        Some(p) => Ok(Json(ApiResponse::new(p))),
        None => Err(ApiError::not_found(format!(
            "Version '{}' of '{}' not found",
            version, attr
        ))),
    }
}

/// Get first occurrence of a specific version.
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
pub async fn get_first_occurrence(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db = state.get_db()?;

    let pkg = queries::get_first_occurrence(db.connection(), &attr, &version)?;

    match pkg {
        Some(p) => Ok(Json(ApiResponse::new(p))),
        None => Err(ApiError::not_found(format!(
            "Version '{}' of '{}' not found",
            version, attr
        ))),
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
pub async fn get_last_occurrence(
    State(state): State<Arc<AppState>>,
    Path((attr, version)): Path<(String, String)>,
) -> Result<Json<ApiResponse<PackageVersion>>, ApiError> {
    let db = state.get_db()?;

    let pkg = queries::get_last_occurrence(db.connection(), &attr, &version)?;

    match pkg {
        Some(p) => Ok(Json(ApiResponse::new(p))),
        None => Err(ApiError::not_found(format!(
            "Version '{}' of '{}' not found",
            version, attr
        ))),
    }
}

/// Get index statistics.
#[utoipa::path(
    get,
    path = "/api/v1/stats",
    responses(
        (status = 200, description = "Index statistics", body = ApiResponse<IndexStatsSchema>),
        (status = 503, description = "Index not available"),
    ),
    tag = "stats"
)]
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ApiResponse<IndexStatsSchema>>, ApiError> {
    let db = state.get_db()?;

    let stats = queries::get_stats(db.connection())?;

    Ok(Json(ApiResponse::new(stats.into())))
}

/// Health check endpoint.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    ),
    tag = "health"
)]
pub async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let index_commit = state
        .get_db()
        .ok()
        .and_then(|db| db.get_meta("last_indexed_commit").ok().flatten());

    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        index_commit,
    })
}

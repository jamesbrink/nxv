//! OpenAPI specification for the nxv API.

use utoipa::OpenApi;

use super::handlers;
use super::types::*;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "nxv API",
        description = "API for querying Nix package version history. Find specific versions of packages across nixpkgs history.",
        version = "1.0.0",
        license(name = "MIT", url = "https://opensource.org/licenses/MIT"),
        contact(name = "nxv", url = "https://github.com/jamesbrink/nxv")
    ),
    servers(
        (url = "/", description = "Local server")
    ),
    paths(
        handlers::search_packages,
        handlers::search_description,
        handlers::get_package,
        handlers::get_version_history,
        handlers::get_version_info,
        handlers::get_first_occurrence,
        handlers::get_last_occurrence,
        handlers::get_stats,
        handlers::health_check,
        handlers::get_metrics,
    ),
    components(schemas(
        PaginationMeta,
        HealthResponse,
        MetricsResponse,
        ServerMetrics,
        DatabaseMetrics,
        RateLimitMetrics,
        VersionHistorySchema,
        PackageVersionSchema,
        IndexStatsSchema,
    )),
    tags(
        (name = "packages", description = "Package search and information"),
        (name = "stats", description = "Index statistics"),
        (name = "health", description = "Health checks"),
        (name = "monitoring", description = "Server metrics and monitoring")
    )
)]
pub struct ApiDoc;

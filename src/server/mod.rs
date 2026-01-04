//! HTTP API server for nxv.
//!
//! This module provides a lightweight read-only API server that exposes
//! all query capabilities of nxv through RESTful endpoints.
//!
//! # Example
//!
//! ```bash
//! # Start the server
//! nxv serve --port 8080
//!
//! # Query packages
//! curl "http://localhost:8080/api/v1/search?q=python&version=3.11"
//!
//! # View API documentation
//! open "http://localhost:8080/docs"
//! ```
//!
//! # Logging
//!
//! The server uses the `tracing` crate for structured logging. Log level can be
//! controlled via the `RUST_LOG` environment variable:
//!
//! ```bash
//! # Debug logging for nxv, info for everything else
//! RUST_LOG=nxv=debug,info nxv serve
//!
//! # Trace-level logging for detailed request/response info
//! RUST_LOG=nxv=trace,tower_http=debug nxv serve
//! ```

pub mod error;
pub mod handlers;
pub mod openapi;
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    http::{HeaderValue, header},
    response::{Html, IntoResponse},
    routing::get,
};
use tokio::sync::Semaphore;
use tower_http::cors::{Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

/// Embedded frontend HTML.
const FRONTEND_HTML: &str = include_str!("../../frontend/index.html");

/// Embedded favicon SVG.
const FAVICON_SVG: &str = include_str!("../../frontend/favicon.svg");

/// Default maximum concurrent database operations.
/// This limits file descriptor usage and prevents spawn_blocking pool exhaustion.
/// Can be overridden via NXV_MAX_DB_CONNECTIONS environment variable.
const DEFAULT_MAX_DB_CONNECTIONS: usize = 32;

/// Default timeout for database operations in seconds.
/// Operations exceeding this will return 504 Gateway Timeout.
/// Can be overridden via NXV_DB_TIMEOUT_SECS environment variable.
const DEFAULT_DB_TIMEOUT_SECS: u64 = 30;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::db::Database;
use crate::error::{NxvError, Result};

/// Initialize the tracing subscriber for structured logging.
///
/// Configures logging based on the `RUST_LOG` environment variable. If not set,
/// defaults to `info` level for all crates. Supports both human-readable (default)
/// and JSON output formats.
///
/// # Examples
///
/// ```bash
/// # Default info-level logging
/// nxv serve
///
/// # Debug logging for nxv, info for everything else
/// RUST_LOG=nxv=debug,info nxv serve
///
/// # JSON output for log aggregation
/// RUST_LOG=info NXV_LOG_FORMAT=json nxv serve
/// ```
pub fn init_tracing() {
    // Check if JSON format is requested
    let use_json = std::env::var("NXV_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    // Build the env filter with sensible defaults
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Default: info for nxv and tower_http, warn for everything else
        EnvFilter::new("nxv=info,tower_http=info,warn")
    });

    // Use try_init() to avoid panicking if a subscriber is already set
    // (e.g., in tests or if run_server is called multiple times)
    let result = if use_json {
        // JSON format for log aggregation systems
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .try_init()
    } else {
        // Human-readable format for development
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(true).with_thread_ids(false))
            .try_init()
    };

    if let Err(e) = result {
        eprintln!("Note: tracing subscriber already initialized: {}", e);
    }
}

/// Shared application state.
pub struct AppState {
    /// Path to the database file.
    pub db_path: PathBuf,
    /// Semaphore to limit concurrent database operations.
    /// This prevents file descriptor exhaustion and spawn_blocking pool saturation.
    pub db_semaphore: Semaphore,
    /// Timeout for database operations.
    pub db_timeout: Duration,
}

impl AppState {
    /// Construct application state with database path and concurrency limits.
    ///
    /// Reads configuration from environment variables:
    /// - `NXV_MAX_DB_CONNECTIONS`: Maximum concurrent DB operations (default: 32)
    /// - `NXV_DB_TIMEOUT_SECS`: Timeout for DB operations in seconds (default: 30)
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// let state = AppState::new(PathBuf::from("index.sqlite"));
    /// assert_eq!(state.db_path, PathBuf::from("index.sqlite"));
    /// ```
    pub fn new(db_path: PathBuf) -> Self {
        let max_connections = std::env::var("NXV_MAX_DB_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_DB_CONNECTIONS);

        let timeout_secs = std::env::var("NXV_DB_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_DB_TIMEOUT_SECS);

        Self {
            db_path,
            db_semaphore: Semaphore::new(max_connections),
            db_timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Construct application state with explicit configuration.
    ///
    /// # Arguments
    ///
    /// * `db_path` - Path to the database file
    /// * `max_connections` - Maximum concurrent database operations
    /// * `timeout` - Timeout for database operations
    #[allow(dead_code)]
    pub fn with_config(db_path: PathBuf, max_connections: usize, timeout: Duration) -> Self {
        Self {
            db_path,
            db_semaphore: Semaphore::new(max_connections),
            db_timeout: timeout,
        }
    }

    /// Returns a read-only database connection opened from this state's path.
    ///
    /// This creates a new connection each time it is called; callers should obtain
    /// a connection per request for read-only operations.
    ///
    /// # Returns
    ///
    /// A `Database` opened in read-only mode on success.
    ///
    /// # Examples
    ///
    /// ```
    /// let state = crate::server::AppState::new(std::path::PathBuf::from("test.db"));
    /// let db = state.get_db().expect("open readonly database");
    /// ```
    #[allow(dead_code)]
    pub fn get_db(&self) -> Result<Database> {
        Database::open_readonly(&self.db_path)
    }
}

/// Server configuration.
pub struct ServerConfig {
    /// Host address to bind to.
    pub host: String,
    /// Port to listen on.
    pub port: u16,
    /// Path to the database.
    pub db_path: PathBuf,
    /// Enable CORS for all origins.
    pub cors: bool,
    /// Specific CORS origins (if cors is true but we want to restrict).
    pub cors_origins: Option<Vec<String>>,
}

/// Constructs the HTTP router with API endpoints, frontend routes, OpenAPI documentation, tracing,
/// and the provided shared state.
///
/// The returned router includes:
/// - API routes mounted under `/api/v1` (search, package lookups, version history, stats, health),
/// - Frontend at `/` and favicon endpoints,
/// - OpenAPI UI at `/docs` and raw spec at `/openapi.json`,
/// - Request tracing middleware, and
/// - The provided application state.
///
/// # Parameters
///
/// - `state`: shared application state to attach to the router.
/// - `cors`: optional CORS layer to apply to the router; if `None`, no CORS layer is applied.
///
/// # Returns
///
/// An `axum::Router` configured with routes, middleware, OpenAPI endpoints, and the supplied state.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use std::path::PathBuf;
/// // Construct minimal AppState for example purposes.
/// let state = Arc::new(crate::server::AppState::new(PathBuf::from("/tmp/db.sqlite")));
/// let router = crate::server::build_router(state, None);
/// // router can now be served with Axum.
/// ```
pub(crate) fn build_router(state: Arc<AppState>, cors: Option<CorsLayer>) -> Router {
    // Cache header values
    let cache_1h = HeaderValue::from_static("public, max-age=3600"); // 1 hour
    let cache_24h = HeaderValue::from_static("public, max-age=86400"); // 24 hours
    let no_cache = HeaderValue::from_static("no-cache, no-store, must-revalidate");

    // Cacheable API routes (1 hour) - package data changes infrequently
    let cacheable_api_routes = Router::new()
        .route("/search", get(handlers::search_packages))
        .route("/search/description", get(handlers::search_description))
        .route("/packages/{attr}", get(handlers::get_package))
        .route(
            "/packages/{attr}/history",
            get(handlers::get_version_history),
        )
        .route(
            "/packages/{attr}/versions/{version}",
            get(handlers::get_version_info),
        )
        .route(
            "/packages/{attr}/versions/{version}/first",
            get(handlers::get_first_occurrence),
        )
        .route(
            "/packages/{attr}/versions/{version}/last",
            get(handlers::get_last_occurrence),
        )
        .route("/stats", get(handlers::get_stats))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            cache_1h,
        ));

    // Health check - never cache (for load balancer checks)
    let health_route = Router::new()
        .route("/health", get(handlers::health_check))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            no_cache,
        ));

    // Combine API routes
    let api_routes = Router::new()
        .merge(cacheable_api_routes)
        .merge(health_route);

    // Static assets with long cache (24 hours)
    let static_routes = Router::new()
        .route("/", get(|| async { Html(FRONTEND_HTML) }))
        .route(
            "/favicon.svg",
            get(|| async {
                ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG).into_response()
            }),
        )
        .route(
            "/favicon.ico",
            get(|| async {
                ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG).into_response()
            }),
        )
        .merge(Scalar::with_url("/docs", openapi::ApiDoc::openapi()))
        .route(
            "/openapi.json",
            get(|| async { axum::Json(openapi::ApiDoc::openapi()) }),
        )
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            cache_24h,
        ));

    // Configure tracing layer with request/response logging
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_request(DefaultOnRequest::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    let mut app = Router::new()
        .merge(static_routes)
        .nest("/api/v1", api_routes)
        .layer(trace_layer)
        .with_state(state);

    if let Some(cors_layer) = cors {
        app = app.layer(cors_layer);
    }

    app
}

/// Start and run the HTTP API server using the provided server configuration.
///
/// Validates that the configured database path exists, configures optional CORS according
/// to the configuration, binds to the configured host and port, serves the application
/// router, and performs a graceful shutdown when an interrupt (Ctrl+C) is received.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
/// use tokio;
///
/// #[tokio::main]
/// async fn main() {
///     let config = ServerConfig {
///         host: "127.0.0.1".into(),
///         port: 8080,
///         db_path: PathBuf::from("/path/to/index.db"),
///         cors: false,
///         cors_origins: None,
///     };
///     // Run the server (will block until shutdown)
///     let _ = run_server(config).await;
/// }
/// ```
///
/// # Returns
///
/// `Ok(())` on clean shutdown; an `NxvError` if the database path is missing, socket
/// binding fails, or the server runtime encounters an I/O error.
pub async fn run_server(config: ServerConfig) -> Result<()> {
    // Initialize tracing for structured logging
    init_tracing();

    // Verify database exists before starting
    if !config.db_path.exists() {
        return Err(NxvError::NoIndex);
    }

    let state = Arc::new(AppState::new(config.db_path));

    // Log concurrency configuration
    tracing::info!(
        max_db_connections = state.db_semaphore.available_permits(),
        db_timeout_secs = state.db_timeout.as_secs(),
        "Database concurrency limits configured"
    );

    // Configure CORS
    let cors = if config.cors && config.cors_origins.is_none() {
        // Warn about permissive CORS when no specific origins are set
        tracing::warn!(
            "CORS enabled for all origins. For production, use --cors-origins to restrict to specific domains."
        );
        Some(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .max_age(Duration::from_secs(3600)),
        )
    } else if let Some(ref origins) = config.cors_origins {
        // Parse specific origins
        let origins: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
        if !origins.is_empty() {
            Some(
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods(Any)
                    .allow_headers(Any)
                    .max_age(Duration::from_secs(3600)),
            )
        } else {
            None
        }
    } else {
        None
    };

    let app = build_router(state, cors);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(NxvError::Io)?;

    tracing::info!(address = %addr, "Starting nxv API server");
    tracing::info!(url = %format!("http://{}/", addr), "Web UI available");
    tracing::info!(url = %format!("http://{}/docs", addr), "API documentation");
    tracing::info!("Press Ctrl+C to stop");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(NxvError::Io)?;

    tracing::info!("Server stopped");

    Ok(())
}

/// Await a CTRL+C (SIGINT) to trigger graceful shutdown.
///
/// Completes when the process receives a CTRL+C; panics if the signal handler cannot be installed.
///
/// # Examples
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// shutdown_signal().await;
/// // proceed with shutdown
/// # });
/// ```
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use rusqlite::Connection;
    use serde_json::Value;
    use tempfile::tempdir;
    use tower::ServiceExt;

    /// Create a test database with sample package data.
    fn create_test_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE package_versions (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                first_commit_hash TEXT NOT NULL,
                first_commit_date INTEGER NOT NULL,
                last_commit_hash TEXT NOT NULL,
                last_commit_date INTEGER NOT NULL,
                attribute_path TEXT NOT NULL,
                description TEXT,
                license TEXT,
                homepage TEXT,
                maintainers TEXT,
                platforms TEXT,
                source_path TEXT,
                known_vulnerabilities TEXT,
                UNIQUE(attribute_path, version, first_commit_hash)
            );
            CREATE INDEX idx_packages_name ON package_versions(name);
            CREATE INDEX idx_packages_attr ON package_versions(attribute_path);
            CREATE VIRTUAL TABLE package_versions_fts USING fts5(
                attribute_path, description, content='package_versions', content_rowid='id'
            );

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'abc1234567890def');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path, description, license)
            VALUES
                ('python', '3.11.0', 'aaa111', 1700000000, 'bbb222', 1700100000, 'python311', 'Python interpreter', 'PSF'),
                ('python', '3.12.0', 'ccc333', 1700200000, 'ddd444', 1700300000, 'python312', 'Python interpreter', 'PSF'),
                ('nodejs', '20.0.0', 'eee555', 1700400000, 'fff666', 1700500000, 'nodejs_20', 'Node.js runtime', 'MIT'),
                ('hello', '2.10', 'ggg777', 1700600000, 'hhh888', 1700700000, 'hello', 'Hello World program', 'GPL-3.0');

            INSERT INTO package_versions_fts (rowid, attribute_path, description)
            SELECT id, attribute_path, description FROM package_versions;
            "#,
        )
        .unwrap();
    }

    /// Helper to make a request and get the response body as JSON.
    async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn test_health_check() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/health").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
        assert_eq!(json["index_commit"], "abc1234567890def");
    }

    #[tokio::test]
    async fn test_search_packages() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/search?q=python").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["data"].is_array());
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 2); // python311 and python312
        assert!(json["meta"]["total"].as_u64().unwrap() >= 2);
    }

    #[tokio::test]
    async fn test_search_exact_match() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/search?q=hello&exact=true").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["attribute_path"], "hello");
    }

    #[tokio::test]
    async fn test_search_with_version_filter() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/search?q=python&version=3.12").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["version"], "3.12.0");
    }

    #[tokio::test]
    async fn test_search_description() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/search/description?q=runtime").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "nodejs");
    }

    #[tokio::test]
    async fn test_get_package() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/packages/hello").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "hello");
        assert_eq!(data[0]["version"], "2.10");
    }

    #[tokio::test]
    async fn test_get_package_not_found() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/packages/nonexistent").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["code"], "NOT_FOUND");
        assert!(json["message"].is_string());
    }

    #[tokio::test]
    async fn test_get_version_history() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/packages/python311/history").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert!(!data.is_empty());
        assert!(data[0]["version"].is_string());
        assert!(data[0]["first_seen"].is_string());
        assert!(data[0]["last_seen"].is_string());
    }

    #[tokio::test]
    async fn test_get_version_info() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/packages/python311/versions/3.11.0").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["name"], "python");
        assert_eq!(json["data"]["version"], "3.11.0");
    }

    #[tokio::test]
    async fn test_get_version_not_found() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/packages/python311/versions/9.9.9").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["code"], "NOT_FOUND");
        assert!(json["message"].is_string());
    }

    #[tokio::test]
    async fn test_get_stats() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) = get_json(&app, "/api/v1/stats").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["total_ranges"], 4);
        assert_eq!(json["data"]["unique_names"], 3); // python, nodejs, hello
    }

    #[tokio::test]
    async fn test_first_occurrence() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) =
            get_json(&app, "/api/v1/packages/python311/versions/3.11.0/first").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["first_commit_hash"], "aaa111");
    }

    #[tokio::test]
    async fn test_last_occurrence() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        let (status, json) =
            get_json(&app, "/api/v1/packages/python311/versions/3.11.0/last").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["last_commit_hash"], "bbb222");
    }

    #[tokio::test]
    async fn test_pagination() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        // Request with limit=1
        let (status, json) = get_json(&app, "/api/v1/search?q=python&limit=1").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(json["meta"]["limit"], 1);
        assert!(json["meta"]["has_more"].as_bool().unwrap());

        // Request with offset
        let (status, json) = get_json(&app, "/api/v1/search?q=python&limit=1&offset=1").await;

        assert_eq!(status, StatusCode::OK);
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(json["meta"]["offset"], 1);
    }

    #[tokio::test]
    async fn test_static_routes() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        // Test homepage
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Test favicon
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/favicon.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Test OpenAPI spec
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Test that concurrent requests can be processed in parallel.
    ///
    /// This test verifies that database operations don't block the async runtime
    /// by spawning multiple concurrent requests and verifying they all complete
    /// successfully. Prior to the spawn_blocking fix, synchronous SQLite calls
    /// would block Tokio worker threads, causing request queuing and potential
    /// timeouts under load.
    #[tokio::test]
    async fn test_concurrent_requests_not_blocked() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        // Spawn 20 concurrent requests to different endpoints
        let mut handles = Vec::new();

        for i in 0..20 {
            let app = app.clone();
            let uri = match i % 4 {
                0 => "/api/v1/search?q=python",
                1 => "/api/v1/packages/hello",
                2 => "/api/v1/stats",
                _ => "/api/v1/health",
            };

            handles.push(tokio::spawn(async move {
                let response = app
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                response.status()
            }));
        }

        // All requests should complete successfully
        let mut ok_count = 0;
        for handle in handles {
            let status = handle.await.unwrap();
            if status == StatusCode::OK {
                ok_count += 1;
            }
        }

        // All 20 concurrent requests should succeed
        assert_eq!(
            ok_count, 20,
            "All concurrent requests should complete successfully"
        );
    }

    /// Test that multiple concurrent search requests can complete.
    ///
    /// This tests the search endpoint specifically, which was the main source
    /// of blocking in production (as it performs the most complex queries).
    #[tokio::test]
    async fn test_concurrent_search_requests() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        // Spawn 10 concurrent search requests
        let mut handles = Vec::new();

        for _ in 0..10 {
            let app = app.clone();
            handles.push(tokio::spawn(async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .uri("/api/v1/search?q=python")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                let status = response.status();
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

                (status, json)
            }));
        }

        // All requests should return OK with valid search results
        for handle in handles {
            let (status, json) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK);
            assert!(json["data"].is_array());
            assert!(!json["data"].as_array().unwrap().is_empty());
        }
    }

    /// Test that requests with a timeout complete promptly.
    ///
    /// This verifies that handlers using spawn_blocking don't cause
    /// excessive latency that would trigger timeouts.
    #[tokio::test]
    async fn test_request_completes_within_timeout() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        create_test_db(&db_path);

        let state = Arc::new(AppState::new(db_path));
        let app = build_router(state, None);

        // Each request should complete within 5 seconds (generous timeout)
        let timeout_duration = std::time::Duration::from_secs(5);

        let result = tokio::time::timeout(timeout_duration, async {
            get_json(&app, "/api/v1/search?q=python").await
        })
        .await;

        assert!(
            result.is_ok(),
            "Request should complete within timeout (spawn_blocking should not cause indefinite blocking)"
        );

        let (status, _) = result.unwrap();
        assert_eq!(status, StatusCode::OK);
    }
}

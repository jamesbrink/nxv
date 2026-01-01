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

pub mod error;
pub mod handlers;
pub mod openapi;
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

/// Embedded frontend HTML.
const FRONTEND_HTML: &str = include_str!("../../frontend/index.html");

/// Embedded favicon SVG.
const FAVICON_SVG: &str = include_str!("../../frontend/favicon.svg");

use crate::db::Database;
use crate::error::{NxvError, Result};

/// Shared application state.
pub struct AppState {
    /// Path to the database file.
    pub db_path: PathBuf,
}

impl AppState {
    /// Construct application state holding the database file path.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// let state = AppState::new(PathBuf::from("index.sqlite"));
    /// assert_eq!(state.db_path, PathBuf::from("index.sqlite"));
    /// ```
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
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
fn build_router(state: Arc<AppState>, cors: Option<CorsLayer>) -> Router {
    let api_routes = Router::new()
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
        .route("/health", get(handlers::health_check));

    let mut app = Router::new()
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
        .nest("/api/v1", api_routes)
        .merge(Scalar::with_url("/docs", openapi::ApiDoc::openapi()))
        .route(
            "/openapi.json",
            get(|| async { axum::Json(openapi::ApiDoc::openapi()) }),
        )
        .layer(TraceLayer::new_for_http())
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
    // Verify database exists before starting
    if !config.db_path.exists() {
        return Err(NxvError::NoIndex);
    }

    let state = Arc::new(AppState::new(config.db_path));

    // Configure CORS
    let cors = if config.cors {
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

    eprintln!("Starting nxv API server on http://{}", addr);
    eprintln!("Web UI: http://{}/", addr);
    eprintln!("API documentation: http://{}/docs", addr);
    eprintln!("OpenAPI spec: http://{}/openapi.json", addr);
    eprintln!();
    eprintln!("Press Ctrl+C to stop");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(NxvError::Io)?;

    eprintln!("\nServer stopped");

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
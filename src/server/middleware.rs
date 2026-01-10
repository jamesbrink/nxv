//! HTTP middleware for the API server.
//!
//! Provides request-level middleware like correlation ID tracking.

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

/// Header name for request correlation IDs.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Request ID stored in request extensions.
///
/// This can be extracted by handlers that need access to the correlation ID.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Field is public for handler access
pub struct RequestId(pub String);

/// Middleware that adds request correlation IDs to each request.
///
/// If the request includes an `X-Request-ID` header, that value is used.
/// Otherwise, a new UUIDv4 is generated.
///
/// The request ID is:
/// - Stored in request extensions (accessible via `Extension<RequestId>`)
/// - Added to a tracing span for the duration of the request
/// - Echoed back in the response `X-Request-ID` header
///
/// # Example
///
/// ```ignore
/// use axum::middleware;
/// use nxv::server::middleware::request_id_middleware;
///
/// let app = Router::new()
///     .route("/", get(handler))
///     .layer(middleware::from_fn(request_id_middleware));
/// ```
pub async fn request_id_middleware(mut request: Request<Body>, next: Next) -> Response {
    // Extract existing request ID or generate a new one
    let request_id = request
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Store in request extensions for handler access
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    // Create a tracing span for this request
    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %request.method(),
        uri = %request.uri(),
    );

    // Execute the request within the span
    let response = {
        let _guard = span.enter();
        next.run(request).await
    };

    // Add request ID to response headers
    let mut response = response;
    if let Ok(header_value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(X_REQUEST_ID.clone(), header_value);
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
    };
    use tower::ServiceExt;

    async fn echo_handler() -> &'static str {
        "ok"
    }

    fn test_app() -> Router {
        Router::new()
            .route("/", get(echo_handler))
            .layer(middleware::from_fn(request_id_middleware))
    }

    #[tokio::test]
    async fn test_generates_request_id() {
        let app = test_app();

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Should have a request ID header
        let request_id = response.headers().get("x-request-id");
        assert!(request_id.is_some());

        // Should be a valid UUID
        let id_str = request_id.unwrap().to_str().unwrap();
        assert!(Uuid::parse_str(id_str).is_ok());
    }

    #[tokio::test]
    async fn test_preserves_provided_request_id() {
        let app = test_app();

        let custom_id = "my-custom-request-id-123";
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("x-request-id", custom_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Should echo back the same request ID
        let request_id = response
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(request_id, custom_id);
    }
}

//! API error handling.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// API error response body.
#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

/// API error type that converts to HTTP responses.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// Construct an ApiError representing HTTP 404 Not Found with the provided message.
    ///
    /// `message` is a human-readable string that will be included in the serialized error body.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = crate::server::error::ApiError::not_found("Package 'foo' not found");
    /// assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
    /// assert_eq!(err.code, "NOT_FOUND");
    /// ```
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "NOT_FOUND".to_string(),
            message: message.into(),
        }
    }

    /// Constructs an ApiError representing a 400 Bad Request.
    ///
    /// The returned error has status `StatusCode::BAD_REQUEST`, code `"BAD_REQUEST"`, and the provided message.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = ApiError::bad_request("invalid input");
    /// assert_eq!(err.status, StatusCode::BAD_REQUEST);
    /// assert_eq!(err.code, "BAD_REQUEST");
    /// assert_eq!(err.message, "invalid input");
    /// ```
    #[allow(dead_code)]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "BAD_REQUEST".to_string(),
            message: message.into(),
        }
    }

    /// Constructs an `ApiError` representing a 500 Internal Server Error.
    ///
    /// The provided message becomes the error's human-readable `message` and the error `code` is set to `"INTERNAL_ERROR"`.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = ApiError::internal("unexpected failure");
    /// assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    /// assert_eq!(err.code, "INTERNAL_ERROR");
    /// assert_eq!(err.message, "unexpected failure");
    /// ```
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "INTERNAL_ERROR".to_string(),
            message: message.into(),
        }
    }

    /// Constructs an `ApiError` for HTTP 503 Service Unavailable.
    ///
    /// # Parameters
    ///
    /// - `message`: Human-readable error message to include in the response body.
    ///
    /// # Returns
    ///
    /// An `ApiError` with status 503, code `"SERVICE_UNAVAILABLE"`, and the provided message.
    ///
    /// # Examples
    ///
    /// ```
    /// let err = crate::server::error::ApiError::unavailable("No package index available");
    /// assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    /// assert_eq!(err.code, "SERVICE_UNAVAILABLE");
    /// assert_eq!(err.message, "No package index available");
    /// ```
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "SERVICE_UNAVAILABLE".to_string(),
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    /// Convert the `ApiError` into an HTTP response with a JSON body.
    ///
    /// The response uses the error's HTTP status code and a JSON-serialized
    /// `ApiErrorBody` containing the `code` and `message`.
    ///
    /// # Examples
    ///
    /// ```
    /// use axum::http::StatusCode;
    /// let resp = crate::server::error::ApiError::not_found("pkg not found").into_response();
    /// assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    /// ```
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            code: self.code,
            message: self.message,
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    /// Converts a rusqlite::Error into an ApiError representing an internal server error.
    ///
    /// The resulting `ApiError` has HTTP status 500, code `INTERNAL_ERROR`, and a message
    /// prefixed with "Database error: " followed by the original error's description.
    ///
    /// # Examples
    ///
    /// ```
    /// // Given a `rusqlite::Error`, convert it to an `ApiError`:
    /// // let db_err: rusqlite::Error = /* obtained from rusqlite */ ;
    /// // let api_err = ApiError::from(db_err);
    /// ```
    fn from(err: rusqlite::Error) -> Self {
        ApiError::internal(format!("Database error: {}", err))
    }
}

impl From<crate::error::NxvError> for ApiError {
    /// Convert an `NxvError` into an `ApiError` suitable for HTTP responses.
    ///
    /// Maps specific `NxvError` variants to HTTP statuses and descriptive messages:
    /// - `NxvError::NoIndex` -> 503 Service Unavailable with guidance to run `nxv update`.
    /// - `NxvError::CorruptIndex(msg)` -> 503 Service Unavailable with `Corrupt index: {msg}`.
    /// - `NxvError::PackageNotFound(name)` -> 404 Not Found with `Package '{name}' not found`.
    /// - all other variants -> 500 Internal Server Error with the error's `to_string()` as the message.
    ///
    /// # Examples
    ///
    /// ```
    /// use http::StatusCode;
    /// use crate::server::error::ApiError;
    /// use crate::error::NxvError;
    ///
    /// let api_err = ApiError::from(NxvError::NoIndex);
    /// assert_eq!(api_err.status, StatusCode::SERVICE_UNAVAILABLE);
    /// ```
    fn from(err: crate::error::NxvError) -> Self {
        use crate::error::NxvError;
        match err {
            NxvError::NoIndex => {
                ApiError::unavailable("No package index found. Run 'nxv update' first.")
            }
            NxvError::CorruptIndex(msg) => ApiError::unavailable(format!("Corrupt index: {}", msg)),
            NxvError::PackageNotFound(name) => {
                ApiError::not_found(format!("Package '{}' not found", name))
            }
            _ => ApiError::internal(err.to_string()),
        }
    }
}
//! Error types for nxv.

use thiserror::Error;

/// Main error type for nxv.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum NxvError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("No index found. Run 'nxv update' to download the package index.")]
    NoIndex,

    #[error("Index is corrupted: {0}. Run 'nxv update --force' to re-download.")]
    CorruptIndex(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("{0}")]
    NetworkMessage(String),

    #[error("Package '{0}' not found")]
    PackageNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("API error: HTTP {status} at {url}")]
    ApiError { status: u16, url: String },

    #[error("Invalid manifest version: {0}. Please update nxv to the latest version.")]
    InvalidManifestVersion(u32),

    #[error("Manifest signature verification failed")]
    InvalidManifestSignature,

    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[cfg(feature = "indexer")]
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[cfg(feature = "indexer")]
    #[error("Nix evaluation failed: {0}")]
    NixEval(String),

    #[cfg(feature = "indexer")]
    #[error("Not a nixpkgs repository: {0}")]
    NotNixpkgsRepo(String),
}

/// Result type alias for nxv operations.
pub type Result<T> = std::result::Result<T, NxvError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_no_index_error_message() {
        let err = NxvError::NoIndex;
        let msg = err.to_string();
        assert!(msg.contains("nxv update"));
        assert!(msg.contains("No index found"));
    }

    #[test]
    fn test_corrupt_index_error_message() {
        let err = NxvError::CorruptIndex("missing table".to_string());
        let msg = err.to_string();
        assert!(msg.contains("missing table"));
        assert!(msg.contains("--force"));
    }

    #[test]
    fn test_package_not_found_error_message() {
        let err = NxvError::PackageNotFound("nonexistent-pkg".to_string());
        let msg = err.to_string();
        assert!(msg.contains("nonexistent-pkg"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn test_api_error_message() {
        let err = NxvError::ApiError {
            status: 500,
            url: "https://example.com/api".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("https://example.com/api"));
    }

    #[test]
    fn test_invalid_manifest_version_error_message() {
        let err = NxvError::InvalidManifestVersion(99);
        let msg = err.to_string();
        assert!(msg.contains("99"));
        assert!(msg.contains("update nxv"));
    }

    #[test]
    fn test_invalid_manifest_signature_error_message() {
        let err = NxvError::InvalidManifestSignature;
        let msg = err.to_string();
        assert!(msg.contains("signature"));
        assert!(msg.contains("failed"));
    }

    #[test]
    fn test_checksum_mismatch_error_message() {
        let err = NxvError::ChecksumMismatch {
            expected: "abc123".to_string(),
            actual: "xyz789".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("abc123"));
        assert!(msg.contains("xyz789"));
        assert!(msg.contains("mismatch"));
    }

    #[test]
    fn test_network_message_passthrough() {
        let err = NxvError::NetworkMessage("custom network error".to_string());
        let msg = err.to_string();
        assert_eq!(msg, "custom network error");
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err: NxvError = io_err.into();
        let msg = err.to_string();
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn test_json_error_conversion() {
        let json_str = "{ invalid json }";
        let json_err = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let err: NxvError = json_err.into();
        let msg = err.to_string();
        assert!(msg.contains("JSON"));
    }

    #[test]
    fn test_database_error_conversion() {
        // Create a database error by trying to open a directory as a database
        let result = rusqlite::Connection::open("/");
        if let Err(db_err) = result {
            let err: NxvError = db_err.into();
            let msg = err.to_string();
            assert!(msg.contains("Database"));
        }
    }

    #[test]
    fn test_error_debug_format() {
        let err = NxvError::PackageNotFound("test".to_string());
        let debug = format!("{:?}", err);
        assert!(debug.contains("PackageNotFound"));
        assert!(debug.contains("test"));
    }

    #[test]
    fn test_result_type_alias() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }

        fn returns_err() -> Result<i32> {
            Err(NxvError::NoIndex)
        }

        assert_eq!(returns_ok().unwrap(), 42);
        assert!(returns_err().is_err());
    }

    #[cfg(feature = "indexer")]
    mod indexer_tests {
        use super::*;

        #[test]
        fn test_nix_eval_error_message() {
            let err = NxvError::NixEval("evaluation failed".to_string());
            let msg = err.to_string();
            assert!(msg.contains("evaluation failed"));
        }

        #[test]
        fn test_not_nixpkgs_repo_error_message() {
            let err = NxvError::NotNixpkgsRepo("/some/path".to_string());
            let msg = err.to_string();
            assert!(msg.contains("/some/path"));
            assert!(msg.contains("nixpkgs"));
        }
    }
}

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

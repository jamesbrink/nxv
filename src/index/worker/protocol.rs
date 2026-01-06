//! IPC protocol for worker subprocess communication.
//!
//! Messages are JSON-serialized and newline-delimited.

use crate::index::extractor::{AttrPosition, PackageInfo};
use serde::{Deserialize, Serialize};

/// Request from parent to worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkRequest {
    /// Extract packages for a specific system and attribute list.
    #[serde(rename = "extract")]
    Extract {
        /// Target system (e.g., "x86_64-linux")
        system: String,
        /// Path to nixpkgs checkout
        repo_path: String,
        /// Attribute names to extract (empty = all)
        attrs: Vec<String>,
    },

    /// Extract attribute positions for file-to-attribute mapping.
    #[serde(rename = "extract_positions")]
    ExtractPositions {
        /// Target system (e.g., "x86_64-linux")
        system: String,
        /// Path to nixpkgs checkout
        repo_path: String,
    },

    /// Graceful shutdown request.
    #[serde(rename = "exit")]
    Exit,
}

/// Response from worker to parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkResponse {
    /// Successful extraction result.
    #[serde(rename = "result")]
    Result {
        /// Extracted packages
        packages: Vec<PackageInfo>,
    },

    /// Successful positions extraction result.
    #[serde(rename = "positions_result")]
    PositionsResult {
        /// Extracted attribute positions
        positions: Vec<AttrPosition>,
    },

    /// Extraction error.
    #[serde(rename = "error")]
    Error {
        /// Error message
        message: String,
    },

    /// Worker is ready for work.
    #[serde(rename = "ready")]
    Ready,

    /// Worker requests restart (memory threshold exceeded).
    #[serde(rename = "restart")]
    Restart,
}

impl WorkRequest {
    /// Create an extraction request.
    pub fn extract(
        system: impl Into<String>,
        repo_path: impl Into<String>,
        attrs: Vec<String>,
    ) -> Self {
        Self::Extract {
            system: system.into(),
            repo_path: repo_path.into(),
            attrs,
        }
    }

    /// Create a positions extraction request.
    pub fn extract_positions(system: impl Into<String>, repo_path: impl Into<String>) -> Self {
        Self::ExtractPositions {
            system: system.into(),
            repo_path: repo_path.into(),
        }
    }

    /// Serialize to JSON line (with newline).
    pub fn to_line(&self) -> String {
        let mut json = serde_json::to_string(self).expect("WorkRequest serialization failed");
        json.push('\n');
        json
    }

    /// Deserialize from JSON line.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

impl WorkResponse {
    /// Create a successful result response.
    pub fn result(packages: Vec<PackageInfo>) -> Self {
        Self::Result { packages }
    }

    /// Create a successful positions result response.
    pub fn positions_result(positions: Vec<AttrPosition>) -> Self {
        Self::PositionsResult { positions }
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Serialize to JSON line (with newline).
    pub fn to_line(&self) -> String {
        let mut json = serde_json::to_string(self).expect("WorkResponse serialization failed");
        json.push('\n');
        json
    }

    /// Deserialize from JSON line.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_request_serialization() {
        let req = WorkRequest::extract("x86_64-linux", "/path/to/nixpkgs", vec!["hello".into()]);
        let line = req.to_line();
        assert!(line.ends_with('\n'));
        assert!(line.contains("extract"));
        assert!(line.contains("x86_64-linux"));

        let parsed = WorkRequest::from_line(&line).unwrap();
        match parsed {
            WorkRequest::Extract {
                system,
                repo_path,
                attrs,
            } => {
                assert_eq!(system, "x86_64-linux");
                assert_eq!(repo_path, "/path/to/nixpkgs");
                assert_eq!(attrs, vec!["hello"]);
            }
            _ => panic!("Expected Extract variant"),
        }
    }

    #[test]
    fn test_work_request_exit() {
        let req = WorkRequest::Exit;
        let line = req.to_line();
        let parsed = WorkRequest::from_line(&line).unwrap();
        assert!(matches!(parsed, WorkRequest::Exit));
    }

    #[test]
    fn test_work_response_serialization() {
        let resp = WorkResponse::error("something went wrong");
        let line = resp.to_line();
        assert!(line.ends_with('\n'));
        assert!(line.contains("error"));

        let parsed = WorkResponse::from_line(&line).unwrap();
        match parsed {
            WorkResponse::Error { message } => {
                assert_eq!(message, "something went wrong");
            }
            _ => panic!("Expected Error variant"),
        }
    }

    #[test]
    fn test_work_response_ready() {
        let resp = WorkResponse::Ready;
        let line = resp.to_line();
        let parsed = WorkResponse::from_line(&line).unwrap();
        assert!(matches!(parsed, WorkResponse::Ready));
    }

    #[test]
    fn test_work_response_restart() {
        let resp = WorkResponse::Restart;
        let line = resp.to_line();
        let parsed = WorkResponse::from_line(&line).unwrap();
        assert!(matches!(parsed, WorkResponse::Restart));
    }
}

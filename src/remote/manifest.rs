//! Manifest parsing for remote index.

use crate::error::{NxvError, Result};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

/// Embedded public key for verifying manifest signatures.
/// This is the minisign public key used to sign official nxv manifests.
///
/// To generate a new keypair:
/// ```sh
/// minisign -G -p nxv.pub -s nxv.key -c "nxv manifest signing key"
/// ```
///
/// To sign a manifest:
/// ```sh
/// minisign -S -s nxv.key -m manifest.json
/// ```
///
/// To verify a manifest:
/// ```sh
/// minisign -Vm manifest.json -P RWSBt4RfZg0FEiiDheTd5vYE60LQTeDH+MHrgWDR6TtIHuGMAuJjMIaL
/// ```
pub const MANIFEST_PUBLIC_KEY: &str = "untrusted comment: minisign public key 12050D665F84B781
RWSBt4RfZg0FEiiDheTd5vYE60LQTeDH+MHrgWDR6TtIHuGMAuJjMIaL";

/// Remote index manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub latest_commit: String,
    pub latest_commit_date: String,
    pub full_index: IndexFile,
    pub bloom_filter: IndexFile,
    #[serde(default)]
    pub deltas: Vec<DeltaFile>,
}

/// A downloadable file with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFile {
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// A delta pack file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaFile {
    pub from_commit: String,
    pub to_commit: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

impl Manifest {
    /// Parse a manifest from JSON content.
    pub fn parse(json: &str) -> Result<Self> {
        let manifest: Manifest = serde_json::from_str(json)?;
        Ok(manifest)
    }

    /// Parse and verify a manifest from JSON content and its signature.
    ///
    /// The signature should be in minisign format.
    pub fn parse_and_verify(json: &str, signature: &str) -> Result<Self> {
        // Verify signature first
        verify_manifest_signature(json.as_bytes(), signature)?;

        // Then parse the manifest
        Self::parse(json)
    }

    /// Find a delta that can update from the given commit.
    pub fn find_delta(&self, from_commit: &str) -> Option<&DeltaFile> {
        self.deltas.iter().find(|d| d.from_commit == from_commit)
    }
}

/// Verify a manifest signature using the embedded public key.
pub fn verify_manifest_signature(manifest_data: &[u8], signature_str: &str) -> Result<()> {
    // Parse the public key
    let pk = minisign::PublicKey::from_base64(MANIFEST_PUBLIC_KEY)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    // Parse the signature
    let sig_box = minisign::SignatureBox::from_string(signature_str)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    // Verify
    let mut cursor = Cursor::new(manifest_data);
    minisign::verify(&pk, &sig_box, &mut cursor, true, false, true)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    Ok(())
}

/// Verify a manifest signature with a custom public key (for testing).
#[allow(dead_code)]
pub fn verify_manifest_signature_with_key(
    manifest_data: &[u8],
    signature_str: &str,
    public_key_str: &str,
) -> Result<()> {
    // Parse the public key
    let pk = minisign::PublicKey::from_base64(public_key_str)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    // Parse the signature
    let sig_box = minisign::SignatureBox::from_string(signature_str)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    // Verify
    let mut cursor = Cursor::new(manifest_data);
    minisign::verify(&pk, &sig_box, &mut cursor, true, false, true)
        .map_err(|_| NxvError::InvalidManifestSignature)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_parse() {
        let json = r#"{
            "version": 2,
            "latest_commit": "abc123def456",
            "latest_commit_date": "2024-01-15T12:00:00Z",
            "full_index": {
                "url": "https://example.com/index.db.zst",
                "size_bytes": 150000000,
                "sha256": "abc123"
            },
            "bloom_filter": {
                "url": "https://example.com/index.bloom",
                "size_bytes": 1200000,
                "sha256": "def456"
            },
            "deltas": []
        }"#;

        let manifest = Manifest::parse(json).unwrap();
        assert_eq!(manifest.version, 2);
        assert_eq!(manifest.latest_commit, "abc123def456");
    }

    #[test]
    fn test_manifest_find_delta() {
        let json = r#"{
            "version": 1,
            "latest_commit": "def456",
            "latest_commit_date": "2024-01-15T12:00:00Z",
            "full_index": {
                "url": "https://example.com/index.db.zst",
                "size_bytes": 150000000,
                "sha256": "abc123"
            },
            "bloom_filter": {
                "url": "https://example.com/index.bloom",
                "size_bytes": 1200000,
                "sha256": "def456"
            },
            "deltas": [
                {
                    "from_commit": "abc123",
                    "to_commit": "def456",
                    "url": "https://example.com/delta.sql.zst",
                    "size_bytes": 1000,
                    "sha256": "delta123"
                }
            ]
        }"#;

        let manifest = Manifest::parse(json).unwrap();
        let delta = manifest.find_delta("abc123");
        assert!(delta.is_some());
        assert_eq!(delta.unwrap().to_commit, "def456");

        let no_delta = manifest.find_delta("unknown");
        assert!(no_delta.is_none());
    }

    #[test]
    fn test_signature_verification_fails_on_tampered_manifest() {
        // This tests that verification fails with an invalid signature
        let manifest = r#"{"version":1,"latest_commit":"abc123"}"#;

        // A fake/invalid signature
        let fake_signature = "untrusted comment: fake signature\nRUTHy8Hb+LSqSJNRMBXPzXl8J5F5WTWmYu5J0CxmZWQ3z8rLnVJk9ABCAAAA\ntrusted comment: fake\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

        let result = verify_manifest_signature(manifest.as_bytes(), fake_signature);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            NxvError::InvalidManifestSignature
        ));
    }

    #[test]
    fn test_signature_verification_fails_on_invalid_format() {
        let manifest = r#"{"version":1}"#;
        let invalid_sig = "not a valid signature format";

        let result = verify_manifest_signature(manifest.as_bytes(), invalid_sig);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            NxvError::InvalidManifestSignature
        ));
    }

    #[test]
    fn test_parse_and_verify_fails_on_bad_signature() {
        let json = r#"{
            "version": 1,
            "latest_commit": "abc123",
            "latest_commit_date": "2024-01-15T12:00:00Z",
            "full_index": {
                "url": "https://example.com/index.db.zst",
                "size_bytes": 100,
                "sha256": "abc"
            },
            "bloom_filter": {
                "url": "https://example.com/index.bloom",
                "size_bytes": 100,
                "sha256": "def"
            },
            "deltas": []
        }"#;

        let bad_sig = "invalid signature";
        let result = Manifest::parse_and_verify(json, bad_sig);
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Manifest::parse should never panic on arbitrary input.
        #[test]
        fn parse_never_panics(s in "\\PC*") {
            // Should return Ok or Err, never panic
            let _ = Manifest::parse(&s);
        }

        /// Valid JSON that doesn't match schema should return Err, not panic.
        #[test]
        fn parse_rejects_wrong_schema(
            version in any::<i32>(),
            commit in "[a-f0-9]{12}",
        ) {
            // Missing required fields
            let json = format!(r#"{{"version": {}, "latest_commit": "{}"}}"#, version, commit);
            let result = Manifest::parse(&json);
            assert!(result.is_err());
        }

        /// Malformed URLs in manifest should parse but be detectable.
        #[test]
        fn parse_accepts_any_url_string(
            url in ".*",
            commit in "[a-f0-9]{12}",
        ) {
            let json = format!(
                r#"{{
                    "version": 1,
                    "latest_commit": "{}",
                    "latest_commit_date": "2024-01-15T12:00:00Z",
                    "full_index": {{"url": "{}", "size_bytes": 100, "sha256": "abc"}},
                    "bloom_filter": {{"url": "{}", "size_bytes": 100, "sha256": "def"}},
                    "deltas": []
                }}"#,
                commit,
                url.replace('\\', "\\\\").replace('"', "\\\""),
                url.replace('\\', "\\\\").replace('"', "\\\"")
            );
            // Should parse without panicking (URL validation is done elsewhere)
            let _ = Manifest::parse(&json);
        }

        /// find_delta should handle any commit string.
        #[test]
        fn find_delta_handles_any_commit(commit in ".*") {
            let json = r#"{
                "version": 1,
                "latest_commit": "def456",
                "latest_commit_date": "2024-01-15T12:00:00Z",
                "full_index": {"url": "https://example.com/index.db.zst", "size_bytes": 100, "sha256": "abc"},
                "bloom_filter": {"url": "https://example.com/bloom.bin", "size_bytes": 100, "sha256": "def"},
                "deltas": [
                    {"from_commit": "abc123", "to_commit": "def456", "url": "https://example.com/delta.sql.zst", "size_bytes": 100, "sha256": "ghi"}
                ]
            }"#;

            let manifest = Manifest::parse(json).unwrap();
            // Should never panic
            let _ = manifest.find_delta(&commit);
        }

        /// Signature verification should reject random data without panicking.
        #[test]
        fn verify_rejects_random_signature(
            manifest in "[\\x00-\\x7F]{10,100}",
            signature in "[\\x00-\\x7F]{10,100}",
        ) {
            let result = verify_manifest_signature(manifest.as_bytes(), &signature);
            // Should return Err, not panic
            assert!(result.is_err());
        }
    }
}

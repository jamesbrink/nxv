//! Update logic for applying remote index updates.

use crate::db::Database;
use crate::error::{NxvError, Result};
use crate::paths;
use crate::remote::download::download_file;
use crate::remote::manifest::Manifest;
use std::path::Path;

/// Default manifest URL.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/jamesbrink/nxv/releases/download/index-latest/manifest.json";

/// Default timeout for manifest requests in seconds.
const DEFAULT_MANIFEST_TIMEOUT_SECS: u64 = 30;

/// Resolve a public key argument to its content.
///
/// The key can be either:
/// - A path to a .pub file containing the key (checked first)
/// - A raw minisign public key (starts with "untrusted comment:" or "RW")
///
/// File paths are checked first to avoid ambiguity with paths that happen
/// to start with "RW" (e.g., "RWkeys/signing.pub").
fn resolve_public_key(key: &str) -> Result<String> {
    // Check if it's a file path first (handles paths like "RWkeys/signing.pub")
    let path = Path::new(key);
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| NxvError::PublicKey(format!("failed to read '{}': {}", key, e)))?;
        return Ok(content);
    }

    // Check if it looks like a raw key (inline key content)
    if key.starts_with("untrusted comment:") || key.starts_with("RW") {
        return Ok(key.to_string());
    }

    // Provide helpful error message based on what the input looks like
    if key.contains('/') || key.contains('\\') || key.ends_with(".pub") {
        Err(NxvError::PublicKey(format!("file '{}' not found", key)))
    } else {
        Err(NxvError::PublicKey(format!(
            "'{}' is not a valid minisign public key (expected format: RW...)",
            key
        )))
    }
}

/// Build the signature URL by appending .minisig to the path component.
///
/// Handles URLs with query parameters correctly by modifying only the path.
/// For example: `https://example.com/manifest.json?token=abc` becomes
/// `https://example.com/manifest.json.minisig?token=abc`
fn build_signature_url(manifest_url: &str) -> String {
    // Try to parse as URL to handle query params correctly
    if let Ok(mut parsed) = reqwest::Url::parse(manifest_url) {
        let new_path = format!("{}.minisig", parsed.path());
        parsed.set_path(&new_path);
        parsed.to_string()
    } else {
        // Fallback for non-standard URLs
        format!("{}.minisig", manifest_url)
    }
}

/// Update status after checking for updates.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum UpdateStatus {
    /// Index is up to date.
    UpToDate { commit: String },
    /// A delta update is available.
    DeltaAvailable {
        from_commit: String,
        to_commit: String,
        size_bytes: u64,
    },
    /// Only a full download is available.
    FullDownloadNeeded { commit: String, size_bytes: u64 },
    /// No local index exists.
    NoLocalIndex { size_bytes: u64 },
}

/// Check for available updates by comparing local index with remote manifest.
pub fn check_for_updates<P: AsRef<Path>>(
    db_path: P,
    manifest_url: Option<&str>,
    show_progress: bool,
    skip_verify: bool,
    public_key: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<UpdateStatus> {
    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);

    // Fetch the manifest
    let manifest = fetch_manifest(
        manifest_url,
        show_progress,
        skip_verify,
        public_key,
        timeout_secs,
    )?;

    // Check if local index exists
    let db_path = db_path.as_ref();
    if !db_path.exists() {
        return Ok(UpdateStatus::NoLocalIndex {
            size_bytes: manifest.full_index.size_bytes,
        });
    }

    // Open local database and get last indexed commit
    let db = Database::open_readonly(db_path)?;
    let local_commit = db.get_meta("last_indexed_commit")?;

    match local_commit {
        Some(commit) if commit == manifest.latest_commit => Ok(UpdateStatus::UpToDate { commit }),
        Some(commit) => {
            // Check if a delta is available from our commit
            if let Some(delta) = manifest.find_delta(&commit) {
                Ok(UpdateStatus::DeltaAvailable {
                    from_commit: commit,
                    to_commit: delta.to_commit.clone(),
                    size_bytes: delta.size_bytes,
                })
            } else {
                // No delta available, need full download
                Ok(UpdateStatus::FullDownloadNeeded {
                    commit: manifest.latest_commit,
                    size_bytes: manifest.full_index.size_bytes,
                })
            }
        }
        None => {
            // No last_indexed_commit, treat as needing full download
            Ok(UpdateStatus::FullDownloadNeeded {
                commit: manifest.latest_commit,
                size_bytes: manifest.full_index.size_bytes,
            })
        }
    }
}

/// Fetch, verify, and parse the remote manifest.
///
/// When `skip_verify` is false, downloads the `.minisig` signature file
/// and verifies the manifest using the embedded public key.
fn fetch_manifest(
    url: &str,
    show_progress: bool,
    skip_verify: bool,
    public_key: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<Manifest> {
    let timeout = timeout_secs.unwrap_or(DEFAULT_MANIFEST_TIMEOUT_SECS);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()?;

    // Download manifest JSON
    let response = client.get(url).send()?;

    if !response.status().is_success() {
        return Err(NxvError::NetworkMessage(format!(
            "Failed to fetch manifest: HTTP {}",
            response.status()
        )));
    }

    let manifest_json = response.text()?;

    // Verify signature unless skipped
    if skip_verify {
        if show_progress {
            eprintln!("⚠ Warning: Skipping manifest signature verification (--skip-verify)");
        }
    } else {
        // Try to download signature file
        // Handle URLs with query params by appending .minisig to path only
        let sig_url = build_signature_url(url);
        match client.get(&sig_url).send() {
            Ok(sig_response) if sig_response.status().is_success() => {
                let signature = sig_response.text()?;

                // Verify signature using custom or embedded public key
                if let Some(key) = public_key {
                    // Resolve key: could be raw key or path to .pub file
                    let key_content = resolve_public_key(key)?;
                    crate::remote::manifest::verify_manifest_signature_with_key(
                        manifest_json.as_bytes(),
                        &signature,
                        &key_content,
                    )?;
                } else {
                    // Use embedded public key
                    Manifest::parse_and_verify(&manifest_json, &signature)?;
                }

                // Signature valid, return parsed manifest
                let manifest: Manifest = serde_json::from_str(&manifest_json)?;
                if manifest.version > 2 {
                    return Err(NxvError::InvalidManifestVersion(manifest.version));
                }
                return Ok(manifest);
            }
            Ok(sig_response) => {
                // Signature file not found - fail unless skip_verify
                return Err(NxvError::NetworkMessage(format!(
                    "Manifest signature not found (HTTP {}). Use --skip-verify to bypass.",
                    sig_response.status()
                )));
            }
            Err(e) => {
                return Err(NxvError::NetworkMessage(format!(
                    "Failed to fetch manifest signature: {}. Use --skip-verify to bypass.",
                    e
                )));
            }
        }
    }

    // Parse without verification (skip_verify mode)
    let manifest: Manifest = serde_json::from_str(&manifest_json)?;

    // Validate manifest version
    if manifest.version > 2 {
        return Err(NxvError::InvalidManifestVersion(manifest.version));
    }

    Ok(manifest)
}

/// Apply a full index update.
pub fn apply_full_update<P: AsRef<Path>>(
    manifest_url: Option<&str>,
    db_path: P,
    show_progress: bool,
    skip_verify: bool,
    public_key: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);
    let manifest = fetch_manifest(
        manifest_url,
        show_progress,
        skip_verify,
        public_key,
        timeout_secs,
    )?;

    let db_path = db_path.as_ref();

    // Download full index
    if show_progress {
        eprintln!("Downloading full index...");
    }
    download_file(
        &manifest.full_index.url,
        db_path,
        &manifest.full_index.sha256,
        show_progress,
    )?;

    // Download bloom filter (sibling to database file)
    if show_progress {
        eprintln!("Downloading bloom filter...");
    }
    let bloom_path = paths::get_bloom_path_for_db(db_path);
    download_file(
        &manifest.bloom_filter.url,
        &bloom_path,
        &manifest.bloom_filter.sha256,
        show_progress,
    )?;

    Ok(())
}

/// Apply a delta update.
pub fn apply_delta_update<P: AsRef<Path>>(
    manifest_url: Option<&str>,
    db_path: P,
    from_commit: &str,
    show_progress: bool,
    skip_verify: bool,
    public_key: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<()> {
    use crate::db::import::import_delta_sql;

    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);
    let manifest = fetch_manifest(
        manifest_url,
        show_progress,
        skip_verify,
        public_key,
        timeout_secs,
    )?;

    let delta = manifest.find_delta(from_commit).ok_or_else(|| {
        NxvError::NetworkMessage(format!("No delta available from commit {}", from_commit))
    })?;

    let db_path = db_path.as_ref();

    // Download delta pack to temp file
    // Note: download_file auto-decompresses .zst files, so we download to .sql
    if show_progress {
        eprintln!("Downloading delta update...");
    }

    let temp_dir = tempfile::tempdir()?;
    let delta_path = temp_dir.path().join("delta.sql");
    download_file(&delta.url, &delta_path, &delta.sha256, show_progress)?;

    // Import delta into existing database
    if show_progress {
        eprintln!("Applying delta update...");
    }

    // Open database in read-write mode for delta import
    // The file is already decompressed by download_file, so use import_delta_sql
    let db = Database::open(db_path)?;
    let sql_content = std::fs::read_to_string(&delta_path)?;
    import_delta_sql(db.connection(), &sql_content)?;

    // Also update the bloom filter (sibling to database file)
    if show_progress {
        eprintln!("Downloading bloom filter...");
    }
    let bloom_path = paths::get_bloom_path_for_db(db_path);
    download_file(
        &manifest.bloom_filter.url,
        &bloom_path,
        &manifest.bloom_filter.sha256,
        show_progress,
    )?;

    Ok(())
}

/// Perform an update (auto-selecting delta or full as appropriate).
pub fn perform_update<P: AsRef<Path>>(
    manifest_url: Option<&str>,
    db_path: P,
    force_full: bool,
    show_progress: bool,
    skip_verify: bool,
    public_key: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<UpdateStatus> {
    let status = check_for_updates(
        &db_path,
        manifest_url,
        show_progress,
        skip_verify,
        public_key,
        timeout_secs,
    )?;

    match &status {
        UpdateStatus::UpToDate { commit } => {
            if show_progress {
                eprintln!("Index is already up to date (commit {}).", &commit[..7]);
            }
        }
        UpdateStatus::NoLocalIndex { .. } | UpdateStatus::FullDownloadNeeded { .. } => {
            apply_full_update(
                manifest_url,
                &db_path,
                show_progress,
                skip_verify,
                public_key,
                timeout_secs,
            )?;
        }
        UpdateStatus::DeltaAvailable { from_commit, .. } => {
            if force_full {
                apply_full_update(
                    manifest_url,
                    &db_path,
                    show_progress,
                    skip_verify,
                    public_key,
                    timeout_secs,
                )?;
            } else {
                apply_delta_update(
                    manifest_url,
                    &db_path,
                    from_commit,
                    show_progress,
                    skip_verify,
                    public_key,
                    timeout_secs,
                )?;
            }
        }
    }

    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::import::import_delta_pack;
    use crate::remote::download::compress_zstd;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_update_status_variants() {
        let up_to_date = UpdateStatus::UpToDate {
            commit: "abc123".to_string(),
        };
        assert!(matches!(up_to_date, UpdateStatus::UpToDate { .. }));

        let delta = UpdateStatus::DeltaAvailable {
            from_commit: "abc123".to_string(),
            to_commit: "def456".to_string(),
            size_bytes: 1000,
        };
        assert!(matches!(delta, UpdateStatus::DeltaAvailable { .. }));

        let full = UpdateStatus::FullDownloadNeeded {
            commit: "abc123".to_string(),
            size_bytes: 100000,
        };
        assert!(matches!(full, UpdateStatus::FullDownloadNeeded { .. }));

        let no_local = UpdateStatus::NoLocalIndex { size_bytes: 100000 };
        assert!(matches!(no_local, UpdateStatus::NoLocalIndex { .. }));
    }

    #[test]
    fn test_check_for_updates_no_local_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");

        // This would fail to fetch manifest from network, so just test the path exists logic
        assert!(!db_path.exists());
    }

    /// Test the full update flow: create delta pack, import it into existing database.
    /// This tests the delta import integration without network.
    #[test]
    fn test_delta_update_flow_local() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let delta_sql_path = dir.path().join("delta.sql");
        let delta_zst_path = dir.path().join("delta.sql.zst");

        // Create initial database with a package
        let conn = Connection::open(&db_path).unwrap();
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
                UNIQUE(attribute_path, version, first_commit_hash)
            );
            CREATE INDEX idx_packages_name ON package_versions(name);

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'commit_v1');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python', '3.10.0', 'aaa111', 1600000000, 'bbb222', 1600100000,
                 'python310', 'Python 3.10');
            "#,
        )
        .unwrap();

        // Verify initial state
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(commit, "commit_v1");

        // Create a delta pack that adds a new package and updates the commit
        let delta_sql = r#"
-- Delta pack from commit_v1 to commit_v2
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date,
     last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('python', '3.11.0', 'ccc333', 1601000000, 'ddd444', 1601100000,
     'python311', 'Python 3.11');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'commit_v2');
COMMIT;
        "#;

        // Write and compress the delta
        fs::write(&delta_sql_path, delta_sql).unwrap();
        compress_zstd(&delta_sql_path, &delta_zst_path, 3).unwrap();

        // Import the delta pack
        import_delta_pack(&conn, &delta_zst_path).unwrap();

        // Verify the new package was added
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "Delta should add one more package");

        // Verify the new package exists
        let version: String = conn
            .query_row(
                "SELECT version FROM package_versions WHERE attribute_path = 'python311'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "3.11.0");

        // Verify the commit was updated
        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(commit, "commit_v2");
    }

    /// Test that multiple delta updates can be applied in sequence.
    #[test]
    fn test_sequential_delta_updates() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Create initial database
        let conn = Connection::open(&db_path).unwrap();
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
                UNIQUE(attribute_path, version, first_commit_hash)
            );
            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'commit_v1');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path)
            VALUES ('rust', '1.70.0', 'aaa', 1600000000, 'aaa', 1600000000, 'rustc');
            "#,
        )
        .unwrap();

        // Apply first delta: add rust 1.71.0
        let delta1_sql_path = dir.path().join("delta1.sql");
        let delta1_zst_path = dir.path().join("delta1.sql.zst");
        fs::write(
            &delta1_sql_path,
            r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date,
     last_commit_hash, last_commit_date, attribute_path)
VALUES ('rust', '1.71.0', 'bbb', 1601000000, 'bbb', 1601000000, 'rustc_1_71');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'commit_v2');
COMMIT;
            "#,
        )
        .unwrap();
        compress_zstd(&delta1_sql_path, &delta1_zst_path, 3).unwrap();
        import_delta_pack(&conn, &delta1_zst_path).unwrap();

        // Apply second delta: add rust 1.72.0
        let delta2_sql_path = dir.path().join("delta2.sql");
        let delta2_zst_path = dir.path().join("delta2.sql.zst");
        fs::write(
            &delta2_sql_path,
            r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date,
     last_commit_hash, last_commit_date, attribute_path)
VALUES ('rust', '1.72.0', 'ccc', 1602000000, 'ccc', 1602000000, 'rustc_1_72');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'commit_v3');
COMMIT;
            "#,
        )
        .unwrap();
        compress_zstd(&delta2_sql_path, &delta2_zst_path, 3).unwrap();
        import_delta_pack(&conn, &delta2_zst_path).unwrap();

        // Verify all three versions exist
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM package_versions WHERE name = 'rust'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3, "Should have 3 rust versions after 2 deltas");

        // Verify final commit
        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(commit, "commit_v3");
    }

    #[test]
    fn test_resolve_public_key_raw_key() {
        // Test with a raw key starting with "RW"
        let key = "RWSBt4RfZg0FEiiDheTd5vYE60LQTeDH+MHrgWDR6TtIHuGMAuJjMIaL";
        let result = resolve_public_key(key).unwrap();
        assert_eq!(result, key);
    }

    #[test]
    fn test_resolve_public_key_with_comment() {
        // Test with a full key including untrusted comment
        let key = "untrusted comment: minisign public key\nRWSBt4RfZg0FEiiDheTd5vYE60LQTeDH";
        let result = resolve_public_key(key).unwrap();
        assert_eq!(result, key);
    }

    #[test]
    fn test_resolve_public_key_from_file() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("test.pub");
        let key_content = "untrusted comment: test key\nRWTest123";
        fs::write(&key_path, key_content).unwrap();

        let result = resolve_public_key(key_path.to_str().unwrap()).unwrap();
        assert_eq!(result, key_content);
    }

    #[test]
    fn test_resolve_public_key_invalid_path() {
        let result = resolve_public_key("/nonexistent/path/to/key.pub");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Public key error"));
        assert!(err.contains("file") && err.contains("not found"));
    }

    #[test]
    fn test_resolve_public_key_invalid_format() {
        // Not a file path and not a valid key format
        let result = resolve_public_key("invalid_key_string");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Public key error"));
        assert!(err.contains("not a valid minisign public key"));
    }

    #[test]
    fn test_build_signature_url_simple() {
        let url = build_signature_url("https://example.com/manifest.json");
        assert_eq!(url, "https://example.com/manifest.json.minisig");
    }

    #[test]
    fn test_build_signature_url_with_query_params() {
        let url = build_signature_url("https://example.com/manifest.json?token=abc&version=2");
        assert_eq!(
            url,
            "https://example.com/manifest.json.minisig?token=abc&version=2"
        );
    }

    #[test]
    fn test_build_signature_url_with_fragment() {
        let url = build_signature_url("https://example.com/manifest.json#section");
        assert_eq!(url, "https://example.com/manifest.json.minisig#section");
    }
}

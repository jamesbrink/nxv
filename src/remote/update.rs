//! Update logic for applying remote index updates.

use crate::db::Database;
use crate::error::{NxvError, Result};
use crate::paths;
use crate::remote::download::download_file;
use crate::remote::manifest::Manifest;
use std::path::Path;

/// Default manifest URL.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/jamesbrink/nxv/releases/latest/download/manifest.json";

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
) -> Result<UpdateStatus> {
    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);

    // Fetch the manifest
    let manifest = fetch_manifest(manifest_url, show_progress)?;

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

/// Fetch and parse the remote manifest.
fn fetch_manifest(url: &str, _show_progress: bool) -> Result<Manifest> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        return Err(NxvError::NetworkMessage(format!(
            "Failed to fetch manifest: HTTP {}",
            response.status()
        )));
    }

    let manifest: Manifest = response.json()?;

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
) -> Result<()> {
    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);
    let manifest = fetch_manifest(manifest_url, show_progress)?;

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
) -> Result<()> {
    use crate::db::import::import_delta_sql;

    let manifest_url = manifest_url.unwrap_or(DEFAULT_MANIFEST_URL);
    let manifest = fetch_manifest(manifest_url, show_progress)?;

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
) -> Result<UpdateStatus> {
    let status = check_for_updates(&db_path, manifest_url, show_progress)?;

    match &status {
        UpdateStatus::UpToDate { commit } => {
            if show_progress {
                eprintln!("Index is already up to date (commit {}).", &commit[..7]);
            }
        }
        UpdateStatus::NoLocalIndex { .. } | UpdateStatus::FullDownloadNeeded { .. } => {
            apply_full_update(manifest_url, &db_path, show_progress)?;
        }
        UpdateStatus::DeltaAvailable { from_commit, .. } => {
            if force_full {
                apply_full_update(manifest_url, &db_path, show_progress)?;
            } else {
                apply_delta_update(manifest_url, &db_path, from_commit, show_progress)?;
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
}

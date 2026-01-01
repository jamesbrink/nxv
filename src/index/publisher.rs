//! Index publishing utilities for generating distributable artifacts.

#![allow(dead_code)]

use crate::bloom::PackageBloomFilter;
use crate::db::Database;
use crate::db::queries::get_all_unique_attrs;
use crate::error::Result;
use crate::remote::download::{compress_zstd, file_sha256};
use crate::remote::manifest::{DeltaFile, IndexFile, Manifest};
use chrono::Utc;
use std::fs;
use std::path::Path;

/// Compression level for zstd (higher = better compression, slower).
const COMPRESSION_LEVEL: i32 = 19;

/// Generate a compressed full index for distribution.
///
/// Creates:
/// - `nxv-index-full.db.zst` - Compressed database
/// - Returns the IndexFile with hash and size info
pub fn generate_full_index<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
) -> Result<(IndexFile, String)> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    let compressed_name = "nxv-index-full.db.zst";
    let compressed_path = output_dir.join(compressed_name);

    // Get database info
    let db = Database::open(db_path)?;
    let last_commit = db.get_meta("last_indexed_commit")?.unwrap_or_default();

    // Compress the database
    compress_zstd(db_path, &compressed_path, COMPRESSION_LEVEL)?;

    // Calculate hash of compressed file
    let sha256 = file_sha256(&compressed_path)?;
    let size = fs::metadata(&compressed_path)?.len();

    let index_file = IndexFile {
        url: compressed_name.to_string(),
        size_bytes: size,
        sha256,
    };

    Ok((index_file, last_commit))
}

/// Generate a delta pack between two commits.
///
/// Creates a compressed SQL file with INSERT/UPDATE statements for changes
/// between from_commit and to_commit.
pub fn generate_delta_pack<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    from_commit: &str,
    to_commit: &str,
    output_dir: Q,
) -> Result<DeltaFile> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    let from_short = &from_commit[..7.min(from_commit.len())];
    let to_short = &to_commit[..7.min(to_commit.len())];
    let delta_name = format!("delta-{}-{}.sql.zst", from_short, to_short);
    let delta_path = output_dir.join(&delta_name);
    let sql_temp_path = output_dir.join(format!("delta-{}-{}.sql", from_short, to_short));

    // Open database and export delta as SQL
    let db = Database::open(db_path)?;

    // Query for new or updated rows since from_commit
    // We'll export rows where the first_commit_date or last_commit_date is after from_commit's date
    let conn = db.connection();

    // Get the timestamp of the from_commit to use as a filter
    let from_commit_date: Option<i64> = conn
        .query_row(
            "SELECT first_commit_date FROM package_versions WHERE first_commit_hash LIKE ?1 || '%' LIMIT 1",
            [from_commit],
            |row| row.get(0),
        )
        .ok();

    let min_date = from_commit_date.unwrap_or(0);

    // Export new/updated rows as INSERT OR REPLACE statements
    let mut stmt = conn.prepare(
        r#"
        SELECT name, version, first_commit_hash, first_commit_date,
               last_commit_hash, last_commit_date, attribute_path,
               description, license, homepage, maintainers, platforms
        FROM package_versions
        WHERE first_commit_date > ?1 OR last_commit_date > ?1
        "#,
    )?;

    let mut sql_content = String::new();
    sql_content.push_str("-- Delta pack from ");
    sql_content.push_str(from_commit);
    sql_content.push_str(" to ");
    sql_content.push_str(to_commit);
    sql_content.push('\n');
    sql_content.push_str("BEGIN TRANSACTION;\n");

    let rows = stmt.query_map([min_date], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
        ))
    })?;

    for row_result in rows {
        let (
            name,
            version,
            first_commit_hash,
            first_commit_date,
            last_commit_hash,
            last_commit_date,
            attribute_path,
            description,
            license,
            homepage,
            maintainers,
            platforms,
        ) = row_result?;

        sql_content.push_str(&format!(
            "INSERT OR REPLACE INTO package_versions (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description, license, homepage, maintainers, platforms) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {});\n",
            sql_quote(&name),
            sql_quote(&version),
            sql_quote(&first_commit_hash),
            first_commit_date,
            sql_quote(&last_commit_hash),
            last_commit_date,
            sql_quote(&attribute_path),
            sql_quote_opt(&description),
            sql_quote_opt(&license),
            sql_quote_opt(&homepage),
            sql_quote_opt(&maintainers),
            sql_quote_opt(&platforms),
        ));
    }

    // Update the last_indexed_commit meta
    sql_content.push_str(&format!(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', {});\n",
        sql_quote(to_commit)
    ));

    sql_content.push_str("COMMIT;\n");

    // Write SQL file
    fs::write(&sql_temp_path, &sql_content)?;

    // Compress the SQL file
    compress_zstd(&sql_temp_path, &delta_path, COMPRESSION_LEVEL)?;

    // Clean up temp file
    let _ = fs::remove_file(&sql_temp_path);

    // Calculate hash
    let sha256 = file_sha256(&delta_path)?;
    let size = fs::metadata(&delta_path)?.len();

    Ok(DeltaFile {
        url: delta_name,
        sha256,
        size_bytes: size,
        from_commit: from_commit.to_string(),
        to_commit: to_commit.to_string(),
    })
}

/// Generate a manifest file for the index.
pub fn generate_manifest<P: AsRef<Path>>(
    output_dir: P,
    full_index: IndexFile,
    latest_commit: &str,
    deltas: Vec<DeltaFile>,
    bloom_filter: IndexFile,
) -> Result<()> {
    let output_dir = output_dir.as_ref();
    let manifest_path = output_dir.join("manifest.json");

    let manifest = Manifest {
        version: 1,
        latest_commit: latest_commit.to_string(),
        latest_commit_date: Utc::now().to_rfc3339(),
        full_index,
        deltas,
        bloom_filter,
    };

    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(manifest_path, json)?;

    Ok(())
}

/// Generate a bloom filter file containing all unique attribute paths from the database.
///
/// The function writes `nxv-bloom.bin` into `output_dir`, populated with every unique
/// attribute path extracted from `db_path`. It returns an `IndexFile` describing the
/// bloom file (URL, size in bytes, and SHA-256 hash).
///
/// # Examples
///
/// ```no_run
/// use tempfile::tempdir;
///
/// let db_path = "path/to/index.db";
/// let out = tempdir().unwrap();
/// let index_file = generate_bloom_filter(db_path, out.path()).unwrap();
/// assert_eq!(index_file.url, "nxv-bloom.bin");
/// ```
pub fn generate_bloom_filter<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
) -> Result<IndexFile> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    let bloom_name = "nxv-bloom.bin";
    let bloom_path = output_dir.join(bloom_name);

    // Get all unique attribute paths from database
    let db = Database::open(db_path)?;
    let attrs = get_all_unique_attrs(db.connection())?;

    // Create bloom filter with 1% FPR
    let count = attrs.len();
    let mut filter = PackageBloomFilter::new(count.max(1000), 0.01);

    for attr in &attrs {
        filter.insert(attr);
    }

    // Save the filter
    filter.save(&bloom_path)?;

    // Calculate hash
    let sha256 = file_sha256(&bloom_path)?;
    let size = fs::metadata(&bloom_path)?.len();

    Ok(IndexFile {
        url: bloom_name.to_string(),
        size_bytes: size,
        sha256,
    })
}

/// Sign the manifest file using minisign.
///
/// This requires a secret key to be available.
/// For now, this is a placeholder that would be implemented
/// when setting up the signing infrastructure.
pub fn sign_manifest<P: AsRef<Path>>(_output_dir: P, _secret_key_path: &str) -> Result<()> {
    // Signing would be done with minisign CLI or library
    // For now, this is a placeholder
    // In production, this would:
    // 1. Load the secret key from secret_key_path
    // 2. Sign manifest.json
    // 3. Write manifest.json.sig
    Ok(())
}

/// Helper to quote a string for SQL.
fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Helper to quote an optional string for SQL.
fn sql_quote_opt(s: &Option<String>) -> String {
    match s {
        Some(v) => sql_quote(v),
        None => "NULL".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_db(path: &Path) {
        use rusqlite::Connection;

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
                UNIQUE(attribute_path, version, first_commit_hash)
            );
            CREATE INDEX idx_packages_name ON package_versions(name);

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'abc1234567890def');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python', '3.11.0', 'aaa111', 1700000000, 'bbb222', 1700100000, 'python311', 'Python'),
                ('nodejs', '20.0.0', 'ccc333', 1700200000, 'ddd444', 1700300000, 'nodejs_20', 'Node.js');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn test_generate_full_index() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let output_dir = dir.path().join("output");

        create_test_db(&db_path);

        let (index_file, last_commit) = generate_full_index(&db_path, &output_dir).unwrap();

        assert!(!index_file.sha256.is_empty());
        assert!(index_file.size_bytes > 0);
        assert_eq!(last_commit, "abc1234567890def");

        // Verify the compressed file exists
        let compressed_path = output_dir.join("nxv-index-full.db.zst");
        assert!(compressed_path.exists());
    }

    #[test]
    fn test_generate_bloom_filter() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let output_dir = dir.path().join("output");

        create_test_db(&db_path);

        let bloom_file = generate_bloom_filter(&db_path, &output_dir).unwrap();

        assert_eq!(bloom_file.url, "nxv-bloom.bin");
        assert!(!bloom_file.sha256.is_empty());

        // Verify the bloom filter file exists
        let bloom_path = output_dir.join("nxv-bloom.bin");
        assert!(bloom_path.exists());

        // Load and verify the bloom filter works (uses attribute_path, not name)
        let filter = PackageBloomFilter::load(&bloom_path).unwrap();
        assert!(filter.contains("python311"));
        assert!(filter.contains("nodejs_20"));
        assert!(!filter.contains("nonexistent"));
    }

    #[test]
    fn test_generate_manifest() {
        let dir = tempdir().unwrap();
        let output_dir = dir.path();

        let full_index = IndexFile {
            url: "nxv-index-full.db.zst".to_string(),
            sha256: "abc123".to_string(),
            size_bytes: 1000,
        };

        let bloom_filter = IndexFile {
            url: "nxv-bloom.bin".to_string(),
            sha256: "def456".to_string(),
            size_bytes: 500,
        };

        generate_manifest(output_dir, full_index, "latest123", vec![], bloom_filter).unwrap();

        let manifest_path = output_dir.join("manifest.json");
        assert!(manifest_path.exists());

        // Verify it's valid JSON
        let content = fs::read_to_string(&manifest_path).unwrap();
        let parsed: Manifest = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.full_index.url, "nxv-index-full.db.zst");
        assert_eq!(parsed.latest_commit, "latest123");
    }

    #[test]
    fn test_sql_quote() {
        assert_eq!(sql_quote("hello"), "'hello'");
        assert_eq!(sql_quote("it's"), "'it''s'");
        assert_eq!(sql_quote(""), "''");
    }

    #[test]
    fn test_sql_quote_opt() {
        assert_eq!(sql_quote_opt(&Some("hello".to_string())), "'hello'");
        assert_eq!(sql_quote_opt(&None), "NULL");
    }
}
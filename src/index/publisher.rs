//! Index publishing utilities for generating distributable artifacts.

use crate::bloom::PackageBloomFilter;
use crate::db::Database;
use crate::db::queries::get_all_unique_attrs;
use crate::error::Result;
use crate::remote::download::{compress_zstd, file_sha256};
use crate::remote::manifest::{DeltaFile, IndexFile, Manifest};
use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Compression level for zstd (higher = better compression, slower).
/// Level 19 provides excellent compression ratio at the cost of speed.
/// For reference: level 3 is default, level 19 is near-max, level 22 is max.
const COMPRESSION_LEVEL: i32 = 19;

/// Default file names for published artifacts.
pub const INDEX_DB_NAME: &str = "index.db.zst";
pub const BLOOM_FILTER_NAME: &str = "bloom.bin";
pub const MANIFEST_NAME: &str = "manifest.json";

/// Format bytes as human-readable size.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Compress a file using zstd with progress indication.
fn compress_zstd_with_progress<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    level: i32,
    show_progress: bool,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    let input_file = File::open(src)?;
    let input_size = input_file.metadata()?.len();
    let mut reader = BufReader::new(input_file);

    let output = BufWriter::new(File::create(dest)?);
    let mut encoder = zstd::Encoder::new(output, level)?;

    let pb = if show_progress {
        let pb = ProgressBar::new(input_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("=>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut buffer = [0u8; 64 * 1024]; // 64KB buffer
    let mut total_read = 0u64;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        encoder.write_all(&buffer[..bytes_read])?;
        total_read += bytes_read as u64;
        if let Some(ref pb) = pb {
            pb.set_position(total_read);
        }
    }

    encoder.finish()?;

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    Ok(())
}

/// Calculate SHA256 hash of a file with progress indication.
fn file_sha256_with_progress<P: AsRef<Path>>(path: P, show_progress: bool) -> Result<String> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();

    let pb = if show_progress {
        let pb = ProgressBar::new(file_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes}")
                .unwrap()
                .progress_chars("=>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut buffer = [0u8; 64 * 1024];
    let mut total_read = 0u64;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        total_read += bytes_read as u64;
        if let Some(ref pb) = pb {
            pb.set_position(total_read);
        }
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Generate a compressed full index for distribution.
///
/// Creates:
/// - `index.db.zst` - Compressed database
/// - Returns the IndexFile with hash and size info
pub fn generate_full_index<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
) -> Result<(IndexFile, String)> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    let compressed_path = output_dir.join(INDEX_DB_NAME);

    // Get database info
    let db = Database::open(db_path)?;
    let last_commit = db.get_meta("last_indexed_commit")?.unwrap_or_default();
    let input_size = fs::metadata(db_path)?.len();

    if show_progress {
        eprintln!(
            "  Compressing database ({}) with zstd level {}...",
            format_bytes(input_size),
            COMPRESSION_LEVEL
        );
    }

    // Compress the database with progress
    compress_zstd_with_progress(db_path, &compressed_path, COMPRESSION_LEVEL, show_progress)?;

    // Calculate hash of compressed file
    if show_progress {
        eprintln!("  Calculating checksum...");
    }
    let sha256 = file_sha256_with_progress(&compressed_path, show_progress)?;
    let size = fs::metadata(&compressed_path)?.len();

    if show_progress {
        let ratio = (size as f64 / input_size as f64) * 100.0;
        eprintln!(
            "  Compressed: {} → {} ({:.1}% of original)",
            format_bytes(input_size),
            format_bytes(size),
            ratio
        );
    }

    let url = match url_prefix {
        Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), INDEX_DB_NAME),
        None => INDEX_DB_NAME.to_string(),
    };

    let index_file = IndexFile {
        url,
        size_bytes: size,
        sha256,
    };

    Ok((index_file, last_commit))
}

/// Generate a delta pack between two commits.
///
/// Creates a compressed SQL file with INSERT/UPDATE statements for changes
/// between from_commit and to_commit.
#[allow(dead_code)]
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
/// The function writes `bloom.bin` into `output_dir`, populated with every unique
/// attribute path extracted from `db_path`. It returns an `IndexFile` describing the
/// bloom file (URL, size in bytes, and SHA-256 hash).
pub fn generate_bloom_filter<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
) -> Result<IndexFile> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    let bloom_path = output_dir.join(BLOOM_FILTER_NAME);

    // Get all unique attribute paths from database
    let db = Database::open(db_path)?;
    let attrs = get_all_unique_attrs(db.connection())?;

    // Create bloom filter with 1% FPR
    let count = attrs.len();

    if show_progress {
        eprintln!("  Building bloom filter for {} packages...", count);
    }

    let mut filter = PackageBloomFilter::new(count.max(1000), 0.01);

    let pb = if show_progress {
        let pb = ProgressBar::new(count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len}")
                .unwrap()
                .progress_chars("=>-"),
        );
        Some(pb)
    } else {
        None
    };

    for (i, attr) in attrs.iter().enumerate() {
        filter.insert(attr);
        if let Some(ref pb) = pb {
            pb.set_position(i as u64 + 1);
        }
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // Save the filter
    filter.save(&bloom_path)?;

    // Calculate hash
    let sha256 = file_sha256(&bloom_path)?;
    let size = fs::metadata(&bloom_path)?.len();

    if show_progress {
        eprintln!("  Bloom filter: {}", format_bytes(size));
    }

    let url = match url_prefix {
        Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), BLOOM_FILTER_NAME),
        None => BLOOM_FILTER_NAME.to_string(),
    };

    Ok(IndexFile {
        url,
        size_bytes: size,
        sha256,
    })
}

/// Generate all publishable artifacts for an index.
///
/// Creates:
/// - `index.db.zst` - Compressed database
/// - `bloom.bin` - Bloom filter for fast lookups
/// - `manifest.json` - Manifest with URLs and checksums
///
/// Returns the path to the output directory.
pub fn publish_index<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
) -> Result<()> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    if show_progress {
        eprintln!("Generating compressed index...");
    }
    let (full_index, last_commit) =
        generate_full_index(db_path, output_dir, url_prefix, show_progress)?;

    if show_progress {
        eprintln!();
        eprintln!("Generating bloom filter...");
    }
    let bloom_filter = generate_bloom_filter(db_path, output_dir, url_prefix, show_progress)?;

    if show_progress {
        eprintln!();
        eprintln!("Writing manifest...");
    }
    generate_manifest(
        output_dir,
        full_index.clone(),
        &last_commit,
        vec![],
        bloom_filter.clone(),
    )?;

    if show_progress {
        let commit_display = if last_commit.is_empty() {
            "unknown (missing meta)".to_string()
        } else {
            last_commit[..12.min(last_commit.len())].to_string()
        };

        eprintln!();
        eprintln!("Published artifacts to: {}", output_dir.display());
        eprintln!(
            "  - {} ({})",
            INDEX_DB_NAME,
            format_bytes(full_index.size_bytes)
        );
        eprintln!(
            "  - {} ({})",
            BLOOM_FILTER_NAME,
            format_bytes(bloom_filter.size_bytes)
        );
        eprintln!("  - {}", MANIFEST_NAME);
        eprintln!();
        eprintln!("Last indexed commit: {}", commit_display);
    }

    Ok(())
}

/// Sign the manifest file using minisign.
///
/// This requires a secret key to be available.
/// For now, this is a placeholder that would be implemented
/// when setting up the signing infrastructure.
#[allow(dead_code)]
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
#[allow(dead_code)]
fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Helper to quote an optional string for SQL.
#[allow(dead_code)]
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

        let (index_file, last_commit) =
            generate_full_index(&db_path, &output_dir, None, false).unwrap();

        assert!(!index_file.sha256.is_empty());
        assert!(index_file.size_bytes > 0);
        assert_eq!(index_file.url, INDEX_DB_NAME);
        assert_eq!(last_commit, "abc1234567890def");

        // Verify the compressed file exists
        let compressed_path = output_dir.join(INDEX_DB_NAME);
        assert!(compressed_path.exists());
    }

    #[test]
    fn test_generate_full_index_with_url_prefix() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let output_dir = dir.path().join("output");

        create_test_db(&db_path);

        let url_prefix = "https://example.com/releases";
        let (index_file, _) =
            generate_full_index(&db_path, &output_dir, Some(url_prefix), false).unwrap();

        assert_eq!(index_file.url, format!("{}/{}", url_prefix, INDEX_DB_NAME));
    }

    #[test]
    fn test_generate_bloom_filter() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let output_dir = dir.path().join("output");

        create_test_db(&db_path);

        let bloom_file = generate_bloom_filter(&db_path, &output_dir, None, false).unwrap();

        assert_eq!(bloom_file.url, BLOOM_FILTER_NAME);
        assert!(!bloom_file.sha256.is_empty());

        // Verify the bloom filter file exists
        let bloom_path = output_dir.join(BLOOM_FILTER_NAME);
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

//! Index publishing utilities for generating distributable artifacts.

use crate::bloom::PackageBloomFilter;
use crate::db::Database;
use crate::db::queries::get_all_unique_attrs;
use crate::error::{NxvError, Result};
use crate::remote::download::{compress_zstd, file_sha256};
use crate::remote::manifest::{DeltaFile, IndexFile, Manifest};
use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Compression level for zstd (higher = better compression, slower).
/// Level 19 provides excellent compression ratio at the cost of speed.
/// For reference: level 3 is default, level 19 is max normal level,
/// and levels 20-22 are "ultra" modes requiring significantly more memory.
const COMPRESSION_LEVEL: i32 = 19;

/// Default file names for published artifacts.
pub const INDEX_DB_NAME: &str = "index.db.zst";
pub const INDEX_FULL_DB_NAME: &str = "index-full.db.zst";
pub const BLOOM_FILTER_NAME: &str = "bloom.bin";
pub const MANIFEST_NAME: &str = "manifest.json";
pub const MANIFEST_SIG_NAME: &str = "manifest.json.minisig";

/// Replace the untrusted comment line in a minisign key string.
///
/// Minisign keys have the format:
/// ```text
/// untrusted comment: <comment>
/// <base64 key data>
/// ```
///
/// This function replaces the first line with a custom comment,
/// making it robust against upstream format changes.
fn replace_untrusted_comment(key_str: &str, new_comment: &str) -> String {
    let mut lines = key_str.lines();
    let first_line = lines.next().unwrap_or("");

    // Only replace if the first line looks like an untrusted comment
    if first_line.starts_with("untrusted comment:") {
        let rest: Vec<&str> = lines.collect();
        format!("{}\n{}", new_comment, rest.join("\n"))
    } else {
        // Unexpected format - prepend our comment but keep original intact
        format!("{}\n{}", new_comment, key_str)
    }
}

/// Generate a new minisign keypair for signing manifests.
///
/// Creates an unencrypted keypair compatible with the minisign Rust crate.
///
/// # Arguments
/// * `secret_key_path` - Where to save the secret key
/// * `public_key_path` - Where to save the public key
/// * `comment` - Comment to embed in the key files
/// * `force` - If true, overwrite existing files; if false, fail if files exist
///
/// # Returns
/// The public key string (for embedding in applications).
pub fn generate_keypair<P: AsRef<Path>, Q: AsRef<Path>>(
    secret_key_path: P,
    public_key_path: Q,
    comment: &str,
    force: bool,
) -> Result<String> {
    let secret_key_path = secret_key_path.as_ref();
    let public_key_path = public_key_path.as_ref();

    // Generate keypair
    let keypair = minisign::KeyPair::generate_unencrypted_keypair()
        .map_err(|e| NxvError::Signing(format!("failed to generate keypair: {}", e)))?;

    // Serialize with comment
    let sk_box = keypair
        .sk
        .to_box(None)
        .map_err(|e| NxvError::Signing(format!("failed to serialize secret key: {}", e)))?;

    let pk_box = keypair
        .pk
        .to_box()
        .map_err(|e| NxvError::Signing(format!("failed to serialize public key: {}", e)))?;

    // Replace minisign's default comments with custom ones.
    // We replace the first line (the untrusted comment) regardless of its content,
    // making this robust against upstream format changes.
    let sk_str = sk_box.to_string();
    let sk_with_comment =
        replace_untrusted_comment(&sk_str, &format!("untrusted comment: {}", comment));

    let pk_str = pk_box.to_string();
    let pk_with_comment = replace_untrusted_comment(
        &pk_str,
        &format!("untrusted comment: {} - public key:", comment),
    );

    // Write key files atomically (use create_new to avoid TOCTOU race)
    write_key_file(secret_key_path, &sk_with_comment, force, "secret key")?;
    write_key_file(public_key_path, &pk_with_comment, force, "public key")?;

    // Extract the base64 public key for embedding
    let pk_base64 = pk_with_comment.lines().nth(1).unwrap_or("").to_string();

    Ok(pk_base64)
}

/// Write a key file, optionally refusing to overwrite existing files.
fn write_key_file(path: &Path, content: &str, force: bool, key_type: &str) -> Result<()> {
    if force {
        // Overwrite any existing file
        fs::write(path, content).map_err(|e| {
            NxvError::Signing(format!(
                "failed to write {} '{}': {}",
                key_type,
                path.display(),
                e
            ))
        })
    } else {
        // Use create_new to atomically check existence and create
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    NxvError::Signing(format!(
                        "{} '{}' already exists. Use --force to overwrite.",
                        key_type,
                        path.display()
                    ))
                } else {
                    NxvError::Signing(format!(
                        "failed to create {} '{}': {}",
                        key_type,
                        path.display(),
                        e
                    ))
                }
            })?;

        file.write_all(content.as_bytes()).map_err(|e| {
            NxvError::Signing(format!(
                "failed to write {} '{}': {}",
                key_type,
                path.display(),
                e
            ))
        })
    }
}

/// Resolve a secret key from either a file path or raw key content.
///
/// This function handles the NXV_SECRET_KEY environment variable which can contain
/// either a path to a key file or the raw key content itself.
///
/// # Arguments
/// * `key` - Either a file path or the raw minisign secret key content
///
/// # Returns
/// The secret key content as a string.
pub fn resolve_secret_key(key: &str) -> Result<String> {
    let key = key.trim();

    // Check if it looks like a file path that exists
    let path = Path::new(key);
    if path.exists() {
        return fs::read_to_string(path)
            .map_err(|e| NxvError::Signing(format!("failed to read secret key '{}': {}", key, e)));
    }

    // Check if it looks like raw key content (starts with "untrusted comment:")
    if key.starts_with("untrusted comment:") {
        return Ok(key.to_string());
    }

    // If it looks like a path but doesn't exist, give a helpful error
    if key.contains('/') || key.contains('\\') || key.ends_with(".key") {
        return Err(NxvError::Signing(format!(
            "secret key file '{}' not found",
            key
        )));
    }

    // Otherwise, assume it's raw key content (user may have stripped the comment)
    // Validate format before returning
    validate_secret_key_format(key)?;
    Ok(key.to_string())
}

/// Validate that a string looks like a valid minisign secret key.
///
/// Performs basic format validation to catch common mistakes early:
/// - Checks that the key is not empty
/// - Verifies the key has proper structure (comment + key data, or just key data)
/// - Detects if a public key was provided instead of a secret key
///
/// Note: This is a format check, not a cryptographic validation. The actual
/// key parsing is still performed by the minisign crate during signing.
fn validate_secret_key_format(key_str: &str) -> Result<()> {
    let lines: Vec<&str> = key_str.lines().collect();

    if lines.is_empty() || key_str.trim().is_empty() {
        return Err(NxvError::Signing("Secret key is empty".into()));
    }

    // Determine which line contains the actual key data
    let key_line = if lines[0].starts_with("untrusted comment:") {
        // Standard format with comment
        if lines.len() < 2 {
            return Err(NxvError::Signing(
                "Secret key appears incomplete: has comment but missing key data".into(),
            ));
        }
        lines[1].trim()
    } else {
        // Raw key data without comment
        lines[0].trim()
    };

    // Check if this is a public key instead of a secret key
    // Public keys start with "RW" prefix, secret keys typically start with "ED" or other prefixes
    if key_line.starts_with("RW") {
        return Err(NxvError::Signing(
            "This appears to be a public key (starts with RW), not a secret key. \
             Secret keys are stored in .key files, public keys in .pub files."
                .into(),
        ));
    }

    // Basic check that the key data looks like base64 (contains only valid base64 chars)
    // This catches common mistakes like passing plain text
    if key_line.len() < 10 {
        return Err(NxvError::Signing(
            "Secret key data is too short to be valid".into(),
        ));
    }

    let valid_base64 = key_line
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=');
    if !valid_base64 {
        return Err(NxvError::Signing(
            "Secret key contains invalid characters. Expected base64-encoded key data.".into(),
        ));
    }

    Ok(())
}

/// Sign a manifest file using a minisign secret key.
///
/// Uses the minisign Rust crate directly for signing.
/// Keys must be generated with `nxv keygen` for compatibility.
///
/// # Arguments
/// * `manifest_path` - Path to the manifest.json file to sign
/// * `secret_key` - Either a file path or raw secret key content
///
/// # Returns
/// The path to the created signature file.
pub fn sign_manifest<P: AsRef<Path>>(
    manifest_path: P,
    secret_key: &str,
) -> Result<std::path::PathBuf> {
    use std::io::Cursor;

    let manifest_path = manifest_path.as_ref();

    // Resolve the secret key (handles both file paths and raw content)
    let sk_string = resolve_secret_key(secret_key)?;

    // Parse the secret key
    let sk_box = minisign::SecretKeyBox::from_string(&sk_string)
        .map_err(|e| NxvError::Signing(format!("failed to parse secret key: {}", e)))?;

    // Load as unencrypted key
    let sk = sk_box.into_unencrypted_secret_key().map_err(|e| {
        NxvError::Signing(format!(
            "failed to load secret key ({}). Keys must be generated with 'nxv keygen'.",
            e
        ))
    })?;

    // Read the manifest content
    let manifest_content = fs::read(manifest_path).map_err(|e| {
        NxvError::Signing(format!(
            "failed to read manifest '{}': {}",
            manifest_path.display(),
            e
        ))
    })?;

    // Sign the manifest
    let mut cursor = Cursor::new(&manifest_content);
    let trusted_comment = format!("timestamp:{}", chrono::Utc::now().to_rfc3339());
    let sig_box = minisign::sign(
        None,
        &sk,
        &mut cursor,
        Some(&trusted_comment),
        Some("nxv manifest signature"),
    )
    .map_err(|e| NxvError::Signing(format!("failed to sign manifest: {}", e)))?;

    // Write the signature file
    let sig_path = manifest_path.with_extension("json.minisig");
    fs::write(&sig_path, sig_box.to_string()).map_err(|e| {
        NxvError::Signing(format!(
            "failed to write signature file '{}': {}",
            sig_path.display(),
            e
        ))
    })?;

    Ok(sig_path)
}

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

/// Generate a slim database from the full database.
///
/// The slim database has one row per (attribute_path, version) pair,
/// consolidating all version ranges into a single row with:
/// - first_commit: earliest first_commit across all ranges
/// - last_commit: latest last_commit across all ranges
///
/// This reduces a ~6M row database to ~200K rows while preserving
/// all unique package versions.
pub fn generate_slim_db<P: AsRef<Path>, Q: AsRef<Path>>(
    full_db_path: P,
    slim_db_path: Q,
    show_progress: bool,
) -> Result<()> {
    use rusqlite::Connection;

    let full_db_path = full_db_path.as_ref();
    let slim_db_path = slim_db_path.as_ref();

    if show_progress {
        eprintln!("  Generating slim database from full index...");
    }

    // Remove existing slim db if present
    if slim_db_path.exists() {
        fs::remove_file(slim_db_path)?;
    }

    // Open full database read-only
    let full_conn =
        Connection::open_with_flags(full_db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    // Create new slim database
    let slim_conn = Connection::open(slim_db_path)?;

    // Set up slim database with same schema
    slim_conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA cache_size = -64000;
        PRAGMA temp_store = MEMORY;

        CREATE TABLE meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

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
            source_path TEXT,
            known_vulnerabilities TEXT,
            store_path_x86_64_linux TEXT,
            store_path_aarch64_linux TEXT,
            store_path_x86_64_darwin TEXT,
            store_path_aarch64_darwin TEXT,
            UNIQUE(attribute_path, version)
        );
        "#,
    )?;

    // Copy meta table
    let mut meta_stmt = full_conn.prepare("SELECT key, value FROM meta")?;
    let meta_rows = meta_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in meta_rows {
        let (key, value) = row?;
        slim_conn.execute(
            "INSERT INTO meta (key, value) VALUES (?, ?)",
            rusqlite::params![key, value],
        )?;
    }

    // Mark this as a slim database
    slim_conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('db_variant', 'slim')",
        [],
    )?;

    // Count total unique (attr, version) pairs for progress
    let total_pairs: i64 = full_conn.query_row(
        "SELECT COUNT(*) FROM (SELECT DISTINCT attribute_path, version FROM package_versions)",
        [],
        |row| row.get(0),
    )?;

    if show_progress {
        eprintln!("  Consolidating {} unique package versions...", total_pairs);
    }

    let pb = if show_progress {
        let pb = ProgressBar::new(total_pairs as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
                .unwrap()
                .progress_chars("=>-"),
        );
        Some(pb)
    } else {
        None
    };

    // Query consolidated data from full database
    // For each (attribute_path, version), get:
    // - MIN(first_commit_date) and its corresponding hash
    // - MAX(last_commit_date) and its corresponding hash
    // - Most recent metadata (from the row with latest last_commit_date)
    let mut stmt = full_conn.prepare(
        r#"
        WITH ranked AS (
            SELECT *,
                ROW_NUMBER() OVER (
                    PARTITION BY attribute_path, version
                    ORDER BY last_commit_date DESC
                ) as rn
            FROM package_versions
        ),
        first_commits AS (
            SELECT attribute_path, version,
                   first_commit_hash, first_commit_date
            FROM package_versions pv
            WHERE first_commit_date = (
                SELECT MIN(first_commit_date)
                FROM package_versions
                WHERE attribute_path = pv.attribute_path AND version = pv.version
            )
            GROUP BY attribute_path, version
        )
        SELECT
            r.name,
            r.version,
            fc.first_commit_hash,
            fc.first_commit_date,
            r.last_commit_hash,
            r.last_commit_date,
            r.attribute_path,
            r.description,
            r.license,
            r.homepage,
            r.maintainers,
            r.platforms,
            r.source_path,
            r.known_vulnerabilities,
            r.store_path_x86_64_linux,
            r.store_path_aarch64_linux,
            r.store_path_x86_64_darwin,
            r.store_path_aarch64_darwin
        FROM ranked r
        JOIN first_commits fc ON r.attribute_path = fc.attribute_path AND r.version = fc.version
        WHERE r.rn = 1
        ORDER BY r.attribute_path, r.version
        "#,
    )?;

    // Insert into slim database in batches
    let mut insert_stmt = slim_conn.prepare(
        r#"
        INSERT INTO package_versions (
            name, version, first_commit_hash, first_commit_date,
            last_commit_hash, last_commit_date, attribute_path,
            description, license, homepage, maintainers, platforms,
            source_path, known_vulnerabilities,
            store_path_x86_64_linux, store_path_aarch64_linux,
            store_path_x86_64_darwin, store_path_aarch64_darwin
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )?;

    let rows = stmt.query_map([], |row| {
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
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, Option<String>>(17)?,
        ))
    })?;

    let mut count = 0u64;
    for row in rows {
        let (
            name,
            version,
            first_hash,
            first_date,
            last_hash,
            last_date,
            attr_path,
            desc,
            license,
            homepage,
            maintainers,
            platforms,
            source_path,
            vulns,
            sp_x86_linux,
            sp_aarch64_linux,
            sp_x86_darwin,
            sp_aarch64_darwin,
        ) = row?;

        insert_stmt.execute(rusqlite::params![
            name,
            version,
            first_hash,
            first_date,
            last_hash,
            last_date,
            attr_path,
            desc,
            license,
            homepage,
            maintainers,
            platforms,
            source_path,
            vulns,
            sp_x86_linux,
            sp_aarch64_linux,
            sp_x86_darwin,
            sp_aarch64_darwin,
        ])?;

        count += 1;
        if let Some(ref pb) = pb {
            pb.set_position(count);
        }
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // Create indexes on slim database
    if show_progress {
        eprintln!("  Creating indexes...");
    }

    slim_conn.execute_batch(
        r#"
        CREATE INDEX idx_packages_name ON package_versions(name);
        CREATE INDEX idx_packages_attr ON package_versions(attribute_path);
        CREATE INDEX idx_packages_last_date ON package_versions(last_commit_date DESC);
        CREATE INDEX idx_attr_date_covering ON package_versions(
            attribute_path, last_commit_date DESC, name, version
        );

        -- Checkpoint WAL to main file
        PRAGMA wal_checkpoint(TRUNCATE);
        "#,
    )?;

    if show_progress {
        let slim_size = fs::metadata(slim_db_path)?.len();
        let full_size = fs::metadata(full_db_path)?.len();
        eprintln!(
            "  Slim database: {} ({} rows, {:.1}x smaller)",
            format_bytes(slim_size),
            count,
            full_size as f64 / slim_size as f64
        );
    }

    Ok(())
}

/// Generate a compressed full index for distribution.
///
/// Creates:
/// - `index.db.zst` - Compressed database
/// - Returns the IndexFile with hash and size info
#[allow(dead_code)] // Used in tests
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

    // Get database info and update last_indexed_date to publish time
    let db = Database::open(db_path)?;
    let last_commit = db.get_meta("last_indexed_commit")?.unwrap_or_default();
    // Set the indexed date to now (publish time) so it matches the manifest
    db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
    // Flush WAL to ensure the meta update is in the main DB file before compression
    db.checkpoint()?;
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
///
/// Note: This function is implemented but not yet exposed via CLI. It will be
/// used for incremental index updates in a future release.
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

    // Update the last_indexed_commit and last_indexed_date meta
    sql_content.push_str(&format!(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', {});\n",
        sql_quote(to_commit)
    ));
    sql_content.push_str(&format!(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_date', {});\n",
        sql_quote(&Utc::now().to_rfc3339())
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
    full_history_index: Option<IndexFile>,
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
        full_history_index,
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
/// - `index.db.zst` - Compressed slim database (one row per attr+version)
/// - `index-full.db.zst` - Compressed full database (all version ranges)
/// - `bloom.bin` - Bloom filter for fast lookups
/// - `manifest.json` - Manifest with URLs and checksums
/// - `manifest.json.minisig` - Signature file (if secret_key is provided)
///
/// Returns the path to the output directory.
pub fn publish_index<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
    secret_key: Option<&str>,
    compression_level: i32,
) -> Result<()> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    fs::create_dir_all(output_dir)?;

    // Step 1: Generate and compress the full history database
    if show_progress {
        eprintln!("Generating compressed full history index...");
    }
    let (full_history_index, last_commit) = generate_full_history_index(
        db_path,
        output_dir,
        url_prefix,
        show_progress,
        compression_level,
    )?;

    // Step 2: Generate slim database from full database
    if show_progress {
        eprintln!();
        eprintln!("Generating slim index...");
    }
    let slim_db_path = output_dir.join("slim-temp.db");
    generate_slim_db(db_path, &slim_db_path, show_progress)?;

    // Step 3: Compress slim database
    if show_progress {
        eprintln!("  Compressing slim database...");
    }
    let slim_index = generate_slim_index(
        &slim_db_path,
        output_dir,
        url_prefix,
        show_progress,
        compression_level,
    )?;

    // Clean up temp slim db
    let _ = fs::remove_file(&slim_db_path);
    // Also remove WAL/SHM files if they exist
    let _ = fs::remove_file(slim_db_path.with_extension("db-wal"));
    let _ = fs::remove_file(slim_db_path.with_extension("db-shm"));

    // Step 4: Generate bloom filter from full database (has all attr paths)
    if show_progress {
        eprintln!();
        eprintln!("Generating bloom filter...");
    }
    let bloom_filter = generate_bloom_filter(db_path, output_dir, url_prefix, show_progress)?;

    // Step 5: Write manifest
    if show_progress {
        eprintln!();
        eprintln!("Writing manifest...");
    }
    generate_manifest(
        output_dir,
        slim_index.clone(),
        Some(full_history_index.clone()),
        &last_commit,
        vec![],
        bloom_filter.clone(),
    )?;

    // Sign the manifest if a secret key was provided
    let signed = if let Some(sk) = secret_key {
        if show_progress {
            eprintln!();
            eprintln!("Signing manifest...");
        }
        let manifest_path = output_dir.join(MANIFEST_NAME);
        sign_manifest(&manifest_path, sk)?;
        true
    } else {
        false
    };

    if show_progress {
        let commit_display = if last_commit.is_empty() {
            "unknown (missing meta)".to_string()
        } else {
            last_commit[..12.min(last_commit.len())].to_string()
        };

        eprintln!();
        eprintln!("Published artifacts to: {}", output_dir.display());
        eprintln!(
            "  - {} (slim, {})",
            INDEX_DB_NAME,
            format_bytes(slim_index.size_bytes)
        );
        eprintln!(
            "  - {} (full history, {})",
            INDEX_FULL_DB_NAME,
            format_bytes(full_history_index.size_bytes)
        );
        eprintln!(
            "  - {} ({})",
            BLOOM_FILTER_NAME,
            format_bytes(bloom_filter.size_bytes)
        );
        eprintln!("  - {}", MANIFEST_NAME);
        if signed {
            eprintln!("  - {}", MANIFEST_SIG_NAME);
        }
        eprintln!();
        eprintln!("Last indexed commit: {}", commit_display);
    }

    Ok(())
}

/// Generate a compressed slim index for distribution.
fn generate_slim_index<P: AsRef<Path>, Q: AsRef<Path>>(
    slim_db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
    compression_level: i32,
) -> Result<IndexFile> {
    let slim_db_path = slim_db_path.as_ref();
    let output_dir = output_dir.as_ref();
    let compressed_path = output_dir.join(INDEX_DB_NAME);

    let input_size = fs::metadata(slim_db_path)?.len();

    // Compress the database with progress
    compress_zstd_with_progress(
        slim_db_path,
        &compressed_path,
        compression_level,
        show_progress,
    )?;

    // Calculate hash of compressed file
    let sha256 = file_sha256_with_progress(&compressed_path, false)?;
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

    Ok(IndexFile {
        url,
        size_bytes: size,
        sha256,
    })
}

/// Generate a compressed full history index for distribution.
fn generate_full_history_index<P: AsRef<Path>, Q: AsRef<Path>>(
    db_path: P,
    output_dir: Q,
    url_prefix: Option<&str>,
    show_progress: bool,
    compression_level: i32,
) -> Result<(IndexFile, String)> {
    let db_path = db_path.as_ref();
    let output_dir = output_dir.as_ref();

    let compressed_path = output_dir.join(INDEX_FULL_DB_NAME);

    // Get database info and update last_indexed_date to publish time
    let db = Database::open(db_path)?;
    let last_commit = db.get_meta("last_indexed_commit")?.unwrap_or_default();
    // Set the indexed date to now (publish time) so it matches the manifest
    db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
    // Mark as full database variant
    db.set_meta("db_variant", "full")?;
    // Flush WAL to ensure the meta update is in the main DB file before compression
    db.checkpoint()?;
    let input_size = fs::metadata(db_path)?.len();

    if show_progress {
        eprintln!(
            "  Compressing database ({}) with zstd level {}...",
            format_bytes(input_size),
            compression_level
        );
    }

    // Compress the database with progress
    compress_zstd_with_progress(db_path, &compressed_path, compression_level, show_progress)?;

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
        Some(prefix) => format!("{}/{}", prefix.trim_end_matches('/'), INDEX_FULL_DB_NAME),
        None => INDEX_FULL_DB_NAME.to_string(),
    };

    let index_file = IndexFile {
        url,
        size_bytes: size,
        sha256,
    };

    Ok((index_file, last_commit))
}

/// Helper to quote a string for SQL (used by `generate_delta_pack`).
#[allow(dead_code)]
fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Helper to quote an optional string for SQL (used by `generate_delta_pack`).
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
                source_path TEXT,
                known_vulnerabilities TEXT,
                store_path_x86_64_linux TEXT,
                store_path_aarch64_linux TEXT,
                store_path_x86_64_darwin TEXT,
                store_path_aarch64_darwin TEXT,
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
    fn test_generate_slim_db() {
        use rusqlite::Connection;

        let dir = tempdir().unwrap();
        let full_db_path = dir.path().join("full.db");
        let slim_db_path = dir.path().join("slim.db");

        // Create a test database with multiple rows for same (attr, version)
        let conn = Connection::open(&full_db_path).unwrap();
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
                source_path TEXT,
                known_vulnerabilities TEXT,
                store_path_x86_64_linux TEXT,
                store_path_aarch64_linux TEXT,
                store_path_x86_64_darwin TEXT,
                store_path_aarch64_darwin TEXT,
                UNIQUE(attribute_path, version, first_commit_hash)
            );

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'test123');

            -- Multiple ranges for same (attr, version) - should be consolidated
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python', '3.11.0', 'aaa', 1000, 'bbb', 1100, 'python311', 'Python 3.11'),
                ('python', '3.11.0', 'ccc', 1200, 'ddd', 1300, 'python311', 'Python 3.11 updated'),
                ('python', '3.11.0', 'eee', 1400, 'fff', 1500, 'python311', 'Python 3.11 latest'),
                -- Different version
                ('python', '3.10.0', 'ggg', 900, 'hhh', 950, 'python310', 'Python 3.10'),
                -- Different package
                ('nodejs', '20.0.0', 'iii', 1000, 'jjj', 1100, 'nodejs_20', 'Node.js');
            "#,
        )
        .unwrap();
        drop(conn);

        // Generate slim database
        generate_slim_db(&full_db_path, &slim_db_path, false).unwrap();

        // Verify slim database
        let slim_conn = Connection::open(&slim_db_path).unwrap();

        // Should have 3 rows (python311, python310, nodejs_20)
        let count: i64 = slim_conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 3,
            "Should consolidate to 3 unique (attr, version) pairs"
        );

        // Verify python311 was consolidated correctly
        let (first_date, last_date, description): (i64, i64, String) = slim_conn
            .query_row(
                "SELECT first_commit_date, last_commit_date, description FROM package_versions WHERE attribute_path = 'python311'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(first_date, 1000, "first_commit_date should be MIN");
        assert_eq!(last_date, 1500, "last_commit_date should be MAX");
        assert_eq!(
            description, "Python 3.11 latest",
            "description should be from most recent row"
        );

        // Verify meta was copied
        let commit: String = slim_conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(commit, "test123");

        // Verify db_variant was set
        let variant: String = slim_conn
            .query_row("SELECT value FROM meta WHERE key = 'db_variant'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(variant, "slim");
    }

    #[test]
    fn test_generate_slim_db_preserves_unique_versions() {
        use rusqlite::Connection;

        let dir = tempdir().unwrap();
        let full_db_path = dir.path().join("full.db");
        let slim_db_path = dir.path().join("slim.db");

        // Create database with same package, different versions
        let conn = Connection::open(&full_db_path).unwrap();
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
                source_path TEXT,
                known_vulnerabilities TEXT,
                store_path_x86_64_linux TEXT,
                store_path_aarch64_linux TEXT,
                store_path_x86_64_darwin TEXT,
                store_path_aarch64_darwin TEXT,
                UNIQUE(attribute_path, version, first_commit_hash)
            );

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'test');

            -- Same attr_path, different versions - should ALL be preserved
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path)
            VALUES
                ('python', '3.9.0', 'a', 100, 'b', 200, 'python3'),
                ('python', '3.10.0', 'c', 300, 'd', 400, 'python3'),
                ('python', '3.11.0', 'e', 500, 'f', 600, 'python3'),
                ('python', '3.12.0', 'g', 700, 'h', 800, 'python3');
            "#,
        )
        .unwrap();
        drop(conn);

        generate_slim_db(&full_db_path, &slim_db_path, false).unwrap();

        let slim_conn = Connection::open(&slim_db_path).unwrap();

        // All 4 versions should be preserved
        let count: i64 = slim_conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 4, "All unique versions should be preserved");

        // Verify each version exists
        let versions: Vec<String> = slim_conn
            .prepare("SELECT version FROM package_versions ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(versions, vec!["3.10.0", "3.11.0", "3.12.0", "3.9.0"]);
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

        generate_manifest(
            output_dir,
            full_index,
            None,
            "latest123",
            vec![],
            bloom_filter,
        )
        .unwrap();

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

    #[test]
    fn test_sign_manifest_missing_key() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.json");
        let secret_key_path = dir.path().join("nonexistent.key");

        // Create a dummy manifest
        fs::write(&manifest_path, r#"{"version":1}"#).unwrap();

        // Should fail because secret key doesn't exist
        let result = sign_manifest(&manifest_path, secret_key_path.to_str().unwrap());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "Unexpected error: {}",
            err_msg
        );
    }

    #[test]
    fn test_sign_manifest_invalid_key() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.json");
        let secret_key_path = dir.path().join("invalid.key");

        // Create a dummy manifest and invalid key
        fs::write(&manifest_path, r#"{"version":1}"#).unwrap();
        fs::write(&secret_key_path, "not a valid key").unwrap();

        // Should fail because key is invalid
        let result = sign_manifest(&manifest_path, secret_key_path.to_str().unwrap());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to parse secret key")
                || err_msg.contains("failed to load secret key"),
            "Unexpected error: {}",
            err_msg
        );
    }

    #[test]
    fn test_generate_and_sign() {
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");
        let manifest_path = dir.path().join("manifest.json");

        // Generate keypair
        let pk = generate_keypair(&sk_path, &pk_path, "test key", false).unwrap();
        assert!(!pk.is_empty());
        assert!(sk_path.exists());
        assert!(pk_path.exists());

        // Create a manifest
        fs::write(&manifest_path, r#"{"version":1}"#).unwrap();

        // Sign it
        let sig_path = sign_manifest(&manifest_path, sk_path.to_str().unwrap()).unwrap();
        assert!(sig_path.exists());

        // Verify signature file contains expected content
        let sig_content = fs::read_to_string(&sig_path).unwrap();
        assert!(sig_content.contains("untrusted comment:"));
        assert!(sig_content.contains("trusted comment:"));
    }

    #[test]
    fn test_sign_manifest_comment_placement() {
        // Verify that the trusted comment contains the timestamp (cryptographically protected)
        // and the untrusted comment contains the description.
        // Per minisign convention, timestamps should be in trusted comments to prevent tampering.
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");
        let manifest_path = dir.path().join("manifest.json");

        generate_keypair(&sk_path, &pk_path, "comment test", false).unwrap();
        fs::write(&manifest_path, r#"{"version":1}"#).unwrap();

        let sig_path = sign_manifest(&manifest_path, sk_path.to_str().unwrap()).unwrap();
        let sig_content = fs::read_to_string(&sig_path).unwrap();

        // Parse the signature file lines
        let lines: Vec<&str> = sig_content.lines().collect();
        assert!(lines.len() >= 4, "Signature should have at least 4 lines");

        // Line 0: untrusted comment
        // Line 1: base64 signature
        // Line 2: trusted comment
        // Line 3: base64 global signature
        let untrusted_line = lines[0];
        let trusted_line = lines[2];

        assert!(
            untrusted_line.starts_with("untrusted comment:"),
            "First line should be untrusted comment"
        );
        assert!(
            trusted_line.starts_with("trusted comment:"),
            "Third line should be trusted comment"
        );

        // The timestamp should be in the TRUSTED comment (cryptographically protected)
        assert!(
            trusted_line.contains("timestamp:"),
            "Trusted comment should contain timestamp, got: {}",
            trusted_line
        );

        // The description should be in the UNTRUSTED comment
        assert!(
            untrusted_line.contains("nxv manifest signature"),
            "Untrusted comment should contain 'nxv manifest signature', got: {}",
            untrusted_line
        );
    }

    #[test]
    fn test_generate_keypair_force_overwrite() {
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");

        // Generate first keypair
        let pk1 = generate_keypair(&sk_path, &pk_path, "first key", false).unwrap();
        let sk1_content = fs::read_to_string(&sk_path).unwrap();

        // Try to generate again without force - should fail
        let result = generate_keypair(&sk_path, &pk_path, "second key", false);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("already exists"));

        // Generate again with force - should succeed
        let pk2 = generate_keypair(&sk_path, &pk_path, "second key", true).unwrap();
        let sk2_content = fs::read_to_string(&sk_path).unwrap();

        // Keys should be different (new keypair generated)
        assert_ne!(pk1, pk2);
        assert_ne!(sk1_content, sk2_content);
    }

    #[test]
    fn test_publish_index_without_signing() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let output_dir = dir.path().join("output");

        create_test_db(&db_path);

        // Publish without signing (use low compression for fast tests)
        publish_index(&db_path, &output_dir, None, false, None, 1).unwrap();

        // Verify all artifacts except signature
        assert!(
            output_dir.join(INDEX_DB_NAME).exists(),
            "slim index should exist"
        );
        assert!(
            output_dir.join(INDEX_FULL_DB_NAME).exists(),
            "full history index should exist"
        );
        assert!(output_dir.join(BLOOM_FILTER_NAME).exists());
        assert!(output_dir.join(MANIFEST_NAME).exists());
        assert!(!output_dir.join(MANIFEST_SIG_NAME).exists());
    }

    #[test]
    fn test_sign_and_verify_roundtrip() {
        use crate::remote::manifest::verify_manifest_signature_with_key;

        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");
        let manifest_path = dir.path().join("manifest.json");

        // Generate keypair
        generate_keypair(&sk_path, &pk_path, "roundtrip test", false).unwrap();

        // Create a manifest
        let manifest_content = r#"{"version":1,"latest_commit":"abc123"}"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        // Sign it
        let sig_path = sign_manifest(&manifest_path, sk_path.to_str().unwrap()).unwrap();

        // Read public key and signature
        let pk_content = fs::read_to_string(&pk_path).unwrap();
        let sig_content = fs::read_to_string(&sig_path).unwrap();

        // Verify the signature
        let result = verify_manifest_signature_with_key(
            manifest_content.as_bytes(),
            &sig_content,
            &pk_content,
        );

        assert!(
            result.is_ok(),
            "Signature verification should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_resolve_secret_key_from_file() {
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");

        // Generate a real keypair
        generate_keypair(&sk_path, &pk_path, "test", false).unwrap();

        // Resolve from file path
        let key_content = resolve_secret_key(sk_path.to_str().unwrap()).unwrap();
        assert!(key_content.contains("untrusted comment:"));
    }

    #[test]
    fn test_resolve_secret_key_from_raw_content() {
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");

        // Generate a real keypair
        generate_keypair(&sk_path, &pk_path, "test", false).unwrap();

        // Read the key content
        let original_content = fs::read_to_string(&sk_path).unwrap();

        // Resolve from raw content
        let resolved = resolve_secret_key(&original_content).unwrap();
        assert_eq!(resolved, original_content);
    }

    #[test]
    fn test_resolve_secret_key_nonexistent_path() {
        // Should fail for path-like strings that don't exist
        let result = resolve_secret_key("/nonexistent/path/to/key.key");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_resolve_secret_key_with_whitespace() {
        let dir = tempdir().unwrap();
        let sk_path = dir.path().join("test.key");
        let pk_path = dir.path().join("test.pub");

        // Generate a real keypair
        generate_keypair(&sk_path, &pk_path, "test", false).unwrap();

        // Resolve with leading/trailing whitespace
        let path_with_space = format!("  {}  ", sk_path.to_str().unwrap());
        let key_content = resolve_secret_key(&path_with_space).unwrap();
        assert!(key_content.contains("untrusted comment:"));
    }
}

//! Database module for nxv index storage.

pub mod import;
pub mod queries;

use crate::error::{NxvError, Result};
#[cfg(feature = "indexer")]
use queries::PackageVersion;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;
#[cfg(feature = "indexer")]
use std::time::Instant;
use tracing::instrument;
#[cfg(feature = "indexer")]
use tracing::trace;

/// Default timeout for SQLite busy handler (in seconds).
/// When the database is locked, SQLite will retry for this duration before returning SQLITE_BUSY.
const DEFAULT_BUSY_TIMEOUT_SECS: u64 = 5;

/// Current schema version.
/// v4: UPSERT model - one row per (attribute_path, version), added version_source column
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
const SCHEMA_VERSION: u32 = 4;

/// Supported systems for store paths.
pub const STORE_PATH_SYSTEMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "x86_64-darwin",
    "aarch64-darwin",
];

/// Minimum schema version this build can read.
/// Indexes with min_schema_version > this value are incompatible.
pub const MIN_READABLE_SCHEMA: u32 = 4;

/// Database connection wrapper.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create a database at the given path.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() && path.is_dir() {
            let path_str = path.display().to_string();
            let path_trimmed = path_str.trim_end_matches('/');
            return Err(NxvError::InvalidPath(format!(
                "'{}' is a directory, not a file. Expected a path like '{}/index.db'",
                path.display(),
                path_trimmed
            )));
        }
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrent performance and durability
        // Set larger cache for better performance with large indexes
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA wal_autocheckpoint = 1000;
            PRAGMA cache_size = -64000;
            PRAGMA temp_store = MEMORY;
            "#,
        )?;

        let db = Self { conn };
        db.init_schema()?;
        db.migrate_if_needed()?;
        Ok(db)
    }

    /// Checkpoint the WAL to ensure data is flushed to disk.
    /// Call this at regular intervals during long-running operations.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    #[instrument(skip(self))]
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Open a database in read-only mode.
    ///
    /// Validates that the database schema is compatible with this version of nxv.
    /// Returns an error if the database was created with a newer, incompatible schema version.
    ///
    /// The connection is configured with a busy timeout to prevent indefinite blocking
    /// when the database is locked by another process.
    pub fn open_readonly<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(NxvError::NoIndex);
        }
        if path.is_dir() {
            let path_str = path.display().to_string();
            let path_trimmed = path_str.trim_end_matches('/');
            return Err(NxvError::InvalidPath(format!(
                "'{}' is a directory, not a file. Expected a path like '{}/index.db'",
                path.display(),
                path_trimmed
            )));
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

        // Set busy timeout to prevent indefinite blocking on database locks.
        // This is critical for preventing thread pool exhaustion under load.
        conn.busy_timeout(Duration::from_secs(DEFAULT_BUSY_TIMEOUT_SECS))?;

        // Performance optimizations for read-only queries
        conn.execute_batch(
            r#"
            PRAGMA cache_size = -64000;
            PRAGMA temp_store = MEMORY;
            PRAGMA case_sensitive_like = ON;
            "#,
        )?;

        let db = Self { conn };

        // Validate schema version compatibility
        db.validate_schema_version()?;

        Ok(db)
    }

    /// Validate that the database schema is compatible with this version of nxv.
    fn validate_schema_version(&self) -> Result<()> {
        // Check if meta table exists (very old or corrupt database)
        let has_meta: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |row| row.get(0),
        )?;

        if !has_meta {
            return Err(NxvError::CorruptIndex("missing meta table".to_string()));
        }

        // Check min_schema_version if set by the indexer (for future schema changes).
        // Falls back to schema_version for indexes that don't have min_schema_version yet.
        let min_version_str = match self.get_meta("min_schema_version")? {
            Some(v) => Some(v),
            None => self.get_meta("schema_version")?,
        };
        let min_version_str = min_version_str.as_deref().unwrap_or("0");
        let min_schema_version: u32 = min_version_str.parse().map_err(|_| {
            NxvError::CorruptIndex(format!(
                "invalid min_schema_version '{}': expected integer",
                min_version_str
            ))
        })?;

        if min_schema_version > MIN_READABLE_SCHEMA {
            return Err(NxvError::IncompatibleIndex(format!(
                "index requires schema version {} but this build only supports up to {}. \
                 Please upgrade nxv to use this index.",
                min_schema_version, MIN_READABLE_SCHEMA
            )));
        }

        Ok(())
    }

    /// Initializes the database schema and related search index.
    ///
    /// Creates the `meta` and `package_versions` tables (including the `source_path` column),
    /// common indexes, and a persistent FTS5 virtual table `package_versions_fts` with triggers
    /// to keep it synchronized with `package_versions`. If the `schema_version` metadata entry
    /// is missing, sets it to the current SCHEMA_VERSION.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the schema is present or was created successfully, `Err(_)` if a database
    /// operation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # use crate::db::Database;
    /// let db = Database::open(":memory:").unwrap();
    /// db.init_schema().unwrap();
    /// ```
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            -- Track indexing state and metadata
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Main package version table
            CREATE TABLE IF NOT EXISTS package_versions (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                version_source TEXT,
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

            -- Indexes for common query patterns
            CREATE INDEX IF NOT EXISTS idx_packages_name ON package_versions(name);
            CREATE INDEX IF NOT EXISTS idx_packages_name_version ON package_versions(name, version, first_commit_date);
            CREATE INDEX IF NOT EXISTS idx_packages_attr ON package_versions(attribute_path);
            CREATE INDEX IF NOT EXISTS idx_packages_first_date ON package_versions(first_commit_date DESC);
            CREATE INDEX IF NOT EXISTS idx_packages_last_date ON package_versions(last_commit_date DESC);

            -- Covering indexes for optimized search queries
            -- These allow ORDER BY to use the index directly without a separate sort
            CREATE INDEX IF NOT EXISTS idx_attr_date_covering ON package_versions(
                attribute_path, last_commit_date DESC, name, version
            );
            CREATE INDEX IF NOT EXISTS idx_name_attr_date_covering ON package_versions(
                name, attribute_path, last_commit_date DESC
            );
            "#,
        )?;

        // Create FTS5 table if it doesn't exist
        let fts_exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='package_versions_fts'",
            [],
            |row| row.get(0),
        )?;

        if !fts_exists {
            self.conn.execute_batch(
                r#"
                CREATE VIRTUAL TABLE package_versions_fts
                USING fts5(name, description, content=package_versions, content_rowid=id);

                -- Triggers to keep FTS5 in sync with package_versions
                CREATE TRIGGER IF NOT EXISTS package_versions_ai AFTER INSERT ON package_versions BEGIN
                    INSERT INTO package_versions_fts(rowid, name, description)
                    VALUES (new.id, new.name, new.description);
                END;

                CREATE TRIGGER IF NOT EXISTS package_versions_ad AFTER DELETE ON package_versions BEGIN
                    INSERT INTO package_versions_fts(package_versions_fts, rowid, name, description)
                    VALUES ('delete', old.id, old.name, old.description);
                END;

                CREATE TRIGGER IF NOT EXISTS package_versions_au AFTER UPDATE ON package_versions BEGIN
                    INSERT INTO package_versions_fts(package_versions_fts, rowid, name, description)
                    VALUES ('delete', old.id, old.name, old.description);
                    INSERT INTO package_versions_fts(rowid, name, description)
                    VALUES (new.id, new.name, new.description);
                END;
                "#,
            )?;
        }

        // Create vulnerability index if column exists (for new databases)
        // This index is conditional because old databases being migrated won't have the column yet
        let has_known_vulns: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='known_vulnerabilities'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if has_known_vulns {
            self.conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_version_vulnerabilities ON package_versions(version) WHERE known_vulnerabilities IS NOT NULL AND known_vulnerabilities != '' AND known_vulnerabilities != '[]' AND known_vulnerabilities != 'null'",
                [],
            )?;
        }

        // Set schema version if not already set
        let version = self.get_meta("schema_version")?;
        if version.is_none() {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        }

        Ok(())
    }

    /// Apply any pending schema migrations to the database.
    ///
    /// This updates the on-disk schema to the module's current `SCHEMA_VERSION`.
    /// Specifically, when upgrading from versions earlier than 2 it adds the
    /// `source_path` TEXT column to the `package_versions` table if that column is
    /// not already present, and then writes the new `schema_version` into the
    /// `meta` table.
    ///
    /// # Examples
    ///
    /// ```
    /// let db = Database::open(std::path::Path::new(":memory:")).unwrap();
    /// db.migrate_if_needed().unwrap();
    /// ```
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    fn migrate_if_needed(&self) -> Result<()> {
        let version_str = self.get_meta("schema_version")?;
        let current_version: u32 = version_str.as_deref().unwrap_or("0").parse().unwrap_or(0);

        if current_version < 2 {
            // Migration v1 -> v2: Add source_path column
            let has_source_path: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='source_path'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !has_source_path {
                self.conn.execute(
                    "ALTER TABLE package_versions ADD COLUMN source_path TEXT",
                    [],
                )?;
            }
        }

        if current_version < 3 {
            // Migration v2 -> v3: Add known_vulnerabilities column
            let has_known_vulns: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='known_vulnerabilities'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !has_known_vulns {
                self.conn.execute(
                    "ALTER TABLE package_versions ADD COLUMN known_vulnerabilities TEXT",
                    [],
                )?;
            }

            // Add index for efficient vulnerability lookups in version history queries
            self.conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_version_vulnerabilities ON package_versions(version) WHERE known_vulnerabilities IS NOT NULL AND known_vulnerabilities != '' AND known_vulnerabilities != '[]' AND known_vulnerabilities != 'null'",
                [],
            )?;
        }

        if current_version < 4 {
            // Migration v3 -> v4: Add version_source column
            let has_version_source: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='version_source'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !has_version_source {
                self.conn.execute(
                    "ALTER TABLE package_versions ADD COLUMN version_source TEXT",
                    [],
                )?;
            }
        }

        // Ensure all schema elements exist (runs unconditionally for idempotent upgrades)
        {
            // Add store_path column to package_versions if not present
            let has_store_path: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='store_path'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !has_store_path {
                self.conn.execute(
                    "ALTER TABLE package_versions ADD COLUMN store_path TEXT",
                    [],
                )?;
            }

            // Add per-architecture store_path columns to package_versions
            for system in STORE_PATH_SYSTEMS {
                let col_name = format!("store_path_{}", system.replace('-', "_"));
                let has_col: bool = self
                    .conn
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='{}'",
                            col_name
                        ),
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(false);

                if !has_col {
                    self.conn.execute(
                        &format!("ALTER TABLE package_versions ADD COLUMN {} TEXT", col_name),
                        [],
                    )?;
                }
            }

            // Migrate existing store_path data to store_path_x86_64_linux
            // (since previous indexer used x86_64-linux as default)
            self.conn.execute(
                "UPDATE package_versions SET store_path_x86_64_linux = store_path WHERE store_path IS NOT NULL AND store_path_x86_64_linux IS NULL",
                [],
            )?;

            // Add covering indexes for optimized search queries
            // These indexes allow ORDER BY to use the index directly without a separate sort step
            self.conn.execute_batch(
                r#"
                -- Covering index for attribute_path searches with date ordering
                CREATE INDEX IF NOT EXISTS idx_attr_date_covering ON package_versions(
                    attribute_path, last_commit_date DESC, name, version
                );

                -- Covering index for name-based searches with attribute_path filtering
                CREATE INDEX IF NOT EXISTS idx_name_attr_date_covering ON package_versions(
                    name, attribute_path, last_commit_date DESC
                );

                -- Update query planner statistics for optimal index selection
                ANALYZE;
                "#,
            )?;
        }

        if current_version < SCHEMA_VERSION {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        }

        Ok(())
    }

    /// Get a metadata value by key.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?", [key], |row| {
                row.get(0)
            });

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a metadata value.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    #[instrument(skip(self, value))]
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?, ?)",
            [key, value],
        )?;
        Ok(())
    }

    /// Get checkpoint for a specific year range (parallel indexing).
    ///
    /// Returns the last indexed commit hash for the given range label.
    #[cfg(feature = "indexer")]
    pub fn get_range_checkpoint(&self, range_label: &str) -> Result<Option<String>> {
        self.get_meta(&format!("last_indexed_commit_{}", range_label))
    }

    /// Set checkpoint for a specific year range (parallel indexing).
    ///
    /// Stores the last indexed commit hash and timestamp for the given range.
    #[cfg(feature = "indexer")]
    #[instrument(skip(self))]
    pub fn set_range_checkpoint(&self, range_label: &str, commit_hash: &str) -> Result<()> {
        self.set_meta(&format!("last_indexed_commit_{}", range_label), commit_hash)?;
        self.set_meta(
            &format!("last_indexed_date_{}", range_label),
            &chrono::Utc::now().to_rfc3339(),
        )?;
        Ok(())
    }

    /// Get all range checkpoints (for resume logic in parallel indexing).
    ///
    /// Returns a map from range label to last indexed commit hash.
    #[cfg(feature = "indexer")]
    #[allow(dead_code)] // Available for future resume functionality and testing
    pub fn get_all_range_checkpoints(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta WHERE key LIKE 'last_indexed_commit_%'")?;
        let rows = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            // Extract range label from key (e.g., "last_indexed_commit_2017" -> "2017")
            let label = key
                .strip_prefix("last_indexed_commit_")
                .unwrap_or(&key)
                .to_string();
            Ok((label, value))
        })?;

        let mut checkpoints = std::collections::HashMap::new();
        for row in rows {
            let (label, hash) = row?;
            checkpoints.insert(label, hash);
        }
        Ok(checkpoints)
    }

    /// Clear all range checkpoints (for fresh start in parallel indexing).
    #[cfg(feature = "indexer")]
    #[allow(dead_code)] // Available for future resume functionality and testing
    pub fn clear_range_checkpoints(&self) -> Result<()> {
        self.conn.execute(
            "DELETE FROM meta WHERE key LIKE 'last_indexed_commit_%' OR key LIKE 'last_indexed_date_%'",
            [],
        )?;
        Ok(())
    }

    /// Get the latest indexed commit across all checkpoint types.
    ///
    /// This unifies the regular incremental checkpoint (`last_indexed_commit`) with
    /// year-range checkpoints (`last_indexed_commit_YEAR`, etc.) by finding the one
    /// with the most recent timestamp.
    ///
    /// Returns the commit hash with the latest `last_indexed_date*` timestamp.
    #[cfg(feature = "indexer")]
    pub fn get_latest_checkpoint(&self) -> Result<Option<String>> {
        // Get all checkpoint keys (both regular and range-specific)
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM meta WHERE key LIKE 'last_indexed_commit%' AND key NOT LIKE '%_date%'"
        )?;

        let checkpoints: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        if checkpoints.is_empty() {
            return Ok(None);
        }

        // Find the checkpoint with the latest date
        let mut latest: Option<(String, i64)> = None;
        for (key, hash) in checkpoints {
            let date_key = key.replace("last_indexed_commit", "last_indexed_date");
            if let Some(date_str) = self.get_meta(&date_key)?
                && let Ok(date) = chrono::DateTime::parse_from_rfc3339(&date_str)
            {
                let ts = date.timestamp();
                if latest.is_none() || ts > latest.as_ref().unwrap().1 {
                    latest = Some((hash, ts));
                }
            }
        }

        Ok(latest.map(|(hash, _)| hash))
    }

    /// Get the underlying connection for advanced operations.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Inserts multiple package version records in a single transaction.
    ///
    /// Uses a transaction for performance and atomicity. Duplicate entries (same
    /// `attribute_path`, `version`, and `first_commit_hash`) are ignored.
    ///
    /// # Returns
    ///
    /// The number of rows that were actually inserted.
    ///
    /// # Examples
    ///
    /// ```
    /// # use crate::db::Database;
    /// # use crate::db::queries::PackageVersion;
    /// # fn example(mut db: Database, packages: Vec<PackageVersion>) {
    /// let upserted = db.upsert_packages_batch(&packages).unwrap();
    /// # }
    /// ```
    #[cfg(feature = "indexer")]
    #[instrument(skip(self, packages), fields(batch_size = packages.len()))]
    pub fn upsert_packages_batch(&mut self, packages: &[PackageVersion]) -> Result<usize> {
        let batch_start = Instant::now();
        let tx = self.conn.transaction()?;
        let tx_start_time = batch_start.elapsed();

        let mut upserted = 0;

        {
            // UPSERT: Insert new rows, or update existing ones.
            // - first_commit: keep the earlier date
            // - last_commit: keep the later date
            // - metadata: use values from the row with later last_commit_date
            // - store_paths: merge (first non-null wins per architecture)
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO package_versions
                    (name, version, version_source, first_commit_hash, first_commit_date,
                     last_commit_hash, last_commit_date, attribute_path,
                     description, license, homepage, maintainers, platforms, source_path,
                     known_vulnerabilities,
                     store_path_x86_64_linux, store_path_aarch64_linux,
                     store_path_x86_64_darwin, store_path_aarch64_darwin)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ON CONFLICT(attribute_path, version) DO UPDATE SET
                    -- Update first_commit if new one is earlier
                    first_commit_hash = CASE
                        WHEN excluded.first_commit_date < first_commit_date
                        THEN excluded.first_commit_hash
                        ELSE first_commit_hash
                    END,
                    first_commit_date = MIN(first_commit_date, excluded.first_commit_date),
                    -- Update last_commit if new one is later
                    last_commit_hash = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN excluded.last_commit_hash
                        ELSE last_commit_hash
                    END,
                    last_commit_date = MAX(last_commit_date, excluded.last_commit_date),
                    -- Update metadata from the row with later last_commit_date
                    description = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.description, description)
                        ELSE description
                    END,
                    license = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.license, license)
                        ELSE license
                    END,
                    homepage = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.homepage, homepage)
                        ELSE homepage
                    END,
                    maintainers = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.maintainers, maintainers)
                        ELSE maintainers
                    END,
                    platforms = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.platforms, platforms)
                        ELSE platforms
                    END,
                    source_path = COALESCE(source_path, excluded.source_path),
                    known_vulnerabilities = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.known_vulnerabilities, known_vulnerabilities)
                        ELSE known_vulnerabilities
                    END,
                    version_source = CASE
                        WHEN excluded.last_commit_date > last_commit_date
                        THEN COALESCE(excluded.version_source, version_source)
                        ELSE version_source
                    END,
                    -- Store paths: first non-null wins per architecture
                    store_path_x86_64_linux = COALESCE(store_path_x86_64_linux, excluded.store_path_x86_64_linux),
                    store_path_aarch64_linux = COALESCE(store_path_aarch64_linux, excluded.store_path_aarch64_linux),
                    store_path_x86_64_darwin = COALESCE(store_path_x86_64_darwin, excluded.store_path_x86_64_darwin),
                    store_path_aarch64_darwin = COALESCE(store_path_aarch64_darwin, excluded.store_path_aarch64_darwin)
                "#,
            )?;

            for pkg in packages {
                stmt.execute(rusqlite::params![
                    pkg.name,
                    pkg.version,
                    pkg.version_source,
                    pkg.first_commit_hash,
                    pkg.first_commit_date.timestamp(),
                    pkg.last_commit_hash,
                    pkg.last_commit_date.timestamp(),
                    pkg.attribute_path,
                    pkg.description,
                    pkg.license,
                    pkg.homepage,
                    pkg.maintainers,
                    pkg.platforms,
                    pkg.source_path,
                    pkg.known_vulnerabilities,
                    pkg.store_paths.get("x86_64-linux"),
                    pkg.store_paths.get("aarch64-linux"),
                    pkg.store_paths.get("x86_64-darwin"),
                    pkg.store_paths.get("aarch64-darwin"),
                ])?;
                upserted += 1;
            }
        }

        let insert_time = batch_start.elapsed();
        tx.commit()?;
        let total_time = batch_start.elapsed();

        trace!(
            batch_size = packages.len(),
            upserted = upserted,
            tx_start_ms = tx_start_time.as_millis(),
            insert_ms = insert_time.as_millis(),
            commit_ms = (total_time - insert_time).as_millis(),
            total_ms = total_time.as_millis(),
            "Batch upsert completed"
        );

        Ok(upserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_database_open_creates_schema() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Check that tables exist
        let table_count: i32 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('meta', 'package_versions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
    }

    #[test]
    fn test_database_meta_operations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Initially no value
        assert!(db.get_meta("test_key").unwrap().is_none());

        // Set and get
        db.set_meta("test_key", "test_value").unwrap();
        assert_eq!(
            db.get_meta("test_key").unwrap(),
            Some("test_value".to_string())
        );

        // Update
        db.set_meta("test_key", "new_value").unwrap();
        assert_eq!(
            db.get_meta("test_key").unwrap(),
            Some("new_value".to_string())
        );
    }

    #[test]
    fn test_database_open_readonly_missing_file() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("nonexistent.db");
        let result = Database::open_readonly(&db_path);
        assert!(matches!(result, Err(NxvError::NoIndex)));
    }

    #[test]
    fn test_schema_versioning() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        let version = db.get_meta("schema_version").unwrap();
        assert_eq!(version, Some(SCHEMA_VERSION.to_string()));
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_upsert_new_packages() {
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let now = Utc::now();
        let packages = vec![
            PackageVersion {
                id: 0,
                name: "python".to_string(),
                version: "3.11.0".to_string(),
                version_source: None,
                first_commit_hash: "abc1234567890".to_string(),
                first_commit_date: now,
                last_commit_hash: "def1234567890".to_string(),
                last_commit_date: now,
                attribute_path: "python311".to_string(),
                description: Some("Python interpreter".to_string()),
                license: None,
                homepage: None,
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: std::collections::HashMap::new(),
            },
            PackageVersion {
                id: 0,
                name: "nodejs".to_string(),
                version: "20.0.0".to_string(),
                version_source: None,
                first_commit_hash: "ghi1234567890".to_string(),
                first_commit_date: now,
                last_commit_hash: "jkl1234567890".to_string(),
                last_commit_date: now,
                attribute_path: "nodejs_20".to_string(),
                description: Some("Node.js runtime".to_string()),
                license: None,
                homepage: None,
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: std::collections::HashMap::new(),
            },
        ];

        let upserted = db.upsert_packages_batch(&packages).unwrap();
        assert_eq!(upserted, 2);

        // Verify data was inserted
        let count: i32 = db
            .conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_upsert_updates_commit_bounds() {
        use chrono::{TimeZone, Utc};

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let date1 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
        let date2 = Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap();
        let date3 = Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap(); // Earlier than date1

        // First insert
        let pkg1 = PackageVersion {
            id: 0,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            version_source: Some("direct".to_string()),
            first_commit_hash: "commit_jan".to_string(),
            first_commit_date: date1,
            last_commit_hash: "commit_jan".to_string(),
            last_commit_date: date1,
            attribute_path: "python311".to_string(),
            description: Some("Python interpreter".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: std::collections::HashMap::new(),
        };
        db.upsert_packages_batch(&[pkg1]).unwrap();

        // Second upsert with later last_commit
        let pkg2 = PackageVersion {
            id: 0,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            version_source: Some("direct".to_string()),
            first_commit_hash: "commit_jun".to_string(),
            first_commit_date: date2,
            last_commit_hash: "commit_jun".to_string(),
            last_commit_date: date2,
            attribute_path: "python311".to_string(),
            description: Some("Updated description".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: std::collections::HashMap::new(),
        };
        db.upsert_packages_batch(&[pkg2]).unwrap();

        // Third upsert with earlier first_commit
        let pkg3 = PackageVersion {
            id: 0,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            version_source: Some("direct".to_string()),
            first_commit_hash: "commit_2021".to_string(),
            first_commit_date: date3,
            last_commit_hash: "commit_2021".to_string(),
            last_commit_date: date3,
            attribute_path: "python311".to_string(),
            description: Some("Old description".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: std::collections::HashMap::new(),
        };
        db.upsert_packages_batch(&[pkg3]).unwrap();

        // Verify: should be one row with first=2021, last=2022-06, description from 2022-06
        let count: i32 = db
            .conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "Should have exactly one row");

        let (first_hash, last_hash, desc): (String, String, String) = db
            .conn
            .query_row(
                "SELECT first_commit_hash, last_commit_hash, description FROM package_versions WHERE attribute_path = 'python311'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(
            first_hash, "commit_2021",
            "first_commit should be the earliest"
        );
        assert_eq!(last_hash, "commit_jun", "last_commit should be the latest");
        assert_eq!(
            desc, "Updated description",
            "description should be from latest"
        );
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_upsert_store_paths_merge() {
        use chrono::Utc;
        use std::collections::HashMap;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let now = Utc::now();

        // First insert with x86_64-linux store path
        let mut store_paths1 = HashMap::new();
        store_paths1.insert(
            "x86_64-linux".to_string(),
            "/nix/store/abc-hello".to_string(),
        );

        let pkg1 = PackageVersion {
            id: 0,
            name: "hello".to_string(),
            version: "2.10".to_string(),
            version_source: None,
            first_commit_hash: "commit1".to_string(),
            first_commit_date: now,
            last_commit_hash: "commit1".to_string(),
            last_commit_date: now,
            attribute_path: "hello".to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: store_paths1,
        };
        db.upsert_packages_batch(&[pkg1]).unwrap();

        // Second insert with aarch64-linux store path
        let mut store_paths2 = HashMap::new();
        store_paths2.insert(
            "aarch64-linux".to_string(),
            "/nix/store/def-hello".to_string(),
        );

        let pkg2 = PackageVersion {
            id: 0,
            name: "hello".to_string(),
            version: "2.10".to_string(),
            version_source: None,
            first_commit_hash: "commit2".to_string(),
            first_commit_date: now,
            last_commit_hash: "commit2".to_string(),
            last_commit_date: now,
            attribute_path: "hello".to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: store_paths2,
        };
        db.upsert_packages_batch(&[pkg2]).unwrap();

        // Verify both store paths are present
        let (x86_path, aarch64_path): (Option<String>, Option<String>) = db
            .conn
            .query_row(
                "SELECT store_path_x86_64_linux, store_path_aarch64_linux FROM package_versions WHERE attribute_path = 'hello'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(x86_path, Some("/nix/store/abc-hello".to_string()));
        assert_eq!(aarch64_path, Some("/nix/store/def-hello".to_string()));
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_fts5_trigger_sync() {
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let now = Utc::now();
        let pkg = PackageVersion {
            id: 0,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
            version_source: None,
            first_commit_hash: "abc1234567890".to_string(),
            first_commit_date: now,
            last_commit_hash: "def1234567890".to_string(),
            last_commit_date: now,
            attribute_path: "python311".to_string(),
            description: Some("Python interpreter for scripting".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            store_paths: std::collections::HashMap::new(),
        };

        db.upsert_packages_batch(&[pkg]).unwrap();

        // FTS5 should be searchable
        let fts_count: i32 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM package_versions_fts WHERE package_versions_fts MATCH 'scripting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1);
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_upsert_10k_performance() {
        use chrono::Utc;
        use std::time::Instant;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let now = Utc::now();
        let packages: Vec<PackageVersion> = (0..10_000)
            .map(|i| PackageVersion {
                id: 0,
                name: format!("package{}", i),
                version: format!("1.0.{}", i),
                version_source: None,
                first_commit_hash: format!("abc{:040}", i),
                first_commit_date: now,
                last_commit_hash: format!("def{:040}", i),
                last_commit_date: now,
                attribute_path: format!("packages.package{}", i),
                description: Some(format!("Test package {}", i)),
                license: None,
                homepage: None,
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: std::collections::HashMap::new(),
            })
            .collect();

        let start = Instant::now();
        let upserted = db.upsert_packages_batch(&packages).unwrap();
        let duration = start.elapsed();

        assert_eq!(upserted, 10_000);
        assert!(
            duration.as_secs() < 5,
            "Batch upsert took {:?}, expected < 5 seconds",
            duration
        );

        // Verify data was inserted
        let count: i32 = db
            .conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 10_000);
    }

    // =============================================
    // Range checkpoint tests (indexer feature only)
    // =============================================

    #[test]
    #[cfg(feature = "indexer")]
    fn test_range_checkpoint_set_and_get() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set checkpoints for multiple ranges
        db.set_range_checkpoint("2017", "abc123").unwrap();
        db.set_range_checkpoint("2018", "def456").unwrap();

        // Retrieve them
        assert_eq!(
            db.get_range_checkpoint("2017").unwrap(),
            Some("abc123".to_string())
        );
        assert_eq!(
            db.get_range_checkpoint("2018").unwrap(),
            Some("def456".to_string())
        );
        assert_eq!(db.get_range_checkpoint("2019").unwrap(), None);
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_range_checkpoint_update() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set initial checkpoint
        db.set_range_checkpoint("2017", "abc123").unwrap();
        assert_eq!(
            db.get_range_checkpoint("2017").unwrap(),
            Some("abc123".to_string())
        );

        // Update checkpoint
        db.set_range_checkpoint("2017", "xyz789").unwrap();
        assert_eq!(
            db.get_range_checkpoint("2017").unwrap(),
            Some("xyz789".to_string())
        );
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_range_checkpoint_get_all() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set checkpoints
        db.set_range_checkpoint("2017", "abc123").unwrap();
        db.set_range_checkpoint("2018", "def456").unwrap();
        db.set_range_checkpoint("2019", "ghi789").unwrap();

        // Get all
        let all = db.get_all_range_checkpoints().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all.get("2017"), Some(&"abc123".to_string()));
        assert_eq!(all.get("2018"), Some(&"def456".to_string()));
        assert_eq!(all.get("2019"), Some(&"ghi789".to_string()));
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_range_checkpoint_clear() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set checkpoints
        db.set_range_checkpoint("2017", "abc123").unwrap();
        db.set_range_checkpoint("2018", "def456").unwrap();

        // Verify they exist
        assert_eq!(db.get_all_range_checkpoints().unwrap().len(), 2);

        // Clear all range checkpoints
        db.clear_range_checkpoints().unwrap();

        // Verify they're gone
        assert_eq!(db.get_all_range_checkpoints().unwrap().len(), 0);
        assert_eq!(db.get_range_checkpoint("2017").unwrap(), None);
        assert_eq!(db.get_range_checkpoint("2018").unwrap(), None);
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_range_checkpoint_does_not_affect_main_checkpoint() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set main checkpoint
        db.set_meta("last_indexed_commit", "main_commit").unwrap();

        // Set range checkpoints
        db.set_range_checkpoint("2017", "range_commit_1").unwrap();
        db.set_range_checkpoint("2018", "range_commit_2").unwrap();

        // Clear range checkpoints
        db.clear_range_checkpoints().unwrap();

        // Main checkpoint should still exist
        assert_eq!(
            db.get_meta("last_indexed_commit").unwrap(),
            Some("main_commit".to_string())
        );
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // No checkpoints initially
        assert!(db.get_latest_checkpoint().unwrap().is_none());
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_only_regular() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set only regular checkpoint
        db.set_meta("last_indexed_commit", "regular_commit_abc").unwrap();
        db.set_meta("last_indexed_date", "2020-06-15T12:00:00Z").unwrap();

        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "regular_commit_abc");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_only_range() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set only range checkpoints (no regular)
        db.set_range_checkpoint("2017", "commit_2017").unwrap();
        // Manually set older date for 2017
        db.set_meta("last_indexed_date_2017", "2020-01-01T00:00:00Z").unwrap();

        db.set_range_checkpoint("2018", "commit_2018").unwrap();
        // 2018 is set by set_range_checkpoint to current time (newer)

        // Should return 2018's commit (newer date)
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "commit_2018");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_mixed_regular_and_range() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set regular checkpoint with older date
        db.set_meta("last_indexed_commit", "old_regular_commit").unwrap();
        db.set_meta("last_indexed_date", "2020-01-01T00:00:00Z").unwrap();

        // Set range checkpoint (newer - set_range_checkpoint uses current time)
        db.set_range_checkpoint("2020-H2", "newer_range_commit").unwrap();

        // Should return the range commit (newer)
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "newer_range_commit");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_regular_is_newer() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set range checkpoint with older date
        db.set_range_checkpoint("2017", "old_range_commit").unwrap();
        db.set_meta("last_indexed_date_2017", "2020-01-01T00:00:00Z").unwrap();

        // Set regular checkpoint with newer date
        db.set_meta("last_indexed_commit", "newer_regular_commit").unwrap();
        db.set_meta("last_indexed_date", "2025-12-31T23:59:59Z").unwrap();

        // Should return the regular commit (newer)
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "newer_regular_commit");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_multiple_ranges_finds_newest() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set multiple range checkpoints with explicit dates
        db.set_meta("last_indexed_commit_2017", "commit_2017").unwrap();
        db.set_meta("last_indexed_date_2017", "2020-01-01T00:00:00Z").unwrap();

        db.set_meta("last_indexed_commit_2018", "commit_2018").unwrap();
        db.set_meta("last_indexed_date_2018", "2020-03-01T00:00:00Z").unwrap();

        db.set_meta("last_indexed_commit_2019", "commit_2019").unwrap();
        db.set_meta("last_indexed_date_2019", "2020-02-01T00:00:00Z").unwrap();

        db.set_meta("last_indexed_commit_2020", "commit_2020").unwrap();
        db.set_meta("last_indexed_date_2020", "2020-04-01T00:00:00Z").unwrap();

        // 2020 has the latest date
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "commit_2020");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_handles_missing_date() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set checkpoint without corresponding date (edge case)
        db.set_meta("last_indexed_commit", "orphan_commit").unwrap();
        // No last_indexed_date set

        // Set range checkpoint with date
        db.set_range_checkpoint("2020", "range_with_date").unwrap();

        // Should return the range commit (has valid date)
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "range_with_date");
    }

    #[test]
    #[cfg(feature = "indexer")]
    fn test_get_latest_checkpoint_crash_recovery_scenario() {
        // Simulates: user ran year-range indexing, some ranges completed, some crashed
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // 2017 completed
        db.set_meta("last_indexed_commit_2017", "2017_final").unwrap();
        db.set_meta("last_indexed_date_2017", "2025-01-10T10:00:00Z").unwrap();

        // 2018 crashed partway (checkpoint exists but earlier)
        db.set_meta("last_indexed_commit_2018", "2018_partial").unwrap();
        db.set_meta("last_indexed_date_2018", "2025-01-10T09:00:00Z").unwrap();

        // 2019 completed
        db.set_meta("last_indexed_commit_2019", "2019_final").unwrap();
        db.set_meta("last_indexed_date_2019", "2025-01-10T11:00:00Z").unwrap();

        // 2020 completed last (newest)
        db.set_meta("last_indexed_commit_2020", "2020_final").unwrap();
        db.set_meta("last_indexed_date_2020", "2025-01-10T12:00:00Z").unwrap();

        // get_latest_checkpoint returns the most recent by date
        // This is correct behavior - it's NOT responsible for gap detection
        // Gap detection (2018 incomplete) is a separate concern for the indexer
        let latest = db.get_latest_checkpoint().unwrap().unwrap();
        assert_eq!(latest, "2020_final");

        // Note: To detect gaps, user should:
        // 1. Check get_all_range_checkpoints() to see which ranges have checkpoints
        // 2. Re-run crashed ranges: nxv index --year-range 2018
    }
}

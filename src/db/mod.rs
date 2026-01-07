//! Database module for nxv index storage.

pub mod import;
pub mod queries;

use crate::error::{NxvError, Result};
use queries::PackageVersion;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;
use tracing::instrument;

/// Default timeout for SQLite busy handler (in seconds).
/// When the database is locked, SQLite will retry for this duration before returning SQLITE_BUSY.
const DEFAULT_BUSY_TIMEOUT_SECS: u64 = 5;

/// Current schema version.
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
const SCHEMA_VERSION: u32 = 7;

/// Supported systems for store paths.
pub const STORE_PATH_SYSTEMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "x86_64-darwin",
    "aarch64-darwin",
];

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

        // Check schema version
        let version_str = self.get_meta("schema_version")?;
        let version_str = version_str.as_deref().unwrap_or("0");
        let db_version: u32 = version_str.parse().map_err(|_| {
            NxvError::CorruptIndex(format!(
                "invalid schema_version '{}': expected integer",
                version_str
            ))
        })?;

        if db_version > SCHEMA_VERSION {
            return Err(NxvError::IndexTooNew {
                index_version: db_version,
                supported_version: SCHEMA_VERSION,
            });
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

            -- Indexes for common query patterns
            CREATE INDEX IF NOT EXISTS idx_packages_name ON package_versions(name);
            CREATE INDEX IF NOT EXISTS idx_packages_name_version ON package_versions(name, version, first_commit_date);
            CREATE INDEX IF NOT EXISTS idx_packages_attr ON package_versions(attribute_path);
            CREATE INDEX IF NOT EXISTS idx_packages_first_date ON package_versions(first_commit_date DESC);
            CREATE INDEX IF NOT EXISTS idx_packages_last_date ON package_versions(last_commit_date DESC);

            -- Covering indexes for optimized search queries (added in v7)
            -- These allow ORDER BY to use the index directly without a separate sort
            CREATE INDEX IF NOT EXISTS idx_attr_date_covering ON package_versions(
                attribute_path, last_commit_date DESC, name, version
            );
            CREATE INDEX IF NOT EXISTS idx_name_attr_date_covering ON package_versions(
                name, attribute_path, last_commit_date DESC
            );

            -- Checkpoint table for persisting open ranges across restarts
            CREATE TABLE IF NOT EXISTS checkpoint_open_ranges (
                key TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                first_commit_hash TEXT NOT NULL,
                first_commit_date INTEGER NOT NULL,
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
                store_path_aarch64_darwin TEXT
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
            // Migration v3 -> v4: Add checkpoint_open_ranges table
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS checkpoint_open_ranges (
                    key TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    first_commit_hash TEXT NOT NULL,
                    first_commit_date INTEGER NOT NULL,
                    attribute_path TEXT NOT NULL,
                    description TEXT,
                    license TEXT,
                    homepage TEXT,
                    maintainers TEXT,
                    platforms TEXT,
                    source_path TEXT,
                    known_vulnerabilities TEXT,
                    store_path TEXT
                );
                "#,
            )?;
        }

        if current_version < 5 {
            // Migration v4 -> v5: Add store_path column for fetchClosure support
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

            // Also add to checkpoint_open_ranges if not present
            let checkpoint_has_store_path: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('checkpoint_open_ranges') WHERE name='store_path'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !checkpoint_has_store_path {
                self.conn.execute(
                    "ALTER TABLE checkpoint_open_ranges ADD COLUMN store_path TEXT",
                    [],
                )?;
            }
        }

        if current_version < 6 {
            // Migration v5 -> v6: Add per-architecture store_path columns
            // Add columns to package_versions
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
            let has_old_store_path: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('package_versions') WHERE name='store_path'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if has_old_store_path {
                self.conn.execute(
                    "UPDATE package_versions SET store_path_x86_64_linux = store_path WHERE store_path IS NOT NULL AND store_path_x86_64_linux IS NULL",
                    [],
                )?;
            }

            // Add columns to checkpoint_open_ranges
            for system in STORE_PATH_SYSTEMS {
                let col_name = format!("store_path_{}", system.replace('-', "_"));
                let has_col: bool = self
                    .conn
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) > 0 FROM pragma_table_info('checkpoint_open_ranges') WHERE name='{}'",
                            col_name
                        ),
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(false);

                if !has_col {
                    self.conn.execute(
                        &format!(
                            "ALTER TABLE checkpoint_open_ranges ADD COLUMN {} TEXT",
                            col_name
                        ),
                        [],
                    )?;
                }
            }
        }

        if current_version < 7 {
            // Migration v6 -> v7: Add covering indexes for optimized search queries
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
    /// let inserted = db.insert_package_ranges_batch(&packages).unwrap();
    /// assert!(inserted <= packages.len());
    /// # }
    /// ```
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    #[instrument(skip(self, packages), fields(batch_size = packages.len()))]
    pub fn insert_package_ranges_batch(&mut self, packages: &[PackageVersion]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;

        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT OR IGNORE INTO package_versions
                    (name, version, first_commit_hash, first_commit_date,
                     last_commit_hash, last_commit_date, attribute_path,
                     description, license, homepage, maintainers, platforms, source_path,
                     known_vulnerabilities,
                     store_path_x86_64_linux, store_path_aarch64_linux,
                     store_path_x86_64_darwin, store_path_aarch64_darwin)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )?;

            for pkg in packages {
                let changes = stmt.execute(rusqlite::params![
                    pkg.name,
                    pkg.version,
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
                inserted += changes;
            }
        }

        tx.commit()?;
        Ok(inserted)
    }

    /// Upserts multiple package version records for slim indexing mode.
    ///
    /// For each package, this either:
    /// - Inserts a new row if (attribute_path, version) doesn't exist
    /// - Updates the existing row to extend the range if it does exist
    ///
    /// This method is used in slim mode where we want one row per (attr, version)
    /// instead of tracking full version ranges.
    ///
    /// # Returns
    ///
    /// A tuple of (inserted, updated) counts.
    #[cfg(feature = "indexer")]
    #[instrument(skip(self, packages), fields(batch_size = packages.len()))]
    pub fn upsert_package_versions_slim(
        &mut self,
        packages: &[PackageVersion],
    ) -> Result<(usize, usize)> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        let mut updated = 0;

        {
            // First, try to update existing rows (matching on attribute_path + version only)
            let mut update_stmt = tx.prepare_cached(
                r#"
                UPDATE package_versions SET
                    -- Extend last_commit if this commit is newer
                    last_commit_hash = CASE
                        WHEN ?1 > last_commit_date THEN ?2 ELSE last_commit_hash
                    END,
                    last_commit_date = CASE
                        WHEN ?1 > last_commit_date THEN ?1 ELSE last_commit_date
                    END,
                    -- Extend first_commit if this commit is older
                    first_commit_hash = CASE
                        WHEN ?3 < first_commit_date THEN ?4 ELSE first_commit_hash
                    END,
                    first_commit_date = CASE
                        WHEN ?3 < first_commit_date THEN ?3 ELSE first_commit_date
                    END,
                    -- Update metadata (prefer non-null values)
                    description = COALESCE(?5, description),
                    license = COALESCE(?6, license),
                    homepage = COALESCE(?7, homepage),
                    maintainers = COALESCE(?8, maintainers),
                    platforms = COALESCE(?9, platforms),
                    source_path = COALESCE(?10, source_path),
                    known_vulnerabilities = COALESCE(?11, known_vulnerabilities),
                    store_path_x86_64_linux = COALESCE(?12, store_path_x86_64_linux),
                    store_path_aarch64_linux = COALESCE(?13, store_path_aarch64_linux),
                    store_path_x86_64_darwin = COALESCE(?14, store_path_x86_64_darwin),
                    store_path_aarch64_darwin = COALESCE(?15, store_path_aarch64_darwin)
                WHERE attribute_path = ?16 AND version = ?17
                "#,
            )?;

            let mut insert_stmt = tx.prepare_cached(
                r#"
                INSERT INTO package_versions
                    (name, version, first_commit_hash, first_commit_date,
                     last_commit_hash, last_commit_date, attribute_path,
                     description, license, homepage, maintainers, platforms, source_path,
                     known_vulnerabilities,
                     store_path_x86_64_linux, store_path_aarch64_linux,
                     store_path_x86_64_darwin, store_path_aarch64_darwin)
                SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
                WHERE NOT EXISTS (
                    SELECT 1 FROM package_versions
                    WHERE attribute_path = ?7 AND version = ?2
                )
                "#,
            )?;

            for pkg in packages {
                // Try update first
                let update_changes = update_stmt.execute(rusqlite::params![
                    pkg.last_commit_date.timestamp(),      // ?1
                    pkg.last_commit_hash,                  // ?2
                    pkg.first_commit_date.timestamp(),     // ?3
                    pkg.first_commit_hash,                 // ?4
                    pkg.description,                       // ?5
                    pkg.license,                           // ?6
                    pkg.homepage,                          // ?7
                    pkg.maintainers,                       // ?8
                    pkg.platforms,                         // ?9
                    pkg.source_path,                       // ?10
                    pkg.known_vulnerabilities,             // ?11
                    pkg.store_paths.get("x86_64-linux"),   // ?12
                    pkg.store_paths.get("aarch64-linux"),  // ?13
                    pkg.store_paths.get("x86_64-darwin"),  // ?14
                    pkg.store_paths.get("aarch64-darwin"), // ?15
                    pkg.attribute_path,                    // ?16
                    pkg.version,                           // ?17
                ])?;

                if update_changes > 0 {
                    updated += update_changes;
                } else {
                    // No existing row, insert new one
                    let insert_changes = insert_stmt.execute(rusqlite::params![
                        pkg.name,
                        pkg.version,
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
                    inserted += insert_changes;
                }
            }
        }

        tx.commit()?;
        Ok((inserted, updated))
    }

    /// Save checkpoint open ranges to the database.
    ///
    /// This persists the current open ranges so they can be restored on resume.
    /// Called during periodic checkpoints and graceful shutdown.
    #[cfg(feature = "indexer")]
    #[instrument(skip(self, ranges), fields(range_count = ranges.len()))]
    pub fn save_checkpoint_ranges(
        &mut self,
        ranges: &std::collections::HashMap<String, crate::index::CheckpointRange>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Clear existing checkpoint ranges
        tx.execute("DELETE FROM checkpoint_open_ranges", [])?;

        // Insert all current ranges
        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO checkpoint_open_ranges
                    (key, name, version, first_commit_hash, first_commit_date,
                     attribute_path, description, license, homepage, maintainers,
                     platforms, source_path, known_vulnerabilities,
                     store_path_x86_64_linux, store_path_aarch64_linux,
                     store_path_x86_64_darwin, store_path_aarch64_darwin)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )?;

            for (key, range) in ranges {
                stmt.execute(rusqlite::params![
                    key,
                    range.name,
                    range.version,
                    range.first_commit_hash,
                    range.first_commit_date.timestamp(),
                    range.attribute_path,
                    range.description,
                    range.license,
                    range.homepage,
                    range.maintainers,
                    range.platforms,
                    range.source_path,
                    range.known_vulnerabilities,
                    range.store_paths.get("x86_64-linux"),
                    range.store_paths.get("aarch64-linux"),
                    range.store_paths.get("x86_64-darwin"),
                    range.store_paths.get("aarch64-darwin"),
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Load checkpoint open ranges from the database.
    ///
    /// Returns the previously saved open ranges for resuming indexing.
    #[cfg(feature = "indexer")]
    #[instrument(skip(self))]
    pub fn load_checkpoint_ranges(
        &self,
    ) -> Result<std::collections::HashMap<String, crate::index::CheckpointRange>> {
        use chrono::{TimeZone, Utc};
        use std::collections::HashMap;

        let mut ranges = HashMap::new();

        let mut stmt = self.conn.prepare(
            r#"
            SELECT key, name, version, first_commit_hash, first_commit_date,
                   attribute_path, description, license, homepage, maintainers,
                   platforms, source_path, known_vulnerabilities,
                   store_path_x86_64_linux, store_path_aarch64_linux,
                   store_path_x86_64_darwin, store_path_aarch64_darwin
            FROM checkpoint_open_ranges
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let timestamp: i64 = row.get(4)?;

            let mut store_paths = HashMap::new();
            if let Some(path) = row.get::<_, Option<String>>(13)? {
                store_paths.insert("x86_64-linux".to_string(), path);
            }
            if let Some(path) = row.get::<_, Option<String>>(14)? {
                store_paths.insert("aarch64-linux".to_string(), path);
            }
            if let Some(path) = row.get::<_, Option<String>>(15)? {
                store_paths.insert("x86_64-darwin".to_string(), path);
            }
            if let Some(path) = row.get::<_, Option<String>>(16)? {
                store_paths.insert("aarch64-darwin".to_string(), path);
            }

            Ok((
                key,
                crate::index::CheckpointRange {
                    name: row.get(1)?,
                    version: row.get(2)?,
                    first_commit_hash: row.get(3)?,
                    first_commit_date: Utc.timestamp_opt(timestamp, 0).single().unwrap_or_default(),
                    attribute_path: row.get(5)?,
                    description: row.get(6)?,
                    license: row.get(7)?,
                    homepage: row.get(8)?,
                    maintainers: row.get(9)?,
                    platforms: row.get(10)?,
                    source_path: row.get(11)?,
                    known_vulnerabilities: row.get(12)?,
                    store_paths,
                },
            ))
        })?;

        for row in rows {
            let (key, range) = row?;
            ranges.insert(key, range);
        }

        Ok(ranges)
    }

    /// Clear checkpoint open ranges from the database.
    ///
    /// Called when indexing completes successfully.
    #[cfg(feature = "indexer")]
    #[instrument(skip(self))]
    pub fn clear_checkpoint_ranges(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM checkpoint_open_ranges", [])?;
        Ok(())
    }

    /// Update the last_commit fields for an existing package version range.
    ///
    /// Used during incremental indexing to extend a range's end point.
    #[allow(dead_code)]
    pub fn update_package_range_end(
        &self,
        attr_path: &str,
        version: &str,
        first_commit_hash: &str,
        last_commit_hash: &str,
        last_commit_date: i64,
        description: Option<&str>,
    ) -> Result<bool> {
        let changes = self.conn.execute(
            r#"
            UPDATE package_versions
            SET last_commit_hash = ?,
                last_commit_date = ?,
                description = COALESCE(?, description)
            WHERE attribute_path = ?
              AND version = ?
              AND first_commit_hash = ?
            "#,
            rusqlite::params![
                last_commit_hash,
                last_commit_date,
                description,
                attr_path,
                version,
                first_commit_hash
            ],
        )?;
        Ok(changes > 0)
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
    fn test_batch_insert() {
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

        let inserted = db.insert_package_ranges_batch(&packages).unwrap();
        assert_eq!(inserted, 2);

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
    fn test_batch_insert_duplicate_handling() {
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let mut db = Database::open(&db_path).unwrap();

        let now = Utc::now();
        let pkg = PackageVersion {
            id: 0,
            name: "python".to_string(),
            version: "3.11.0".to_string(),
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
        };

        // First insert should succeed
        let inserted1 = db
            .insert_package_ranges_batch(std::slice::from_ref(&pkg))
            .unwrap();
        assert_eq!(inserted1, 1);

        // Second insert of same package should be ignored (no error)
        let inserted2 = db
            .insert_package_ranges_batch(std::slice::from_ref(&pkg))
            .unwrap();
        assert_eq!(inserted2, 0);

        // Verify only one row exists
        let count: i32 = db
            .conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
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

        db.insert_package_ranges_batch(&[pkg]).unwrap();

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
    fn test_batch_insert_10k_performance() {
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
        let inserted = db.insert_package_ranges_batch(&packages).unwrap();
        let duration = start.elapsed();

        assert_eq!(inserted, 10_000);
        assert!(
            duration.as_secs() < 5,
            "Batch insert took {:?}, expected < 5 seconds",
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
}

//! Database module for nxv index storage.

pub mod import;
pub mod queries;

use crate::error::{NxvError, Result};
use queries::PackageVersion;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Current schema version.
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
const SCHEMA_VERSION: u32 = 1;

/// Database connection wrapper.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create a database at the given path.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrent performance and durability
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA wal_autocheckpoint = 1000;
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
    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Open a database in read-only mode.
    pub fn open_readonly<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(NxvError::NoIndex);
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
    }

    /// Initialize the database schema.
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
                UNIQUE(attribute_path, version, first_commit_hash)
            );

            -- Indexes for common query patterns
            CREATE INDEX IF NOT EXISTS idx_packages_name ON package_versions(name);
            CREATE INDEX IF NOT EXISTS idx_packages_name_version ON package_versions(name, version, first_commit_date);
            CREATE INDEX IF NOT EXISTS idx_packages_attr ON package_versions(attribute_path);
            CREATE INDEX IF NOT EXISTS idx_packages_first_date ON package_versions(first_commit_date DESC);
            CREATE INDEX IF NOT EXISTS idx_packages_last_date ON package_versions(last_commit_date DESC);
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

        // Set schema version if not already set
        let version = self.get_meta("schema_version")?;
        if version.is_none() {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        }

        Ok(())
    }

    /// Migrate the database schema if needed.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    fn migrate_if_needed(&self) -> Result<()> {
        let version_str = self.get_meta("schema_version")?;
        let current_version: u32 = version_str.as_deref().unwrap_or("0").parse().unwrap_or(0);

        if current_version < SCHEMA_VERSION {
            // Future migrations would go here
            // For now, just update the version
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

    /// Batch insert package version ranges.
    ///
    /// Uses a transaction for performance and atomicity.
    /// Duplicate entries (same attr_path+version+first_commit_hash) are ignored.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn insert_package_ranges_batch(&mut self, packages: &[PackageVersion]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;

        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT OR IGNORE INTO package_versions
                    (name, version, first_commit_hash, first_commit_date,
                     last_commit_hash, last_commit_date, attribute_path,
                     description, license, homepage, maintainers, platforms)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                ])?;
                inserted += changes;
            }
        }

        tx.commit()?;
        Ok(inserted)
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
        assert_eq!(version, Some("1".to_string()));
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
        };

        // First insert should succeed
        let inserted1 = db.insert_package_ranges_batch(&[pkg.clone()]).unwrap();
        assert_eq!(inserted1, 1);

        // Second insert of same package should be ignored (no error)
        let inserted2 = db.insert_package_ranges_batch(&[pkg]).unwrap();
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

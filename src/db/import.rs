//! Delta pack import logic.

#![allow(dead_code)]

use crate::error::Result;
use crate::remote::download::decompress_zstd;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

/// Import a delta pack into the database.
///
/// Delta packs are zstd-compressed SQL files containing:
/// - `INSERT OR REPLACE INTO package_versions ...` statements for new/updated packages
/// - `INSERT OR REPLACE INTO meta ...` to update last_indexed_commit
///
/// The import is wrapped in a transaction for atomicity.
pub fn import_delta_pack<P: AsRef<Path>>(conn: &Connection, path: P) -> Result<()> {
    let path = path.as_ref();

    // Create a temp directory for decompression
    let temp_dir = tempdir()?;
    let sql_path = temp_dir.path().join("delta.sql");

    // Decompress the zstd file
    decompress_zstd(path, &sql_path, false)?;

    // Read the SQL content
    let sql_content = fs::read_to_string(&sql_path)?;

    // Execute the SQL statements
    // The delta pack already contains BEGIN TRANSACTION and COMMIT
    // But we should handle the case where it might not
    if sql_content.contains("BEGIN TRANSACTION") {
        // Execute as-is since it has its own transaction
        conn.execute_batch(&sql_content)?;
    } else {
        // Wrap in transaction for safety
        conn.execute_batch(&format!("BEGIN TRANSACTION;\n{}\nCOMMIT;", sql_content))?;
    }

    Ok(())
}

/// Import a delta pack from raw SQL content (uncompressed).
///
/// This is useful for testing and when the delta is already decompressed.
pub fn import_delta_sql(conn: &Connection, sql_content: &str) -> Result<()> {
    // Execute the SQL statements
    if sql_content.contains("BEGIN TRANSACTION") {
        conn.execute_batch(sql_content)?;
    } else {
        conn.execute_batch(&format!("BEGIN TRANSACTION;\n{}\nCOMMIT;", sql_content))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::download::compress_zstd;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn create_test_db(conn: &Connection) {
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

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'old_commit_abc');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python', '3.10.0', 'aaa111', 1600000000, 'bbb222', 1600100000, 'python310', 'Python 3.10');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn test_import_delta_sql_adds_new_rows() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_db(&conn);

        // Create delta SQL that adds a new package
        let delta_sql = r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('python', '3.11.0', 'ccc333', 1601000000, 'ddd444', 1601100000, 'python311', 'Python 3.11');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'new_commit_xyz');
COMMIT;
        "#;

        import_delta_sql(&conn, delta_sql).unwrap();

        // Verify new package was added
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM package_versions WHERE name = 'python'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Verify the new package exists
        let version: String = conn
            .query_row(
                "SELECT version FROM package_versions WHERE attribute_path = 'python311'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "3.11.0");

        // Verify meta was updated
        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(commit, "new_commit_xyz");
    }

    #[test]
    fn test_import_delta_sql_updates_existing_row() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_db(&conn);

        // Create delta SQL that updates an existing package (same attr_path+version+first_commit)
        let delta_sql = r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('python', '3.10.0', 'aaa111', 1600000000, 'eee555', 1602000000, 'python310', 'Python 3.10 - updated');
COMMIT;
        "#;

        import_delta_sql(&conn, delta_sql).unwrap();

        // Verify only one python310 exists (not a duplicate)
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM package_versions WHERE attribute_path = 'python310'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify the description was updated
        let desc: String = conn
            .query_row(
                "SELECT description FROM package_versions WHERE attribute_path = 'python310'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(desc, "Python 3.10 - updated");

        // Verify last_commit was updated
        let last_hash: String = conn
            .query_row(
                "SELECT last_commit_hash FROM package_versions WHERE attribute_path = 'python310'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_hash, "eee555");
    }

    #[test]
    fn test_import_delta_sql_without_transaction_wrapper() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_db(&conn);

        // Delta SQL without explicit transaction (import should wrap it)
        let delta_sql = r#"
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('nodejs', '20.0.0', 'fff666', 1603000000, 'ggg777', 1603100000, 'nodejs_20', 'Node.js 20');
        "#;

        import_delta_sql(&conn, delta_sql).unwrap();

        // Verify package was added
        let version: String = conn
            .query_row(
                "SELECT version FROM package_versions WHERE name = 'nodejs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "20.0.0");
    }

    #[test]
    fn test_import_delta_pack_compressed() {
        let dir = tempdir().unwrap();
        let sql_path = dir.path().join("delta.sql");
        let compressed_path = dir.path().join("delta.sql.zst");

        // Create delta SQL
        let delta_sql = r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('rust', '1.70.0', 'hhh888', 1604000000, 'iii999', 1604100000, 'rustc', 'Rust compiler');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'delta_commit_123');
COMMIT;
        "#;

        // Write and compress
        fs::write(&sql_path, delta_sql).unwrap();
        compress_zstd(&sql_path, &compressed_path, 3).unwrap();

        // Create test database
        let conn = Connection::open_in_memory().unwrap();
        create_test_db(&conn);

        // Import the compressed delta
        import_delta_pack(&conn, &compressed_path).unwrap();

        // Verify package was added
        let version: String = conn
            .query_row(
                "SELECT version FROM package_versions WHERE name = 'rust'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "1.70.0");

        // Verify meta was updated
        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(commit, "delta_commit_123");
    }

    #[test]
    fn test_import_delta_updates_last_indexed_commit() {
        let conn = Connection::open_in_memory().unwrap();
        create_test_db(&conn);

        // Check initial commit
        let initial_commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(initial_commit, "old_commit_abc");

        // Import delta that updates commit
        let delta_sql = r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'updated_commit_xyz');
COMMIT;
        "#;

        import_delta_sql(&conn, delta_sql).unwrap();

        // Verify commit was updated
        let new_commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_commit, "updated_commit_xyz");
    }
}

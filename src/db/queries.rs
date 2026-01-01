//! Database query operations for package searches.

use crate::error::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Represents a package version entry from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageVersion {
    pub id: i64,
    pub name: String,
    pub version: String,
    pub first_commit_hash: String,
    pub first_commit_date: DateTime<Utc>,
    pub last_commit_hash: String,
    pub last_commit_date: DateTime<Utc>,
    pub attribute_path: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub maintainers: Option<String>,
    pub platforms: Option<String>,
    /// Source file path relative to nixpkgs root
    pub source_path: Option<String>,
}

impl PackageVersion {
    /// Parse a row from the database.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let first_commit_ts: i64 = row.get("first_commit_date")?;
        let last_commit_ts: i64 = row.get("last_commit_date")?;

        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            version: row.get("version")?,
            first_commit_hash: row.get("first_commit_hash")?,
            first_commit_date: Utc.timestamp_opt(first_commit_ts, 0).unwrap(),
            last_commit_hash: row.get("last_commit_hash")?,
            last_commit_date: Utc.timestamp_opt(last_commit_ts, 0).unwrap(),
            attribute_path: row.get("attribute_path")?,
            description: row.get("description")?,
            license: row.get("license")?,
            homepage: row.get("homepage")?,
            maintainers: row.get("maintainers")?,
            platforms: row.get("platforms")?,
            source_path: row.get("source_path").ok().flatten(),
        })
    }

    /// Get the short (7-char) first commit hash.
    pub fn first_commit_short(&self) -> &str {
        &self.first_commit_hash[..7.min(self.first_commit_hash.len())]
    }

    /// Get the short (7-char) last commit hash.
    pub fn last_commit_short(&self) -> &str {
        &self.last_commit_hash[..7.min(self.last_commit_hash.len())]
    }
}

/// Index statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    pub total_ranges: i64,
    pub unique_names: i64,
    pub unique_versions: i64,
    pub oldest_commit_date: Option<DateTime<Utc>>,
    pub newest_commit_date: Option<DateTime<Utc>>,
}

/// Search for packages by name.
pub fn search_by_name(
    conn: &rusqlite::Connection,
    name: &str,
    exact: bool,
) -> Result<Vec<PackageVersion>> {
    let sql = if exact {
        "SELECT * FROM package_versions WHERE name = ? ORDER BY last_commit_date DESC"
    } else {
        "SELECT * FROM package_versions WHERE name LIKE ? ORDER BY last_commit_date DESC"
    };

    let pattern = if exact {
        name.to_string()
    } else {
        format!("{}%", name)
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([&pattern], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Search for packages by attribute path.
pub fn search_by_attr(conn: &rusqlite::Connection, attr_path: &str) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path LIKE ? ORDER BY last_commit_date DESC",
    )?;
    let pattern = format!("{}%", attr_path);
    let rows = stmt.query_map([&pattern], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Search for packages by name and version.
pub fn search_by_name_version(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
) -> Result<Vec<PackageVersion>> {
    // Search by attribute_path (package) and version prefix
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path LIKE ? AND version LIKE ? ORDER BY first_commit_date DESC",
    )?;
    let package_pattern = format!("{}%", package);
    let version_pattern = format!("{}%", version);
    let rows = stmt.query_map(
        [&package_pattern, &version_pattern],
        PackageVersion::from_row,
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the first occurrence of a package version.
pub fn get_first_occurrence(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
) -> Result<Option<PackageVersion>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path = ? AND version = ? ORDER BY first_commit_date ASC LIMIT 1",
    )?;

    let result = stmt.query_row([package, version], PackageVersion::from_row);
    match result {
        Ok(pkg) => Ok(Some(pkg)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get the last occurrence of a package version.
pub fn get_last_occurrence(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
) -> Result<Option<PackageVersion>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path = ? AND version = ? ORDER BY last_commit_date DESC LIMIT 1",
    )?;

    let result = stmt.query_row([package, version], PackageVersion::from_row);
    match result {
        Ok(pkg) => Ok(Some(pkg)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Version history entry: (version, first_seen, last_seen).
pub type VersionHistoryEntry = (String, DateTime<Utc>, DateTime<Utc>);

/// Get version history for a package.
pub fn get_version_history(
    conn: &rusqlite::Connection,
    package: &str,
) -> Result<Vec<VersionHistoryEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT version, MIN(first_commit_date) as first_seen, MAX(last_commit_date) as last_seen
        FROM package_versions
        WHERE attribute_path = ?
        GROUP BY version
        ORDER BY first_seen DESC
        "#,
    )?;

    let rows = stmt.query_map([package], |row| {
        let version: String = row.get(0)?;
        let first_ts: i64 = row.get(1)?;
        let last_ts: i64 = row.get(2)?;
        Ok((
            version,
            Utc.timestamp_opt(first_ts, 0).unwrap(),
            Utc.timestamp_opt(last_ts, 0).unwrap(),
        ))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get index statistics.
pub fn get_stats(conn: &rusqlite::Connection) -> Result<IndexStats> {
    let total_ranges: i64 = conn.query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
        row.get(0)
    })?;

    let unique_names: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT name) FROM package_versions",
        [],
        |row| row.get(0),
    )?;

    let unique_versions: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT version) FROM package_versions",
        [],
        |row| row.get(0),
    )?;

    let oldest: Option<i64> = conn
        .query_row(
            "SELECT MIN(first_commit_date) FROM package_versions",
            [],
            |row| row.get(0),
        )
        .ok();

    let newest: Option<i64> = conn
        .query_row(
            "SELECT MAX(last_commit_date) FROM package_versions",
            [],
            |row| row.get(0),
        )
        .ok();

    Ok(IndexStats {
        total_ranges,
        unique_names,
        unique_versions,
        oldest_commit_date: oldest.map(|ts| Utc.timestamp_opt(ts, 0).unwrap()),
        newest_commit_date: newest.map(|ts| Utc.timestamp_opt(ts, 0).unwrap()),
    })
}

/// Search using FTS5 for description text.
pub fn search_by_description(
    conn: &rusqlite::Connection,
    query: &str,
) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT pv.* FROM package_versions pv
        INNER JOIN package_versions_fts fts ON pv.id = fts.rowid
        WHERE package_versions_fts MATCH ?
        ORDER BY pv.last_commit_date DESC
        "#,
    )?;

    let rows = stmt.query_map([query], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get all unique package attribute paths in the database.
///
/// This is used to build the bloom filter for fast "not found" lookups.
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
pub fn get_all_unique_attrs(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT attribute_path FROM package_versions ORDER BY attribute_path")?;
    let rows = stmt.query_map([], |row| row.get(0))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    fn create_test_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Insert test data - attribute_path is the "Package" that users install with
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python-3.11.0', '3.11.0', 'abc1234567890', 1700000000, 'def1234567890', 1700100000, 'python', 'Python interpreter'),
                ('python-3.12.0', '3.12.0', 'ghi1234567890', 1701000000, 'jkl1234567890', 1701100000, 'python', 'Python interpreter'),
                ('python2-2.7.18', '2.7.18', 'mno1234567890', 1600000000, 'pqr1234567890', 1600100000, 'python2', 'Python 2 interpreter'),
                ('nodejs-20.0.0', '20.0.0', 'stu1234567890', 1702000000, 'vwx1234567890', 1702100000, 'nodejs', 'Node.js runtime')
            "#,
                [],
            )
            .unwrap();

        (dir, db)
    }

    #[test]
    fn test_search_by_name_exact() {
        let (_dir, db) = create_test_db();
        // search_by_name still searches the name field
        let results = search_by_name(db.connection(), "python-3.11.0", true).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_by_name_prefix() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "python", false).unwrap();
        assert_eq!(results.len(), 3); // python-3.11.0, python-3.12.0, python2-2.7.18
    }

    #[test]
    fn test_search_by_name_version() {
        let (_dir, db) = create_test_db();
        // Now searches by attribute_path (package) and version
        let results = search_by_name_version(db.connection(), "python", "3.11").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].version, "3.11.0");
    }

    #[test]
    fn test_get_version_history() {
        let (_dir, db) = create_test_db();
        // Now uses attribute_path
        let history = get_version_history(db.connection(), "python").unwrap();
        assert_eq!(history.len(), 2);
        // Should be ordered by first_seen DESC, so 3.12.0 first
        assert_eq!(history[0].0, "3.12.0");
        assert_eq!(history[1].0, "3.11.0");
    }

    #[test]
    fn test_get_stats() {
        let (_dir, db) = create_test_db();
        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 4);
        assert_eq!(stats.unique_names, 4); // python-3.11.0, python-3.12.0, python2-2.7.18, nodejs-20.0.0
    }
}

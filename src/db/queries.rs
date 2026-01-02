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
    /// Known security vulnerabilities or EOL notices (JSON array)
    pub known_vulnerabilities: Option<String>,
}

impl PackageVersion {
    /// Constructs a PackageVersion from a database row.
    ///
    /// The row must contain the columns:
    /// `id`, `name`, `version`, `first_commit_hash`, `first_commit_date` (i64 seconds since epoch),
    /// `last_commit_hash`, `last_commit_date` (i64 seconds since epoch), `attribute_path`,
    /// `description`, `license`, `homepage`, `maintainers`, `platforms`, and optionally `source_path`.
    ///
    /// # Examples
    ///
    /// ```
    /// // `row` is a rusqlite::Row obtained from a query that selects the required columns.
    /// let pv = PackageVersion::from_row(&row).unwrap();
    /// assert_eq!(pv.name, row.get::<_, String>("name").unwrap());
    /// ```
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
            known_vulnerabilities: row.get("known_vulnerabilities").ok().flatten(),
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

    /// Check if the last commit predates flake.nix in nixpkgs.
    /// flake.nix was added on 2020-02-10 via PR #68897.
    pub fn predates_flakes(&self) -> bool {
        // 2020-02-10 00:00:00 UTC
        const FLAKE_EPOCH: i64 = 1581292800;
        self.last_commit_date.timestamp() < FLAKE_EPOCH
    }

    /// Generate the appropriate nix shell command based on commit date and security status.
    pub fn nix_shell_cmd(&self) -> String {
        let insecure_prefix = if self.is_insecure() {
            "NIXPKGS_ALLOW_INSECURE=1 "
        } else {
            ""
        };

        if self.predates_flakes() {
            format!(
                "{}nix-shell -p '(import (fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}'",
                insecure_prefix,
                self.last_commit_short(),
                self.attribute_path
            )
        } else {
            let impure_flag = if self.is_insecure() { " --impure" } else { "" };
            format!(
                "{}nix shell{} nixpkgs/{}#{}",
                insecure_prefix,
                impure_flag,
                self.last_commit_short(),
                self.attribute_path
            )
        }
    }

    /// Generate the appropriate nix run command based on commit date and security status.
    pub fn nix_run_cmd(&self) -> String {
        let insecure_prefix = if self.is_insecure() {
            "NIXPKGS_ALLOW_INSECURE=1 "
        } else {
            ""
        };

        if self.predates_flakes() {
            format!(
                "{}nix-shell -p '(import (fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}' --run {}",
                insecure_prefix,
                self.last_commit_short(),
                self.attribute_path,
                self.attribute_path
            )
        } else {
            let impure_flag = if self.is_insecure() { " --impure" } else { "" };
            format!(
                "{}nix run{} nixpkgs/{}#{}",
                insecure_prefix,
                impure_flag,
                self.last_commit_short(),
                self.attribute_path
            )
        }
    }

    /// Check if the package has known vulnerabilities.
    pub fn is_insecure(&self) -> bool {
        self.known_vulnerabilities
            .as_ref()
            .is_some_and(|v| !v.is_empty() && v != "[]" && v != "null")
    }

    /// Get parsed known vulnerabilities as a vector of strings.
    pub fn vulnerabilities(&self) -> Vec<String> {
        self.known_vulnerabilities
            .as_ref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or_default()
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

/// Finds package versions whose attribute path and version start with the given prefixes.
///
/// The `package` argument is matched as a prefix against the `attribute_path` column
/// (i.e., `attribute_path LIKE 'package%'`) and the `version` argument is matched as a
/// prefix against the `version` column. Results are ordered by `first_commit_date` descending.
///
/// # Returns
///
/// A vector of `PackageVersion` entries that match the provided package and version prefixes.
///
/// # Examples
///
/// ```
/// // Assuming `conn` is a valid rusqlite::Connection populated with package_versions...
/// let matches = search_by_name_version(&conn, "python", "3.11").unwrap();
/// for pv in matches {
///     assert!(pv.attribute_path.starts_with("python"));
///     assert!(pv.version.starts_with("3.11"));
/// }
/// ```
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

/// Locate the earliest recorded entry for a package version.
///
/// `package` is the package's attribute path to match; `version` is the version string to match exactly.
///
/// # Returns
///
/// `Some(PackageVersion)` containing the earliest (by first_commit_date) matching record, `None` if no match.
///
/// # Examples
///
/// ```
/// // Given a connection `conn` and a package attribute path and version:
/// let first = get_first_occurrence(&conn, "python", "3.11.0")?;
/// if let Some(pkg) = first {
///     println!("{}", pkg.version);
/// }
/// ```
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

/// Retrieve the most recent package version entry that matches the given attribute path and version.
///
/// # Parameters
///
/// - `package`: Package attribute path to match (the `attribute_path` column).
/// - `version`: Version string to match (the `version` column).
///
/// # Returns
///
/// `Some(PackageVersion)` containing the entry with the latest `last_commit_date` if a matching row exists, `None` if no match is found.
///
/// # Examples
///
/// ```
/// // Assume `conn` is a valid rusqlite::Connection with the `package_versions` table populated.
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// let result = get_last_occurrence(&conn, "python", "3.11.0").unwrap();
/// match result {
///     Some(pkg) => println!("Found package: {} {}", pkg.name, pkg.version),
///     None => println!("No matching package found"),
/// }
/// ```
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

/// Version history entry: (version, first_seen, last_seen, is_insecure).
pub type VersionHistoryEntry = (String, DateTime<Utc>, DateTime<Utc>, bool);

/// Retrieves the version history for a package attribute path.
///
/// Returns each distinct version for the given `package` (matched against `attribute_path`)
/// along with the earliest `first_commit_date`, the latest `last_commit_date` for that version,
/// and a flag indicating if any record for that version has known vulnerabilities.
/// Results are ordered by `first_seen` (earliest first_commit_date) descending.
///
/// # Arguments
///
/// * `package` - The package attribute path to filter versions by.
///
/// # Returns
///
/// A `Vec<VersionHistoryEntry>` where each entry is `(version, first_seen, last_seen, is_insecure)`,
/// and `first_seen` / `last_seen` are `DateTime<Utc>` values.
///
/// # Examples
///
/// ```
/// // assumes `conn` is an open rusqlite::Connection
/// let history = get_version_history(&conn, "python").unwrap();
/// for (version, first_seen, last_seen, is_insecure) in history {
///     println!("{}: {} - {} (insecure: {})", version, first_seen, last_seen, is_insecure);
/// }
/// ```
pub fn get_version_history(
    conn: &rusqlite::Connection,
    package: &str,
) -> Result<Vec<VersionHistoryEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT version,
               MIN(first_commit_date) as first_seen,
               MAX(last_commit_date) as last_seen,
               MAX(CASE WHEN known_vulnerabilities IS NOT NULL
                        AND known_vulnerabilities != ''
                        AND known_vulnerabilities != '[]'
                        AND known_vulnerabilities != 'null'
                   THEN 1 ELSE 0 END) as is_insecure
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
        let is_insecure: i64 = row.get(3)?;
        Ok((
            version,
            Utc.timestamp_opt(first_ts, 0).unwrap(),
            Utc.timestamp_opt(last_ts, 0).unwrap(),
            is_insecure != 0,
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

/// Search package versions by description text using SQLite FTS5.
///
/// Performs a full-text search of package descriptions and returns matching
/// package version records ordered by `last_commit_date` descending.
/// The `query` parameter is interpreted using FTS5 `MATCH` syntax.
///
/// # Returns
///
/// A `Vec<PackageVersion>` containing matching entries ordered by last commit date.
///
/// # Examples
///
/// ```
/// // `conn` is a valid rusqlite::Connection
/// let matches = search_by_description(&conn, "python");
/// assert!(matches.is_ok());
/// let results = matches.unwrap();
/// ```
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

/// Return all distinct package attribute paths from the database, ordered by attribute_path.
///
/// The results are suitable for building membership structures (e.g., a bloom filter) used to
/// quickly determine absent entries.
///
/// # Examples
///
/// ```
/// use rusqlite::Connection;
/// // create an in-memory DB and a minimal table for demonstration
/// let conn = Connection::open_in_memory().unwrap();
/// conn.execute_batch("CREATE TABLE package_versions (attribute_path TEXT);
///                    INSERT INTO package_versions (attribute_path) VALUES ('pkg::a'), ('pkg::b'), ('pkg::a');").unwrap();
///
/// let attrs = crate::db::queries::get_all_unique_attrs(&conn).unwrap();
/// assert_eq!(attrs, vec!["pkg::a".to_string(), "pkg::b".to_string()]);
/// ```
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

    /// Creates a temporary SQLite database pre-populated with sample package_versions rows for use in tests.
    ///
    /// The returned TempDir owns the temporary file location and should be kept alive while the Database is used.
    /// The database is populated with four sample entries (two python versions, one python2, one nodejs).
    ///
    /// # Examples
    ///
    /// ```
    /// let (_tmp_dir, db) = create_test_db();
    /// // use `db` to run queries against the sample data
    /// ```
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
    fn test_search_by_name_exact_no_match() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "nonexistent-pkg", true).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_name_prefix() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "python", false).unwrap();
        assert_eq!(results.len(), 3); // python-3.11.0, python-3.12.0, python2-2.7.18
    }

    #[test]
    fn test_search_by_name_prefix_no_match() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "zzz", false).unwrap();
        assert!(results.is_empty());
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
    fn test_search_by_name_version_no_match() {
        let (_dir, db) = create_test_db();
        let results = search_by_name_version(db.connection(), "python", "99.99").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_attr() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr(db.connection(), "python").unwrap();
        // Should match "python" and "python2" attribute paths
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_by_attr_exact_match() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr(db.connection(), "nodejs").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "nodejs");
    }

    #[test]
    fn test_get_first_occurrence() {
        let (_dir, db) = create_test_db();
        let result = get_first_occurrence(db.connection(), "python", "3.11.0").unwrap();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.version, "3.11.0");
        assert_eq!(pkg.first_commit_hash, "abc1234567890");
    }

    #[test]
    fn test_get_first_occurrence_not_found() {
        let (_dir, db) = create_test_db();
        let result = get_first_occurrence(db.connection(), "nonexistent", "1.0.0").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_last_occurrence() {
        let (_dir, db) = create_test_db();
        let result = get_last_occurrence(db.connection(), "python", "3.11.0").unwrap();
        assert!(result.is_some());
        let pkg = result.unwrap();
        assert_eq!(pkg.version, "3.11.0");
        assert_eq!(pkg.last_commit_hash, "def1234567890");
    }

    #[test]
    fn test_get_last_occurrence_not_found() {
        let (_dir, db) = create_test_db();
        let result = get_last_occurrence(db.connection(), "nonexistent", "1.0.0").unwrap();
        assert!(result.is_none());
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
    fn test_get_version_history_empty() {
        let (_dir, db) = create_test_db();
        let history = get_version_history(db.connection(), "nonexistent").unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_get_stats() {
        let (_dir, db) = create_test_db();
        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 4);
        assert_eq!(stats.unique_names, 4); // python-3.11.0, python-3.12.0, python2-2.7.18, nodejs-20.0.0
    }

    #[test]
    fn test_get_stats_empty_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("empty.db");
        let db = Database::open(&db_path).unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 0);
        assert_eq!(stats.unique_names, 0);
        assert_eq!(stats.unique_versions, 0);
    }

    #[test]
    fn test_get_all_unique_attrs() {
        let (_dir, db) = create_test_db();
        let attrs = get_all_unique_attrs(db.connection()).unwrap();
        assert_eq!(attrs.len(), 3); // nodejs, python, python2
        assert!(attrs.contains(&"python".to_string()));
        assert!(attrs.contains(&"python2".to_string()));
        assert!(attrs.contains(&"nodejs".to_string()));
    }

    #[test]
    fn test_get_all_unique_attrs_empty() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("empty.db");
        let db = Database::open(&db_path).unwrap();

        let attrs = get_all_unique_attrs(db.connection()).unwrap();
        assert!(attrs.is_empty());
    }

    #[test]
    fn test_package_version_first_commit_short() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "python-3.11.0", true).unwrap();
        let pkg = &results[0];
        assert_eq!(pkg.first_commit_short(), "abc1234");
    }

    #[test]
    fn test_package_version_last_commit_short() {
        let (_dir, db) = create_test_db();
        let results = search_by_name(db.connection(), "python-3.11.0", true).unwrap();
        let pkg = &results[0];
        assert_eq!(pkg.last_commit_short(), "def1234");
    }

    #[test]
    fn test_package_version_short_commit_with_short_hash() {
        // Test edge case where hash is shorter than 7 chars
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES ('test', '1.0', 'abc', 1700000000, 'xyz', 1700100000, 'test', 'test')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_name(db.connection(), "test", true).unwrap();
        let pkg = &results[0];
        assert_eq!(pkg.first_commit_short(), "abc");
        assert_eq!(pkg.last_commit_short(), "xyz");
    }

    #[test]
    fn test_search_by_description() {
        let (_dir, db) = create_test_db();
        let results = search_by_description(db.connection(), "interpreter").unwrap();
        assert_eq!(results.len(), 3); // python, python2
    }

    #[test]
    fn test_search_by_description_partial() {
        let (_dir, db) = create_test_db();
        let results = search_by_description(db.connection(), "runtime").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "nodejs-20.0.0");
    }

    #[test]
    fn test_search_by_description_no_match() {
        let (_dir, db) = create_test_db();
        let results =
            search_by_description(db.connection(), "nonexistent description xyz").unwrap();
        assert!(results.is_empty());
    }

    // Helper to create a PackageVersion for testing helper methods
    fn make_test_package(
        last_commit_date: DateTime<Utc>,
        known_vulnerabilities: Option<String>,
    ) -> PackageVersion {
        PackageVersion {
            id: 1,
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            first_commit_hash: "abc1234567890".to_string(),
            first_commit_date: Utc.timestamp_opt(1500000000, 0).unwrap(),
            last_commit_hash: "def1234567890".to_string(),
            last_commit_date,
            attribute_path: "test".to_string(),
            description: Some("Test package".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities,
        }
    }

    #[test]
    fn test_is_insecure_with_vulnerabilities() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some(r#"["CVE-2023-1234", "CVE-2023-5678"]"#.to_string()),
        );
        assert!(pkg.is_insecure());
    }

    #[test]
    fn test_is_insecure_with_empty_array() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("[]".to_string()),
        );
        assert!(!pkg.is_insecure());
    }

    #[test]
    fn test_is_insecure_with_null() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("null".to_string()),
        );
        assert!(!pkg.is_insecure());
    }

    #[test]
    fn test_is_insecure_with_none() {
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        assert!(!pkg.is_insecure());
    }

    #[test]
    fn test_is_insecure_with_empty_string() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("".to_string()),
        );
        assert!(!pkg.is_insecure());
    }

    #[test]
    fn test_vulnerabilities_parsing() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some(r#"["CVE-2023-1234", "CVE-2023-5678"]"#.to_string()),
        );
        let vulns = pkg.vulnerabilities();
        assert_eq!(vulns.len(), 2);
        assert_eq!(vulns[0], "CVE-2023-1234");
        assert_eq!(vulns[1], "CVE-2023-5678");
    }

    #[test]
    fn test_vulnerabilities_empty_array() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("[]".to_string()),
        );
        let vulns = pkg.vulnerabilities();
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_vulnerabilities_none() {
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        let vulns = pkg.vulnerabilities();
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_vulnerabilities_invalid_json() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("invalid json".to_string()),
        );
        let vulns = pkg.vulnerabilities();
        assert!(vulns.is_empty());
    }

    #[test]
    fn test_predates_flakes_old_commit() {
        // 2019-01-01 - before flakes (2020-02-10)
        let pkg = make_test_package(Utc.timestamp_opt(1546300800, 0).unwrap(), None);
        assert!(pkg.predates_flakes());
    }

    #[test]
    fn test_predates_flakes_new_commit() {
        // 2023-11-14 - after flakes
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        assert!(!pkg.predates_flakes());
    }

    #[test]
    fn test_nix_shell_cmd_modern_secure() {
        // Modern commit (after flakes), no vulnerabilities
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        let cmd = pkg.nix_shell_cmd();
        assert_eq!(cmd, "nix shell nixpkgs/def1234#test");
        assert!(!cmd.contains("NIXPKGS_ALLOW_INSECURE"));
        assert!(!cmd.contains("--impure"));
    }

    #[test]
    fn test_nix_shell_cmd_modern_insecure() {
        // Modern commit (after flakes), with vulnerabilities
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some(r#"["CVE-2023-1234"]"#.to_string()),
        );
        let cmd = pkg.nix_shell_cmd();
        assert!(cmd.starts_with("NIXPKGS_ALLOW_INSECURE=1 "));
        assert!(cmd.contains(" --impure "));
        assert!(cmd.contains("nixpkgs/def1234#test"));
    }

    #[test]
    fn test_nix_shell_cmd_legacy_secure() {
        // Legacy commit (before flakes), no vulnerabilities
        let pkg = make_test_package(Utc.timestamp_opt(1546300800, 0).unwrap(), None);
        let cmd = pkg.nix_shell_cmd();
        assert!(cmd.starts_with("nix-shell -p"));
        assert!(cmd.contains("fetchTarball"));
        assert!(cmd.contains("def1234"));
        assert!(!cmd.contains("NIXPKGS_ALLOW_INSECURE"));
    }

    #[test]
    fn test_nix_shell_cmd_legacy_insecure() {
        // Legacy commit (before flakes), with vulnerabilities
        let pkg = make_test_package(
            Utc.timestamp_opt(1546300800, 0).unwrap(),
            Some(r#"["CVE-2023-1234"]"#.to_string()),
        );
        let cmd = pkg.nix_shell_cmd();
        assert!(cmd.starts_with("NIXPKGS_ALLOW_INSECURE=1 "));
        assert!(cmd.contains("nix-shell -p"));
        assert!(cmd.contains("fetchTarball"));
        // Legacy commands don't use --impure
        assert!(!cmd.contains("--impure"));
    }

    #[test]
    fn test_nix_run_cmd_modern_secure() {
        // Modern commit (after flakes), no vulnerabilities
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        let cmd = pkg.nix_run_cmd();
        assert_eq!(cmd, "nix run nixpkgs/def1234#test");
        assert!(!cmd.contains("NIXPKGS_ALLOW_INSECURE"));
        assert!(!cmd.contains("--impure"));
    }

    #[test]
    fn test_nix_run_cmd_modern_insecure() {
        // Modern commit (after flakes), with vulnerabilities
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some(r#"["CVE-2023-1234"]"#.to_string()),
        );
        let cmd = pkg.nix_run_cmd();
        assert!(cmd.starts_with("NIXPKGS_ALLOW_INSECURE=1 "));
        assert!(cmd.contains(" --impure "));
        assert!(cmd.contains("nixpkgs/def1234#test"));
    }

    #[test]
    fn test_nix_run_cmd_legacy_secure() {
        // Legacy commit (before flakes), no vulnerabilities
        let pkg = make_test_package(Utc.timestamp_opt(1546300800, 0).unwrap(), None);
        let cmd = pkg.nix_run_cmd();
        assert!(cmd.contains("nix-shell -p"));
        assert!(cmd.contains("--run test"));
        assert!(!cmd.contains("NIXPKGS_ALLOW_INSECURE"));
    }

    #[test]
    fn test_nix_run_cmd_legacy_insecure() {
        // Legacy commit (before flakes), with vulnerabilities
        let pkg = make_test_package(
            Utc.timestamp_opt(1546300800, 0).unwrap(),
            Some(r#"["CVE-2023-1234"]"#.to_string()),
        );
        let cmd = pkg.nix_run_cmd();
        assert!(cmd.starts_with("NIXPKGS_ALLOW_INSECURE=1 "));
        assert!(cmd.contains("nix-shell -p"));
        assert!(cmd.contains("--run test"));
        // Legacy commands don't use --impure
        assert!(!cmd.contains("--impure"));
    }
}

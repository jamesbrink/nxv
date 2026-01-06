//! Database query operations for package searches.

use crate::error::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Escapes SQL LIKE wildcard characters (`%`, `_`, `\`) in user input.
///
/// This prevents SQL wildcard injection where users could pass `%` to match
/// all records or `_` to match single characters unexpectedly.
///
/// The escaped string should be used with `LIKE ? ESCAPE '\'` in SQL queries.
fn escape_like_pattern(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Escapes user input for use in SQLite FTS5 MATCH queries.
///
/// FTS5 has its own query syntax with operators like `NOT`, `OR`, `AND`, `*`, `^`,
/// and special quoting rules. To prevent users from accidentally or maliciously
/// using these operators, we wrap the input in double quotes (forcing phrase matching)
/// and escape any internal double quotes by doubling them.
///
/// # Examples
///
/// ```
/// assert_eq!(escape_fts5_query("python"), "\"python\"");
/// assert_eq!(escape_fts5_query("NOT python"), "\"NOT python\"");
/// assert_eq!(escape_fts5_query("say \"hello\""), "\"say \"\"hello\"\"\"");
/// ```
fn escape_fts5_query(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

/// Unix timestamp for when flake.nix was added to nixpkgs (2020-02-10 00:00:00 UTC).
/// Commits before this date require legacy nix-shell syntax instead of flake references.
/// See: https://github.com/NixOS/nixpkgs/pull/68897
const FLAKE_EPOCH_TIMESTAMP: i64 = 1581292800;

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
    /// Store paths per architecture (e.g., {"x86_64-linux": "/nix/store/hash-name-version"})
    /// Only populated for commits from 2020-01-01 onwards.
    #[serde(default)]
    pub store_paths: HashMap<String, String>,
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

        // Use single() instead of unwrap() to safely handle invalid timestamps
        let first_commit_date =
            Utc.timestamp_opt(first_commit_ts, 0)
                .single()
                .ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        format!("Invalid first_commit_date timestamp: {}", first_commit_ts).into(),
                    )
                })?;

        let last_commit_date = Utc
            .timestamp_opt(last_commit_ts, 0)
            .single()
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    format!("Invalid last_commit_date timestamp: {}", last_commit_ts).into(),
                )
            })?;

        // Build store_paths from the 4 architecture columns
        let mut store_paths = HashMap::new();
        if let Some(path) = row
            .get::<_, Option<String>>("store_path_x86_64_linux")
            .ok()
            .flatten()
        {
            store_paths.insert("x86_64-linux".to_string(), path);
        }
        if let Some(path) = row
            .get::<_, Option<String>>("store_path_aarch64_linux")
            .ok()
            .flatten()
        {
            store_paths.insert("aarch64-linux".to_string(), path);
        }
        if let Some(path) = row
            .get::<_, Option<String>>("store_path_x86_64_darwin")
            .ok()
            .flatten()
        {
            store_paths.insert("x86_64-darwin".to_string(), path);
        }
        if let Some(path) = row
            .get::<_, Option<String>>("store_path_aarch64_darwin")
            .ok()
            .flatten()
        {
            store_paths.insert("aarch64-darwin".to_string(), path);
        }

        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            version: row.get("version")?,
            first_commit_hash: row.get("first_commit_hash")?,
            first_commit_date,
            last_commit_hash: row.get("last_commit_hash")?,
            last_commit_date,
            attribute_path: row.get("attribute_path")?,
            description: row.get("description")?,
            license: row.get("license")?,
            homepage: row.get("homepage")?,
            maintainers: row.get("maintainers")?,
            platforms: row.get("platforms")?,
            source_path: row.get("source_path").ok().flatten(),
            known_vulnerabilities: row.get("known_vulnerabilities").ok().flatten(),
            store_paths,
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
    pub fn predates_flakes(&self) -> bool {
        self.last_commit_date.timestamp() < FLAKE_EPOCH_TIMESTAMP
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
                "{}nix-shell -p '(import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}'",
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
    ///
    /// Note: For legacy (pre-flake) packages, this uses `nix-shell --run` with the
    /// attribute path as the command. This works when the binary name matches the
    /// attribute path (e.g., `python`), but may fail for packages where they differ
    /// (e.g., `python27` attribute but `python` binary). Users may need to adjust
    /// the command for such cases.
    pub fn nix_run_cmd(&self) -> String {
        let insecure_prefix = if self.is_insecure() {
            "NIXPKGS_ALLOW_INSECURE=1 "
        } else {
            ""
        };

        if self.predates_flakes() {
            format!(
                "{}nix-shell -p '(import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}' --run {}",
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
            .and_then(|v| {
                serde_json::from_str(v)
                    .map_err(|e| {
                        tracing::warn!(
                            attr = %self.attribute_path,
                            error = %e,
                            raw_value = %v,
                            "Failed to parse known_vulnerabilities JSON"
                        );
                        e
                    })
                    .ok()
            })
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
    /// The commit hash that was last indexed (from meta table).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_commit: Option<String>,
    /// When the index was last updated (from meta table).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_date: Option<String>,
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
        "SELECT * FROM package_versions WHERE name LIKE ? ESCAPE '\\' ORDER BY last_commit_date DESC"
    };

    let pattern = if exact {
        name.to_string()
    } else {
        format!("{}%", escape_like_pattern(name))
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([&pattern], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Search for packages by attribute path with relevance ranking.
///
/// Matches packages where:
/// - The attribute path starts with the query (prefix match)
/// - The attribute path contains the query after a dot (suffix/component match)
///
/// Results are ranked by relevance:
/// 1. Exact match (attribute_path == query)
/// 2. Top-level prefix match (query at start, no dot before next segment)
/// 3. Final component exact match (last segment after dot == query)
/// 4. Final component prefix match (last segment starts with query)
/// 5. Other matches (query appears elsewhere)
///
/// Within each relevance tier, results are sorted by last_commit_date DESC.
///
/// This allows searching "python" to show `python` first, then `python2`,
/// before nested packages like `python3Packages.numpy`.
pub fn search_by_attr(conn: &rusqlite::Connection, attr_path: &str) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(
        r#"SELECT *,
           CASE
               -- Exact match: highest priority
               WHEN attribute_path = ?1 THEN 1
               -- Top-level prefix: "python" matches "python2" but not "python3Packages.foo"
               -- Match if starts with query and either ends there or next char isn't a dot
               WHEN attribute_path LIKE ?2 ESCAPE '\' AND attribute_path NOT LIKE ?3 ESCAPE '\' THEN 2
               -- Final component exact match: "qtwebengine" matches "qt5.qtwebengine"
               WHEN attribute_path LIKE ?4 ESCAPE '\' THEN 3
               -- Final component prefix: "numpy" matches "python3Packages.numpy1"
               WHEN attribute_path LIKE ?5 ESCAPE '\' THEN 4
               -- Other prefix matches
               WHEN attribute_path LIKE ?2 ESCAPE '\' THEN 5
               -- Other substring matches
               ELSE 6
           END as relevance
           FROM package_versions
           WHERE attribute_path LIKE ?2 ESCAPE '\'
              OR attribute_path LIKE ?6 ESCAPE '\'
           ORDER BY relevance, last_commit_date DESC"#,
    )?;
    let escaped = escape_like_pattern(attr_path);
    // ?1: exact match
    let exact = attr_path;
    // ?2: prefix pattern (python%)
    let prefix_pattern = format!("{}%", escaped);
    // ?3: prefix with dot pattern (python%.%) - excludes nested packages from tier 2
    let prefix_dot_pattern = format!("{}%.%", escaped);
    // ?4: final component exact match (%.python)
    let final_exact_pattern = format!("%.{}", escaped);
    // ?5: final component prefix match (%.python%)
    let final_prefix_pattern = format!("%.{}%", escaped);
    // ?6: suffix/contains pattern (%.python%)
    let suffix_pattern = format!("%.{}%", escaped);

    let rows = stmt.query_map(
        rusqlite::params![
            exact,
            prefix_pattern,
            prefix_dot_pattern,
            final_exact_pattern,
            final_prefix_pattern,
            suffix_pattern
        ],
        PackageVersion::from_row,
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Finds package versions whose attribute path matches the query and version starts with given prefix.
///
/// The `package` argument matches attribute paths where:
/// - The path starts with the query (prefix match)
/// - The path contains the query after a dot (suffix/component match)
///
/// The `version` argument is matched as a prefix against the `version` column.
///
/// Results are ranked by relevance (same as `search_by_attr`):
/// 1. Exact match (attribute_path == query)
/// 2. Top-level prefix match
/// 3. Final component exact match
/// 4. Final component prefix match
/// 5. Other matches
///
/// Within each relevance tier, results are sorted by first_commit_date DESC.
///
/// # Returns
///
/// A vector of `PackageVersion` entries that match the provided package and version patterns.
///
/// # Examples
///
/// ```
/// // Assuming `conn` is a valid rusqlite::Connection populated with package_versions...
/// let matches = search_by_name_version(&conn, "numpy", "1.24").unwrap();
/// // Matches both "numpy" and "python3Packages.numpy"
/// ```
pub fn search_by_name_version(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
) -> Result<Vec<PackageVersion>> {
    // Search by attribute_path (package) and version prefix with relevance ranking
    let mut stmt = conn.prepare(
        r#"SELECT *,
           CASE
               WHEN attribute_path = ?1 THEN 1
               WHEN attribute_path LIKE ?2 ESCAPE '\' AND attribute_path NOT LIKE ?3 ESCAPE '\' THEN 2
               WHEN attribute_path LIKE ?4 ESCAPE '\' THEN 3
               WHEN attribute_path LIKE ?5 ESCAPE '\' THEN 4
               WHEN attribute_path LIKE ?2 ESCAPE '\' THEN 5
               ELSE 6
           END as relevance
           FROM package_versions
           WHERE (attribute_path LIKE ?2 ESCAPE '\' OR attribute_path LIKE ?6 ESCAPE '\')
             AND version LIKE ?7 ESCAPE '\'
           ORDER BY relevance, first_commit_date DESC"#,
    )?;
    let escaped_pkg = escape_like_pattern(package);
    let exact = package;
    let prefix_pattern = format!("{}%", escaped_pkg);
    let prefix_dot_pattern = format!("{}%.%", escaped_pkg);
    let final_exact_pattern = format!("%.{}", escaped_pkg);
    let final_prefix_pattern = format!("%.{}%", escaped_pkg);
    let suffix_pattern = format!("%.{}%", escaped_pkg);
    let version_pattern = format!("{}%", escape_like_pattern(version));

    let rows = stmt.query_map(
        rusqlite::params![
            exact,
            prefix_pattern,
            prefix_dot_pattern,
            final_exact_pattern,
            final_prefix_pattern,
            suffix_pattern,
            version_pattern
        ],
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
    // Query history for the specific attribute path, but check if ANY package
    // with the same version has vulnerabilities (since insecurity is about the
    // version, not the attribute path - e.g., Python 2.7 is EOL regardless of
    // whether it's called "python" or "python27")
    //
    // Uses a CTE to pre-compute insecure versions once, avoiding a correlated
    // subquery that would run for each row in the result set.
    let mut stmt = conn.prepare(
        r#"
        WITH insecure_versions AS (
            SELECT DISTINCT version
            FROM package_versions
            WHERE known_vulnerabilities IS NOT NULL
              AND known_vulnerabilities != ''
              AND known_vulnerabilities != '[]'
              AND known_vulnerabilities != 'null'
        )
        SELECT pv.version,
               MIN(pv.first_commit_date) as first_seen,
               MAX(pv.last_commit_date) as last_seen,
               CASE WHEN iv.version IS NOT NULL THEN 1 ELSE 0 END as is_insecure
        FROM package_versions pv
        LEFT JOIN insecure_versions iv ON pv.version = iv.version
        WHERE pv.attribute_path = ?
        GROUP BY pv.version
        ORDER BY first_seen DESC
        "#,
    )?;

    let rows = stmt.query_map([package], |row| {
        let version: String = row.get(0)?;
        let first_ts: i64 = row.get(1)?;
        let last_ts: i64 = row.get(2)?;
        let is_insecure: i64 = row.get(3)?;

        let first_seen = Utc.timestamp_opt(first_ts, 0).single().ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                format!("Invalid first_seen timestamp: {}", first_ts).into(),
            )
        })?;

        let last_seen = Utc.timestamp_opt(last_ts, 0).single().ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Integer,
                format!("Invalid last_seen timestamp: {}", last_ts).into(),
            )
        })?;

        Ok((version, first_seen, last_seen, is_insecure != 0))
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

    // Get meta values (backwards compatible - returns None if not present)
    let last_indexed_commit: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
            [],
            |row| row.get(0),
        )
        .ok();

    let last_indexed_date: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_date'",
            [],
            |row| row.get(0),
        )
        .ok();

    Ok(IndexStats {
        total_ranges,
        unique_names,
        unique_versions,
        oldest_commit_date: oldest.and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        newest_commit_date: newest.and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        last_indexed_commit,
        last_indexed_date,
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

    // Escape user input to prevent FTS5 syntax injection
    let escaped_query = escape_fts5_query(query);
    let rows = stmt.query_map([&escaped_query], PackageVersion::from_row)?;

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

/// Return distinct package attribute paths matching a prefix, limited to a maximum count.
///
/// This function is optimized for shell tab completion, returning only unique attribute
/// paths that start with the given prefix. Results are ordered alphabetically.
///
/// Special characters `%` and `_` in the prefix are escaped to prevent them from being
/// interpreted as SQL LIKE wildcards.
///
/// # Arguments
///
/// * `prefix` - The prefix to match against attribute paths (case-sensitive)
/// * `limit` - Maximum number of results to return
pub fn complete_package_prefix(
    conn: &rusqlite::Connection,
    prefix: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT attribute_path FROM package_versions WHERE attribute_path LIKE ? ESCAPE '\\' ORDER BY attribute_path LIMIT ?",
    )?;
    let pattern = format!("{}%", escape_like_pattern(prefix));
    let rows = stmt.query_map(rusqlite::params![&pattern, limit as i64], |row| row.get(0))?;

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
    /// The database is populated with sample entries including nested packages (qt5.qtwebengine, python3Packages.numpy).
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
        // Includes nested packages like qt5.qtwebengine and python3Packages.numpy
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('python-3.11.0', '3.11.0', 'abc1234567890', 1700000000, 'def1234567890', 1700100000, 'python', 'Python interpreter'),
                ('python-3.12.0', '3.12.0', 'ghi1234567890', 1701000000, 'jkl1234567890', 1701100000, 'python', 'Python interpreter'),
                ('python2-2.7.18', '2.7.18', 'mno1234567890', 1600000000, 'pqr1234567890', 1600100000, 'python2', 'Python 2 interpreter'),
                ('nodejs-20.0.0', '20.0.0', 'stu1234567890', 1702000000, 'vwx1234567890', 1702100000, 'nodejs', 'Node.js runtime'),
                ('qtwebengine-5.15.2', '5.15.2', 'qtw1234567890', 1703000000, 'qtx1234567890', 1703100000, 'qt5.qtwebengine', 'Qt WebEngine'),
                ('numpy-1.24.0', '1.24.0', 'npy1234567890', 1704000000, 'npz1234567890', 1704100000, 'python3Packages.numpy', 'NumPy for Python')
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
        // Should match "python", "python2", and "python3Packages.numpy" attribute paths
        assert_eq!(results.len(), 4);
    }

    #[test]
    fn test_search_by_attr_exact_match() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr(db.connection(), "nodejs").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "nodejs");
    }

    #[test]
    fn test_search_by_attr_nested_package() {
        let (_dir, db) = create_test_db();
        // Searching "qtwebengine" should find "qt5.qtwebengine"
        let results = search_by_attr(db.connection(), "qtwebengine").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "qt5.qtwebengine");

        // Searching "numpy" should find "python3Packages.numpy"
        let results = search_by_attr(db.connection(), "numpy").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "python3Packages.numpy");
    }

    #[test]
    fn test_search_by_attr_full_nested_path() {
        let (_dir, db) = create_test_db();
        // Searching full path should also work
        let results = search_by_attr(db.connection(), "qt5.qtwebengine").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "qt5.qtwebengine");
    }

    #[test]
    fn test_search_by_name_version_nested_package() {
        let (_dir, db) = create_test_db();
        // Searching "numpy" with version should find nested package
        let results = search_by_name_version(db.connection(), "numpy", "1.24").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "python3Packages.numpy");
    }

    /// Regression test: Verify that search results are ordered by relevance.
    ///
    /// When searching for "python", results should be ordered:
    /// 1. Exact match: "python"
    /// 2. Top-level prefix matches: "python2", "python3" (no dots)
    /// 3. Nested packages: "python3Packages.numpy" (contains dots)
    ///
    /// This test ensures that top-level packages appear before nested packages,
    /// which is critical for user experience when searching common package names.
    #[test]
    fn test_search_by_attr_relevance_ranking() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("relevance_test.db");
        let db = Database::open(&db_path).unwrap();

        // Insert packages at different relevance tiers with varying timestamps
        // to ensure relevance ordering takes precedence over date ordering.
        // The nested package has the NEWEST date to verify relevance wins over recency.
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                -- Tier 5: Nested package with prefix match (newest date - would be first if sorted by date)
                ('python-lib', '1.0.0', 'nested1234567', 1800000000, 'nested7654321', 1800100000, 'python3Packages.python-dateutil', 'Date utilities'),
                -- Tier 1: Exact match (oldest date)
                ('python', '3.11.0', 'exact1234567', 1600000000, 'exact7654321', 1600100000, 'python', 'Python interpreter'),
                -- Tier 5: Another nested package (second newest)
                ('numpy', '1.24.0', 'numpy1234567', 1750000000, 'numpy7654321', 1750100000, 'python3Packages.numpy', 'NumPy'),
                -- Tier 2: Top-level prefix without dot
                ('python2', '2.7.18', 'py2_1234567', 1650000000, 'py2_7654321', 1650100000, 'python2', 'Python 2'),
                -- Tier 2: Another top-level prefix
                ('python3', '3.10.0', 'py3_1234567', 1700000000, 'py3_7654321', 1700100000, 'python3', 'Python 3'),
                -- Tier 3: Final component exact match (e.g., linuxPackages.python)
                ('kernel-python', '3.9.0', 'kern1234567', 1680000000, 'kern7654321', 1680100000, 'linuxPackages.python', 'Python for kernel')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_attr(db.connection(), "python").unwrap();

        // Should have 6 results total
        assert_eq!(results.len(), 6, "Expected 6 results for 'python' search");

        // Verify ordering by relevance tiers:
        // Tier 1: Exact match should be first
        assert_eq!(
            results[0].attribute_path, "python",
            "Exact match 'python' should be first"
        );

        // Tier 2: Top-level prefix matches (python2, python3) should come next
        // Order within tier is by last_commit_date DESC
        let tier2_packages: Vec<&str> = results[1..3]
            .iter()
            .map(|p| p.attribute_path.as_str())
            .collect();
        assert!(
            tier2_packages.contains(&"python2") && tier2_packages.contains(&"python3"),
            "Top-level prefixes 'python2' and 'python3' should be in positions 1-2, got: {:?}",
            tier2_packages
        );

        // Tier 3: Final component exact match
        assert_eq!(
            results[3].attribute_path, "linuxPackages.python",
            "Final component exact match should be in position 3"
        );

        // Tier 5: Nested packages should be last (positions 4-5)
        let nested_packages: Vec<&str> = results[4..6]
            .iter()
            .map(|p| p.attribute_path.as_str())
            .collect();
        assert!(
            nested_packages.contains(&"python3Packages.numpy")
                && nested_packages.contains(&"python3Packages.python-dateutil"),
            "Nested packages should be in positions 4-5, got: {:?}",
            nested_packages
        );

        // Verify that date ordering within the same tier works correctly
        // python3 (1700100000) should come before python2 (1650100000) within tier 2
        assert_eq!(
            results[1].attribute_path, "python3",
            "Within tier 2, python3 (newer) should come before python2"
        );
        assert_eq!(
            results[2].attribute_path, "python2",
            "Within tier 2, python2 (older) should come after python3"
        );
    }

    /// Regression test: Verify relevance ranking for search_by_name_version.
    #[test]
    fn test_search_by_name_version_relevance_ranking() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("relevance_version_test.db");
        let db = Database::open(&db_path).unwrap();

        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                -- Nested package (newest date)
                ('numpy', '1.24.0', 'numpy1234567', 1800000000, 'numpy7654321', 1800100000, 'python3Packages.numpy', 'NumPy'),
                -- Exact match (oldest date)
                ('numpy', '1.24.0', 'exact1234567', 1600000000, 'exact7654321', 1600100000, 'numpy', 'NumPy standalone'),
                -- Another nested
                ('numpy', '1.24.1', 'np2_1234567', 1750000000, 'np2_7654321', 1750100000, 'python39Packages.numpy', 'NumPy for Py39')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_name_version(db.connection(), "numpy", "1.24").unwrap();

        assert_eq!(results.len(), 3, "Expected 3 results");

        // Exact match should be first despite having oldest date
        assert_eq!(
            results[0].attribute_path, "numpy",
            "Exact match 'numpy' should be first"
        );

        // Nested packages should follow
        assert!(
            results[1].attribute_path.contains('.') && results[2].attribute_path.contains('.'),
            "Nested packages should be after exact match"
        );
    }

    /// Regression test: Verify relevance ranking with Qt-style nested packages.
    ///
    /// Qt packages in nixpkgs are typically structured as qt5.qtwebengine, qt6.qtbase, etc.
    /// When searching for "qtwebengine", results should prioritize:
    /// 1. Exact match: "qtwebengine" (if it exists)
    /// 2. Final component exact match: "qt5.qtwebengine"
    /// 3. Final component prefix: "qt5.qtwebengine-extra"
    #[test]
    fn test_search_by_attr_relevance_ranking_qt_style() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("qt_relevance_test.db");
        let db = Database::open(&db_path).unwrap();

        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                -- Tier 4: Final component prefix match (newest)
                ('qtwebengine-extra', '5.15.2', 'extra1234567', 1800000000, 'extra7654321', 1800100000, 'qt5.qtwebengine-extra', 'Qt WebEngine Extra'),
                -- Tier 3: Final component exact match
                ('qtwebengine', '5.15.2', 'qt5we1234567', 1700000000, 'qt5we7654321', 1700100000, 'qt5.qtwebengine', 'Qt5 WebEngine'),
                -- Tier 1: Exact match (oldest)
                ('qtwebengine', '6.0.0', 'exact1234567', 1600000000, 'exact7654321', 1600100000, 'qtwebengine', 'Standalone WebEngine'),
                -- Tier 3: Another final component exact match
                ('qtwebengine', '6.2.0', 'qt6we1234567', 1750000000, 'qt6we7654321', 1750100000, 'qt6.qtwebengine', 'Qt6 WebEngine')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_attr(db.connection(), "qtwebengine").unwrap();

        assert_eq!(results.len(), 4, "Expected 4 results");

        // Tier 1: Exact match should be first
        assert_eq!(
            results[0].attribute_path, "qtwebengine",
            "Exact match 'qtwebengine' should be first"
        );

        // Tier 3: Final component exact matches should be next (ordered by date DESC)
        let tier3_packages: Vec<&str> = results[1..3]
            .iter()
            .map(|p| p.attribute_path.as_str())
            .collect();
        assert!(
            tier3_packages.contains(&"qt5.qtwebengine")
                && tier3_packages.contains(&"qt6.qtwebengine"),
            "Final component exact matches should be in positions 1-2, got: {:?}",
            tier3_packages
        );
        // qt6.qtwebengine (1750100000) should come before qt5.qtwebengine (1700100000)
        assert_eq!(results[1].attribute_path, "qt6.qtwebengine");
        assert_eq!(results[2].attribute_path, "qt5.qtwebengine");

        // Tier 4: Final component prefix match should be last
        assert_eq!(
            results[3].attribute_path, "qt5.qtwebengine-extra",
            "Final component prefix match should be last"
        );
    }

    /// Regression test: Verify relevance ranking with Haskell-style packages.
    ///
    /// Haskell packages are typically structured as haskellPackages.aeson, etc.
    /// This tests another common pattern in nixpkgs.
    #[test]
    fn test_search_by_attr_relevance_ranking_haskell_style() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("haskell_relevance_test.db");
        let db = Database::open(&db_path).unwrap();

        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                -- Nested in haskellPackages (newest)
                ('aeson', '2.1.0', 'hask1234567', 1800000000, 'hask7654321', 1800100000, 'haskellPackages.aeson', 'Haskell JSON'),
                -- Exact match (oldest)
                ('aeson', '2.0.0', 'exact1234567', 1600000000, 'exact7654321', 1600100000, 'aeson', 'Standalone Aeson'),
                -- Different nesting
                ('aeson', '2.0.5', 'ghc_1234567', 1700000000, 'ghc_7654321', 1700100000, 'haskell.compiler.ghc92.aeson', 'GHC92 Aeson')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_attr(db.connection(), "aeson").unwrap();

        assert_eq!(results.len(), 3, "Expected 3 results");

        // Exact match should be first despite being oldest
        assert_eq!(
            results[0].attribute_path, "aeson",
            "Exact match 'aeson' should be first"
        );

        // Nested packages should follow (tier 3 - final component exact matches)
        assert!(
            results[1].attribute_path.ends_with(".aeson")
                && results[2].attribute_path.ends_with(".aeson"),
            "Nested packages with .aeson suffix should follow"
        );
    }

    /// Regression test: Verify that top-level prefix beats nested prefix.
    ///
    /// When searching for "gtk", "gtk3" (top-level) should appear before
    /// "gnome.gtk" (nested), even if the nested one is newer.
    #[test]
    fn test_search_by_attr_relevance_toplevel_beats_nested() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("toplevel_test.db");
        let db = Database::open(&db_path).unwrap();

        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                -- Nested prefix (newest - would be first if sorted by date)
                ('gtk', '4.0.0', 'gnome1234567', 1800000000, 'gnome7654321', 1800100000, 'gnome.gtk', 'GNOME GTK'),
                -- Top-level exact (oldest)
                ('gtk', '3.24.0', 'exact1234567', 1500000000, 'exact7654321', 1500100000, 'gtk', 'GTK toolkit'),
                -- Another nested
                ('gtk-doc', '1.0.0', 'doc_1234567', 1750000000, 'doc_7654321', 1750100000, 'gnome.gtk-doc', 'GTK Documentation'),
                -- Top-level prefix
                ('gtk3', '3.24.0', 'gtk3_1234567', 1600000000, 'gtk3_7654321', 1600100000, 'gtk3', 'GTK3 toolkit'),
                -- Top-level prefix
                ('gtk4', '4.0.0', 'gtk4_1234567', 1700000000, 'gtk4_7654321', 1700100000, 'gtk4', 'GTK4 toolkit')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_attr(db.connection(), "gtk").unwrap();

        assert_eq!(results.len(), 5, "Expected 5 results");

        // Tier 1: Exact match first
        assert_eq!(
            results[0].attribute_path, "gtk",
            "Exact match should be first"
        );

        // Tier 2: Top-level prefixes next (gtk3, gtk4), ordered by date DESC
        assert_eq!(
            results[1].attribute_path, "gtk4",
            "gtk4 (newer) should be second"
        );
        assert_eq!(
            results[2].attribute_path, "gtk3",
            "gtk3 (older) should be third"
        );

        // Tier 3: Final component exact match
        assert_eq!(
            results[3].attribute_path, "gnome.gtk",
            "gnome.gtk (final component exact) should be fourth"
        );

        // Tier 4: Final component prefix match
        assert_eq!(
            results[4].attribute_path, "gnome.gtk-doc",
            "gnome.gtk-doc (final component prefix) should be last"
        );
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
        assert_eq!(stats.total_ranges, 6);
        assert_eq!(stats.unique_names, 6); // python-3.11.0, python-3.12.0, python2-2.7.18, nodejs-20.0.0, qtwebengine-5.15.2, numpy-1.24.0
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
    fn test_get_stats_with_last_indexed_date() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Set meta values
        db.set_meta("last_indexed_commit", "abc1234567890").unwrap();
        db.set_meta("last_indexed_date", "2026-01-03T12:00:00Z")
            .unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.last_indexed_commit, Some("abc1234567890".to_string()));
        assert_eq!(
            stats.last_indexed_date,
            Some("2026-01-03T12:00:00Z".to_string())
        );
    }

    #[test]
    fn test_get_stats_backwards_compatible_no_date() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Only set commit (simulating older database without date)
        db.set_meta("last_indexed_commit", "abc1234567890").unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.last_indexed_commit, Some("abc1234567890".to_string()));
        assert_eq!(stats.last_indexed_date, None); // Should be None for backwards compat
    }

    #[test]
    fn test_get_all_unique_attrs() {
        let (_dir, db) = create_test_db();
        let attrs = get_all_unique_attrs(db.connection()).unwrap();
        assert_eq!(attrs.len(), 5); // nodejs, python, python2, python3Packages.numpy, qt5.qtwebengine
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
    fn test_complete_package_prefix() {
        let (_dir, db) = create_test_db();
        // Test with prefix that matches multiple packages
        let results = complete_package_prefix(db.connection(), "python", 10).unwrap();
        assert_eq!(results.len(), 3); // python, python2, python3Packages.numpy
        assert!(results.contains(&"python".to_string()));
        assert!(results.contains(&"python2".to_string()));
        assert!(results.contains(&"python3Packages.numpy".to_string()));
    }

    #[test]
    fn test_complete_package_prefix_exact() {
        let (_dir, db) = create_test_db();
        // Test with prefix that matches exactly one package
        let results = complete_package_prefix(db.connection(), "nodejs", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "nodejs");
    }

    #[test]
    fn test_complete_package_prefix_no_match() {
        let (_dir, db) = create_test_db();
        // Test with prefix that matches nothing
        let results = complete_package_prefix(db.connection(), "zzz", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_complete_package_prefix_limit() {
        let (_dir, db) = create_test_db();
        // Test that limit is respected
        let results = complete_package_prefix(db.connection(), "python", 1).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_complete_package_prefix_empty() {
        let (_dir, db) = create_test_db();
        // Empty prefix should return all packages (up to limit)
        let results = complete_package_prefix(db.connection(), "", 10).unwrap();
        assert_eq!(results.len(), 5); // nodejs, python, python2, python3Packages.numpy, qt5.qtwebengine
    }

    #[test]
    fn test_complete_package_prefix_escapes_wildcards() {
        let (_dir, db) = create_test_db();
        // SQL LIKE wildcards should be escaped - % and _ should not match anything
        let results = complete_package_prefix(db.connection(), "%", 10).unwrap();
        assert!(results.is_empty(), "% should not match as wildcard");

        let results = complete_package_prefix(db.connection(), "_", 10).unwrap();
        assert!(results.is_empty(), "_ should not match as wildcard");

        let results = complete_package_prefix(db.connection(), "py%on", 10).unwrap();
        assert!(
            results.is_empty(),
            "% in middle should not match as wildcard"
        );
    }

    #[test]
    fn test_search_by_name_escapes_wildcards() {
        let (_dir, db) = create_test_db();
        // SQL LIKE wildcards should be escaped - % should not match everything
        let results = search_by_name(db.connection(), "%", false).unwrap();
        assert!(results.is_empty(), "% should not match as wildcard");

        let results = search_by_name(db.connection(), "_", false).unwrap();
        assert!(results.is_empty(), "_ should not match as wildcard");

        // Test that % in the middle doesn't act as wildcard
        let results = search_by_name(db.connection(), "py%on", false).unwrap();
        assert!(
            results.is_empty(),
            "% in middle should not match as wildcard"
        );

        // Backslash should be escaped too
        let results = search_by_name(db.connection(), "\\", false).unwrap();
        assert!(results.is_empty(), "\\ should not cause issues");
    }

    #[test]
    fn test_search_by_attr_escapes_wildcards() {
        let (_dir, db) = create_test_db();
        // SQL LIKE wildcards should be escaped - % should not match everything
        let results = search_by_attr(db.connection(), "%").unwrap();
        assert!(results.is_empty(), "% should not match as wildcard");

        let results = search_by_attr(db.connection(), "_").unwrap();
        assert!(results.is_empty(), "_ should not match as wildcard");

        // Test that normal prefix search still works
        let results = search_by_attr(db.connection(), "python").unwrap();
        assert_eq!(results.len(), 4); // python (x2 versions) + python2 + python3Packages.numpy
    }

    #[test]
    fn test_search_by_name_version_escapes_wildcards() {
        let (_dir, db) = create_test_db();
        // SQL LIKE wildcards should be escaped
        let results = search_by_name_version(db.connection(), "%", "%").unwrap();
        assert!(
            results.is_empty(),
            "% should not match as wildcard in either field"
        );

        let results = search_by_name_version(db.connection(), "python", "%").unwrap();
        assert!(
            results.is_empty(),
            "% should not match as wildcard in version"
        );

        let results = search_by_name_version(db.connection(), "%", "3.11").unwrap();
        assert!(
            results.is_empty(),
            "% should not match as wildcard in package"
        );

        // Underscore should also be escaped
        let results = search_by_name_version(db.connection(), "_", "_").unwrap();
        assert!(results.is_empty(), "_ should not match as wildcard");
    }

    #[test]
    fn test_escape_like_pattern() {
        // Test the helper function directly
        assert_eq!(escape_like_pattern("normal"), "normal");
        assert_eq!(escape_like_pattern("%"), "\\%");
        assert_eq!(escape_like_pattern("_"), "\\_");
        assert_eq!(escape_like_pattern("\\"), "\\\\");
        assert_eq!(
            escape_like_pattern("foo%bar_baz\\qux"),
            "foo\\%bar\\_baz\\\\qux"
        );
        assert_eq!(escape_like_pattern(""), "");
        assert_eq!(escape_like_pattern("%%%"), "\\%\\%\\%");
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

    #[test]
    fn test_search_by_description_fts5_operators_escaped() {
        let (_dir, db) = create_test_db();
        // FTS5 operators like NOT, OR, AND should be treated as literal text, not operators
        // Previously this would error with "fts5: syntax error near NOT"
        let results = search_by_description(db.connection(), "NOT python").unwrap();
        assert!(
            results.is_empty(),
            "Should not error and should return empty (no literal 'NOT python' in descriptions)"
        );

        let results = search_by_description(db.connection(), "python OR rust").unwrap();
        assert!(
            results.is_empty(),
            "Should treat 'OR' as literal text, not operator"
        );

        let results = search_by_description(db.connection(), "python AND runtime").unwrap();
        assert!(
            results.is_empty(),
            "Should treat 'AND' as literal text, not operator"
        );
    }

    #[test]
    fn test_search_by_description_fts5_special_chars_escaped() {
        let (_dir, db) = create_test_db();
        // Special FTS5 characters should be escaped and not cause syntax errors
        // Note: FTS5's tokenizer may strip punctuation, so `py*` might still match "python"
        // The key is that the query doesn't error and wildcards aren't interpreted as FTS5 operators

        // Wildcard - should not cause syntax error
        let results = search_by_description(db.connection(), "py*");
        assert!(
            results.is_ok(),
            "Wildcard should not cause FTS5 syntax error"
        );

        // Caret - should not cause syntax error (tokenizer may strip it)
        let results = search_by_description(db.connection(), "^python");
        assert!(results.is_ok(), "Caret should not cause FTS5 syntax error");

        // Unbalanced quotes should not cause errors
        let results = search_by_description(db.connection(), "\"unbalanced");
        assert!(
            results.is_ok(),
            "Unbalanced quote should not cause FTS5 syntax error"
        );
    }

    #[test]
    fn test_search_by_description_with_quotes() {
        let (_dir, db) = create_test_db();
        // Quotes in user input should be properly escaped
        let results = search_by_description(db.connection(), "say \"hello\"").unwrap();
        assert!(
            results.is_empty(),
            "Quoted text should be handled without error"
        );
    }

    #[test]
    fn test_escape_fts5_query() {
        // Test the helper function directly
        assert_eq!(escape_fts5_query("python"), "\"python\"");
        assert_eq!(escape_fts5_query("NOT python"), "\"NOT python\"");
        assert_eq!(escape_fts5_query("say \"hello\""), "\"say \"\"hello\"\"\"");
        assert_eq!(escape_fts5_query(""), "\"\"");
        assert_eq!(escape_fts5_query("py*"), "\"py*\"");
        assert_eq!(escape_fts5_query("a OR b AND c"), "\"a OR b AND c\"");
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
            store_paths: HashMap::new(),
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
        assert!(cmd.contains("builtins.fetchTarball"));
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
        assert!(cmd.contains("builtins.fetchTarball"));
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

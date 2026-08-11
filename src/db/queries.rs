//! Database query operations for package searches.

use crate::error::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

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

/// Return the exclusive upper bound for a binary-collated prefix range.
///
/// For example, `python` becomes `pythoo`, so callers can express a literal
/// prefix search as `attribute_path >= 'python' AND attribute_path < 'pythoo'`.
/// That lets SQLite seek into the `attribute_path` index instead of scanning it
/// for `LIKE 'python%'`.
fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last = chars.pop()?;
    let next = char::from_u32(last as u32 + 1)?;
    chars.push(next);
    Some(chars.into_iter().collect())
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

/// Boundary for emitting flake-style commands, padded for observation dates.
///
/// flake.nix landed in nixpkgs on 2020-02-10 (NixOS/nixpkgs#68897), but v4
/// rows carry channel-release *observation* dates, which lag the underlying
/// commit by hours to (in stalls) over two weeks. 2020-03-26 sits in the gap
/// between the last nix-env-era release (2020-03-21) and the first
/// packages.json release (2020-03-27): every observation at or after it is
/// guaranteed to reference a flake-capable tree, and emitting the legacy
/// `fetchTarball` form for the handful of early-2020 pre-boundary rows is
/// harmless (it works on any tree).
const FLAKE_EPOCH_TIMESTAMP: i64 = 1585180800; // 2020-03-26 00:00:00 UTC

/// Nix keywords that must be quoted when used as attribute-path segments
/// (`aspellDicts.or` must be emitted as `aspellDicts."or"`).
const NIX_KEYWORDS: &[&str] = &[
    "or", "if", "then", "else", "assert", "with", "let", "in", "rec", "inherit",
];

/// True when a segment is usable bare in a Nix attribute path.
fn is_plain_nix_identifier(segment: &str) -> bool {
    !segment.is_empty()
        && !NIX_KEYWORDS.contains(&segment)
        && segment
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-'))
}

/// Prepare an attribute path for command emission: re-quote segments that
/// aren't valid bare Nix identifiers. Returns the printable path and whether
/// any segment needed quoting (the caller must then shell-quote the ref).
pub(crate) fn nix_attr_for_command(attr: &str) -> (String, bool) {
    let mut quoted_any = false;
    let parts: Vec<String> = attr
        .split('.')
        .map(|segment| {
            if is_plain_nix_identifier(segment) {
                segment.to_string()
            } else {
                quoted_any = true;
                format!(
                    "\"{}\"",
                    segment
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace("${", "\\${")
                )
            }
        })
        .collect();
    (parts.join("."), quoted_any)
}

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
    /// SPDX identifiers. Stored as a JSON array string; serialized as an array.
    #[serde(
        serialize_with = "super::json_array::serialize_opt",
        deserialize_with = "super::json_array::deserialize_opt"
    )]
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// Maintainer handles. Stored as a JSON array string; serialized as an array.
    #[serde(
        serialize_with = "super::json_array::serialize_opt",
        deserialize_with = "super::json_array::deserialize_opt"
    )]
    pub maintainers: Option<String>,
    /// Supported systems. Stored as a JSON array string; serialized as an array.
    #[serde(
        serialize_with = "super::json_array::serialize_opt",
        deserialize_with = "super::json_array::deserialize_opt"
    )]
    pub platforms: Option<String>,
    /// Source file path relative to nixpkgs root
    pub source_path: Option<String>,
    /// Known security vulnerabilities or EOL notices. Stored as a JSON array
    /// string; serialized as an array.
    #[serde(
        serialize_with = "super::json_array::serialize_opt",
        deserialize_with = "super::json_array::deserialize_opt"
    )]
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
    ///
    /// Commands always embed the full commit hash: `nix` resolves `github:` refs
    /// through GitHub's API, which rejects abbreviated SHAs that are ambiguous
    /// in nixpkgs' ~1M-commit history (issue #21).
    pub fn nix_shell_cmd(&self) -> String {
        let insecure_prefix = if self.is_insecure() {
            "NIXPKGS_ALLOW_INSECURE=1 "
        } else {
            ""
        };

        let (attr, attr_quoted) = nix_attr_for_command(&self.attribute_path);

        if self.predates_flakes() {
            format!(
                "{}nix-shell -p '(import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}'",
                insecure_prefix, self.last_commit_hash, attr
            )
        } else {
            let impure_flag = if self.is_insecure() { " --impure" } else { "" };
            let flake_ref = format!("nixpkgs/{}#{}", self.last_commit_hash, attr);
            // Quoted attr segments contain double quotes the shell would eat.
            let flake_ref = if attr_quoted {
                format!("'{flake_ref}'")
            } else {
                flake_ref
            };
            format!("{insecure_prefix}nix shell{impure_flag} {flake_ref}")
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

        let (attr, attr_quoted) = nix_attr_for_command(&self.attribute_path);

        if self.predates_flakes() {
            format!(
                "{}nix-shell -p '(import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{}.tar.gz\") {{}}).{}' --run {}",
                insecure_prefix, self.last_commit_hash, attr, self.attribute_path
            )
        } else {
            let impure_flag = if self.is_insecure() { " --impure" } else { "" };
            let flake_ref = format!("nixpkgs/{}#{}", self.last_commit_hash, attr);
            let flake_ref = if attr_quoted {
                format!("'{flake_ref}'")
            } else {
                flake_ref
            };
            format!("{insecure_prefix}nix run{impure_flag} {flake_ref}")
        }
    }

    /// Check if the package has known vulnerabilities.
    pub fn is_insecure(&self) -> bool {
        has_vulnerabilities(self.known_vulnerabilities.as_deref())
    }

    /// Get parsed known vulnerabilities as a vector of strings.
    pub fn vulnerabilities(&self) -> Vec<String> {
        self.known_vulnerabilities
            .as_deref()
            .map(super::json_array::parse)
            .unwrap_or_default()
    }
}

/// Whether a stored `known_vulnerabilities` value represents a real advisory.
///
/// The column holds a JSON array, but nixpkgs eras and ingestion paths spell "no
/// advisory" several ways — SQL `NULL`, the empty string, whitespace, `[]`, and
/// the `null` / `None` sentinels. Deferring to [`json_array::parse`] keeps this
/// answer identical to what the value actually renders as, so a version can
/// never be flagged insecure and then display an empty advisory list.
///
/// This is the single definition of "insecure". The SQL in
/// [`get_version_history`] approximates it with `NOT IN ('', '[]', 'null')` as a
/// cheap pre-filter; anything that slips through is dropped here.
pub fn has_vulnerabilities(value: Option<&str>) -> bool {
    value.is_some_and(|v| !super::json_array::parse(v).is_empty())
}

/// Per-channel snapshot ingestion coverage (schema v4 indexes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCoverageStat {
    pub channel: String,
    pub releases_ingested: i64,
    pub releases_pending: i64,
    /// Of `releases_pending`, those from the pre-2020 nix-env era. Ordinary
    /// runs never retry these — they are only ingested under
    /// `nxv index --backfill-evals`, which needs `nix` on the machine.
    #[serde(default)]
    pub releases_pending_eval_era: i64,
    pub releases_failed: i64,
    pub releases_skipped: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_release_date: Option<DateTime<Utc>>,
}

/// Metadata keys for cached aggregate stats. They are populated by the indexer
/// and publisher after package writes settle, so read-only `stats` calls don't
/// have to rescan the full v4 package table.
const META_STATS_TOTAL_RANGES: &str = "stats_total_ranges";
const META_STATS_UNIQUE_NAMES: &str = "stats_unique_names";
const META_STATS_UNIQUE_VERSIONS: &str = "stats_unique_versions";
const META_STATS_OLDEST_COMMIT_DATE: &str = "stats_oldest_commit_date";
const META_STATS_NEWEST_COMMIT_DATE: &str = "stats_newest_commit_date";
const META_STATS_CALCULATED_AT: &str = "stats_calculated_at";

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
    /// Per-channel release coverage. Empty for pre-v4 indexes and old
    /// servers (serde(default) keeps old clients/servers compatible).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelCoverageStat>,
}

/// Search for packages by name.
/// Hard cap on rows any single search query materializes.
///
/// With 144k+ attrs (nested sets included), a broad prefix like `python`
/// matches tens of thousands of rows; the search layer paginates to at most
/// 100 per response, so pulling more than this from SQLite is pure waste.
/// High enough that page-through and post-sort stay correct in practice.
const SEARCH_SQL_CAP: usize = 5000;

/// Ranking expression: shallower attribute paths first (top-level packages
/// before `python313Packages.*`), then recency.
const ATTR_DEPTH_RANK: &str = "(LENGTH(attribute_path) - LENGTH(REPLACE(attribute_path, '.', '')))";

pub fn search_by_name(
    conn: &rusqlite::Connection,
    name: &str,
    exact: bool,
) -> Result<Vec<PackageVersion>> {
    let sql = if exact {
        format!(
            "SELECT * FROM package_versions WHERE name = ? \
             ORDER BY {ATTR_DEPTH_RANK} ASC, last_commit_date DESC LIMIT {SEARCH_SQL_CAP}"
        )
    } else {
        format!(
            "SELECT * FROM package_versions WHERE name LIKE ? ESCAPE '\\' \
             ORDER BY {ATTR_DEPTH_RANK} ASC, last_commit_date DESC LIMIT {SEARCH_SQL_CAP}"
        )
    };

    let pattern = if exact {
        name.to_string()
    } else {
        format!("{}%", escape_like_pattern(name))
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([&pattern], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Search for packages by exact attribute path.
pub fn search_by_attr_exact(
    conn: &rusqlite::Connection,
    attr_path: &str,
) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path = ? \
         ORDER BY last_commit_date DESC",
    )?;
    let rows = stmt.query_map([attr_path], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Search for packages by exact attribute path, restricted to a version prefix.
///
/// This is the `--exact` counterpart to [`search_by_name_version`]: the
/// attribute path must match exactly while the version is still a prefix match
/// (`2.7` matches `2.7.18`), matching the documented flag semantics — `--exact`
/// constrains the attribute, never the version.
///
/// Also backs `nxv info <pkg> <version>`, which resolves the package as an exact
/// attribute path before considering any prefix fallback.
///
/// No row cap is applied: a single attribute path holds at most one row per
/// distinct version, so the result set is inherently small.
pub fn search_by_attr_exact_version(
    conn: &rusqlite::Connection,
    attr_path: &str,
    version: &str,
) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM package_versions WHERE attribute_path = ? \
           AND version LIKE ? ESCAPE '\\' \
         ORDER BY last_commit_date DESC",
    )?;
    let version_pattern = format!("{}%", escape_like_pattern(version));
    let rows = stmt.query_map([attr_path, &version_pattern], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Reports whether an attribute path is known to the index at any version.
///
/// Callers use this to tell two different lookup failures apart: a package name the
/// index has never seen (which may be a partial name worth widening into a prefix
/// search) versus a known package that simply lacks the requested version (which
/// should stay a precise "not found" instead of widening).
///
/// # Examples
///
/// ```no_run
/// # use rusqlite::Connection;
/// # let conn = Connection::open_in_memory().unwrap();
/// // Against a populated index, `python311` is known while a typo is not.
/// if attribute_path_exists(&conn, "python311")? {
///     println!("python311 is a known attribute path");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn attribute_path_exists(conn: &rusqlite::Connection, attr_path: &str) -> Result<bool> {
    let mut stmt =
        conn.prepare("SELECT 1 FROM package_versions WHERE attribute_path = ? LIMIT 1")?;
    Ok(stmt.exists([attr_path])?)
}

/// Search for packages by attribute path prefix.
pub fn search_by_attr(conn: &rusqlite::Connection, attr_path: &str) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(&format!(
        "WITH candidates AS MATERIALIZED ( \
             SELECT id, {ATTR_DEPTH_RANK} AS attr_depth, last_commit_date AS rank_date \
               FROM package_versions \
              WHERE attribute_path LIKE ? ESCAPE '\\' \
              ORDER BY attr_depth ASC, last_commit_date DESC, id ASC \
              LIMIT {SEARCH_SQL_CAP} \
         ) \
         SELECT pv.* \
           FROM candidates c \
           JOIN package_versions pv ON pv.id = c.id \
          ORDER BY c.attr_depth ASC, c.rank_date DESC, c.id ASC"
    ))?;
    let pattern = format!("{}%", escape_like_pattern(attr_path));
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
    // Search by attribute_path (package) and version prefix. Keep SQLite LIKE's
    // historical ASCII-case-insensitive behavior here so versioned and
    // unversioned prefix searches agree for mixed-case package-set segments
    // such as `python313Packages`.
    let mut stmt = conn.prepare(&format!(
        "WITH candidates AS MATERIALIZED ( \
             SELECT id, {ATTR_DEPTH_RANK} AS attr_depth, first_commit_date AS rank_date \
               FROM package_versions \
              WHERE attribute_path LIKE ? ESCAPE '\\' \
                AND version LIKE ? ESCAPE '\\' \
              ORDER BY attr_depth ASC, first_commit_date DESC, id ASC \
              LIMIT {SEARCH_SQL_CAP} \
         ) \
         SELECT pv.* \
           FROM candidates c \
           JOIN package_versions pv ON pv.id = c.id \
          ORDER BY c.attr_depth ASC, c.rank_date DESC, c.id ASC"
    ))?;
    let package_pattern = format!("{}%", escape_like_pattern(package));
    let version_pattern = format!("{}%", escape_like_pattern(version));
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

/// Return the shallowest attribute-path depth for a literal prefix.
///
/// The derived `package_attrs` table avoids scanning every version row. Test
/// fixtures and hand-built databases may not have refreshed that cache yet, so
/// an empty cache result falls back to the covering package-version index.
pub fn min_attribute_depth(conn: &rusqlite::Connection, package: &str) -> Result<Option<usize>> {
    let package_lc = package.to_ascii_lowercase();
    let upper = prefix_upper_bound(&package_lc);

    let cached_result: rusqlite::Result<Option<i64>> = if let Some(ref upper) = upper {
        conn.query_row(
            &format!(
                "SELECT MIN({ATTR_DEPTH_RANK}) FROM package_attrs \
                 WHERE attribute_path_lc >= ? AND attribute_path_lc < ?"
            ),
            [&package_lc, upper],
            |row| row.get(0),
        )
    } else {
        Ok(None)
    };
    let cached = match cached_result {
        Ok(depth) => depth,
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("no such table: package_attrs")
                || message.contains("no such column: attribute_path_lc") =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(depth) = cached {
        return Ok(Some(depth as usize));
    }

    let pattern = format!("{}%", escape_like_pattern(package));
    let depth: Option<i64> = conn.query_row(
        &format!(
            "SELECT MIN({ATTR_DEPTH_RANK}) FROM package_versions \
             WHERE attribute_path LIKE ? ESCAPE '\\'"
        ),
        [&pattern],
        |row| row.get(0),
    )?;
    Ok(depth.map(|value| value as usize))
}

/// Search a package/version prefix only at one attribute-path depth.
pub fn search_by_name_version_at_depth(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
    depth: usize,
) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(&format!(
        "WITH candidates AS MATERIALIZED ( \
             SELECT id, first_commit_date AS rank_date \
               FROM package_versions \
              WHERE attribute_path LIKE ? ESCAPE '\\' \
                AND version LIKE ? ESCAPE '\\' \
                AND {ATTR_DEPTH_RANK} = ? \
              ORDER BY first_commit_date DESC, id ASC \
              LIMIT {SEARCH_SQL_CAP} \
         ) \
         SELECT pv.* \
           FROM candidates c \
           JOIN package_versions pv ON pv.id = c.id \
          ORDER BY c.rank_date DESC, c.id ASC"
    ))?;
    let package_pattern = format!("{}%", escape_like_pattern(package));
    let version_pattern = format!("{}%", escape_like_pattern(version));
    let rows = stmt.query_map(
        rusqlite::params![package_pattern, version_pattern, depth as i64],
        PackageVersion::from_row,
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Fetch a bounded pool of versions from one resolved attribute scope.
///
/// This is used only to build miss suggestions. Rows sharing the requested
/// version family are selected first so an intentionally small candidate cap
/// still contains useful historical versions.
pub fn search_version_suggestion_candidates(
    conn: &rusqlite::Connection,
    package: &str,
    depth: usize,
    preferred_version_prefix: &str,
    limit: usize,
) -> Result<Vec<PackageVersion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM package_versions \
          WHERE attribute_path LIKE ? ESCAPE '\\' \
            AND {ATTR_DEPTH_RANK} = ? \
          ORDER BY CASE WHEN version LIKE ? ESCAPE '\\' THEN 0 ELSE 1 END, \
                   CASE WHEN attribute_path = ? THEN 0 ELSE 1 END, \
                   last_commit_date DESC, id ASC \
          LIMIT ?"
    ))?;
    let package_pattern = format!("{}%", escape_like_pattern(package));
    let version_pattern = format!("{}%", escape_like_pattern(preferred_version_prefix));
    let rows = stmt.query_map(
        rusqlite::params![
            package_pattern,
            depth as i64,
            version_pattern,
            package,
            limit as i64
        ],
        PackageVersion::from_row,
    )?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Whether the requested version exists below the resolved attribute depth.
pub fn deeper_version_match_exists(
    conn: &rusqlite::Connection,
    package: &str,
    version: &str,
    depth: usize,
) -> Result<bool> {
    let package_pattern = format!("{}%", escape_like_pattern(package));
    let version_pattern = format!("{}%", escape_like_pattern(version));
    let mut stmt = conn.prepare(&format!(
        "SELECT 1 FROM package_versions \
          WHERE attribute_path LIKE ? ESCAPE '\\' \
            AND version LIKE ? ESCAPE '\\' \
            AND {ATTR_DEPTH_RANK} > ? \
          LIMIT 1"
    ))?;
    Ok(stmt.exists(rusqlite::params![
        package_pattern,
        version_pattern,
        depth as i64
    ])?)
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

/// One version in a package's history, with the advisory that applies to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionHistoryEntry {
    pub version: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Whether nixpkgs reports a known advisory for this version.
    pub insecure: bool,
    /// The advisory JSON array covering this version, or `None` when clean.
    ///
    /// Sourced from this `(attribute_path, version)` row when it carries one,
    /// otherwise from the newest sibling attribute packaging the same `name` at
    /// the same `version` — see [`get_version_history`] for why.
    ///
    /// `None` while `insecure` is `true` only happens against a remote server
    /// that predates this field and sends the flag by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vulnerabilities: Option<String>,
}

impl VersionHistoryEntry {
    /// Build an entry from a stored `known_vulnerabilities` value, deriving the
    /// flag so the two can never disagree.
    pub fn from_advisory(
        version: String,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
        vulnerabilities: Option<String>,
    ) -> Self {
        let vulnerabilities = vulnerabilities.filter(|v| has_vulnerabilities(Some(v)));
        Self {
            version,
            first_seen,
            last_seen,
            insecure: vulnerabilities.is_some(),
            vulnerabilities,
        }
    }

    /// Whether this version has a known advisory in nixpkgs.
    pub fn is_insecure(&self) -> bool {
        self.insecure
    }

    /// Advisory strings for display, empty when none are known.
    pub fn vulnerability_list(&self) -> Vec<String> {
        self.vulnerabilities
            .as_deref()
            .map(super::json_array::parse)
            .unwrap_or_default()
    }
}

/// Retrieves the version history for a package attribute path.
///
/// Returns each distinct version for the given `package` (matched against `attribute_path`)
/// along with the earliest `first_commit_date`, the latest `last_commit_date` for that version,
/// and the advisory that applies to it. Results are ordered by `first_seen` descending.
///
/// # Advisory scope
///
/// The advisory is looked up by `(name, version)` — the package's pname and version,
/// **not** the attribute path and **not** the version string alone.
///
/// Matching on the version string alone is wrong: `nxv` 0.7.1 is unrelated to
/// `zeronet` 0.7.1, and flagging one because of the other tainted 125k rows (issue #80).
///
/// Matching on the attribute path alone is also wrong: nixpkgs packages the same
/// software under several attributes, and `known_vulnerabilities` follows the newest
/// observation of each `(attribute_path, version)` row without being backfilled. So
/// `emacs` 28.2 retired in 2023 holding `NULL` while `emacs28` 28.2 carried the same
/// build until 2025 and picked up CVE-2024-53920. The `name` scope recovers those.
///
/// # Arguments
///
/// * `package` - The package attribute path to filter versions by.
///
/// # Examples
///
/// ```
/// // assumes `conn` is an open rusqlite::Connection
/// let history = get_version_history(&conn, "python").unwrap();
/// for entry in history {
///     println!(
///         "{}: {} - {} (insecure: {})",
///         entry.version, entry.first_seen, entry.last_seen, entry.is_insecure()
///     );
/// }
/// ```
pub fn get_version_history(
    conn: &rusqlite::Connection,
    package: &str,
) -> Result<Vec<VersionHistoryEntry>> {
    let schema_version = meta_value(conn, "schema_version")?
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    if schema_version < 4 {
        return get_version_history_grouped(conn, package);
    }

    // Schema v4 stores one row per (attribute_path, version), so the historical
    // GROUP BY/MIN/MAX query is redundant. Prefer this row's own advisory and fall
    // back to the newest sibling attribute packaging the same name at the same
    // version (see "Advisory scope" above).
    let mut stmt = conn.prepare(
        r#"
        SELECT pv.version,
               pv.first_commit_date as first_seen,
               pv.last_commit_date as last_seen,
               COALESCE(
                   NULLIF(NULLIF(NULLIF(pv.known_vulnerabilities, ''), '[]'), 'null'),
                   (
                       SELECT iv.known_vulnerabilities
                       FROM package_versions iv
                       WHERE iv.name = pv.name
                         AND iv.version = pv.version
                         AND iv.known_vulnerabilities IS NOT NULL
                         AND iv.known_vulnerabilities NOT IN ('', '[]', 'null')
                       -- attribute_path breaks last_commit_date ties so the
                       -- same index always answers with the same advisory.
                       ORDER BY iv.last_commit_date DESC, iv.attribute_path ASC
                       LIMIT 1
                   )
               ) as vulnerabilities
        FROM package_versions pv
        WHERE pv.attribute_path = ?
        ORDER BY pv.first_commit_date DESC
        "#,
    )?;

    let rows = stmt.query_map([package], version_history_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Shared row mapper for both version-history query shapes.
fn version_history_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VersionHistoryEntry> {
    let version: String = row.get(0)?;
    let first_ts: i64 = row.get(1)?;
    let last_ts: i64 = row.get(2)?;
    let vulnerabilities: Option<String> = row.get(3)?;

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

    Ok(VersionHistoryEntry::from_advisory(
        version,
        first_seen,
        last_seen,
        vulnerabilities,
    ))
}

fn get_version_history_grouped(
    conn: &rusqlite::Connection,
    package: &str,
) -> Result<Vec<VersionHistoryEntry>> {
    let has_vulnerabilities_column =
        table_has_column(conn, "package_versions", "known_vulnerabilities")?;
    // Pre-v4 indexes hold several rows per (attribute_path, version), so the advisory
    // is aggregated rather than read off a single row. Scoped by (name, version) for
    // the same reasons as the v4 query above.
    //
    // The CTE is restricted to the names this attribute path actually uses. Without
    // that it groups the whole table on every call — the partial vulnerability index
    // is keyed on version alone, so adding `name` forces a full materialization
    // (measured: 19 s per call on a 1.8M-row index, against 0.4 s restricted).
    //
    // MAX() picks lexicographically when one (name, version) carries several
    // distinct advisory texts. That is deterministic but not the newest-wins rule
    // the v4 query uses; the two can only disagree on a genuine tie, and pre-v4
    // indexes are read-only legacy.
    let sql = if has_vulnerabilities_column {
        r#"
        WITH insecure_packages AS (
            SELECT name, version, MAX(known_vulnerabilities) as vulnerabilities
            FROM package_versions
            WHERE name IN (SELECT name FROM package_versions WHERE attribute_path = ?1)
              AND known_vulnerabilities IS NOT NULL
              AND known_vulnerabilities NOT IN ('', '[]', 'null')
            GROUP BY name, version
        )
        SELECT pv.version,
               MIN(pv.first_commit_date) as first_seen,
               MAX(pv.last_commit_date) as last_seen,
               COALESCE(
                   MAX(NULLIF(NULLIF(NULLIF(pv.known_vulnerabilities, ''), '[]'), 'null')),
                   MAX(iv.vulnerabilities)
               ) as vulnerabilities
        FROM package_versions pv
        LEFT JOIN insecure_packages iv
               ON iv.name = pv.name AND iv.version = pv.version
        WHERE pv.attribute_path = ?1
        GROUP BY pv.version
        ORDER BY first_seen DESC
        "#
    } else {
        r#"
        SELECT version,
               MIN(first_commit_date) as first_seen,
               MAX(last_commit_date) as last_seen,
               NULL as vulnerabilities
        FROM package_versions
        WHERE attribute_path = ?
        GROUP BY version
        ORDER BY first_seen DESC
        "#
    };
    let mut stmt = conn.prepare(sql)?;

    let rows = stmt.query_map([package], version_history_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn table_has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("SELECT COUNT(*) > 0 FROM pragma_table_info({table:?}) WHERE name = ?");
    Ok(conn.query_row(&sql, [column], |row| row.get(0))?)
}

fn meta_value(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let result = conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |row| {
        row.get(0)
    });
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn cached_i64(conn: &rusqlite::Connection, key: &str) -> Result<Option<i64>> {
    match meta_value(conn, key)? {
        Some(value) => Ok(value.parse::<i64>().ok()),
        None => Ok(None),
    }
}

struct PackageStatsCache {
    total_ranges: i64,
    unique_names: i64,
    unique_versions: i64,
    oldest: Option<i64>,
    newest: Option<i64>,
}

fn cached_package_stats(conn: &rusqlite::Connection) -> Result<Option<PackageStatsCache>> {
    // Treat the calculated-at marker as the commit marker for the cache. It is
    // written in the same INSERT statement as the values, so missing marker =>
    // old/partial cache and live scans are safer.
    if meta_value(conn, META_STATS_CALCULATED_AT)?.is_none() {
        return Ok(None);
    }
    let Some(total_ranges) = cached_i64(conn, META_STATS_TOTAL_RANGES)? else {
        return Ok(None);
    };
    let Some(unique_names) = cached_i64(conn, META_STATS_UNIQUE_NAMES)? else {
        return Ok(None);
    };
    let Some(unique_versions) = cached_i64(conn, META_STATS_UNIQUE_VERSIONS)? else {
        return Ok(None);
    };
    Ok(Some(PackageStatsCache {
        total_ranges,
        unique_names,
        unique_versions,
        oldest: cached_i64(conn, META_STATS_OLDEST_COMMIT_DATE)?,
        newest: cached_i64(conn, META_STATS_NEWEST_COMMIT_DATE)?,
    }))
}

fn compute_package_stats(conn: &rusqlite::Connection) -> Result<PackageStatsCache> {
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

    let oldest: Option<i64> = conn.query_row(
        "SELECT MIN(first_commit_date) FROM package_versions",
        [],
        |row| row.get(0),
    )?;

    let newest: Option<i64> = conn.query_row(
        "SELECT MAX(last_commit_date) FROM package_versions",
        [],
        |row| row.get(0),
    )?;

    Ok(PackageStatsCache {
        total_ranges,
        unique_names,
        unique_versions,
        oldest,
        newest,
    })
}

/// Refresh cached package-table statistics in `meta`.
///
/// This is intended for index/publish finish steps, after package writes and
/// before WAL checkpoint/compression. Read-only callers automatically fall back
/// to live scans when these keys are absent (older indexes).
pub fn refresh_stats_cache(conn: &rusqlite::Connection) -> Result<()> {
    let stats = compute_package_stats(conn)?;
    let calculated_at = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        INSERT OR REPLACE INTO meta (key, value) VALUES
            (?1, ?2),
            (?3, ?4),
            (?5, ?6),
            (?7, ?8),
            (?9, ?10),
            (?11, ?12)
        "#,
        rusqlite::params![
            META_STATS_TOTAL_RANGES,
            stats.total_ranges.to_string(),
            META_STATS_UNIQUE_NAMES,
            stats.unique_names.to_string(),
            META_STATS_UNIQUE_VERSIONS,
            stats.unique_versions.to_string(),
            META_STATS_OLDEST_COMMIT_DATE,
            stats.oldest.map(|v| v.to_string()).unwrap_or_default(),
            META_STATS_NEWEST_COMMIT_DATE,
            stats.newest.map(|v| v.to_string()).unwrap_or_default(),
            META_STATS_CALCULATED_AT,
            calculated_at,
        ],
    )?;
    Ok(())
}

/// Get index statistics.
pub fn get_stats(conn: &rusqlite::Connection) -> Result<IndexStats> {
    let stats = match cached_package_stats(conn)? {
        Some(stats) => stats,
        None => compute_package_stats(conn)?,
    };

    // Get meta values (backwards compatible - returns None if not present)
    let last_indexed_commit = meta_value(conn, "last_indexed_commit")?;
    let last_indexed_date = meta_value(conn, "last_indexed_date")?;

    Ok(IndexStats {
        total_ranges: stats.total_ranges,
        unique_names: stats.unique_names,
        unique_versions: stats.unique_versions,
        oldest_commit_date: stats
            .oldest
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        newest_commit_date: stats
            .newest
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        last_indexed_commit,
        last_indexed_date,
        channels: get_channel_coverage(conn).unwrap_or_default(),
    })
}

/// Per-channel release coverage from the v4 `releases` ledger. Returns an
/// empty vec on pre-v4 databases (no releases table).
fn get_channel_coverage(conn: &rusqlite::Connection) -> Result<Vec<ChannelCoverageStat>> {
    let has_table: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='releases'",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT r.channel,
               SUM(r.status = 'ingested'),
               SUM(r.status = 'pending'),
               SUM(r.status = 'pending' AND r.source = 'nix_env'
                   AND r.release_date < ?1),
               SUM(r.status = 'failed'),
               SUM(r.status = 'skipped'),
               (SELECT release_name FROM releases n
                 WHERE n.channel = r.channel AND n.status = 'ingested'
                 ORDER BY n.release_date DESC LIMIT 1),
               (SELECT release_date FROM releases n
                 WHERE n.channel = r.channel AND n.status = 'ingested'
                 ORDER BY n.release_date DESC LIMIT 1)
          FROM releases r GROUP BY r.channel ORDER BY r.channel
        "#,
    )?;
    let rows = stmt.query_map([super::releases::eval_era_before().timestamp()], |row| {
        Ok(ChannelCoverageStat {
            channel: row.get(0)?,
            releases_ingested: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            releases_pending: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            releases_pending_eval_era: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            releases_failed: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            releases_skipped: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            newest_release: row.get(6)?,
            newest_release_date: row
                .get::<_, Option<i64>>(7)?
                .and_then(|ts| Utc.timestamp_opt(ts, 0).single()),
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
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
    let mut stmt = conn.prepare(&format!(
        r#"
        SELECT pv.* FROM package_versions pv
        INNER JOIN package_versions_fts fts ON pv.id = fts.rowid
        WHERE package_versions_fts MATCH ?
        ORDER BY pv.last_commit_date DESC LIMIT {SEARCH_SQL_CAP}
        "#
    ))?;

    // Escape user input to prevent FTS5 syntax injection
    let escaped_query = escape_fts5_query(query);
    let rows = stmt.query_map([&escaped_query], PackageVersion::from_row)?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn package_attrs_available(conn: &rusqlite::Connection) -> Result<bool> {
    let has_table: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='package_attrs'",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(false);
    }
    let has_lc: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('package_attrs') WHERE name='attribute_path_lc'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if !has_lc {
        return Ok(false);
    }
    let has_rows: Option<i64> = conn
        .query_row("SELECT 1 FROM package_attrs LIMIT 1", [], |row| row.get(0))
        .ok();
    Ok(has_rows.is_some())
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
    let sql = if package_attrs_available(conn)? {
        "SELECT attribute_path FROM package_attrs ORDER BY attribute_path"
    } else {
        "SELECT DISTINCT attribute_path FROM package_versions ORDER BY attribute_path"
    };
    let mut stmt = conn.prepare(sql)?;
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
/// * `prefix` - The prefix to match against attribute paths. Matching follows SQLite
///   `LIKE` case behavior on legacy tables and is ASCII-case-insensitive when the
///   `package_attrs` cache is available.
/// * `limit` - Maximum number of results to return
pub fn complete_package_prefix(
    conn: &rusqlite::Connection,
    prefix: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut results = Vec::new();
    let use_package_attrs = package_attrs_available(conn)?;
    let table = if use_package_attrs {
        "package_attrs"
    } else {
        "package_versions"
    };
    let distinct = if use_package_attrs { "" } else { "DISTINCT " };

    let lower_prefix = prefix.to_ascii_lowercase();
    if use_package_attrs && let Some(upper) = prefix_upper_bound(&lower_prefix) {
        let mut stmt = conn.prepare(
            "SELECT attribute_path FROM package_attrs \
             WHERE attribute_path_lc >= ? AND attribute_path_lc < ? \
             ORDER BY attribute_path LIMIT ?",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![lower_prefix, upper, limit as i64],
            |row| row.get(0),
        )?;
        for row in rows {
            results.push(row?);
        }
        return Ok(results);
    }

    let sql = format!(
        "SELECT {distinct}attribute_path FROM {table} WHERE attribute_path LIKE ? ESCAPE '\\' ORDER BY attribute_path LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let pattern = format!("{}%", escape_like_pattern(prefix));
    let rows = stmt.query_map(rusqlite::params![&pattern, limit as i64], |row| row.get(0))?;
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
    fn test_search_by_name_version_preserves_like_case_behavior() {
        let (_dir, db) = create_test_db();
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES ('requests', '2.32.0', 'abc', 1700000000, 'def', 1700100000,
                'python313Packages.requests', 'Requests')
            "#,
                [],
            )
            .unwrap();

        let results = search_by_name_version(db.connection(), "python313packages", "2.32").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "python313Packages.requests");
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
    fn test_search_by_attr_exact_excludes_prefix_siblings() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr_exact(db.connection(), "python").unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|p| p.attribute_path == "python"));
    }

    /// Regression for #52: with a version filter present, the exact-attribute
    /// restriction must still hold. `python2` carries 2.7.18 and would be
    /// returned by the prefix-matching version query.
    #[test]
    fn test_search_by_attr_exact_version_excludes_prefix_siblings() {
        let (_dir, db) = create_test_db();

        // The prefix path matches the sibling attr `python2`...
        let prefixed = search_by_name_version(db.connection(), "python", "2.7").unwrap();
        assert_eq!(prefixed.len(), 1);
        assert_eq!(prefixed[0].attribute_path, "python2");

        // ...while the exact path must not: `python` was never at 2.7.x.
        let exact = search_by_attr_exact_version(db.connection(), "python", "2.7").unwrap();
        assert!(
            exact.is_empty(),
            "--exact must not leak sibling attrs, got {:?}",
            exact.iter().map(|p| &p.attribute_path).collect::<Vec<_>>()
        );
    }

    /// The version half of an exact search stays a prefix match: `3.11`
    /// resolves `3.11.0` without requiring the full version string.
    #[test]
    fn test_search_by_attr_exact_version_matches_version_prefix() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr_exact_version(db.connection(), "python", "3.11").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "python");
        assert_eq!(results[0].version, "3.11.0");
    }

    /// LIKE metacharacters in the version filter are literals, not wildcards.
    #[test]
    fn test_search_by_attr_exact_version_escapes_wildcards() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr_exact_version(db.connection(), "python", "%").unwrap();
        assert!(
            results.is_empty(),
            "'%' must be a literal, not a match-all wildcard"
        );
    }

    // --- Exact attribute path + version, `nxv info` side (issue #53) ---

    /// A shorter version prefix may legitimately span several versions of the
    /// same attribute; all of them belong to that one attribute path.
    #[test]
    fn test_search_by_attr_exact_version_matches_multiple_versions() {
        let (_dir, db) = create_test_db();
        let results = search_by_attr_exact_version(db.connection(), "python", "3.1").unwrap();
        assert_eq!(results.len(), 2, "3.1 matches both 3.11.0 and 3.12.0");
        assert!(results.iter().all(|p| p.attribute_path == "python"));
    }

    #[test]
    fn test_attribute_path_exists() {
        let (_dir, db) = create_test_db();
        assert!(attribute_path_exists(db.connection(), "python").unwrap());
        // Prefix siblings must not make an unknown exact path look known.
        assert!(!attribute_path_exists(db.connection(), "pyth").unwrap());
        assert!(!attribute_path_exists(db.connection(), "nonexistent").unwrap());
    }

    // --- Covering prefix-search candidate index (ticket: nxv-covering-prefix-search) ---

    /// The unversioned prefix search must plan through the covering candidate
    /// index rather than scanning the wide `package_versions` table.
    #[test]
    fn test_search_by_attr_uses_covering_prefix_index() {
        let (_dir, db) = create_test_db();
        let sql = format!(
            "EXPLAIN QUERY PLAN WITH candidates AS MATERIALIZED ( \
                 SELECT id, {ATTR_DEPTH_RANK} AS attr_depth, last_commit_date AS rank_date \
                   FROM package_versions \
                  WHERE attribute_path LIKE ? ESCAPE '\\' \
                  ORDER BY attr_depth ASC, last_commit_date DESC, id ASC \
                  LIMIT {SEARCH_SQL_CAP} \
             ) \
             SELECT pv.* FROM candidates c \
             JOIN package_versions pv ON pv.id = c.id \
             ORDER BY c.attr_depth ASC, c.rank_date DESC, c.id ASC"
        );
        let mut stmt = db.connection().prepare(&sql).unwrap();
        let plan: Vec<String> = stmt
            .query_map(["python%"], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert!(
            plan.iter()
                .any(|detail| detail.contains("COVERING INDEX idx_packages_search_nocase")),
            "expected covering prefix index in query plan, got: {plan:?}"
        );
    }

    /// The version-filtered prefix search must plan through the same covering
    /// candidate index.
    #[test]
    fn test_search_by_name_version_uses_covering_prefix_index() {
        let (_dir, db) = create_test_db();
        let sql = format!(
            "EXPLAIN QUERY PLAN WITH candidates AS MATERIALIZED ( \
                 SELECT id, {ATTR_DEPTH_RANK} AS attr_depth, first_commit_date AS rank_date \
                   FROM package_versions \
                  WHERE attribute_path LIKE ? ESCAPE '\\' \
                    AND version LIKE ? ESCAPE '\\' \
                  ORDER BY attr_depth ASC, first_commit_date DESC, id ASC \
                  LIMIT {SEARCH_SQL_CAP} \
             ) \
             SELECT pv.* FROM candidates c \
             JOIN package_versions pv ON pv.id = c.id \
             ORDER BY c.attr_depth ASC, c.rank_date DESC, c.id ASC"
        );
        let mut stmt = db.connection().prepare(&sql).unwrap();
        let plan: Vec<String> = stmt
            .query_map(["python%", "3.11%"], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert!(
            plan.iter()
                .any(|detail| detail.contains("COVERING INDEX idx_packages_search_nocase")),
            "expected covering prefix index in query plan, got: {plan:?}"
        );
    }

    #[test]
    fn test_scoped_version_search_uses_attribute_and_package_covering_indexes() {
        let (_dir, db) = create_test_db();
        db.refresh_package_attrs().unwrap();

        let mut attrs_stmt = db
            .connection()
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT MIN({ATTR_DEPTH_RANK}) \
                 FROM package_attrs \
                 WHERE attribute_path_lc >= ? AND attribute_path_lc < ?"
            ))
            .unwrap();
        let attrs_plan: Vec<String> = attrs_stmt
            .query_map(["python", "pythoo"], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            attrs_plan
                .iter()
                .any(|detail| detail.contains("COVERING INDEX idx_package_attrs_lc")),
            "expected attribute cache index in query plan, got: {attrs_plan:?}"
        );

        let sql = format!(
            "EXPLAIN QUERY PLAN WITH candidates AS MATERIALIZED ( \
                 SELECT id, first_commit_date AS rank_date \
                   FROM package_versions \
                  WHERE attribute_path LIKE ? ESCAPE '\\' \
                    AND version LIKE ? ESCAPE '\\' \
                    AND {ATTR_DEPTH_RANK} = ? \
                  ORDER BY first_commit_date DESC, id ASC \
                  LIMIT {SEARCH_SQL_CAP} \
             ) \
             SELECT pv.* FROM candidates c \
             JOIN package_versions pv ON pv.id = c.id \
             ORDER BY c.rank_date DESC, c.id ASC"
        );
        let mut packages_stmt = db.connection().prepare(&sql).unwrap();
        let packages_plan: Vec<String> = packages_stmt
            .query_map(rusqlite::params!["python%", "3.11%", 0], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            packages_plan
                .iter()
                .any(|detail| detail.contains("COVERING INDEX idx_packages_search_nocase")),
            "expected package covering index in query plan, got: {packages_plan:?}"
        );
    }

    /// An old v4 database that predates the covering index must still return
    /// identical results through the scan fallback.
    #[test]
    fn test_search_by_attr_is_compatible_without_covering_index() {
        let (_dir, db) = create_test_db();
        let indexed_ids: Vec<i64> = search_by_attr(db.connection(), "python")
            .unwrap()
            .into_iter()
            .map(|package| package.id)
            .collect();

        db.connection()
            .execute("DROP INDEX idx_packages_search_nocase", [])
            .unwrap();
        let fallback_ids: Vec<i64> = search_by_attr(db.connection(), "python")
            .unwrap()
            .into_iter()
            .map(|package| package.id)
            .collect();

        assert_eq!(fallback_ids, indexed_ids);
    }

    /// The version-filtered path is equally correct without the covering index.
    #[test]
    fn test_search_by_name_version_is_compatible_without_covering_index() {
        let (_dir, db) = create_test_db();
        let indexed_ids: Vec<i64> = search_by_name_version(db.connection(), "python", "3.1")
            .unwrap()
            .into_iter()
            .map(|package| package.id)
            .collect();

        db.connection()
            .execute("DROP INDEX idx_packages_search_nocase", [])
            .unwrap();
        let fallback_ids: Vec<i64> = search_by_name_version(db.connection(), "python", "3.1")
            .unwrap()
            .into_iter()
            .map(|package| package.id)
            .collect();

        assert_eq!(fallback_ids, indexed_ids);
        assert_eq!(indexed_ids.len(), 2); // python 3.11.0 and 3.12.0
    }

    /// When attribute depth and rank date tie, `id ASC` is the deterministic
    /// final tie-breaker for the unversioned path.
    #[test]
    fn test_search_by_attr_deterministic_id_tie() {
        let (_dir, db) = create_test_db();
        // Two rows with identical depth (1 dot) and identical last_commit_date,
        // differing only by insertion order (id).
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('tie-a', '1.0', 'a', 1700000000, 'a', 1700000000, 'tiepkg.alpha', 't'),
                ('tie-b', '1.0', 'b', 1700000000, 'b', 1700000000, 'tiepkg.beta', 't')
            "#,
                [],
            )
            .unwrap();

        let ids: Vec<i64> = search_by_attr(db.connection(), "tiepkg")
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids[0] < ids[1], "expected id ASC tie-break, got {ids:?}");
    }

    /// The version-filtered path applies the same `id ASC` tie-break.
    #[test]
    fn test_search_by_name_version_deterministic_id_tie() {
        let (_dir, db) = create_test_db();
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('tie-a', '9.9.0', 'a', 1700000000, 'a', 1700000000, 'vtie.alpha', 't'),
                ('tie-b', '9.9.0', 'b', 1700000000, 'b', 1700000000, 'vtie.beta', 't')
            "#,
                [],
            )
            .unwrap();

        let ids: Vec<i64> = search_by_name_version(db.connection(), "vtie", "9.9")
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids[0] < ids[1], "expected id ASC tie-break, got {ids:?}");
    }

    /// Literal `_`/`%` in the query must stay literal through the covering
    /// candidate query (no wildcard injection).
    #[test]
    fn test_search_by_attr_escapes_literal_wildcards() {
        let (_dir, db) = create_test_db();
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES ('under', '1.0', 'a', 1700000000, 'a', 1700100000, 'python_test', 'u')
            "#,
                [],
            )
            .unwrap();

        // `python_` must match only the literal underscore row, not `python2`.
        let results = search_by_attr(db.connection(), "python_").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute_path, "python_test");
    }

    /// Mixed-case prefixes match through the ASCII-case-insensitive covering
    /// index, preserving SQLite LIKE's historical behavior.
    #[test]
    fn test_search_by_attr_mixed_case_prefix() {
        let (_dir, db) = create_test_db();
        let upper: Vec<String> = search_by_attr(db.connection(), "PYTHON")
            .unwrap()
            .into_iter()
            .map(|p| p.attribute_path)
            .collect();
        let lower: Vec<String> = search_by_attr(db.connection(), "python")
            .unwrap()
            .into_iter()
            .map(|p| p.attribute_path)
            .collect();
        assert_eq!(upper, lower);
        assert_eq!(upper.len(), 3);
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
        assert_eq!(history[0].version, "3.12.0");
        assert_eq!(history[1].version, "3.11.0");
    }

    /// Build a v4 index holding just the rows a test cares about.
    fn advisory_test_db(rows: &str) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta (key, value) VALUES ('schema_version', '4');
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
                UNIQUE(attribute_path, version)
            );
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path,
                 known_vulnerabilities)
            VALUES {rows};
            "#
        ))
        .unwrap();
        conn
    }

    /// Regression for issue #80: `nxv` 0.7.1 was flagged insecure because
    /// `zeronet` 0.7.1 carries an advisory. A shared version string is not a
    /// shared package — this tainted 125k rows across 72k attribute paths.
    #[test]
    fn test_version_history_ignores_unrelated_package_at_same_version() {
        let conn = advisory_test_db(
            "('nxv', '0.7.1', 'a', 100, 'b', 200, 'nxv', NULL),
             ('zeronet', '0.7.1', 'c', 100, 'd', 200, 'zeronet',
              '[\"Unmaintained. Probable XSS/code injection vulnerability.\"]')",
        );

        let history = get_version_history(&conn, "nxv").unwrap();
        assert_eq!(history.len(), 1);
        assert!(
            !history[0].is_insecure(),
            "nxv 0.7.1 must not inherit zeronet's advisory"
        );
        assert_eq!(history[0].vulnerabilities, None);

        // ...and the package that really is insecure keeps saying so.
        let history = get_version_history(&conn, "zeronet").unwrap();
        assert!(history[0].is_insecure());
    }

    /// The signal the fix must NOT lose. `known_vulnerabilities` follows the
    /// newest observation of each (attribute_path, version) row and is never
    /// backfilled, so `emacs` 28.2 retired in 2023 holding NULL while `emacs28`
    /// 28.2 shipped the same build until 2025 and picked up the CVEs. Scoping by
    /// name recovers it; scoping by attribute_path would not.
    #[test]
    fn test_version_history_inherits_advisory_from_sibling_attribute() {
        let conn = advisory_test_db(
            "('emacs', '28.2', 'a', 100, 'b', 200, 'emacs', NULL),
             ('emacs', '28.2', 'c', 100, 'd', 300, 'emacs28',
              '[\"CVE-2024-53920\"]')",
        );

        let history = get_version_history(&conn, "emacs").unwrap();
        assert_eq!(history.len(), 1);
        assert!(
            history[0].is_insecure(),
            "emacs 28.2 is the same build as emacs28 28.2"
        );
        assert_eq!(
            history[0].vulnerability_list(),
            vec!["CVE-2024-53920".to_string()],
            "the advisory text travels with the flag"
        );
    }

    /// A row's own advisory wins over a sibling's, and the empty spellings of
    /// "no advisory" (NULL, '', '[]', 'null') all read as clean.
    #[test]
    fn test_version_history_prefers_own_advisory_and_ignores_empty_spellings() {
        let conn = advisory_test_db(
            "('pkg', '1.0', 'a', 100, 'b', 200, 'pkg', '[\"OWN-CVE\"]'),
             ('pkg', '1.0', 'c', 100, 'd', 300, 'pkg-alias', '[\"SIBLING-CVE\"]'),
             ('clean', '2.0', 'e', 100, 'f', 200, 'clean-empty', ''),
             ('clean', '2.0', 'g', 100, 'h', 200, 'clean-array', '[]'),
             ('clean', '2.0', 'i', 100, 'j', 200, 'clean-null', 'null')",
        );

        let history = get_version_history(&conn, "pkg").unwrap();
        assert_eq!(
            history[0].vulnerability_list(),
            vec!["OWN-CVE".to_string()],
            "the row's own advisory takes precedence over a newer sibling's"
        );

        for attr in ["clean-empty", "clean-array", "clean-null"] {
            let history = get_version_history(&conn, attr).unwrap();
            assert!(!history[0].is_insecure(), "{attr} should read as clean");
        }
    }

    /// `has_vulnerabilities` must agree with what the value actually renders
    /// as, or a version gets a red flag and an empty advisory list under it.
    #[test]
    fn test_has_vulnerabilities_matches_rendered_output() {
        for clean in [None, Some(""), Some("  "), Some("[]"), Some("null")] {
            assert!(!has_vulnerabilities(clean), "{clean:?} should read clean");
        }
        // Sentinels json_array::parse discards must not count as advisories.
        for sentinel in ["None", " null ", "\n"] {
            assert!(!has_vulnerabilities(Some(sentinel)), "{sentinel:?}");
            assert!(super::super::json_array::parse(sentinel).is_empty());
        }
        assert!(has_vulnerabilities(Some(r#"["CVE-1"]"#)));

        // Whatever the predicate accepts must render as something.
        let entry = VersionHistoryEntry::from_advisory(
            "1.0".to_string(),
            Utc.timestamp_opt(1, 0).unwrap(),
            Utc.timestamp_opt(2, 0).unwrap(),
            Some("None".to_string()),
        );
        assert!(!entry.is_insecure());
        assert!(entry.vulnerability_list().is_empty());
    }

    /// Same two cases against the pre-v4 grouped query, which aggregates several
    /// rows per (attribute_path, version).
    #[test]
    fn test_legacy_version_history_scopes_advisories_by_name() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta (key, value) VALUES ('schema_version', '3');
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
                known_vulnerabilities TEXT
            );
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path,
                 known_vulnerabilities)
            VALUES
                ('nxv', '0.7.1', 'a', 100, 'b', 200, 'nxv', NULL),
                ('nxv', '0.7.1', 'a2', 50, 'b2', 250, 'nxv', NULL),
                ('zeronet', '0.7.1', 'c', 100, 'd', 200, 'zeronet', '["XSS"]'),
                ('emacs', '28.2', 'e', 100, 'f', 200, 'emacs', NULL),
                ('emacs', '28.2', 'g', 100, 'h', 300, 'emacs28', '["CVE-2024-53920"]');
            "#,
        )
        .unwrap();

        let history = get_version_history(&conn, "nxv").unwrap();
        assert_eq!(history.len(), 1, "grouped query still collapses rows");
        assert!(!history[0].is_insecure());

        let history = get_version_history(&conn, "emacs").unwrap();
        assert!(history[0].is_insecure());
        assert_eq!(
            history[0].vulnerability_list(),
            vec!["CVE-2024-53920".to_string()]
        );
    }

    #[test]
    fn test_get_version_history_empty() {
        let (_dir, db) = create_test_db();
        let history = get_version_history(db.connection(), "nonexistent").unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_get_version_history_groups_legacy_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta (key, value) VALUES ('schema_version', '3');
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
                known_vulnerabilities TEXT
            );
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path)
            VALUES
                ('python', '3.11.0', 'a', 100, 'b', 200, 'python'),
                ('python', '3.11.0', 'c', 50, 'd', 300, 'python');
            "#,
        )
        .unwrap();

        let history = get_version_history(&conn, "python").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, "3.11.0");
        assert_eq!(history[0].first_seen, Utc.timestamp_opt(50, 0).unwrap());
        assert_eq!(history[0].last_seen, Utc.timestamp_opt(300, 0).unwrap());
    }

    #[test]
    fn test_get_version_history_legacy_without_vulnerabilities_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta (key, value) VALUES ('schema_version', '2');
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
                source_path TEXT
            );
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date,
                 last_commit_hash, last_commit_date, attribute_path)
            VALUES
                ('python', '3.11.0', 'a', 100, 'b', 200, 'python'),
                ('python', '3.11.0', 'c', 50, 'd', 300, 'python');
            "#,
        )
        .unwrap();

        let history = get_version_history(&conn, "python").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, "3.11.0");
        assert_eq!(history[0].first_seen, Utc.timestamp_opt(50, 0).unwrap());
        assert_eq!(history[0].last_seen, Utc.timestamp_opt(300, 0).unwrap());
        assert!(!history[0].is_insecure());
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
    fn test_get_stats_uses_cache_when_present() {
        let (_dir, db) = create_test_db();
        db.set_meta(META_STATS_TOTAL_RANGES, "123").unwrap();
        db.set_meta(META_STATS_UNIQUE_NAMES, "45").unwrap();
        db.set_meta(META_STATS_UNIQUE_VERSIONS, "67").unwrap();
        db.set_meta(META_STATS_OLDEST_COMMIT_DATE, "1600000000")
            .unwrap();
        db.set_meta(META_STATS_NEWEST_COMMIT_DATE, "1700000000")
            .unwrap();
        db.set_meta(META_STATS_CALCULATED_AT, "2026-01-01T00:00:00Z")
            .unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 123);
        assert_eq!(stats.unique_names, 45);
        assert_eq!(stats.unique_versions, 67);
        assert_eq!(
            stats.oldest_commit_date.unwrap(),
            Utc.timestamp_opt(1600000000, 0).unwrap()
        );
        assert_eq!(
            stats.newest_commit_date.unwrap(),
            Utc.timestamp_opt(1700000000, 0).unwrap()
        );
    }

    #[test]
    fn test_get_stats_ignores_partial_cache_without_marker() {
        let (_dir, db) = create_test_db();
        db.set_meta(META_STATS_TOTAL_RANGES, "123").unwrap();
        db.set_meta(META_STATS_UNIQUE_NAMES, "45").unwrap();
        db.set_meta(META_STATS_UNIQUE_VERSIONS, "67").unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 4);
        assert_eq!(stats.unique_names, 4);
        assert_eq!(stats.unique_versions, 4);
    }

    #[test]
    fn test_refresh_stats_cache_writes_live_counts() {
        let (_dir, db) = create_test_db();
        refresh_stats_cache(db.connection()).unwrap();

        let stats = get_stats(db.connection()).unwrap();
        assert_eq!(stats.total_ranges, 4);
        assert_eq!(stats.unique_names, 4);
        assert_eq!(stats.unique_versions, 4);
        assert!(db.get_meta(META_STATS_CALCULATED_AT).unwrap().is_some());
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
    fn test_get_all_unique_attrs_uses_package_attrs_cache() {
        let (_dir, db) = create_test_db();
        db.refresh_package_attrs().unwrap();

        db.connection()
            .execute("DELETE FROM package_versions", [])
            .unwrap();

        let attrs = get_all_unique_attrs(db.connection()).unwrap();
        assert_eq!(attrs.len(), 3);
        assert!(attrs.contains(&"python".to_string()));
        assert!(attrs.contains(&"python2".to_string()));
        assert!(attrs.contains(&"nodejs".to_string()));
    }

    #[test]
    fn test_complete_package_prefix_uses_package_attrs_cache() {
        let (_dir, db) = create_test_db();
        db.refresh_package_attrs().unwrap();

        db.connection()
            .execute("DELETE FROM package_versions", [])
            .unwrap();

        let results = complete_package_prefix(db.connection(), "python", 10).unwrap();
        assert_eq!(results, vec!["python".to_string(), "python2".to_string()]);
    }

    #[test]
    fn test_complete_package_prefix_cache_is_case_insensitive() {
        let (_dir, db) = create_test_db();
        db.connection()
            .execute(
                r#"
            INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date,
                last_commit_hash, last_commit_date, attribute_path, description)
            VALUES ('requests', '2.32.0', 'abc', 1700000000, 'def', 1700100000,
                'python313Packages.requests', 'Requests')
            "#,
                [],
            )
            .unwrap();
        db.refresh_package_attrs().unwrap();

        let results = complete_package_prefix(db.connection(), "python313packages", 10).unwrap();
        assert_eq!(results, vec!["python313Packages.requests".to_string()]);
    }

    #[test]
    fn test_complete_package_prefix() {
        let (_dir, db) = create_test_db();
        // Test with prefix that matches multiple packages
        let results = complete_package_prefix(db.connection(), "python", 10).unwrap();
        assert_eq!(results.len(), 2); // python, python2
        assert!(results.contains(&"python".to_string()));
        assert!(results.contains(&"python2".to_string()));
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
        assert_eq!(results.len(), 3); // nodejs, python, python2
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
        assert_eq!(results.len(), 3); // python (x2 versions) + python2
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
    fn test_vulnerabilities_invalid_json_is_preserved() {
        let pkg = make_test_package(
            Utc.timestamp_opt(1700000000, 0).unwrap(),
            Some("invalid json".to_string()),
        );
        // Unparseable content is surfaced verbatim rather than dropped:
        // `is_insecure()` already flags such a row, so returning an empty list
        // would print the "has known vulnerabilities" banner with no reason.
        assert!(pkg.is_insecure());
        assert_eq!(pkg.vulnerabilities(), ["invalid json"]);
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
    fn test_nix_attr_for_command_escapes_string_interpolation() {
        let (attribute, quoted) = nix_attr_for_command(r#"set.${builtins.readFile "/secret"}"#);

        assert!(quoted);
        assert_eq!(attribute, r#"set."\${builtins"."readFile \"/secret\"}""#);
    }

    #[test]
    fn test_nix_shell_cmd_modern_secure() {
        // Modern commit (after flakes), no vulnerabilities
        let pkg = make_test_package(Utc.timestamp_opt(1700000000, 0).unwrap(), None);
        let cmd = pkg.nix_shell_cmd();
        assert_eq!(cmd, "nix shell nixpkgs/def1234567890#test");
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
        assert!(cmd.contains("nixpkgs/def1234567890#test"));
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
        assert_eq!(cmd, "nix run nixpkgs/def1234567890#test");
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
        assert!(cmd.contains("nixpkgs/def1234567890#test"));
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

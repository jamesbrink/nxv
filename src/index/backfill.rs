//! Backfill missing metadata from nixpkgs.
//!
//! This module provides functionality to update existing database records
//! with metadata (source_path, homepage) extracted from nixpkgs.
//!
//! Two modes are supported:
//! - HEAD mode (default): Extract from current nixpkgs HEAD. Fast but may miss
//!   renamed/removed packages.
//! - Historical mode (--history): Traverse git history to extract metadata from
//!   the original commit where each package first appeared. Slower but accurate.
//!
//! Safe to run while the indexer is running.

use crate::db::Database;
use crate::error::Result;
use crate::index::extractor::{self, AttrPath};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Result of a backfill operation.
#[derive(Debug, Default)]
pub struct BackfillResult {
    /// Number of packages checked.
    pub packages_checked: usize,
    /// Number of records updated.
    pub records_updated: usize,
    /// Number of source_path fields filled.
    pub source_paths_filled: usize,
    /// Number of homepage fields filled.
    pub homepages_filled: usize,
    /// Number of known_vulnerabilities fields filled.
    pub vulnerabilities_filled: usize,
    /// Number of commits processed (historical mode only).
    pub commits_processed: usize,
    /// Whether the operation was interrupted.
    pub was_interrupted: bool,
}

/// Configuration for backfill operation.
#[derive(Debug, Clone)]
pub struct BackfillConfig {
    /// Which fields to backfill.
    pub fields: Vec<String>,
    /// Maximum packages to process (None = all).
    pub limit: Option<usize>,
    /// Dry run mode.
    pub dry_run: bool,
    /// Use historical mode (traverse git history).
    pub use_history: bool,
    /// Filter to packages first seen after this date (YYYY-MM-DD).
    pub since: Option<String>,
    /// Filter to packages first seen before this date (YYYY-MM-DD).
    pub until: Option<String>,
    /// Maximum number of commits to process (historical mode only).
    pub max_commits: Option<usize>,
}

impl Default for BackfillConfig {
    /// Creates a BackfillConfig initialized with the module's sensible defaults.
    ///
    /// The default configuration backfills the `source_path`, `homepage`, and
    /// `known_vulnerabilities` fields, does not impose a processing limit, and
    /// runs in non-dry, non-historical mode with no date filtering.
    fn default() -> Self {
        Self {
            fields: vec![
                "source_path".to_string(),
                "homepage".to_string(),
                "known_vulnerabilities".to_string(),
            ],
            limit: None,
            dry_run: false,
            use_history: false,
            since: None,
            until: None,
            max_commits: None,
        }
    }
}

/// Create a shutdown flag for graceful interruption.
pub fn create_shutdown_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Dispatches a backfill run to populate missing package metadata (source_path and/or homepage).
///
/// Chooses historical mode when `config.use_history` is true and HEAD mode otherwise. The provided
/// `shutdown_flag` can be set to request a graceful interruption of the operation.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// let shutdown = crate::index::backfill::create_shutdown_flag();
/// let cfg = crate::index::backfill::BackfillConfig::default();
/// // Run backfill (may return an error if paths or DB are unavailable).
/// let _ = crate::index::backfill::run_backfill("path/to/nixpkgs", "path/to/db.sqlite", cfg, shutdown);
/// ```
pub fn run_backfill<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<BackfillResult> {
    let db_path = db_path.as_ref();

    // SAFEGUARD: Preserve last_indexed_commit before backfill operations.
    // Backfill should NEVER modify this value - it only updates package metadata.
    // This guards against any unexpected database state changes.
    let preserved_commit = {
        let db = Database::open(db_path)?;
        db.get_meta("last_indexed_commit")?
    };

    let result = if config.use_history {
        run_backfill_historical(nixpkgs_path, db_path, config, shutdown_flag)
    } else {
        run_backfill_head(nixpkgs_path, db_path, config, shutdown_flag)
    };

    // SAFEGUARD: Restore last_indexed_commit if it was somehow modified.
    // This should never happen, but we protect against it defensively.
    if let Some(ref original_commit) = preserved_commit {
        let db = Database::open(db_path)?;
        let current_commit = db.get_meta("last_indexed_commit")?;
        if current_commit.as_ref() != Some(original_commit) {
            eprintln!("Warning: last_indexed_commit was unexpectedly modified during backfill.");
            eprintln!(
                "  Expected: {}, Found: {:?}",
                &original_commit[..12.min(original_commit.len())],
                current_commit.as_ref().map(|c| &c[..12.min(c.len())])
            );
            eprintln!("  Restoring original value to preserve indexer checkpoint.");
            db.set_meta("last_indexed_commit", original_commit)?;
        }
    }

    result
}

/// Backfills missing package metadata (source_path and/or homepage) by extracting values from the current nixpkgs HEAD.
///
/// Performs extraction in batches, updates matching database records, and returns metrics about the operation.
/// If `config.dry_run` is true, no changes are written and a preview is printed. Interruption via `shutdown_flag` stops processing and is reflected in the returned result.
///
/// # Returns
///
/// A `BackfillResult` summarizing the operation: number of packages checked, records updated, source paths filled, homepages filled, whether processing was interrupted, and commits processed (always zero for HEAD mode).
///
/// # Examples
///
/// ```ignore
/// use std::sync::Arc;
/// use std::sync::atomic::AtomicBool;
/// use crate::index::backfill::{run_backfill_head, BackfillConfig, create_shutdown_flag};
///
/// let nixpkgs_path = "/path/to/nixpkgs";
/// let db_path = "/path/to/database.sqlite";
/// let config = BackfillConfig::default(); // backfills source_path and homepage by default
/// let shutdown = create_shutdown_flag();
///
/// // Run head-mode backfill (this is a long-running operation)
/// let result = run_backfill_head(nixpkgs_path, db_path, config, shutdown).unwrap();
/// println!("Updated {} records", result.records_updated);
/// ```
fn run_backfill_head<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<BackfillResult> {
    let nixpkgs_path = nixpkgs_path.as_ref();
    let db = Database::open(&db_path)?;

    // Determine which fields to backfill
    let backfill_source_path =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "source_path");
    let backfill_homepage =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "homepage");
    let backfill_vulnerabilities =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "known_vulnerabilities");

    // Get unique attribute paths that need backfilling
    let attrs_to_backfill = get_attrs_needing_backfill(
        db.connection(),
        backfill_source_path,
        backfill_homepage,
        backfill_vulnerabilities,
        config.limit,
    )?;

    if attrs_to_backfill.is_empty() {
        println!("No packages need backfilling.");
        return Ok(BackfillResult::default());
    }

    println!(
        "Found {} unique packages needing metadata backfill",
        attrs_to_backfill.len()
    );

    if config.dry_run {
        println!("Dry run mode - no changes will be made");
        for (attr, _) in attrs_to_backfill.iter().take(20) {
            println!("  Would backfill: {}", attr);
        }
        if attrs_to_backfill.len() > 20 {
            println!("  ... and {} more", attrs_to_backfill.len() - 20);
        }
        return Ok(BackfillResult {
            packages_checked: attrs_to_backfill.len(),
            ..Default::default()
        });
    }

    // Extract metadata from current nixpkgs
    println!("Extracting metadata from current nixpkgs HEAD...");
    let attr_list: Vec<String> = attrs_to_backfill.into_keys().collect();

    // Process in batches to avoid memory issues
    let batch_size = 1000;
    let mut result = BackfillResult::default();

    let progress = ProgressBar::new(attr_list.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
            .unwrap()
            .progress_chars("█▓▒░  "),
    );

    for batch in attr_list.chunks(batch_size) {
        // Check for interruption
        if shutdown_flag.load(Ordering::SeqCst) {
            result.was_interrupted = true;
            progress.finish_with_message("Interrupted");
            return Ok(result);
        }

        let batch_vec: Vec<String> = batch.to_vec();

        // Convert attr strings to AttrPath instances for nested package support
        // (e.g., "python3Packages.numpy" → AttrPath(["python3Packages", "numpy"]))
        let attr_paths: Vec<AttrPath> = batch_vec.iter().map(|s| AttrPath::parse(s)).collect();

        // Extract from x86_64-linux (most common)
        let packages = match extractor::extract_packages_for_attr_paths(
            nixpkgs_path,
            "x86_64-linux",
            &attr_paths,
        ) {
            Ok(pkgs) => pkgs,
            Err(e) => {
                progress.println(format!("Warning: Extraction failed for batch: {}", e));
                continue;
            }
        };

        // Build lookup map
        let mut metadata_map: HashMap<String, PackageMetadata> = HashMap::new();
        for pkg in packages {
            metadata_map.insert(
                pkg.attribute_path.clone(),
                (
                    pkg.source_path.clone(),
                    pkg.homepage.clone(),
                    pkg.known_vulnerabilities_json(),
                ),
            );
        }

        // Update database
        let updates = apply_backfill_updates(
            db.connection(),
            &metadata_map,
            backfill_source_path,
            backfill_homepage,
            backfill_vulnerabilities,
            None, // No commit filter for HEAD mode
        )?;

        result.records_updated += updates.0;
        result.source_paths_filled += updates.1;
        result.homepages_filled += updates.2;
        result.vulnerabilities_filled += updates.3;
        result.packages_checked += batch.len();

        progress.set_position(result.packages_checked as u64);
        progress.set_message(format!(
            "{} updated, {} source, {} home, {} vuln",
            result.records_updated,
            result.source_paths_filled,
            result.homepages_filled,
            result.vulnerabilities_filled
        ));
    }

    progress.finish_with_message(format!("Done! {} records updated", result.records_updated));

    Ok(result)
}

/// Traverses the nixpkgs Git history and backfills missing package metadata in the database.
///
/// For each commit that introduced one or more attributes, this checks out that commit,
/// extracts metadata available at that commit (e.g., `source_path`, `homepage`), and
/// updates database records whose `first_commit` matches the processed commit. The
/// operation respects the provided `BackfillConfig` (which controls which fields to
/// update, limits, and dry-run mode) and can be interrupted by setting the provided
/// shutdown flag.
///
/// # Examples
///
/// ```
/// use crate::index::backfill::{BackfillConfig, create_shutdown_flag, run_backfill_historical};
///
/// let shutdown = create_shutdown_flag();
/// let config = BackfillConfig::default();
/// // `nixpkgs_path` and `db_path` must point to a local nixpkgs repository and a database file.
/// let _result = run_backfill_historical("path/to/nixpkgs", "path/to/db.sqlite", config, shutdown).unwrap();
/// ```
fn run_backfill_historical<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<BackfillResult> {
    use crate::index::git::{NixpkgsRepo, WorktreeSession};

    let nixpkgs_path = nixpkgs_path.as_ref();
    let db = Database::open(&db_path)?;
    let repo = NixpkgsRepo::open(nixpkgs_path)?;

    // Determine which fields to backfill
    let backfill_source_path =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "source_path");
    let backfill_homepage =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "homepage");
    let backfill_vulnerabilities =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "known_vulnerabilities");

    // Get packages grouped by their first_commit (with date filtering)
    let packages_by_commit = get_packages_by_commit(
        db.connection(),
        backfill_source_path,
        backfill_homepage,
        backfill_vulnerabilities,
        config.limit,
        config.since.as_deref(),
        config.until.as_deref(),
    )?;

    if packages_by_commit.is_empty() {
        println!("No packages need backfilling.");
        return Ok(BackfillResult::default());
    }

    let total_packages: usize = packages_by_commit.values().map(|v| v.len()).sum();
    let total_commits = packages_by_commit.len();

    println!(
        "Found {} packages across {} commits needing metadata backfill",
        total_packages, total_commits
    );

    if config.dry_run {
        println!("Dry run mode - no changes will be made");
        for (commit, attrs) in packages_by_commit.iter().take(5) {
            println!("  Commit {}: {} packages", &commit[..12], attrs.len());
            for attr in attrs.iter().take(3) {
                println!("    - {}", attr);
            }
            if attrs.len() > 3 {
                println!("    ... and {} more", attrs.len() - 3);
            }
        }
        if total_commits > 5 {
            println!("  ... and {} more commits", total_commits - 5);
        }
        return Ok(BackfillResult {
            packages_checked: total_packages,
            commits_processed: total_commits,
            ..Default::default()
        });
    }

    println!("Traversing git history to extract metadata...");
    println!("Using worktree to avoid modifying main nixpkgs checkout.");

    // Apply max_commits limit
    let commits_to_process: Vec<_> = if let Some(max) = config.max_commits {
        packages_by_commit.into_iter().take(max).collect()
    } else {
        packages_by_commit.into_iter().collect()
    };

    let total_commits = commits_to_process.len();
    let mut result = BackfillResult::default();

    let progress = ProgressBar::new(total_commits as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {pos}/{len} commits ({percent}%) {msg}")
            .unwrap()
            .progress_chars("█▓▒░  "),
    );

    // Get first commit to initialize worktree session
    let first_commit = match commits_to_process.first() {
        Some((commit, _)) => commit.clone(),
        None => {
            progress.finish_with_message("No commits to process");
            return Ok(result);
        }
    };

    // Create worktree session - doesn't modify the main repo
    let session = match WorktreeSession::new(&repo, &first_commit) {
        Ok(s) => s,
        Err(e) => {
            progress.finish_with_message(format!("Failed to create worktree: {}", e));
            return Err(e);
        }
    };

    // Process commits using the worktree session
    for (commit, attr_paths) in &commits_to_process {
        // Check for interruption
        if shutdown_flag.load(Ordering::SeqCst) {
            result.was_interrupted = true;
            progress.finish_with_message("Interrupted");
            // Worktree cleanup happens automatically via Drop
            return Ok(result);
            // WorktreeSession auto-cleans up on drop
        }

        // Checkout the commit in the worktree
        if let Err(e) = session.checkout(commit) {
            progress.println(format!(
                "Warning: Failed to checkout {}: {}",
                &commit[..12.min(commit.len())],
                e
            ));
            continue;
        }

        // Convert attr strings to AttrPath instances for nested package support
        let attr_path_objs: Vec<AttrPath> = attr_paths.iter().map(|s| AttrPath::parse(s)).collect();

        // Extract metadata for these packages from the worktree
        let packages = match extractor::extract_packages_for_attr_paths(
            session.path(),
            "x86_64-linux",
            &attr_path_objs,
        ) {
            Ok(pkgs) => pkgs,
            Err(e) => {
                progress.println(format!(
                    "Warning: Extraction failed for {}: {}",
                    &commit[..12.min(commit.len())],
                    e
                ));
                continue;
            }
        };

        // Build lookup map
        let mut metadata_map: HashMap<String, PackageMetadata> = HashMap::new();
        for pkg in packages {
            metadata_map.insert(
                pkg.attribute_path.clone(),
                (
                    pkg.source_path.clone(),
                    pkg.homepage.clone(),
                    pkg.known_vulnerabilities_json(),
                ),
            );
        }

        // Update database - only update records with this specific first_commit
        let updates = apply_backfill_updates(
            db.connection(),
            &metadata_map,
            backfill_source_path,
            backfill_homepage,
            backfill_vulnerabilities,
            Some(commit),
        )?;

        result.records_updated += updates.0;
        result.source_paths_filled += updates.1;
        result.homepages_filled += updates.2;
        result.vulnerabilities_filled += updates.3;
        result.packages_checked += attr_paths.len();
        result.commits_processed += 1;

        progress.set_position(result.commits_processed as u64);
        progress.set_message(format!(
            "{} pkgs, {} updated",
            result.packages_checked, result.records_updated
        ));
    }

    // WorktreeSession auto-cleans up on drop
    progress.finish_with_message(format!(
        "Done! {} commits, {} records updated",
        result.commits_processed, result.records_updated
    ));

    Ok(result)
}

/// Collects attribute paths that are missing requested metadata and returns each attribute's first commit hash.
///
/// The query filters packages where `source_path`, `homepage`, and/or `known_vulnerabilities` are NULL
/// according to the boolean flags, and returns a map from attribute path to the package's `first_commit_hash`.
/// If no flags are set, an empty map is returned.
fn get_attrs_needing_backfill(
    conn: &rusqlite::Connection,
    need_source_path: bool,
    need_homepage: bool,
    need_vulnerabilities: bool,
    limit: Option<usize>,
) -> Result<HashMap<String, String>> {
    let mut conditions = Vec::new();
    if need_source_path {
        conditions.push("source_path IS NULL");
    }
    if need_homepage {
        conditions.push("homepage IS NULL");
    }
    if need_vulnerabilities {
        conditions.push("known_vulnerabilities IS NULL");
    }

    if conditions.is_empty() {
        return Ok(HashMap::new());
    }

    let where_clause = conditions.join(" OR ");
    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();

    // Get unique attr paths with their first commit
    let sql = format!(
        "SELECT attribute_path, first_commit_hash FROM package_versions WHERE ({}) GROUP BY attribute_path{}",
        where_clause, limit_clause
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut attrs = HashMap::new();
    for row in rows {
        let (attr, commit) = row?;
        attrs.insert(attr, commit);
    }

    Ok(attrs)
}

/// Group package attribute paths by their first commit hash for packages missing specified metadata.
///
/// Queries the `package_versions` table for rows where `source_path`, `homepage`, and/or
/// `known_vulnerabilities` are NULL (based on the boolean flags) and returns a map from each
/// `first_commit_hash` to the list of `attribute_path`s that first appeared in that commit.
/// Optionally filters by `first_commit_date` range.
fn get_packages_by_commit(
    conn: &rusqlite::Connection,
    need_source_path: bool,
    need_homepage: bool,
    need_vulnerabilities: bool,
    limit: Option<usize>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut conditions = Vec::new();
    if need_source_path {
        conditions.push("source_path IS NULL".to_string());
    }
    if need_homepage {
        conditions.push("homepage IS NULL".to_string());
    }
    if need_vulnerabilities {
        conditions.push("known_vulnerabilities IS NULL".to_string());
    }

    if conditions.is_empty() {
        return Ok(HashMap::new());
    }

    // Add date filters
    if let Some(since_date) = since {
        conditions.push(format!("first_commit_date >= '{}'", since_date));
    }
    if let Some(until_date) = until {
        conditions.push(format!("first_commit_date <= '{}'", until_date));
    }

    // First N conditions are field conditions (OR'd), rest are date conditions (AND'd)
    let field_count =
        need_source_path as usize + need_homepage as usize + need_vulnerabilities as usize;
    let (field_conditions, date_conditions) = conditions.split_at(field_count);

    let mut where_clause = format!("({})", field_conditions.join(" OR "));
    for date_cond in date_conditions {
        where_clause.push_str(&format!(" AND {}", date_cond));
    }

    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();

    // Get packages with their first_commit_hash, ordered by commit date for chronological processing
    let sql = format!(
        "SELECT attribute_path, first_commit_hash FROM package_versions WHERE {} ORDER BY first_commit_date{}",
        where_clause, limit_clause
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut by_commit: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (attr, commit) = row?;
        by_commit.entry(commit).or_default().push(attr);
    }

    Ok(by_commit)
}

/// Metadata extracted for a package: (source_path, homepage, known_vulnerabilities_json)
type PackageMetadata = (Option<String>, Option<String>, Option<String>);

/// Apply extracted metadata to package_versions rows in the database.
///
/// Updates the `source_path`, `homepage`, and/or `known_vulnerabilities` columns for rows
/// whose current value is NULL using entries from `metadata`. Each key in `metadata` is an
/// `attribute_path`; the value is a tuple `(source_path, homepage, known_vulnerabilities_json)`
/// where each element is applied only if `Some`.
///
/// If `commit` is `Some`, only rows whose `first_commit_hash` equals that value
/// are eligible for update; otherwise all matching rows for an `attribute_path`
/// are considered.
///
/// # Returns
///
/// A tuple `(total_updated, source_paths_filled, homepages_filled, vulnerabilities_filled)`.
fn apply_backfill_updates(
    conn: &rusqlite::Connection,
    metadata: &HashMap<String, PackageMetadata>,
    update_source_path: bool,
    update_homepage: bool,
    update_vulnerabilities: bool,
    commit: Option<&str>,
) -> Result<(usize, usize, usize, usize)> {
    let mut total_updated = 0;
    let mut source_paths = 0;
    let mut homepages = 0;
    let mut vulnerabilities = 0;

    // Update source_path where NULL
    if update_source_path {
        let sql = if commit.is_some() {
            "UPDATE package_versions SET source_path = ? WHERE attribute_path = ? AND first_commit_hash = ? AND source_path IS NULL"
        } else {
            "UPDATE package_versions SET source_path = ? WHERE attribute_path = ? AND source_path IS NULL"
        };
        let mut stmt = conn.prepare(sql)?;

        for (attr, (source_path, _, _)) in metadata {
            if let Some(path) = source_path {
                let changes = if let Some(c) = commit {
                    stmt.execute(rusqlite::params![path, attr, c])?
                } else {
                    stmt.execute(rusqlite::params![path, attr])?
                };
                if changes > 0 {
                    source_paths += changes;
                    total_updated += changes;
                }
            }
        }
    }

    // Update homepage where NULL
    if update_homepage {
        let sql = if commit.is_some() {
            "UPDATE package_versions SET homepage = ? WHERE attribute_path = ? AND first_commit_hash = ? AND homepage IS NULL"
        } else {
            "UPDATE package_versions SET homepage = ? WHERE attribute_path = ? AND homepage IS NULL"
        };
        let mut stmt = conn.prepare(sql)?;

        for (attr, (_, homepage, _)) in metadata {
            if let Some(hp) = homepage {
                let changes = if let Some(c) = commit {
                    stmt.execute(rusqlite::params![hp, attr, c])?
                } else {
                    stmt.execute(rusqlite::params![hp, attr])?
                };
                if changes > 0 {
                    homepages += changes;
                    total_updated += changes;
                }
            }
        }
    }

    // Update known_vulnerabilities where NULL
    if update_vulnerabilities {
        let sql = if commit.is_some() {
            "UPDATE package_versions SET known_vulnerabilities = ? WHERE attribute_path = ? AND first_commit_hash = ? AND known_vulnerabilities IS NULL"
        } else {
            "UPDATE package_versions SET known_vulnerabilities = ? WHERE attribute_path = ? AND known_vulnerabilities IS NULL"
        };
        let mut stmt = conn.prepare(sql)?;

        for (attr, (_, _, vulns_json)) in metadata {
            if let Some(v) = vulns_json {
                let changes = if let Some(c) = commit {
                    stmt.execute(rusqlite::params![v, attr, c])?
                } else {
                    stmt.execute(rusqlite::params![v, attr])?
                };
                if changes > 0 {
                    vulnerabilities += changes;
                    total_updated += changes;
                }
            }
        }
    }

    Ok((total_updated, source_paths, homepages, vulnerabilities))
}

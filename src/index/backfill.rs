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
use crate::index::extractor;
use crate::index::git::NixpkgsRepo;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::Path;

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
    /// Number of commits processed (historical mode only).
    pub commits_processed: usize,
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
}

impl Default for BackfillConfig {
    fn default() -> Self {
        Self {
            fields: vec!["source_path".to_string(), "homepage".to_string()],
            limit: None,
            dry_run: false,
            use_history: false,
        }
    }
}

/// Run backfill to update missing metadata.
pub fn run_backfill<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
) -> Result<BackfillResult> {
    if config.use_history {
        run_backfill_historical(nixpkgs_path, db_path, config)
    } else {
        run_backfill_head(nixpkgs_path, db_path, config)
    }
}

/// Run backfill from current nixpkgs HEAD (fast mode).
fn run_backfill_head<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
) -> Result<BackfillResult> {
    let nixpkgs_path = nixpkgs_path.as_ref();
    let db = Database::open(&db_path)?;

    // Determine which fields to backfill
    let backfill_source_path =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "source_path");
    let backfill_homepage =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "homepage");

    // Get unique attribute paths that need backfilling
    let attrs_to_backfill = get_attrs_needing_backfill(
        db.connection(),
        backfill_source_path,
        backfill_homepage,
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
        let batch_vec: Vec<String> = batch.to_vec();

        // Extract from x86_64-linux (most common)
        let packages =
            match extractor::extract_packages_for_attrs(nixpkgs_path, "x86_64-linux", &batch_vec) {
                Ok(pkgs) => pkgs,
                Err(e) => {
                    progress.println(format!("Warning: Extraction failed for batch: {}", e));
                    continue;
                }
            };

        // Build lookup map
        let mut metadata_map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for pkg in packages {
            metadata_map.insert(
                pkg.attribute_path.clone(),
                (pkg.source_path.clone(), pkg.homepage.clone()),
            );
        }

        // Update database
        let updates = apply_backfill_updates(
            db.connection(),
            &metadata_map,
            backfill_source_path,
            backfill_homepage,
            None, // No commit filter for HEAD mode
        )?;

        result.records_updated += updates.0;
        result.source_paths_filled += updates.1;
        result.homepages_filled += updates.2;
        result.packages_checked += batch.len();

        progress.set_position(result.packages_checked as u64);
        progress.set_message(format!(
            "{} updated, {} source_paths, {} homepages",
            result.records_updated, result.source_paths_filled, result.homepages_filled
        ));
    }

    progress.finish_with_message(format!("Done! {} records updated", result.records_updated));

    Ok(result)
}

/// Run backfill by traversing git history (accurate mode).
fn run_backfill_historical<P: AsRef<Path>, Q: AsRef<Path>>(
    nixpkgs_path: P,
    db_path: Q,
    config: BackfillConfig,
) -> Result<BackfillResult> {
    let nixpkgs_path = nixpkgs_path.as_ref();
    let db = Database::open(&db_path)?;
    let repo = NixpkgsRepo::open(nixpkgs_path)?;

    // Save original ref to restore later
    let original_ref = repo.head_ref()?;

    // Determine which fields to backfill
    let backfill_source_path =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "source_path");
    let backfill_homepage =
        config.fields.is_empty() || config.fields.iter().any(|f| f == "homepage");

    // Get packages grouped by their first_commit
    let packages_by_commit = get_packages_by_commit(
        db.connection(),
        backfill_source_path,
        backfill_homepage,
        config.limit,
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

    let mut result = BackfillResult::default();

    let progress = ProgressBar::new(total_commits as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {pos}/{len} commits ({percent}%) {msg}")
            .unwrap()
            .progress_chars("█▓▒░  "),
    );

    // Process commits in batches - group nearby commits to reduce checkouts
    // For now, process each commit individually (can optimize later)
    for (commit, attr_paths) in &packages_by_commit {
        // Checkout the commit
        if let Err(e) = repo.checkout_commit(commit) {
            progress.println(format!(
                "Warning: Failed to checkout {}: {}",
                &commit[..12],
                e
            ));
            continue;
        }

        // Extract metadata for these packages
        let packages =
            match extractor::extract_packages_for_attrs(nixpkgs_path, "x86_64-linux", attr_paths) {
                Ok(pkgs) => pkgs,
                Err(e) => {
                    progress.println(format!(
                        "Warning: Extraction failed for {}: {}",
                        &commit[..12],
                        e
                    ));
                    continue;
                }
            };

        // Build lookup map
        let mut metadata_map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for pkg in packages {
            metadata_map.insert(
                pkg.attribute_path.clone(),
                (pkg.source_path.clone(), pkg.homepage.clone()),
            );
        }

        // Update database - only update records with this specific first_commit
        let updates = apply_backfill_updates(
            db.connection(),
            &metadata_map,
            backfill_source_path,
            backfill_homepage,
            Some(commit),
        )?;

        result.records_updated += updates.0;
        result.source_paths_filled += updates.1;
        result.homepages_filled += updates.2;
        result.packages_checked += attr_paths.len();
        result.commits_processed += 1;

        progress.set_position(result.commits_processed as u64);
        progress.set_message(format!(
            "{} pkgs, {} updated",
            result.packages_checked, result.records_updated
        ));
    }

    // Restore original ref
    if let Err(e) = repo.restore_ref(&original_ref) {
        progress.println(format!("Warning: Failed to restore git state: {}", e));
    }

    progress.finish_with_message(format!(
        "Done! {} commits, {} records updated",
        result.commits_processed, result.records_updated
    ));

    Ok(result)
}

/// Get attribute paths that need backfilling, mapped to their first_commit.
/// Returns HashMap<attribute_path, first_commit>.
fn get_attrs_needing_backfill(
    conn: &rusqlite::Connection,
    need_source_path: bool,
    need_homepage: bool,
    limit: Option<usize>,
) -> Result<HashMap<String, String>> {
    let mut conditions = Vec::new();
    if need_source_path {
        conditions.push("source_path IS NULL");
    }
    if need_homepage {
        conditions.push("homepage IS NULL");
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

/// Get packages grouped by their first_commit for historical backfill.
/// Returns HashMap<commit, Vec<attribute_path>>.
fn get_packages_by_commit(
    conn: &rusqlite::Connection,
    need_source_path: bool,
    need_homepage: bool,
    limit: Option<usize>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut conditions = Vec::new();
    if need_source_path {
        conditions.push("source_path IS NULL");
    }
    if need_homepage {
        conditions.push("homepage IS NULL");
    }

    if conditions.is_empty() {
        return Ok(HashMap::new());
    }

    let where_clause = conditions.join(" OR ");
    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();

    // Get packages with their first_commit_hash, ordered by commit for efficient checkout
    let sql = format!(
        "SELECT attribute_path, first_commit_hash FROM package_versions WHERE ({}) ORDER BY first_commit_hash{}",
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

/// Apply backfill updates to the database.
/// If commit is Some, only update records with that first_commit.
/// Returns (total_updated, source_paths_filled, homepages_filled).
fn apply_backfill_updates(
    conn: &rusqlite::Connection,
    metadata: &HashMap<String, (Option<String>, Option<String>)>,
    update_source_path: bool,
    update_homepage: bool,
    commit: Option<&str>,
) -> Result<(usize, usize, usize)> {
    let mut total_updated = 0;
    let mut source_paths = 0;
    let mut homepages = 0;

    // Update source_path where NULL
    if update_source_path {
        let sql = if commit.is_some() {
            "UPDATE package_versions SET source_path = ? WHERE attribute_path = ? AND first_commit_hash = ? AND source_path IS NULL"
        } else {
            "UPDATE package_versions SET source_path = ? WHERE attribute_path = ? AND source_path IS NULL"
        };
        let mut stmt = conn.prepare(sql)?;

        for (attr, (source_path, _)) in metadata {
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

        for (attr, (_, homepage)) in metadata {
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

    Ok((total_updated, source_paths, homepages))
}

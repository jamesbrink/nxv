//! Backfill missing metadata from current nixpkgs state.
//!
//! This module provides functionality to update existing database records
//! with metadata (source_path, homepage) extracted from the current nixpkgs HEAD.
//! Safe to run while the indexer is running.

use crate::db::Database;
use crate::error::Result;
use crate::index::extractor;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
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
}

impl Default for BackfillConfig {
    fn default() -> Self {
        Self {
            fields: vec!["source_path".to_string(), "homepage".to_string()],
            limit: None,
            dry_run: false,
        }
    }
}

/// Run backfill to update missing metadata from current nixpkgs.
pub fn run_backfill<P: AsRef<Path>, Q: AsRef<Path>>(
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
        for attr in attrs_to_backfill.iter().take(20) {
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
    println!("Extracting metadata from current nixpkgs...");
    let attr_list: Vec<String> = attrs_to_backfill.into_iter().collect();

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

/// Get attribute paths that need backfilling.
fn get_attrs_needing_backfill(
    conn: &rusqlite::Connection,
    need_source_path: bool,
    need_homepage: bool,
    limit: Option<usize>,
) -> Result<HashSet<String>> {
    let mut conditions = Vec::new();
    if need_source_path {
        conditions.push("source_path IS NULL");
    }
    if need_homepage {
        conditions.push("homepage IS NULL");
    }

    if conditions.is_empty() {
        return Ok(HashSet::new());
    }

    let where_clause = conditions.join(" OR ");
    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();

    let sql = format!(
        "SELECT DISTINCT attribute_path FROM package_versions WHERE ({}){}",
        where_clause, limit_clause
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut attrs = HashSet::new();
    for row in rows {
        attrs.insert(row?);
    }

    Ok(attrs)
}

/// Apply backfill updates to the database.
/// Returns (total_updated, source_paths_filled, homepages_filled).
fn apply_backfill_updates(
    conn: &rusqlite::Connection,
    metadata: &HashMap<String, (Option<String>, Option<String>)>,
    update_source_path: bool,
    update_homepage: bool,
) -> Result<(usize, usize, usize)> {
    let mut total_updated = 0;
    let mut source_paths = 0;
    let mut homepages = 0;

    // Update source_path where NULL
    if update_source_path {
        let mut stmt = conn.prepare(
            "UPDATE package_versions SET source_path = ? WHERE attribute_path = ? AND source_path IS NULL"
        )?;

        for (attr, (source_path, _)) in metadata {
            if let Some(path) = source_path {
                let changes = stmt.execute(rusqlite::params![path, attr])?;
                if changes > 0 {
                    source_paths += changes;
                    total_updated += changes;
                }
            }
        }
    }

    // Update homepage where NULL
    if update_homepage {
        let mut stmt = conn.prepare(
            "UPDATE package_versions SET homepage = ? WHERE attribute_path = ? AND homepage IS NULL"
        )?;

        for (attr, (_, homepage)) in metadata {
            if let Some(hp) = homepage {
                let changes = stmt.execute(rusqlite::params![hp, attr])?;
                if changes > 0 {
                    homepages += changes;
                    total_updated += changes;
                }
            }
        }
    }

    Ok((total_updated, source_paths, homepages))
}

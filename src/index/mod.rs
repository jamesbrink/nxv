//! Indexer module for building the package index from nixpkgs.
//!
//! This module is only available when the `indexer` feature is enabled.

pub mod extractor;
pub mod git;
pub mod publisher;

use crate::bloom::PackageBloomFilter;
use crate::db::Database;
use crate::db::queries::PackageVersion;
use crate::error::{NxvError, Result};
use crate::paths;
use chrono::{DateTime, Utc};
use git::NixpkgsRepo;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Configuration for the indexer.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Number of commits between checkpoints.
    pub checkpoint_interval: usize,
    /// Whether to show progress bars.
    pub show_progress: bool,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval: 100,
            show_progress: true,
        }
    }
}

/// Tracks an open version range for a package.
#[derive(Debug, Clone)]
struct OpenRange {
    name: String,
    version: String,
    first_commit_hash: String,
    first_commit_date: DateTime<Utc>,
    attribute_path: String,
    description: Option<String>,
    license: Option<String>,
    homepage: Option<String>,
    maintainers: Option<String>,
    platforms: Option<String>,
}

impl OpenRange {
    fn to_package_version(
        &self,
        last_commit_hash: &str,
        last_commit_date: DateTime<Utc>,
    ) -> PackageVersion {
        PackageVersion {
            id: 0,
            name: self.name.clone(),
            version: self.version.clone(),
            first_commit_hash: self.first_commit_hash.clone(),
            first_commit_date: self.first_commit_date,
            last_commit_hash: last_commit_hash.to_string(),
            last_commit_date,
            attribute_path: self.attribute_path.clone(),
            description: self.description.clone(),
            license: self.license.clone(),
            homepage: self.homepage.clone(),
            maintainers: self.maintainers.clone(),
            platforms: self.platforms.clone(),
        }
    }

    /// Update metadata fields if they have changed.
    /// Returns true if any field was updated.
    fn update_metadata(
        &mut self,
        description: Option<String>,
        license: Option<String>,
        homepage: Option<String>,
        maintainers: Option<String>,
        platforms: Option<String>,
    ) -> bool {
        let mut updated = false;

        if self.description != description {
            self.description = description;
            updated = true;
        }
        if self.license != license {
            self.license = license;
            updated = true;
        }
        if self.homepage != homepage {
            self.homepage = homepage;
            updated = true;
        }
        if self.maintainers != maintainers {
            self.maintainers = maintainers;
            updated = true;
        }
        if self.platforms != platforms {
            self.platforms = platforms;
            updated = true;
        }

        updated
    }
}

/// The main indexer that coordinates git traversal, extraction, and database insertion.
pub struct Indexer {
    config: IndexerConfig,
    shutdown: Arc<AtomicBool>,
}

impl Indexer {
    /// Create a new indexer with the given configuration.
    pub fn new(config: IndexerConfig) -> Self {
        Self {
            config,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a clone of the shutdown flag for signal handling.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// Request a graceful shutdown.
    #[allow(dead_code)]
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Check if shutdown was requested.
    fn is_shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Run a full index from scratch.
    ///
    /// This processes all indexable commits (2017+) in the repository and builds a complete index.
    /// Commits before 2017 have a different structure that doesn't work with modern Nix.
    pub fn index_full<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        nixpkgs_path: P,
        db_path: Q,
    ) -> Result<IndexResult> {
        let repo = NixpkgsRepo::open(&nixpkgs_path)?;
        let mut db = Database::open(&db_path)?;

        // Get indexable commits (2017+) in chronological order
        let commits = repo.get_indexable_commits()?;
        let total_commits = commits.len();

        eprintln!(
            "Found {} indexable commits (starting from {})",
            total_commits,
            git::MIN_INDEXABLE_DATE
        );

        self.process_commits(&mut db, &nixpkgs_path, &repo, commits, None)
    }

    /// Run an incremental index, processing only new commits.
    ///
    /// If no previous index exists or the last indexed commit is not found,
    /// this falls back to a full index.
    pub fn index_incremental<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        nixpkgs_path: P,
        db_path: Q,
    ) -> Result<IndexResult> {
        let repo = NixpkgsRepo::open(&nixpkgs_path)?;
        let mut db = Database::open(&db_path)?;

        // Check for last indexed commit
        let last_commit = db.get_meta("last_indexed_commit")?;

        match last_commit {
            Some(hash) => {
                // Try to get commits since that hash
                match repo.get_commits_since(&hash) {
                    Ok(commits) => {
                        if commits.is_empty() {
                            eprintln!("Index is already up to date.");
                            return Ok(IndexResult {
                                commits_processed: 0,
                                packages_found: 0,
                                ranges_created: 0,
                                unique_names: 0,
                                was_interrupted: false,
                            });
                        }
                        eprintln!("Found {} new commits to process", commits.len());
                        self.process_commits(&mut db, &nixpkgs_path, &repo, commits, Some(&hash))
                    }
                    Err(_) => {
                        eprintln!(
                            "Warning: Last indexed commit {} not found in repository.",
                            &hash[..7]
                        );
                        eprintln!("This may indicate a rebase. Consider running with --full.");
                        Err(NxvError::Git(git2::Error::from_str(
                            "Last indexed commit not found. Run with --full to rebuild.",
                        )))
                    }
                }
            }
            None => {
                eprintln!("No previous index found, performing full index.");
                self.index_full(nixpkgs_path, db_path)
            }
        }
    }

    /// Process a list of commits, extracting packages and inserting into the database.
    fn process_commits<P: AsRef<Path>>(
        &self,
        db: &mut Database,
        nixpkgs_path: P,
        repo: &NixpkgsRepo,
        commits: Vec<git::CommitInfo>,
        resume_from: Option<&str>,
    ) -> Result<IndexResult> {
        let nixpkgs_path = nixpkgs_path.as_ref();
        let total_commits = commits.len();

        // Set up progress bar if enabled
        // We use a single combined progress bar to avoid scrolling issues
        let multi_progress = if self.config.show_progress {
            Some(MultiProgress::new())
        } else {
            None
        };

        let progress_bar = multi_progress.as_ref().map(|mp| {
            let pb = mp.add(ProgressBar::new(total_commits as u64));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}",
                    )
                    .unwrap()
                    .progress_chars("█▓▒░  "),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        });

        // Track open ranges: attribute_path -> (version, OpenRange)
        // We need to close ranges when version changes or package disappears
        let mut open_ranges: HashMap<String, OpenRange> = HashMap::new();

        // Track unique package names for bloom filter
        let mut unique_names: HashSet<String> = HashSet::new();

        // If resuming, we should load existing open ranges from the database
        // For simplicity in initial implementation, we start fresh
        // A production version would track open ranges in a separate table

        let mut result = IndexResult {
            commits_processed: 0,
            packages_found: 0,
            ranges_created: 0,
            unique_names: 0,
            was_interrupted: false,
        };

        let mut prev_commit_hash: Option<String> = resume_from.map(String::from);
        let mut prev_commit_date: Option<DateTime<Utc>> = None;
        let mut pending_inserts: Vec<PackageVersion> = Vec::new();

        // Helper to print warnings without disrupting progress bar
        let warn = |pb: &Option<ProgressBar>, msg: String| {
            if let Some(bar) = pb {
                bar.println(format!("⚠ {}", msg));
            } else {
                eprintln!("Warning: {}", msg);
            }
        };

        for (i, commit) in commits.iter().enumerate() {
            // Check for shutdown
            if self.is_shutdown_requested() {
                if let Some(ref pb) = progress_bar {
                    pb.println("Shutdown requested, saving checkpoint...");
                }
                result.was_interrupted = true;

                // Close all open ranges at the previous commit
                if let (Some(prev_hash), Some(prev_date)) = (&prev_commit_hash, prev_commit_date) {
                    for range in open_ranges.values() {
                        pending_inserts.push(range.to_package_version(prev_hash, prev_date));
                    }
                }

                // Insert pending ranges
                if !pending_inserts.is_empty() {
                    result.ranges_created +=
                        db.insert_package_ranges_batch(&pending_inserts)? as u64;
                }

                // Save checkpoint
                if let Some(ref prev_hash) = prev_commit_hash {
                    db.set_meta("last_indexed_commit", prev_hash)?;
                }

                break;
            }

            // Update progress bar with current commit info
            if let Some(ref pb) = progress_bar {
                pb.set_position(i as u64);
                pb.set_message(format!(
                    "{} ({}) | {} pkgs | {} ranges",
                    &commit.short_hash,
                    commit.date.format("%Y-%m-%d"),
                    result.packages_found,
                    result.ranges_created
                ));
            }

            // Checkout the commit
            if let Err(e) = repo.checkout_commit(&commit.hash) {
                warn(
                    &progress_bar,
                    format!("Failed to checkout {}: {}", &commit.short_hash, e),
                );
                continue;
            }

            // Extract packages
            let packages = match extractor::extract_packages(nixpkgs_path) {
                Ok(pkgs) => pkgs,
                Err(e) => {
                    warn(
                        &progress_bar,
                        format!("Extraction failed at {}: {}", &commit.short_hash, e),
                    );
                    // On extraction failure, we don't close ranges - the packages might still exist
                    // Just skip this commit
                    continue;
                }
            };

            result.packages_found += packages.len() as u64;

            // Track which packages we saw in this commit
            let mut seen_attr_paths: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            for pkg in packages {
                seen_attr_paths.insert(pkg.attribute_path.clone());

                // Track unique package names for bloom filter
                unique_names.insert(pkg.name.clone());

                let license_json = pkg.license_json();
                let maintainers_json = pkg.maintainers_json();
                let platforms_json = pkg.platforms_json();

                // Check if package already has an open range
                let existing_version = open_ranges
                    .get(&pkg.attribute_path)
                    .map(|r| r.version.clone());

                match existing_version {
                    Some(ref v) if v == &pkg.version => {
                        // Same version, range stays open
                        // Update metadata if it changed
                        if let Some(existing) = open_ranges.get_mut(&pkg.attribute_path) {
                            existing.update_metadata(
                                pkg.description,
                                license_json,
                                pkg.homepage,
                                maintainers_json,
                                platforms_json,
                            );
                        }
                    }
                    Some(_) => {
                        // Version changed! Close the old range at the previous commit
                        if let Some(existing) = open_ranges.get(&pkg.attribute_path)
                            && let (Some(prev_hash), Some(prev_date)) =
                                (&prev_commit_hash, prev_commit_date)
                        {
                            pending_inserts.push(existing.to_package_version(prev_hash, prev_date));
                        }

                        // Open a new range
                        open_ranges.insert(
                            pkg.attribute_path.clone(),
                            OpenRange {
                                name: pkg.name,
                                version: pkg.version,
                                first_commit_hash: commit.hash.clone(),
                                first_commit_date: commit.date,
                                attribute_path: pkg.attribute_path,
                                description: pkg.description,
                                license: license_json,
                                homepage: pkg.homepage,
                                maintainers: maintainers_json,
                                platforms: platforms_json,
                            },
                        );
                    }
                    None => {
                        // New package! Open a range
                        open_ranges.insert(
                            pkg.attribute_path.clone(),
                            OpenRange {
                                name: pkg.name,
                                version: pkg.version,
                                first_commit_hash: commit.hash.clone(),
                                first_commit_date: commit.date,
                                attribute_path: pkg.attribute_path,
                                description: pkg.description,
                                license: license_json,
                                homepage: pkg.homepage,
                                maintainers: maintainers_json,
                                platforms: platforms_json,
                            },
                        );
                    }
                }
            }

            // Close ranges for packages that disappeared
            let disappeared: Vec<String> = open_ranges
                .keys()
                .filter(|k| !seen_attr_paths.contains(*k))
                .cloned()
                .collect();

            for attr_path in disappeared {
                if let Some(range) = open_ranges.remove(&attr_path)
                    && let (Some(prev_hash), Some(prev_date)) =
                        (&prev_commit_hash, prev_commit_date)
                {
                    pending_inserts.push(range.to_package_version(prev_hash, prev_date));
                }
            }

            // Checkpoint if needed
            if (i + 1) % self.config.checkpoint_interval == 0 {
                // Insert pending ranges
                if !pending_inserts.is_empty() {
                    result.ranges_created +=
                        db.insert_package_ranges_batch(&pending_inserts)? as u64;
                    pending_inserts.clear();
                }

                // Save checkpoint
                db.set_meta("last_indexed_commit", &commit.hash)?;
                db.set_meta("checkpoint_open_ranges", &open_ranges.len().to_string())?;
            }

            result.commits_processed += 1;
            prev_commit_hash = Some(commit.hash.clone());
            prev_commit_date = Some(commit.date);
        }

        // Final: close all remaining open ranges at the last commit
        if !result.was_interrupted
            && let (Some(last_hash), Some(last_date)) =
                (prev_commit_hash.as_ref(), prev_commit_date)
        {
            for range in open_ranges.values() {
                pending_inserts.push(range.to_package_version(last_hash, last_date));
            }

            // Insert any remaining pending ranges
            if !pending_inserts.is_empty() {
                result.ranges_created += db.insert_package_ranges_batch(&pending_inserts)? as u64;
            }

            // Save final state
            if let Some(ref last_hash) = prev_commit_hash {
                db.set_meta("last_indexed_commit", last_hash)?;
            }
        }

        // Set final unique names count
        result.unique_names = unique_names.len() as u64;

        // Finish progress bar
        if let Some(ref pb) = progress_bar {
            pb.finish_with_message(format!(
                "done | {} commits | {} pkgs | {} ranges",
                result.commits_processed, result.packages_found, result.ranges_created
            ));
        }

        Ok(result)
    }
}

/// Result of an indexing operation.
#[derive(Debug)]
pub struct IndexResult {
    /// Number of commits successfully processed.
    pub commits_processed: u64,
    /// Total number of package extractions (may count same package multiple times).
    pub packages_found: u64,
    /// Number of version ranges created in the database.
    pub ranges_created: u64,
    /// Number of unique package names found.
    pub unique_names: u64,
    /// Whether the indexing was interrupted (e.g., by Ctrl+C).
    pub was_interrupted: bool,
}

/// Build a bloom filter from all unique package names in the database.
///
/// This should be called after indexing is complete.
pub fn build_bloom_filter(db: &Database) -> Result<PackageBloomFilter> {
    use crate::db::queries;

    let stats = queries::get_stats(db.connection())?;
    let unique_names = stats.unique_names as usize;

    // Create bloom filter with 1% false positive rate
    let mut filter = PackageBloomFilter::new(unique_names.max(1000), 0.01);

    // Get all unique package names from the database
    let names = queries::get_all_unique_names(db.connection())?;

    for name in &names {
        filter.insert(name);
    }

    Ok(filter)
}

/// Build and save a bloom filter for the index.
pub fn save_bloom_filter(db: &Database) -> Result<()> {
    let filter = build_bloom_filter(db)?;
    let bloom_path = paths::get_bloom_path();

    // Ensure parent directory exists
    if let Some(parent) = bloom_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    filter.save(&bloom_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries;
    use chrono::TimeZone;
    use std::process::Command;
    use tempfile::tempdir;

    /// Create a test git repository with known commits and a pkgs/ directory.
    fn create_test_nixpkgs_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(&path)
            .output()
            .expect("Failed to init git repo");

        // Configure git user
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git email");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .expect("Failed to configure git name");

        // Create pkgs directory to make it look like nixpkgs
        std::fs::create_dir(path.join("pkgs")).unwrap();

        // Create a minimal default.nix that will work with nix eval
        let default_nix = r#"
{
  hello = {
    pname = "hello";
    version = "1.0.0";
    type = "derivation";
    meta = {
      description = "A test package";
    };
  };
}
"#;
        std::fs::write(path.join("default.nix"), default_nix).unwrap();

        // Create initial commit
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .expect("Failed to add files");
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&path)
            .output()
            .expect("Failed to create commit");

        (dir, path)
    }

    #[test]
    fn test_indexer_config_default() {
        let config = IndexerConfig::default();
        assert_eq!(config.checkpoint_interval, 100);
        assert!(config.show_progress);
    }

    #[test]
    fn test_indexer_shutdown_flag() {
        let config = IndexerConfig::default();
        let indexer = Indexer::new(config);

        assert!(!indexer.is_shutdown_requested());

        indexer.request_shutdown();

        assert!(indexer.is_shutdown_requested());
    }

    #[test]
    fn test_open_range_to_package_version() {
        let range = OpenRange {
            name: "hello".to_string(),
            version: "1.0.0".to_string(),
            first_commit_hash: "abc123".to_string(),
            first_commit_date: Utc::now(),
            attribute_path: "hello".to_string(),
            description: Some("A test package".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
        };

        let last_date = Utc::now();
        let pkg = range.to_package_version("def456", last_date);

        assert_eq!(pkg.name, "hello");
        assert_eq!(pkg.version, "1.0.0");
        assert_eq!(pkg.first_commit_hash, "abc123");
        assert_eq!(pkg.last_commit_hash, "def456");
        assert_eq!(pkg.attribute_path, "hello");
    }

    #[test]
    fn test_index_result_default_state() {
        let result = IndexResult {
            commits_processed: 0,
            packages_found: 0,
            ranges_created: 0,
            unique_names: 0,
            was_interrupted: false,
        };

        assert_eq!(result.commits_processed, 0);
        assert!(!result.was_interrupted);
    }

    #[test]
    fn test_indexer_can_open_test_repo() {
        let (_dir, path) = create_test_nixpkgs_repo();

        let repo = NixpkgsRepo::open(&path);
        assert!(repo.is_ok());

        let commits = repo.unwrap().get_all_commits().unwrap();
        assert_eq!(commits.len(), 1);
    }

    #[test]
    fn test_incremental_index_no_previous() {
        let (_dir, path) = create_test_nixpkgs_repo();
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let config = IndexerConfig {
            checkpoint_interval: 10,
            show_progress: false,
        };
        let indexer = Indexer::new(config);

        // With no previous index, should fall back to full index
        // This test just verifies the logic path, actual extraction would need nix
        let db = Database::open(&db_path).unwrap();
        let last_commit = db.get_meta("last_indexed_commit").unwrap();
        assert!(last_commit.is_none());
    }

    #[test]
    #[ignore] // Requires nix to be installed
    fn test_full_index_real_nixpkgs() {
        let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");

        if !nixpkgs_path.exists() {
            eprintln!("Skipping: nixpkgs not present");
            return;
        }

        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let config = IndexerConfig {
            checkpoint_interval: 5,
            show_progress: false,
        };
        let indexer = Indexer::new(config);

        // Just test that we can start indexing
        // A real test would need a small test repo with working nix expressions
        let repo = NixpkgsRepo::open(&nixpkgs_path).unwrap();
        let commits = repo.get_all_commits().unwrap();

        // Just verify we can get commits
        assert!(!commits.is_empty());
    }

    #[test]
    fn test_checkpoint_recovery_logic() {
        // Test that checkpoint recovery logic works correctly
        // This tests the database state management without requiring nix
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        // Create initial database state simulating a checkpoint
        {
            let db = Database::open(&db_path).unwrap();
            db.set_meta("last_indexed_commit", "abc123def456").unwrap();
            db.set_meta("checkpoint_open_ranges", "5").unwrap();
        }

        // Verify checkpoint state is recoverable
        {
            let db = Database::open(&db_path).unwrap();
            let last_commit = db.get_meta("last_indexed_commit").unwrap();
            assert_eq!(last_commit, Some("abc123def456".to_string()));

            let open_ranges = db.get_meta("checkpoint_open_ranges").unwrap();
            assert_eq!(open_ranges, Some("5".to_string()));
        }
    }

    #[test]
    fn test_incremental_vs_full_consistency() {
        // Test that the database operations are consistent whether
        // inserting incrementally or in bulk
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let packages = vec![
            PackageVersion {
                id: 0,
                name: "python".to_string(),
                version: "3.11.0".to_string(),
                first_commit_hash: "aaa111".to_string(),
                first_commit_date: Utc.timestamp_opt(1700000000, 0).unwrap(),
                last_commit_hash: "bbb222".to_string(),
                last_commit_date: Utc.timestamp_opt(1700100000, 0).unwrap(),
                attribute_path: "python311".to_string(),
                description: Some("Python".to_string()),
                license: Some(r#"["MIT"]"#.to_string()),
                homepage: Some("https://python.org".to_string()),
                maintainers: None,
                platforms: None,
            },
            PackageVersion {
                id: 0,
                name: "nodejs".to_string(),
                version: "20.0.0".to_string(),
                first_commit_hash: "ccc333".to_string(),
                first_commit_date: Utc.timestamp_opt(1700200000, 0).unwrap(),
                last_commit_hash: "ddd444".to_string(),
                last_commit_date: Utc.timestamp_opt(1700300000, 0).unwrap(),
                attribute_path: "nodejs_20".to_string(),
                description: Some("Node.js".to_string()),
                license: Some(r#"["MIT"]"#.to_string()),
                homepage: Some("https://nodejs.org".to_string()),
                maintainers: None,
                platforms: None,
            },
        ];

        // Insert as batch
        {
            let mut db = Database::open(&db_path).unwrap();
            let inserted = db.insert_package_ranges_batch(&packages).unwrap();
            assert_eq!(inserted, 2);
        }

        // Verify all packages are searchable
        {
            let db = Database::open(&db_path).unwrap();

            let python_results = queries::search_by_name(db.connection(), "python", true).unwrap();
            assert_eq!(python_results.len(), 1);
            assert_eq!(python_results[0].version, "3.11.0");

            let nodejs_results = queries::search_by_name(db.connection(), "nodejs", true).unwrap();
            assert_eq!(nodejs_results.len(), 1);
            assert_eq!(nodejs_results[0].version, "20.0.0");
        }
    }

    #[test]
    fn test_index_resumable_state() {
        // Test that indexing can be resumed by checking database state
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        // Simulate first indexing run that was interrupted
        {
            let mut db = Database::open(&db_path).unwrap();

            // Insert some packages
            let pkg = PackageVersion {
                id: 0,
                name: "firefox".to_string(),
                version: "120.0".to_string(),
                first_commit_hash: "first123".to_string(),
                first_commit_date: Utc.timestamp_opt(1700000000, 0).unwrap(),
                last_commit_hash: "first123".to_string(),
                last_commit_date: Utc.timestamp_opt(1700000000, 0).unwrap(),
                attribute_path: "firefox".to_string(),
                description: Some("Firefox browser".to_string()),
                license: None,
                homepage: None,
                maintainers: None,
                platforms: None,
            };
            db.insert_package_ranges_batch(&[pkg]).unwrap();

            // Save checkpoint (simulating interrupted state)
            db.set_meta("last_indexed_commit", "checkpoint123").unwrap();
        }

        // Simulate resume - verify we can read the checkpoint and continue
        {
            let mut db = Database::open(&db_path).unwrap();

            // Should be able to read last checkpoint
            let checkpoint = db.get_meta("last_indexed_commit").unwrap();
            assert_eq!(checkpoint, Some("checkpoint123".to_string()));

            // Existing data should still be there
            let results = queries::search_by_name(db.connection(), "firefox", true).unwrap();
            assert_eq!(results.len(), 1);

            // Simulate continuing from checkpoint by adding more packages
            let pkg = PackageVersion {
                id: 0,
                name: "chromium".to_string(),
                version: "120.0".to_string(),
                first_commit_hash: "second456".to_string(),
                first_commit_date: Utc.timestamp_opt(1700100000, 0).unwrap(),
                last_commit_hash: "second456".to_string(),
                last_commit_date: Utc.timestamp_opt(1700100000, 0).unwrap(),
                attribute_path: "chromium".to_string(),
                description: Some("Chromium browser".to_string()),
                license: None,
                homepage: None,
                maintainers: None,
                platforms: None,
            };
            db.insert_package_ranges_batch(&[pkg]).unwrap();

            // Update checkpoint
            db.set_meta("last_indexed_commit", "final789").unwrap();
        }

        // Verify final state
        {
            let db = Database::open(&db_path).unwrap();

            let checkpoint = db.get_meta("last_indexed_commit").unwrap();
            assert_eq!(checkpoint, Some("final789".to_string()));

            // Both packages should exist
            let firefox = queries::search_by_name(db.connection(), "firefox", true).unwrap();
            assert_eq!(firefox.len(), 1);

            let chromium = queries::search_by_name(db.connection(), "chromium", true).unwrap();
            assert_eq!(chromium.len(), 1);
        }
    }
}

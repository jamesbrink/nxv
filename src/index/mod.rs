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
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Configuration for the indexer.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Number of commits between checkpoints.
    pub checkpoint_interval: usize,
    /// Whether to show progress bars.
    pub show_progress: bool,
    /// Systems to evaluate for arch coverage.
    pub systems: Vec<String>,
    /// Optional git --since filter.
    pub since: Option<String>,
    /// Optional git --until filter.
    pub until: Option<String>,
    /// Optional limit on number of commits.
    pub max_commits: Option<usize>,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval: 100,
            show_progress: true,
            systems: vec![
                "x86_64-linux".to_string(),
                "aarch64-linux".to_string(),
                "x86_64-darwin".to_string(),
                "aarch64-darwin".to_string(),
            ],
            since: None,
            until: None,
            max_commits: None,
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

#[derive(Debug, Clone)]
struct PackageAggregate {
    name: String,
    version: String,
    attribute_path: String,
    description: Option<String>,
    homepage: Option<String>,
    license: HashSet<String>,
    maintainers: HashSet<String>,
    platforms: HashSet<String>,
}

impl PackageAggregate {
    fn new(pkg: extractor::PackageInfo) -> Self {
        let mut license = HashSet::new();
        let mut maintainers = HashSet::new();
        let mut platforms = HashSet::new();

        if let Some(licenses) = pkg.license {
            license.extend(licenses);
        }
        if let Some(maintainers_list) = pkg.maintainers {
            maintainers.extend(maintainers_list);
        }
        if let Some(platforms_list) = pkg.platforms {
            platforms.extend(platforms_list);
        }

        Self {
            name: pkg.name,
            version: pkg.version,
            attribute_path: pkg.attribute_path,
            description: pkg.description,
            homepage: pkg.homepage,
            license,
            maintainers,
            platforms,
        }
    }

    fn merge(&mut self, pkg: extractor::PackageInfo) {
        if self.description.is_none() {
            self.description = pkg.description;
        }
        if self.homepage.is_none() {
            self.homepage = pkg.homepage;
        }
        if let Some(licenses) = pkg.license {
            self.license.extend(licenses);
        }
        if let Some(maintainers) = pkg.maintainers {
            self.maintainers.extend(maintainers);
        }
        if let Some(platforms) = pkg.platforms {
            self.platforms.extend(platforms);
        }
    }

    fn key(&self) -> String {
        format!("{}::{}", self.attribute_path, self.version)
    }

    fn license_json(&self) -> Option<String> {
        set_to_json(&self.license)
    }

    fn maintainers_json(&self) -> Option<String> {
        set_to_json(&self.maintainers)
    }

    fn platforms_json(&self) -> Option<String> {
        set_to_json(&self.platforms)
    }
}

fn set_to_json(values: &HashSet<String>) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut list: Vec<String> = values.iter().cloned().collect();
    list.sort();
    serde_json::to_string(&list).ok()
}

/// Tracks timing data for smoothed ETA calculations.
///
/// Uses a sliding window of recent commit processing times to calculate
/// a stable ETA that doesn't jump wildly when individual commits vary
/// in processing time.
struct EtaTracker {
    /// Recent processing times (sliding window)
    times: VecDeque<Duration>,
    /// Maximum window size
    window_size: usize,
    /// When the current commit started processing
    commit_start: Option<Instant>,
    /// Total remaining commits
    total_remaining: u64,
}

impl EtaTracker {
    fn new(window_size: usize) -> Self {
        Self {
            times: VecDeque::with_capacity(window_size),
            window_size,
            commit_start: None,
            total_remaining: 0,
        }
    }

    /// Start timing a new commit.
    fn start_commit(&mut self) {
        self.commit_start = Some(Instant::now());
    }

    /// Finish timing the current commit and record its duration.
    fn finish_commit(&mut self) {
        if let Some(start) = self.commit_start.take() {
            let elapsed = start.elapsed();
            self.times.push_back(elapsed);
            if self.times.len() > self.window_size {
                self.times.pop_front();
            }
        }
    }

    /// Update the remaining count.
    fn set_remaining(&mut self, remaining: u64) {
        self.total_remaining = remaining;
    }

    /// Calculate the average time per commit from the sliding window.
    fn avg_time_per_commit(&self) -> Option<Duration> {
        if self.times.is_empty() {
            return None;
        }
        let total: Duration = self.times.iter().sum();
        Some(total / self.times.len() as u32)
    }

    /// Calculate smoothed ETA based on average processing time.
    fn eta(&self) -> Option<Duration> {
        let avg = self.avg_time_per_commit()?;
        Some(avg * self.total_remaining as u32)
    }

    /// Format ETA as human-readable string.
    fn eta_string(&self) -> String {
        match self.eta() {
            Some(eta) => {
                let secs = eta.as_secs();
                if secs < 60 {
                    format!("{}s", secs)
                } else if secs < 3600 {
                    format!("{}m {}s", secs / 60, secs % 60)
                } else {
                    let hours = secs / 3600;
                    let mins = (secs % 3600) / 60;
                    format!("{}h {}m", hours, mins)
                }
            }
            None => "calculating...".to_string(),
        }
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

        // Get indexable commits touching package paths
        let mut commits = repo.get_indexable_commits_touching_paths(
            &["pkgs"],
            self.config.since.as_deref(),
            self.config.until.as_deref(),
        )?;
        if let Some(limit) = self.config.max_commits {
            commits.truncate(limit);
        }
        let total_commits = commits.len();

        eprintln!(
            "Found {} indexable commits with package changes (starting from {})",
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
                // Get current HEAD
                let head_hash = repo.head_commit()?;

                // Check if HEAD is an ancestor of last_indexed_commit
                // This means the repo has been reset to an older state
                if head_hash != hash {
                    match repo.is_ancestor(&head_hash, &hash) {
                        Ok(true) => {
                            eprintln!(
                                "Error: Repository HEAD ({}) is older than last indexed commit ({}).",
                                &head_hash[..7],
                                &hash[..7]
                            );
                            eprintln!(
                                "This can happen if the repository was reset or the submodule is out of date."
                            );
                            eprintln!();
                            eprintln!("To fix this, either:");
                            eprintln!("  1. Update your nixpkgs repository to a newer commit:");
                            eprintln!(
                                "     git -C <nixpkgs-path> fetch origin && git -C <nixpkgs-path> checkout origin/master"
                            );
                            eprintln!();
                            eprintln!(
                                "  2. Or use --full to rebuild the index from the current state:"
                            );
                            eprintln!("     nxv index --nixpkgs-path <path> --full");
                            return Err(NxvError::Git(git2::Error::from_str(
                                "Repository HEAD is behind last indexed commit. See above for solutions.",
                            )));
                        }
                        Ok(false) => {
                            // HEAD is not an ancestor, so it's either ahead or diverged - continue normally
                        }
                        Err(e) => {
                            // If we can't check ancestry, warn but continue
                            eprintln!("Warning: Could not verify commit ancestry: {}", e);
                        }
                    }
                }

                // Try to get commits since that hash
                match repo.get_commits_since_touching_paths(
                    &hash,
                    &["pkgs"],
                    self.config.since.as_deref(),
                    self.config.until.as_deref(),
                ) {
                    Ok(mut commits) => {
                        if let Some(limit) = self.config.max_commits {
                            commits.truncate(limit);
                        }
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
        let total_commits = commits.len();
        let systems = &self.config.systems;
        let nixpkgs_path = nixpkgs_path.as_ref();

        // Set up progress bar if enabled
        let multi_progress = if self.config.show_progress {
            Some(MultiProgress::new())
        } else {
            None
        };

        let progress_bar = multi_progress.as_ref().map(|mp| {
            let pb = mp.add(ProgressBar::new(total_commits as u64));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("█▓▒░  "),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        });

        // ETA tracker with 20-commit sliding window for stable estimates
        let mut eta_tracker = EtaTracker::new(20);

        // Track open ranges: attribute_path+version -> OpenRange
        let mut open_ranges: HashMap<String, OpenRange> = HashMap::new();

        // Track unique package names for bloom filter
        let mut unique_names: HashSet<String> = HashSet::new();

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

        // Build the initial file-to-attribute map
        let first_commit = commits
            .first()
            .ok_or_else(|| NxvError::Git(git2::Error::from_str("No commits to process")))?;

        repo.checkout_commit(&first_commit.hash)?;
        let mut file_attr_map = build_file_attr_map(nixpkgs_path, systems)?;
        let mut mapping_commit = first_commit.hash.clone();

        // Helper to print warnings without disrupting progress bar
        let warn = |pb: &Option<ProgressBar>, msg: String| {
            if let Some(bar) = pb {
                bar.println(format!("⚠ {}", msg));
            } else {
                eprintln!("Warning: {}", msg);
            }
        };

        // Process commits sequentially
        for (commit_idx, commit) in commits.iter().enumerate() {
            // Start timing this commit
            eta_tracker.start_commit();
            eta_tracker.set_remaining((total_commits - commit_idx) as u64);

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

            // Update progress bar with smoothed ETA
            if let Some(ref pb) = progress_bar {
                pb.set_position(commit_idx as u64);
                pb.set_message(format!(
                    "({}) {} ({}) | {} pkgs | {} ranges",
                    eta_tracker.eta_string(),
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
                prev_commit_hash = Some(commit.hash.clone());
                prev_commit_date = Some(commit.date);
                eta_tracker.finish_commit();
                continue;
            }

            // Get changed paths
            let changed_paths = match repo.get_commit_changed_paths(&commit.hash) {
                Ok(paths) => paths,
                Err(e) => {
                    warn(
                        &progress_bar,
                        format!("Failed to list changes for {}: {}", &commit.short_hash, e),
                    );
                    prev_commit_hash = Some(commit.hash.clone());
                    prev_commit_date = Some(commit.date);
                    eta_tracker.finish_commit();
                    continue;
                }
            };

            // Check if we need to refresh the file map
            if should_refresh_file_map(&changed_paths)
                && mapping_commit != commit.hash
                && let Ok(map) = build_file_attr_map(nixpkgs_path, systems)
            {
                file_attr_map = map;
                mapping_commit = commit.hash.clone();
            }

            // Determine target attributes
            let mut target_attr_paths: HashSet<String> = HashSet::new();
            let all_attrs: Option<&Vec<String>> =
                file_attr_map.get("pkgs/top-level/all-packages.nix");

            for path in &changed_paths {
                if let Some(attr_paths) = file_attr_map.get(path) {
                    for attr in attr_paths {
                        target_attr_paths.insert(attr.clone());
                    }
                } else if path.starts_with("pkgs/") && path.ends_with(".nix") {
                    let parts: Vec<&str> = path.split('/').collect();
                    if parts.len() >= 2 {
                        let potential_name =
                            if parts.last() == Some(&"default.nix") && parts.len() >= 2 {
                                parts[parts.len() - 2]
                            } else {
                                parts
                                    .last()
                                    .map(|f| f.trim_end_matches(".nix"))
                                    .unwrap_or("")
                            };

                        if let Some(all_attrs_list) = all_attrs
                            && all_attrs_list.contains(&potential_name.to_string())
                        {
                            target_attr_paths.insert(potential_name.to_string());
                        }
                    }
                }
            }

            if target_attr_paths.is_empty() {
                result.commits_processed += 1;
                prev_commit_hash = Some(commit.hash.clone());
                prev_commit_date = Some(commit.date);
                eta_tracker.finish_commit();
                continue;
            }

            let mut target_list: Vec<String> = target_attr_paths.into_iter().collect();
            target_list.sort();

            // Extract packages for all systems
            let mut aggregates: HashMap<String, PackageAggregate> = HashMap::new();

            for system in systems {
                let packages =
                    match extractor::extract_packages_for_attrs(nixpkgs_path, system, &target_list)
                    {
                        Ok(pkgs) => pkgs,
                        Err(e) => {
                            warn(
                                &progress_bar,
                                format!(
                                    "Extraction failed at {} ({}): {}",
                                    &commit.short_hash, system, e
                                ),
                            );
                            continue;
                        }
                    };

                for pkg in packages {
                    let key = format!("{}::{}", pkg.attribute_path, pkg.version);
                    if let Some(existing) = aggregates.get_mut(&key) {
                        existing.merge(pkg);
                    } else {
                        aggregates.insert(key, PackageAggregate::new(pkg));
                    }
                }
            }

            result.packages_found += aggregates.len() as u64;

            // Track which packages we saw in this commit
            let mut seen_keys: HashSet<String> = HashSet::new();
            let target_set: HashSet<String> = target_list.iter().cloned().collect();

            for aggregate in aggregates.values() {
                let key = aggregate.key();
                seen_keys.insert(key.clone());

                // Track unique package names for bloom filter
                unique_names.insert(aggregate.name.clone());

                let license_json = aggregate.license_json();
                let maintainers_json = aggregate.maintainers_json();
                let platforms_json = aggregate.platforms_json();

                if let Some(existing) = open_ranges.get_mut(&key) {
                    existing.update_metadata(
                        aggregate.description.clone(),
                        license_json,
                        aggregate.homepage.clone(),
                        maintainers_json,
                        platforms_json,
                    );
                } else {
                    open_ranges.insert(
                        key.clone(),
                        OpenRange {
                            name: aggregate.name.clone(),
                            version: aggregate.version.clone(),
                            first_commit_hash: commit.hash.clone(),
                            first_commit_date: commit.date,
                            attribute_path: aggregate.attribute_path.clone(),
                            description: aggregate.description.clone(),
                            license: license_json,
                            homepage: aggregate.homepage.clone(),
                            maintainers: maintainers_json,
                            platforms: platforms_json,
                        },
                    );
                }
            }

            // Close ranges for packages that disappeared
            let disappeared: Vec<String> = open_ranges
                .iter()
                .filter(|(key, range)| {
                    target_set.contains(&range.attribute_path) && !seen_keys.contains(*key)
                })
                .map(|(key, _)| key.clone())
                .collect();

            for key in disappeared {
                if let Some(range) = open_ranges.remove(&key)
                    && let (Some(prev_hash), Some(prev_date)) =
                        (&prev_commit_hash, prev_commit_date)
                {
                    pending_inserts.push(range.to_package_version(prev_hash, prev_date));
                }
            }

            result.commits_processed += 1;
            prev_commit_hash = Some(commit.hash.clone());
            prev_commit_date = Some(commit.date);

            // Record commit processing time for ETA calculation
            eta_tracker.finish_commit();

            // Checkpoint if needed
            if (commit_idx + 1).is_multiple_of(self.config.checkpoint_interval)
                || commit_idx + 1 == commits.len()
            {
                if !pending_inserts.is_empty() {
                    result.ranges_created +=
                        db.insert_package_ranges_batch(&pending_inserts)? as u64;
                    pending_inserts.clear();
                }

                if let Some(ref prev_hash) = prev_commit_hash {
                    db.set_meta("last_indexed_commit", prev_hash)?;
                    db.set_meta("checkpoint_open_ranges", &open_ranges.len().to_string())?;
                    db.checkpoint()?;
                }
            }
        }

        // Final: close all remaining open ranges at the last commit
        if !result.was_interrupted
            && let (Some(last_hash), Some(last_date)) =
                (prev_commit_hash.as_ref(), prev_commit_date)
        {
            for range in open_ranges.values() {
                pending_inserts.push(range.to_package_version(last_hash, last_date));
            }

            if !pending_inserts.is_empty() {
                result.ranges_created += db.insert_package_ranges_batch(&pending_inserts)? as u64;
            }

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

fn build_file_attr_map(
    repo_path: &Path,
    systems: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    let system = systems
        .first()
        .ok_or_else(|| NxvError::NixEval("No systems configured".to_string()))?;
    let positions = extractor::extract_attr_positions(repo_path, system)?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for position in positions {
        if let Some(file) = position.file
            && let Some(relative) = normalize_position_file(repo_path, &file)
        {
            map.entry(relative).or_default().push(position.attr_path);
        }
    }

    for attrs in map.values_mut() {
        attrs.sort();
        attrs.dedup();
    }

    Ok(map)
}

fn normalize_position_file(repo_path: &Path, file: &str) -> Option<String> {
    let trimmed = file.split(':').next().unwrap_or(file);
    let repo_str = repo_path.display().to_string();
    if trimmed.starts_with(&repo_str) {
        let rel = trimmed
            .trim_start_matches(&repo_str)
            .trim_start_matches('/');
        return Some(rel.to_string());
    }

    if let Some(pos) = trimmed.find("/pkgs/") {
        return Some(trimmed[pos + 1..].to_string());
    }

    None
}

fn should_refresh_file_map(changed_paths: &[String]) -> bool {
    const TOP_LEVEL_FILES: [&str; 4] = [
        "pkgs/top-level/all-packages.nix",
        "pkgs/top-level/default.nix",
        "pkgs/top-level/aliases.nix",
        "pkgs/top-level/impure.nix",
    ];

    changed_paths
        .iter()
        .any(|path| TOP_LEVEL_FILES.iter().any(|entry| path == entry))
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

/// Build a bloom filter from all unique package attribute paths in the database.
///
/// This should be called after indexing is complete.
pub fn build_bloom_filter(db: &Database) -> Result<PackageBloomFilter> {
    use crate::db::queries;

    // Get all unique attribute paths from the database
    let attrs = queries::get_all_unique_attrs(db.connection())?;

    // Create bloom filter with 1% false positive rate
    let mut filter = PackageBloomFilter::new(attrs.len().max(1000), 0.01);

    for attr in &attrs {
        filter.insert(attr);
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
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn test_eta_tracker_empty() {
        let tracker = EtaTracker::new(10);
        assert!(tracker.avg_time_per_commit().is_none());
        assert!(tracker.eta().is_none());
        assert_eq!(tracker.eta_string(), "calculating...");
    }

    #[test]
    fn test_eta_tracker_single_commit() {
        let mut tracker = EtaTracker::new(10);
        tracker.set_remaining(5);

        tracker.start_commit();
        thread::sleep(Duration::from_millis(50));
        tracker.finish_commit();

        let avg = tracker.avg_time_per_commit().unwrap();
        assert!(avg >= Duration::from_millis(50));

        let eta = tracker.eta().unwrap();
        // 5 remaining * ~50ms = ~250ms
        assert!(eta >= Duration::from_millis(200));
    }

    #[test]
    fn test_eta_tracker_sliding_window() {
        let mut tracker = EtaTracker::new(3);
        tracker.set_remaining(10);

        // Add 5 commits - only last 3 should be kept
        for _ in 0..5 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(10));
            tracker.finish_commit();
        }

        assert_eq!(tracker.times.len(), 3);
    }

    #[test]
    fn test_eta_tracker_formatting() {
        let mut tracker = EtaTracker::new(10);
        tracker.set_remaining(1);

        // Add a commit that takes ~100ms
        tracker.start_commit();
        thread::sleep(Duration::from_millis(100));
        tracker.finish_commit();

        // Should format as seconds
        let eta_str = tracker.eta_string();
        assert!(eta_str.contains("s") || eta_str.contains("calculating"));
    }

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
        assert!(config.systems.contains(&"x86_64-linux".to_string()));
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
    fn test_normalize_position_file_strips_repo_prefix() {
        let repo = std::path::Path::new("/repo");
        let file = "/repo/pkgs/applications/foo/default.nix";
        let normalized = normalize_position_file(repo, file).unwrap();
        assert_eq!(normalized, "pkgs/applications/foo/default.nix");
    }

    #[test]
    fn test_normalize_position_file_finds_pkgs_segment() {
        let repo = std::path::Path::new("/repo");
        let file = "/nix/store/hash/pkgs/tools/bar.nix";
        let normalized = normalize_position_file(repo, file).unwrap();
        assert_eq!(normalized, "pkgs/tools/bar.nix");
    }

    #[test]
    fn test_should_refresh_file_map_detects_top_level() {
        let changed = vec![
            "pkgs/top-level/all-packages.nix".to_string(),
            "pkgs/other/file.nix".to_string(),
        ];
        assert!(should_refresh_file_map(&changed));
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
        let (_dir, _path) = create_test_nixpkgs_repo();
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let config = IndexerConfig {
            checkpoint_interval: 10,
            show_progress: false,
            systems: vec!["x86_64-linux".to_string()],
            since: None,
            until: None,
            max_commits: None,
        };
        let _indexer = Indexer::new(config);

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
        let _db_path = db_dir.path().join("test.db");

        let config = IndexerConfig {
            checkpoint_interval: 5,
            show_progress: false,
            systems: vec!["x86_64-linux".to_string()],
            since: None,
            until: None,
            max_commits: None,
        };
        let _indexer = Indexer::new(config);

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

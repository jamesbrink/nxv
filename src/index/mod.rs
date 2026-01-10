//! Indexer module for building the package index from nixpkgs.
//!
//! This module is only available when the `indexer` feature is enabled.
//!
//! The indexer uses UPSERT semantics: one row per (attribute_path, version) pair.
//! When the same package version is seen across multiple commits, the database
//! row is updated to track the earliest first_commit and latest last_commit.

pub mod backfill;
pub mod extractor;
pub mod gc;
pub mod git;
pub mod nix_ffi;
pub mod publisher;
pub mod worker;

use crate::bloom::PackageBloomFilter;
use crate::db::Database;
use crate::db::queries::PackageVersion;
use crate::error::{NxvError, Result};
use chrono::{DateTime, TimeZone, Utc};
use git::{NixpkgsRepo, WorktreeSession};

/// Cutoff date for store path extraction (2020-01-01).
///
/// Store paths are only extracted for commits from this date onwards because:
///
/// 1. **Binary cache availability**: cache.nixos.org has performed garbage collection
///    events that removed "ancient store paths" (announced January 2024). Binaries
///    from 2020+ are generally still available, while older ones are less reliable.
///
/// 2. **Practical utility**: Users wanting historical versions typically need relatively
///    recent ones. Very old packages (pre-2020) often have other issues like incompatible
///    Nix evaluation or missing dependencies.
///
/// 3. **Index size**: Including store paths for all historical packages would
///    significantly increase database size with diminishing returns.
///
/// This date is used by:
/// - `is_after_store_path_cutoff()` to filter during indexing
/// - Documentation in `PackageVersion.store_path` and API responses
///
/// See `docs/specs/store-path-indexing.md` for full rationale.
pub const STORE_PATH_CUTOFF_DATE: (i32, u32, u32) = (2020, 1, 1);

/// Check if a commit date is after the store path extraction cutoff.
///
/// Store paths are only extracted for commits from [`STORE_PATH_CUTOFF_DATE`]
/// onwards because older binaries are unlikely to be in cache.nixos.org.
fn is_after_store_path_cutoff(date: DateTime<Utc>) -> bool {
    let (year, month, day) = STORE_PATH_CUTOFF_DATE;
    let cutoff = Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap();
    date >= cutoff
}
use crate::theme::Themed;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info_span, instrument, trace};

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
    /// Number of parallel worker processes for evaluation.
    /// If None, uses the number of systems for parallel evaluation.
    /// If Some(1), disables parallel evaluation (sequential mode).
    pub worker_count: Option<usize>,
    /// Memory threshold (MiB) before worker restart.
    pub max_memory_mib: usize,
    /// Show verbose output including extraction warnings.
    pub verbose: bool,
    /// Number of checkpoints between garbage collection runs.
    /// Set to 0 to disable automatic garbage collection.
    /// Default: 5 (GC every 500 commits with default checkpoint_interval of 100)
    pub gc_interval: usize,
    /// Minimum available disk space (bytes) before triggering GC.
    /// If available space falls below this, GC runs at next checkpoint.
    /// Default: 10 GB
    pub gc_min_free_bytes: u64,
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
            worker_count: None, // Default: use parallel evaluation with one worker per system
            max_memory_mib: 6 * 1024, // 6 GiB
            verbose: false,
            gc_interval: 20, // GC every 20 checkpoints (2000 commits by default)
            gc_min_free_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
        }
    }
}

#[derive(Debug, Clone)]
struct PackageAggregate {
    name: String,
    version: String,
    /// Source of version information: "direct", "unwrapped", "passthru", "name", or None.
    version_source: Option<String>,
    attribute_path: String,
    description: Option<String>,
    homepage: Option<String>,
    license: HashSet<String>,
    maintainers: HashSet<String>,
    platforms: HashSet<String>,
    source_path: Option<String>,
    known_vulnerabilities: Option<Vec<String>>,
    /// Store paths per architecture
    store_paths: HashMap<String, String>,
}

impl PackageAggregate {
    /// Creates a PackageAggregate from an extracted PackageInfo.
    ///
    /// Initializes the license, maintainers, and platforms as sets populated from the
    /// corresponding optional lists in `pkg`, and copies scalar metadata fields
    /// (name, version, attribute_path, description, homepage, source_path).
    ///
    /// # Examples
    ///
    /// ```
    /// // Construct a minimal PackageInfo for illustration.
    /// let pkg = extractor::PackageInfo {
    ///     name: "foo".to_string(),
    ///     version: "1.0".to_string(),
    ///     attribute_path: "pkgs.foo".to_string(),
    ///     description: Some("Example".to_string()),
    ///     homepage: Some("https://example.org".to_string()),
    ///     license: Some(vec!["MIT".to_string()]),
    ///     maintainers: Some(vec!["alice".to_string()]),
    ///     platforms: Some(vec!["x86_64-linux".to_string()]),
    ///     source_path: Some("pkgs/foo/default.nix".to_string()),
    /// };
    /// let agg = PackageAggregate::new(pkg);
    /// assert_eq!(agg.name, "foo");
    /// assert!(agg.license.contains("MIT"));
    /// ```
    fn new(pkg: extractor::PackageInfo, system: &str) -> Self {
        let mut license = HashSet::new();
        let mut maintainers = HashSet::new();
        let mut platforms = HashSet::new();
        let mut store_paths = HashMap::new();

        if let Some(licenses) = pkg.license {
            license.extend(licenses);
        }
        if let Some(maintainers_list) = pkg.maintainers {
            maintainers.extend(maintainers_list);
        }
        if let Some(platforms_list) = pkg.platforms {
            platforms.extend(platforms_list);
        }
        if let Some(path) = pkg.out_path {
            store_paths.insert(system.to_string(), path);
        }

        Self {
            name: pkg.name,
            // Use empty string for packages without version
            version: pkg.version.unwrap_or_default(),
            version_source: pkg.version_source,
            attribute_path: pkg.attribute_path,
            description: pkg.description,
            homepage: pkg.homepage,
            license,
            maintainers,
            platforms,
            source_path: pkg.source_path,
            known_vulnerabilities: pkg.known_vulnerabilities,
            store_paths,
        }
    }

    /// Merge metadata from an extracted `PackageInfo` into this aggregate.
    ///
    /// This will set `description`, `homepage`, and `source_path` only if they are
    /// currently `None`, and will extend the `license`, `maintainers`, and
    /// `platforms` sets with any values present on `pkg`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashSet;
    ///
    /// // Construct an example aggregate (fields omitted for brevity)
    /// let mut agg = PackageAggregate {
    ///     name: "foo".into(),
    ///     version: "1.0".into(),
    ///     attribute_path: "pkgs.foo".into(),
    ///     description: None,
    ///     homepage: None,
    ///     license: HashSet::new(),
    ///     maintainers: HashSet::new(),
    ///     platforms: HashSet::new(),
    ///     source_path: None,
    /// };
    ///
    /// // Simulated extracted package info with some metadata
    /// let pkg = extractor::PackageInfo {
    ///     name: "foo".into(),
    ///     version: "1.0".into(),
    ///     attribute_path: "pkgs.foo".into(),
    ///     description: Some("A package".into()),
    ///     homepage: Some("https://example/".into()),
    ///     license: Some(HashSet::from(["MIT".into()])),
    ///     maintainers: Some(HashSet::from(["alice".into()])),
    ///     platforms: Some(HashSet::from(["x86_64-linux".into()])),
    ///     source_path: Some("pkgs/foo/default.nix".into()),
    /// };
    ///
    /// agg.merge(pkg);
    ///
    /// assert_eq!(agg.description.as_deref(), Some("A package"));
    /// assert!(agg.license.contains("MIT"));
    /// assert_eq!(agg.source_path.as_deref(), Some("pkgs/foo/default.nix"));
    /// ```
    fn merge(&mut self, pkg: extractor::PackageInfo, system: &str) {
        if self.description.is_none() {
            self.description = pkg.description;
        }
        if self.homepage.is_none() {
            self.homepage = pkg.homepage;
        }
        if self.source_path.is_none() {
            self.source_path = pkg.source_path;
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
        // Merge known_vulnerabilities - keep existing or use new
        if self.known_vulnerabilities.is_none() {
            self.known_vulnerabilities = pkg.known_vulnerabilities;
        }
        // Merge store_path for this system - each architecture gets its own path
        if let Some(path) = pkg.out_path {
            self.store_paths.entry(system.to_string()).or_insert(path);
        }
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

    fn known_vulnerabilities_json(&self) -> Option<String> {
        self.known_vulnerabilities
            .as_ref()
            .filter(|v| !v.is_empty())
            .map(|v| serde_json::to_string(v).unwrap_or_default())
    }

    /// Convert this aggregate into a PackageVersion for database insertion.
    ///
    /// The commit hash and date are used for both first and last commit fields.
    /// When UPSERT is used, the database will update these bounds appropriately.
    fn to_package_version(&self, commit_hash: &str, commit_date: DateTime<Utc>) -> PackageVersion {
        PackageVersion {
            id: 0,
            name: self.name.clone(),
            version: self.version.clone(),
            version_source: self.version_source.clone(),
            first_commit_hash: commit_hash.to_string(),
            first_commit_date: commit_date,
            last_commit_hash: commit_hash.to_string(),
            last_commit_date: commit_date,
            attribute_path: self.attribute_path.clone(),
            description: self.description.clone(),
            license: self.license_json(),
            homepage: self.homepage.clone(),
            maintainers: self.maintainers_json(),
            platforms: self.platforms_json(),
            source_path: self.source_path.clone(),
            known_vulnerabilities: self.known_vulnerabilities_json(),
            store_paths: self.store_paths.clone(),
        }
    }
}

/// Converts a set of strings into a sorted JSON array string.
///
/// Returns `Some` containing the JSON array (with elements sorted lexicographically) if `values` is non-empty, `None` if `values` is empty.
///
/// # Examples
///
/// ```
/// use std::collections::HashSet;
/// let mut s = HashSet::new();
/// s.insert("b".to_string());
/// s.insert("a".to_string());
/// assert_eq!(set_to_json(&s), Some("[\"a\",\"b\"]".to_string()));
/// ```
fn set_to_json(values: &HashSet<String>) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut list: Vec<String> = values.iter().cloned().collect();
    list.sort();
    serde_json::to_string(&list).ok()
}

/// Tracks timing data for smoothed ETA calculations using exponential moving average (EMA).
///
/// Uses EMA blended with median for stability:
/// - Commit processing times vary wildly (some touch 1 file, others touch thousands)
/// - Pure EMA is too volatile, pure median too slow to adapt
/// - Blending provides smooth, stable estimates
pub(super) struct EtaTracker {
    /// Exponential moving average of commit duration (in seconds)
    ema_secs: Option<f64>,
    /// EMA smoothing factor (0.0-1.0, higher = more responsive)
    alpha: f64,
    /// When the current commit started processing
    commit_start: Option<Instant>,
    /// Total remaining commits
    total_remaining: u64,
    /// Number of commits processed (for warm-up)
    commits_processed: u64,
    /// Minimum samples before showing ETA
    warmup_count: u64,
    /// Recent durations for median calculation and outlier detection
    recent_durations: VecDeque<f64>,
    /// Window size for median calculation
    median_window: usize,
}

impl EtaTracker {
    /// Creates an EtaTracker with exponential moving average smoothing.
    ///
    /// `window_size` controls the effective smoothing - larger values mean
    /// more smoothing (less responsive but more stable).
    ///
    /// The EMA alpha is calculated as 2/(window_size+1), which gives:
    /// - window_size=50 -> alpha=0.039 (very smooth)
    /// - window_size=100 -> alpha=0.020 (extremely smooth)
    fn new(window_size: usize) -> Self {
        // Convert window size to EMA alpha: alpha = 2/(N+1)
        // This gives equivalent smoothing to a simple moving average of size N
        let alpha = 2.0 / (window_size as f64 + 1.0);

        Self {
            ema_secs: None,
            alpha,
            commit_start: None,
            total_remaining: 0,
            commits_processed: 0,
            warmup_count: 20, // Don't show ETA until 20 commits processed
            recent_durations: VecDeque::with_capacity(100),
            median_window: 100,
        }
    }

    /// Begin timing for the current commit.
    fn start_commit(&mut self) {
        self.commit_start = Some(Instant::now());
    }

    /// Stops the current commit timer and updates the EMA.
    ///
    /// Uses outlier rejection for very fast commits (skipped commits)
    /// to prevent them from making the ETA too optimistic.
    /// Slow commits are given full weight since they represent real work.
    fn finish_commit(&mut self) {
        if let Some(start) = self.commit_start.take() {
            let elapsed_secs = start.elapsed().as_secs_f64();
            self.commits_processed += 1;

            // Track recent durations for median calculation
            self.recent_durations.push_back(elapsed_secs);
            if self.recent_durations.len() > self.median_window {
                self.recent_durations.pop_front();
            }

            // Calculate median for outlier detection
            let median = self.calculate_median();

            // Only dampen very fast commits (likely skipped commits)
            // Slow commits get full weight - they represent real work and
            // dampening them makes ETA too optimistic
            let effective_alpha = if let Some(med) = median {
                if elapsed_secs < med * 0.1 {
                    // Outlier (very fast): reduce impact to avoid ETA being too optimistic
                    self.alpha * 0.3
                } else {
                    self.alpha
                }
            } else {
                self.alpha
            };

            // Update EMA: new_ema = alpha * sample + (1-alpha) * old_ema
            self.ema_secs = Some(match self.ema_secs {
                Some(current) => effective_alpha * elapsed_secs + (1.0 - effective_alpha) * current,
                None => elapsed_secs, // First sample becomes the initial EMA
            });
        }
    }

    /// Skip the current commit timer without updating the EMA.
    ///
    /// Use this for commits that are skipped early (no packages to extract).
    /// This prevents fast skips from making the ETA too optimistic.
    fn skip_commit(&mut self) {
        self.commit_start = None;
        self.commits_processed += 1;
    }

    /// Calculate median of recent durations for outlier detection.
    fn calculate_median(&self) -> Option<f64> {
        if self.recent_durations.len() < 5 {
            return None;
        }

        let mut sorted: Vec<f64> = self.recent_durations.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mid = sorted.len() / 2;
        if sorted.len().is_multiple_of(2) {
            Some((sorted[mid - 1] + sorted[mid]) / 2.0)
        } else {
            Some(sorted[mid])
        }
    }

    /// Sets the number of remaining commits used to compute the ETA.
    fn set_remaining(&mut self, remaining: u64) {
        self.total_remaining = remaining;
    }

    /// Get the number of commits processed.
    #[allow(dead_code)]
    fn processed(&self) -> u64 {
        self.commits_processed
    }

    /// Calculate percentage complete.
    fn percentage(&self, total: u64) -> f64 {
        if total == 0 {
            return 100.0;
        }
        (self.commits_processed as f64 / total as f64) * 100.0
    }

    /// Returns a formatted progress string with percentage and ETA.
    fn progress_string(&self, total: u64) -> String {
        let pct = self.percentage(total);
        let eta = self.eta_string();
        format!("{:.1}% | {}", pct, eta)
    }

    /// Compute the average duration per commit from the EMA.
    #[allow(dead_code)]
    fn avg_time_per_commit(&self) -> Option<Duration> {
        self.ema_secs.map(Duration::from_secs_f64)
    }

    /// Compute the estimated remaining duration.
    ///
    /// Returns None during warm-up period to avoid showing wildly inaccurate ETAs.
    /// Uses a blend of EMA and median for stability - EMA adapts to trends while
    /// median resists outliers.
    fn eta(&self) -> Option<Duration> {
        // Don't show ETA until we have enough samples
        if self.commits_processed < self.warmup_count {
            return None;
        }

        let ema = self.ema_secs?;
        let median = self.calculate_median()?;

        // Blend EMA (40%) with median (60%) for stability
        // Median is more stable, EMA helps track trends
        let blended_avg = ema * 0.4 + median * 0.6;
        let remaining_secs = blended_avg * self.total_remaining as f64;

        // Cap at reasonable maximum (30 days)
        let max_secs = 30.0 * 24.0 * 3600.0;
        Some(Duration::from_secs_f64(remaining_secs.min(max_secs)))
    }

    /// Returns a human-readable ETA string for the remaining work.
    ///
    /// Shows "warming up..." during initial sample collection,
    /// then provides stable ETA estimates.
    fn eta_string(&self) -> String {
        if self.commits_processed < self.warmup_count {
            let remaining = self.warmup_count - self.commits_processed;
            return format!("warming up ({} more)...", remaining);
        }

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

        // Clean up orphaned worktrees from previous crashed runs
        repo.prune_worktrees()?;

        // Clean up temp eval store from previous runs (silently at startup)
        let temp_store_freed = gc::cleanup_temp_eval_store();

        // Check store health before starting
        if !gc::verify_store() {
            eprintln!(
                "{} Nix store verification failed. Run 'nix-store --verify --repair' to fix.",
                "Warning:".warning()
            );
        }

        // Check available disk space
        if gc::is_store_low_on_space(self.config.gc_min_free_bytes) {
            eprintln!(
                "{} Low disk space detected. Running garbage collection...",
                "Warning:".warning()
            );
            if let Some(duration) = gc::run_garbage_collection() {
                eprintln!(
                    "Garbage collection completed in {:.1}s",
                    duration.as_secs_f64()
                );
            }
        }

        let mut db = Database::open(&db_path)?;

        eprintln!("Performing full index rebuild...");
        eprintln!(
            "Checkpoint interval: {} commits",
            self.config.checkpoint_interval
        );

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
            self.config
                .since
                .as_deref()
                .unwrap_or(git::MIN_INDEXABLE_DATE)
        );

        // Report temp store cleanup after "Found X commits"
        if let Some(bytes) = temp_store_freed
            && bytes > 0
        {
            eprintln!(
                "Cleaned up temp eval store ({:.1} MB freed)",
                bytes as f64 / 1_000_000.0
            );
        }

        self.process_commits(&mut db, &nixpkgs_path, &repo, commits, None)
    }

    /// Run an incremental index, processing only commits that have not yet been indexed.
    ///
    /// If a last indexed commit is recorded in the database this attempts to index commits
    /// since that commit that touch the `pkgs` tree. If the last indexed commit is missing
    /// from the repository or no previous index exists, this falls back to performing a full index.
    /// The function verifies repository ancestry and will error if the repository HEAD is older
    /// than the last indexed commit; ancestry check failures are warned and indexing proceeds when possible.
    ///
    /// # Returns
    ///
    /// `Ok(IndexResult)` containing counts and status for the indexing run; returns `Err` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::Path;
    /// # use crate::index::{Indexer, IndexerConfig};
    /// // Create an indexer and run incremental indexing against paths (example only).
    /// let indexer = Indexer::new(IndexerConfig::default());
    /// let result = indexer.index_incremental("path/to/nixpkgs", "path/to/db");
    /// assert!(result.is_ok());
    /// ```
    pub fn index_incremental<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        nixpkgs_path: P,
        db_path: Q,
    ) -> Result<IndexResult> {
        let repo = NixpkgsRepo::open(&nixpkgs_path)?;

        // Clean up orphaned worktrees from previous crashed runs
        repo.prune_worktrees()?;

        // Clean up temp eval store from previous runs (silently at startup)
        let temp_store_freed = gc::cleanup_temp_eval_store();

        // Check store health before starting
        if !gc::verify_store() {
            eprintln!(
                "{} Nix store verification failed. Run 'nix-store --verify --repair' to fix.",
                "Warning:".warning()
            );
        }

        // Check available disk space
        if gc::is_store_low_on_space(self.config.gc_min_free_bytes) {
            eprintln!(
                "{} Low disk space detected. Running garbage collection...",
                "Warning:".warning()
            );
            if let Some(duration) = gc::run_garbage_collection() {
                eprintln!(
                    "Garbage collection completed in {:.1}s",
                    duration.as_secs_f64()
                );
            }
        }

        let mut db = Database::open(&db_path)?;

        // Check for last indexed commit
        let last_commit = db.get_meta("last_indexed_commit")?;

        match last_commit {
            Some(hash) => {
                eprintln!("Performing incremental index from commit {}...", &hash[..7]);
                eprintln!(
                    "Checkpoint interval: {} commits",
                    self.config.checkpoint_interval
                );

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
                                "     git -C <nixpkgs-path> fetch origin && git -C <nixpkgs-path> checkout origin/nixpkgs-unstable"
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
                            // Still update the indexed date to record when we last checked
                            db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
                            return Ok(IndexResult {
                                commits_processed: 0,
                                packages_found: 0,
                                packages_upserted: 0,
                                unique_names: 0,
                                was_interrupted: false,
                                extraction_failures: 0,
                            });
                        }
                        eprintln!("Found {} new commits to process", commits.len());

                        // Report temp store cleanup after "Found X commits"
                        if let Some(bytes) = temp_store_freed
                            && bytes > 0
                        {
                            eprintln!(
                                "Cleaned up temp eval store ({:.1} MB freed)",
                                bytes as f64 / 1_000_000.0
                            );
                        }

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

    /// Processes a sequence of commits: extracts package metadata for configured systems
    /// and UPSERTs package versions into the database.
    ///
    /// This method iterates the provided commits in order, checking out each commit,
    /// extracting packages for the indexer's configured target systems, and merging per-system
    /// metadata. Package versions are UPSERTed in batches - the database maintains one row
    /// per (attribute_path, version) pair, updating the first/last commit bounds as packages
    /// are seen across multiple commits.
    ///
    /// The method supports graceful shutdown (saving a checkpoint and flushing pending
    /// UPSERTs), periodic checkpoints controlled by the indexer's configuration, and optional
    /// progress reporting with a smoothed ETA. It updates the "last_indexed_commit" meta key.
    ///
    /// # Returns
    ///
    /// An `IndexResult` summarizing the indexing operation: number of commits processed,
    /// packages found, packages upserted, unique package names observed, and whether the run
    /// was interrupted.
    ///
    /// # Errors
    ///
    /// Propagates errors from git operations, extraction, and database interactions.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # // pseudocode; adapt with real repo/db objects in tests
    /// # use crate::index::{Indexer, IndexerConfig};
    /// # use crate::db::Database;
    /// let indexer = Indexer::new(IndexerConfig::default());
    /// let mut db = Database::open("/tmp/index.db").unwrap();
    /// let repo = open_nixpkgs_repo("/path/to/nixpkgs").unwrap();
    /// let commits = repo.list_commits_touching_pkgs().unwrap();
    /// let result = indexer.process_commits(&mut db, "/path/to/nixpkgs", &repo, commits, None).unwrap();
    /// println!("Indexed {} commits", result.commits_processed);
    /// ```
    #[instrument(skip(self, db, nixpkgs_path, repo, commits, resume_from), fields(total_commits = commits.len()))]
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
        // Note: nixpkgs_path is unused here because we use WorktreeSession for all checkouts
        let _ = nixpkgs_path.as_ref();

        // Determine if we should use parallel evaluation
        let use_parallel = self.config.worker_count != Some(1) && systems.len() > 1;
        let worker_count = self.config.worker_count.unwrap_or(systems.len());

        // Create worker pool for parallel evaluation (if enabled)
        let worker_pool = if use_parallel && worker_count > 1 {
            let pool_config = worker::WorkerPoolConfig {
                worker_count,
                max_memory_mib: self.config.max_memory_mib,
                ..Default::default()
            };
            match worker::WorkerPool::new(pool_config) {
                Ok(pool) => {
                    eprintln!(
                        "Using parallel evaluation with {} workers",
                        pool.worker_count()
                    );
                    Some(pool)
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to create worker pool ({}), falling back to sequential",
                        e
                    );
                    None
                }
            }
        } else {
            if systems.len() > 1 {
                eprintln!("Using sequential evaluation (--workers=1)");
            }
            None
        };

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
                    // Template is a compile-time constant, this should never fail
                    .expect("Invalid progress bar template")
                    .progress_chars("█▓▒░  "),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        });

        // ETA tracker with window size 50 and EMA/median blending for stable estimates
        let mut eta_tracker = EtaTracker::new(50);

        // Track unique package names for bloom filter
        let mut unique_names: HashSet<String> = HashSet::new();

        let mut result = IndexResult {
            commits_processed: 0,
            packages_found: 0,
            packages_upserted: 0,
            unique_names: 0,
            was_interrupted: false,
            extraction_failures: 0,
        };

        // Buffer for batch UPSERT operations
        let mut pending_upserts: Vec<PackageVersion> = Vec::new();
        let mut checkpoints_since_gc: usize = 0;
        let mut last_processed_commit: Option<String> = resume_from.map(String::from);

        // Build the initial file-to-attribute map
        let first_commit = commits
            .first()
            .ok_or_else(|| NxvError::Git(git2::Error::from_str("No commits to process")))?;

        // Create a worktree session for isolated checkouts (auto-cleaned on drop)
        let session = WorktreeSession::new(repo, &first_commit.hash)?;
        let worktree_path = session.path();

        // Build initial file-to-attribute map, handling failure gracefully
        // If this fails (e.g., Nix eval error on first commit), start with empty map
        // and try to rebuild on first commit that changes top-level files
        let (mut file_attr_map, mut mapping_commit) =
            match build_file_attr_map(worktree_path, systems, worker_pool.as_ref()) {
                Ok(map) => (map, first_commit.hash.clone()),
                Err(e) => {
                    eprintln!(
                        "Warning: Initial file-to-attribute map failed ({}), using empty map",
                        e
                    );
                    (HashMap::new(), String::new())
                }
            };

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
                    pb.println(format!(
                        "{} saving checkpoint...",
                        "Shutdown requested,".warning()
                    ));
                }
                result.was_interrupted = true;

                // UPSERT any pending packages before exiting
                if !pending_upserts.is_empty() {
                    result.packages_upserted += db.upsert_packages_batch(&pending_upserts)? as u64;
                }

                // Save checkpoint - just the last processed commit
                if let Some(ref last_hash) = last_processed_commit {
                    db.set_meta("last_indexed_commit", last_hash)?;
                    db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
                    db.checkpoint()?;

                    if let Some(ref pb) = progress_bar {
                        pb.println(format!(
                            "{} checkpoint at {}",
                            "Saved".success(),
                            &last_hash[..7]
                        ));
                    }
                }

                break;
            }

            // Update progress bar with percentage and smoothed ETA
            if let Some(ref pb) = progress_bar {
                use owo_colors::OwoColorize;
                pb.set_position(commit_idx as u64);
                pb.set_message(format!(
                    "{} | {} {} | {} {} | {} {}",
                    eta_tracker.progress_string(total_commits as u64),
                    commit.short_hash.commit(),
                    format!("({})", commit.date.format("%Y-%m-%d")).dimmed(),
                    result.packages_found.count_found(),
                    "pkgs".dimmed(),
                    (result.packages_upserted + pending_upserts.len() as u64).count(),
                    "upserted".dimmed()
                ));
            }

            // Checkout the commit in the worktree
            if let Err(e) = session.checkout(&commit.hash) {
                warn(
                    &progress_bar,
                    format!("Failed to checkout {}: {}", &commit.short_hash, e),
                );
                eta_tracker.skip_commit();
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
                    eta_tracker.skip_commit();
                    continue;
                }
            };

            // Check if we need to refresh the file map
            // Also try to rebuild if map is empty (e.g., initial extraction failed)
            let need_refresh = file_attr_map.is_empty() || should_refresh_file_map(&changed_paths);
            if need_refresh
                && mapping_commit != commit.hash
                && let Ok(map) = build_file_attr_map(worktree_path, systems, worker_pool.as_ref())
            {
                file_attr_map = map;
                mapping_commit = commit.hash.clone();
            }

            // Determine target attributes
            let mut target_attr_paths: HashSet<String> = HashSet::new();
            let all_attrs: Option<&Vec<String>> =
                file_attr_map.get("pkgs/top-level/all-packages.nix");

            // Check for infrastructure files and parse their diffs to extract affected attrs
            // For the first commit, always do full extraction to capture baseline state
            let mut needs_full_extraction = commit_idx == 0;
            for infra_file in INFRASTRUCTURE_FILES {
                if changed_paths.contains(&infra_file.to_string()) {
                    // Get the diff for this infrastructure file
                    match repo.get_file_diff(&commit.hash, infra_file) {
                        Ok(diff) => {
                            if let Some(extracted_attrs) = extract_attrs_from_diff(&diff) {
                                // Validate extracted attrs against known package names
                                for attr in extracted_attrs {
                                    if let Some(all_attrs_list) = all_attrs {
                                        if all_attrs_list.contains(&attr) {
                                            target_attr_paths.insert(attr);
                                        }
                                    } else {
                                        // No all_attrs available, trust the extracted attr
                                        target_attr_paths.insert(attr);
                                    }
                                }

                                trace!(
                                    commit = %commit.short_hash,
                                    file = %infra_file,
                                    attrs_extracted = target_attr_paths.len(),
                                    "Extracted attrs from infrastructure file diff"
                                );
                            } else {
                                // extract_attrs_from_diff returned None (large diff or fallback needed)
                                debug!(
                                    commit = %commit.short_hash,
                                    file = %infra_file,
                                    "Large diff in infrastructure file, triggering full extraction"
                                );
                                needs_full_extraction = true;
                            }
                        }
                        Err(e) => {
                            trace!(
                                commit = %commit.short_hash,
                                file = %infra_file,
                                error = %e,
                                "Failed to get diff for infrastructure file"
                            );
                        }
                    }
                }
            }

            // If full extraction is needed (first commit or large infrastructure diff),
            // extract all packages from all-packages.nix
            if needs_full_extraction && let Some(all_attrs_list) = all_attrs {
                for attr in all_attrs_list {
                    target_attr_paths.insert(attr.clone());
                }
                if commit_idx == 0 {
                    debug!(
                        commit = %commit.short_hash,
                        total_attrs = target_attr_paths.len(),
                        "Full extraction for first commit to capture baseline state"
                    );
                } else {
                    debug!(
                        commit = %commit.short_hash,
                        total_attrs = target_attr_paths.len(),
                        "Full extraction triggered due to large infrastructure diff"
                    );
                }
            }

            for path in &changed_paths {
                if let Some(attr_paths) = file_attr_map.get(path) {
                    for attr in attr_paths {
                        target_attr_paths.insert(attr.clone());
                    }
                } else if path.starts_with("pkgs/") && path.ends_with(".nix") {
                    let parts: Vec<&str> = path.split('/').collect();
                    if parts.len() >= 2 {
                        // pkgs/by-name/XX/pkgname/package.nix -> pkgname
                        // These are auto-discovered and don't need all_attrs validation
                        if path.starts_with("pkgs/by-name/") && parts.len() >= 4 {
                            let pkg_name = parts[3];
                            if !pkg_name.is_empty() {
                                target_attr_paths.insert(pkg_name.to_string());
                            }
                        } else {
                            // Traditional paths: extract name and validate against all_attrs
                            let potential_name =
                                // pkgs/.../something/default.nix -> something
                                if parts.last() == Some(&"default.nix") && parts.len() >= 2 {
                                    parts[parts.len() - 2]
                                }
                                // pkgs/.../something.nix -> something
                                else {
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
            }

            if target_attr_paths.is_empty() {
                result.commits_processed += 1;
                last_processed_commit = Some(commit.hash.clone());
                eta_tracker.skip_commit();
                continue;
            }

            let mut target_list: Vec<String> = target_attr_paths.into_iter().collect();
            target_list.sort();

            debug!(
                commit = %commit.short_hash,
                target_count = target_list.len(),
                "Processing commit"
            );

            // Trace: show which files triggered extraction and target attrs
            trace!(
                commit = %commit.short_hash,
                changed_files = changed_paths.len(),
                target_attrs = ?target_list,
                "Commit changed files mapped to target attributes"
            );

            // Extract packages for all systems
            let mut aggregates: HashMap<String, PackageAggregate> = HashMap::new();

            // Use parallel evaluation if worker pool is available, otherwise sequential
            let extraction_results: Vec<(String, Result<Vec<extractor::PackageInfo>>)> = {
                let _extract_span = info_span!(
                    "extract_packages",
                    targets = target_list.len(),
                    systems = systems.len()
                )
                .entered();

                if let Some(ref pool) = worker_pool {
                    // Parallel extraction using worker pool
                    let results = pool.extract_parallel(worktree_path, systems, &target_list);
                    systems.iter().cloned().zip(results).collect()
                } else {
                    // Sequential extraction (fallback)
                    systems
                        .iter()
                        .map(|system| {
                            let result = extractor::extract_packages_for_attrs(
                                worktree_path,
                                system,
                                &target_list,
                            );
                            (system.clone(), result)
                        })
                        .collect()
                }
            };

            // Process results from all systems
            for (system, packages_result) in extraction_results {
                let packages = match packages_result {
                    Ok(pkgs) => {
                        trace!(
                            commit = %commit.short_hash,
                            system = %system,
                            packages_extracted = pkgs.len(),
                            "System extraction completed"
                        );
                        pkgs
                    }
                    Err(e) => {
                        result.extraction_failures += 1;
                        tracing::warn!(
                            commit = %commit.short_hash,
                            system = %system,
                            error = %e,
                            "Extraction failed for system"
                        );
                        if self.config.verbose {
                            warn(
                                &progress_bar,
                                format!(
                                    "Extraction failed at {} ({}): {}",
                                    &commit.short_hash, system, e
                                ),
                            );
                        }
                        continue;
                    }
                };

                for pkg in packages {
                    let key = format!(
                        "{}::{}",
                        pkg.attribute_path,
                        pkg.version.as_deref().unwrap_or("")
                    );
                    if let Some(existing) = aggregates.get_mut(&key) {
                        existing.merge(pkg, &system);
                    } else {
                        let mut agg = PackageAggregate::new(pkg, &system);
                        // Clear store_paths for commits before 2020-01-01
                        // (older binaries unlikely to be in cache.nixos.org)
                        if !is_after_store_path_cutoff(commit.date) {
                            agg.store_paths.clear();
                        }
                        aggregates.insert(key, agg);
                    }
                }
            }

            result.packages_found += aggregates.len() as u64;

            trace!(
                commit = %commit.short_hash,
                unique_packages = aggregates.len(),
                "Aggregation complete"
            );

            // Convert aggregates to PackageVersions and add to pending upserts
            for aggregate in aggregates.values() {
                // Track unique package names for bloom filter
                unique_names.insert(aggregate.name.clone());

                // Convert aggregate to PackageVersion for UPSERT
                pending_upserts.push(aggregate.to_package_version(&commit.hash, commit.date));
            }

            result.commits_processed += 1;
            last_processed_commit = Some(commit.hash.clone());

            // Record commit processing time for ETA calculation
            eta_tracker.finish_commit();

            // Checkpoint if needed
            if (commit_idx + 1).is_multiple_of(self.config.checkpoint_interval)
                || commit_idx + 1 == commits.len()
            {
                let checkpoint_start = Instant::now();

                if !pending_upserts.is_empty() {
                    let upsert_start = Instant::now();
                    let upsert_count = pending_upserts.len();
                    result.packages_upserted += db.upsert_packages_batch(&pending_upserts)? as u64;
                    trace!(
                        upsert_count = upsert_count,
                        upsert_time_ms = upsert_start.elapsed().as_millis(),
                        "Database batch upsert completed"
                    );
                    pending_upserts.clear();
                }

                db.set_meta("last_indexed_commit", &commit.hash)?;
                db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
                db.checkpoint()?;

                // Garbage collection: run periodically or when disk is low
                checkpoints_since_gc += 1;
                let should_gc = if self.config.gc_interval > 0 {
                    // Periodic GC based on checkpoint count
                    checkpoints_since_gc >= self.config.gc_interval
                        // Or emergency GC if disk space is critically low
                        || gc::is_store_low_on_space(self.config.gc_min_free_bytes)
                } else {
                    // GC disabled, but still run if critically low on disk
                    gc::is_store_low_on_space(self.config.gc_min_free_bytes / 2)
                };

                if should_gc {
                    if let Some(ref pb) = progress_bar {
                        pb.set_message("Running garbage collection...".to_string());
                    }

                    // Clean up temp eval store to free disk space
                    if let Some(bytes) = gc::cleanup_temp_eval_store()
                        && bytes > 0
                        && let Some(ref pb) = progress_bar
                    {
                        pb.println(format!(
                            "Cleaned up temp eval store ({:.1} MB freed)",
                            bytes as f64 / 1_000_000.0
                        ));
                    }

                    if let Some(duration) = gc::run_garbage_collection() {
                        if let Some(ref pb) = progress_bar {
                            pb.println(format!(
                                "{} garbage collection in {:.1}s",
                                "Completed".success(),
                                duration.as_secs_f64()
                            ));
                        }
                    } else if let Some(ref pb) = progress_bar {
                        pb.println(format!(
                            "{} garbage collection (GC command failed)",
                            "Skipped".warning()
                        ));
                    }
                    checkpoints_since_gc = 0;
                }

                trace!(
                    commit_idx = commit_idx + 1,
                    checkpoint_time_ms = checkpoint_start.elapsed().as_millis(),
                    "Checkpoint completed"
                );
            }
        }

        // Final: UPSERT any remaining pending packages
        if !result.was_interrupted && !pending_upserts.is_empty() {
            result.packages_upserted += db.upsert_packages_batch(&pending_upserts)? as u64;
        }

        // Update final metadata
        if !result.was_interrupted
            && let Some(ref last_hash) = last_processed_commit
        {
            db.set_meta("last_indexed_commit", last_hash)?;
            db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
        }

        // Set final unique names count
        result.unique_names = unique_names.len() as u64;

        // Finish progress bar
        if let Some(ref pb) = progress_bar {
            use owo_colors::OwoColorize;
            pb.finish_with_message(format!(
                "{} | {} {} | {} {} | {} {}",
                "done".success(),
                result.commits_processed.count(),
                "commits".dimmed(),
                result.packages_found.count_found(),
                "pkgs".dimmed(),
                result.packages_upserted.count(),
                "upserted".dimmed()
            ));
        }

        // WorktreeSession auto-cleans on drop - no need to restore HEAD

        // Clean up temp eval store on exit
        if let Some(bytes) = gc::cleanup_temp_eval_store()
            && bytes > 0
        {
            eprintln!(
                "Cleaned up temp eval store ({:.1} MB freed)",
                bytes as f64 / 1_000_000.0
            );
        }

        Ok(result)
    }
}

/// Infrastructure files that affect many packages but rarely indicate version changes.
///
/// These files are excluded from the file-to-attribute mapping because:
/// 1. `all-packages.nix` imports/exports all packages, so any change triggers 18k+ targets
/// 2. `aliases.nix` just defines aliases, not actual package versions
/// 3. Changes to actual package files are still detected via path-based fallback heuristics
const INFRASTRUCTURE_FILES: &[&str] = &[
    "pkgs/top-level/all-packages.nix",
    "pkgs/top-level/aliases.nix",
];

/// Maximum number of lines in a diff before we fall back to full extraction.
/// Large diffs typically indicate bulk updates where parsing individual attributes
/// is less efficient than extracting everything.
const DIFF_FALLBACK_THRESHOLD: usize = 100;

/// Parse a git diff and extract affected attribute names.
///
/// This function extracts attribute names from diff lines that match common
/// nixpkgs patterns:
/// - Assignment: `  attrName = ...`
/// - callPackage: `  attrName = callPackage ...`
/// - Override: `  attrName = prev.pkg.override ...`
/// - Inherit: `  inherit (foo) attr1 attr2 attr3;`
///
/// Returns `None` if the diff should trigger a full extraction (too large or unparseable).
fn extract_attrs_from_diff(diff: &str) -> Option<Vec<String>> {
    let mut attrs: Vec<String> = Vec::new();
    let mut line_count = 0;

    for line in diff.lines() {
        // Skip diff header lines
        if line.starts_with("@@")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("---")
            || line.starts_with("+++")
        {
            continue;
        }

        // Only process added/modified/removed lines
        if !line.starts_with('+') && !line.starts_with('-') {
            continue;
        }

        line_count += 1;

        // Get the content after +/- prefix
        let content = &line[1..];

        // Try assignment pattern: `  attrName = ...`
        if let Some(attr_name) = extract_assignment_attr(content) {
            if !is_non_package_attr(&attr_name) {
                attrs.push(attr_name);
            }
            continue;
        }

        // Try inherit pattern: `  inherit (foo) attr1 attr2;`
        if let Some(inherited_attrs) = extract_inherit_attrs(content) {
            for attr_name in inherited_attrs {
                if !is_non_package_attr(&attr_name) {
                    attrs.push(attr_name);
                }
            }
        }
    }

    // If diff is too large, signal fallback
    if line_count > DIFF_FALLBACK_THRESHOLD {
        tracing::debug!(
            line_count,
            attrs_found = attrs.len(),
            "Large diff detected, suggesting fallback to full extraction"
        );
        return None;
    }

    // Sort and deduplicate
    attrs.sort();
    attrs.dedup();

    Some(attrs)
}

/// Extract attribute name from an assignment line like `  attrName = ...`
fn extract_assignment_attr(line: &str) -> Option<String> {
    let trimmed = line.trim_start();

    // Find the `=` sign
    let eq_pos = trimmed.find('=')?;

    // Get the part before `=` and trim it
    let before_eq = trimmed[..eq_pos].trim();

    // Validate: should be a single valid Nix identifier
    // Valid Nix identifiers: start with letter or _, followed by letters, numbers, _, -
    if before_eq.is_empty() {
        return None;
    }

    let first_char = before_eq.chars().next()?;
    if !first_char.is_ascii_alphabetic() && first_char != '_' {
        return None;
    }

    // Check that the whole identifier is valid
    if before_eq
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(before_eq.to_string())
    } else {
        None
    }
}

/// Extract attribute names from an inherit line like `  inherit (foo) attr1 attr2 attr3;`
fn extract_inherit_attrs(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim_start();

    // Check if it starts with "inherit"
    if !trimmed.starts_with("inherit") {
        return None;
    }

    let rest = trimmed.strip_prefix("inherit")?.trim_start();

    // If there's a parenthesized source, skip past it
    let attrs_part = if rest.starts_with('(') {
        // Find the closing parenthesis
        let close_paren = rest.find(')')?;
        rest[close_paren + 1..].trim_start()
    } else {
        rest
    };

    // Remove trailing semicolon if present
    let attrs_str = attrs_part.trim_end_matches(';').trim();

    // Split by whitespace and filter valid identifiers
    let attrs: Vec<String> = attrs_str
        .split_whitespace()
        .filter(|s| {
            !s.is_empty()
                && s.chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                    .unwrap_or(false)
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .map(|s| s.to_string())
        .collect();

    if attrs.is_empty() { None } else { Some(attrs) }
}

/// Check if an attribute name is a known non-package attribute.
/// These are helper functions, let bindings, or other structural elements.
fn is_non_package_attr(name: &str) -> bool {
    // Common prefixes/patterns that aren't packages
    let non_package_patterns = [
        "inherit",
        "let",
        "in",
        "with",
        "if",
        "then",
        "else",
        "import",
        "callPackages", // Note: plural version is a function, not a package
        "self",
        "super",
        "prev",
        "final",
        "__",
    ];

    for pattern in non_package_patterns {
        if name == pattern || name.starts_with(pattern) {
            return true;
        }
    }

    false
}

fn build_file_attr_map(
    repo_path: &Path,
    systems: &[String],
    worker_pool: Option<&worker::WorkerPool>,
) -> Result<HashMap<String, Vec<String>>> {
    let system = systems
        .first()
        .ok_or_else(|| NxvError::NixEval("No systems configured".to_string()))?;

    // Use worker pool if available to avoid memory accumulation in parent process
    let positions = if let Some(pool) = worker_pool {
        pool.extract_positions(system, repo_path)?
    } else {
        extractor::extract_attr_positions(repo_path, system)?
    };

    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for position in positions {
        if let Some(file) = position.file
            && let Some(relative) = normalize_position_file(repo_path, &file)
        {
            // Include all files in the map, including infrastructure files.
            // Infrastructure files are handled specially during incremental indexing
            // (via diff parsing), but we need them in the map for full extraction.
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
    /// Number of packages upserted into the database.
    pub packages_upserted: u64,
    /// Number of unique package names found.
    pub unique_names: u64,
    /// Whether the indexing was interrupted (e.g., by Ctrl+C).
    pub was_interrupted: bool,
    /// Number of extraction failures (per-system failures during indexing).
    pub extraction_failures: u64,
}

/// Constructs a Bloom filter containing all unique package attribute paths from the database.
///
/// The filter is created with a target false-positive rate of 1% and an initial capacity
/// derived from the number of attributes (minimum of 1000). Iterate over all unique
/// attribute paths stored in the database and insert each into the filter.
///
/// # Examples
///
/// ```
/// # // Hidden setup: obtain a `Database` instance appropriate for your environment.
/// # use crate::db::Database;
/// # use crate::index::build_bloom_filter;
/// # fn try_build(db: &Database) -> anyhow::Result<()> {
/// let filter = build_bloom_filter(db)?;
/// // `filter` can now be queried for probable membership of attribute paths.
/// // (Bloom filter may yield false positives but not false negatives.)
/// # Ok(())
/// # }
/// ```
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
///
/// # Arguments
/// * `db` - The database to build the bloom filter from
/// * `bloom_path` - Path where the bloom filter should be saved
pub fn save_bloom_filter<P: AsRef<std::path::Path>>(db: &Database, bloom_path: P) -> Result<()> {
    let filter = build_bloom_filter(db)?;
    let bloom_path = bloom_path.as_ref();

    // Ensure parent directory exists
    if let Some(parent) = bloom_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    filter.save(bloom_path)?;
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
        // Shows warm-up message when no commits processed
        assert!(tracker.eta_string().contains("warming up"));
    }

    #[test]
    fn test_eta_tracker_warmup_period() {
        let mut tracker = EtaTracker::new(10);
        tracker.set_remaining(100);

        // Process fewer commits than warmup_count
        for _ in 0..5 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(5));
            tracker.finish_commit();
        }

        // Should still show warming up
        assert!(tracker.eta_string().contains("warming up"));
        assert!(tracker.eta().is_none());
    }

    #[test]
    fn test_eta_tracker_after_warmup() {
        let mut tracker = EtaTracker::new(10);
        tracker.set_remaining(50);

        // Process more commits than warmup_count (20)
        for _ in 0..25 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(5));
            tracker.finish_commit();
        }

        // Should now show actual ETA
        let eta = tracker.eta();
        assert!(eta.is_some());
        assert!(!tracker.eta_string().contains("warming up"));
    }

    #[test]
    fn test_eta_tracker_ema_smoothing() {
        let mut tracker = EtaTracker::new(10);
        tracker.set_remaining(10);

        // Add consistent commits to establish baseline (~20ms each)
        for _ in 0..15 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(20));
            tracker.finish_commit();
        }

        let ema_before = tracker.ema_secs.unwrap();

        // Very fast commits (skipped) should be dampened to prevent ETA from being too optimistic
        // These represent skipped commits that didn't do real work
        for _ in 0..5 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(1)); // Very fast - < 10% of median
            tracker.finish_commit();
        }

        let ema_after_fast = tracker.ema_secs.unwrap();

        // EMA should not have dropped dramatically despite fast outliers
        // (fast outliers get reduced alpha to prevent optimistic ETA)
        assert!(
            ema_after_fast > ema_before * 0.5,
            "EMA dropped too much after fast outliers: before={:.4}, after={:.4}",
            ema_before,
            ema_after_fast
        );

        // Slow commits (real work) should have full weight
        tracker.start_commit();
        thread::sleep(Duration::from_millis(100)); // Slow - represents real work
        tracker.finish_commit();

        let ema_after_slow = tracker.ema_secs.unwrap();

        // EMA should increase noticeably because slow commits aren't dampened
        assert!(
            ema_after_slow > ema_after_fast,
            "Slow commit should increase EMA: fast={:.4}, slow={:.4}",
            ema_after_fast,
            ema_after_slow
        );
    }

    #[test]
    fn test_eta_tracker_formatting() {
        let mut tracker = EtaTracker::new(5);
        tracker.set_remaining(1);

        // Process enough commits to pass warmup (20)
        for _ in 0..25 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(5));
            tracker.finish_commit();
        }

        // Should format with time units
        let eta_str = tracker.eta_string();
        assert!(
            eta_str.contains("s") || eta_str.contains("m") || eta_str.contains("h"),
            "Expected time format, got: {}",
            eta_str
        );
    }

    /// Creates a temporary git repository resembling a minimal nixpkgs checkout.
    ///
    /// The repository contains a pkgs/ directory, a minimal default.nix defining
    /// a single package, and an initial commit. Returns the temporary directory
    /// (kept alive by the caller) and the repository path.
    ///
    /// # Examples
    ///
    /// ```
    /// let (_tmpdir, repo_path) = create_test_nixpkgs_repo();
    /// assert!(repo_path.join("pkgs").exists());
    /// assert!(repo_path.join("default.nix").exists());
    /// assert!(repo_path.join(".git").exists());
    /// ```
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
    fn test_package_aggregate_to_package_version() {
        let pkg_info = extractor::PackageInfo {
            name: "hello".to_string(),
            version: Some("1.0.0".to_string()),
            version_source: Some("direct".to_string()),
            attribute_path: "hello".to_string(),
            description: Some("A test package".to_string()),
            license: Some(vec!["MIT".to_string()]),
            homepage: Some("https://example.org".to_string()),
            maintainers: None,
            platforms: None,
            source_path: Some("pkgs/hello/default.nix".to_string()),
            known_vulnerabilities: None,
            out_path: Some("/nix/store/abc-hello-1.0.0".to_string()),
        };

        let aggregate = PackageAggregate::new(pkg_info, "x86_64-linux");
        let commit_date = Utc::now();
        let pkg = aggregate.to_package_version("abc123", commit_date);

        assert_eq!(pkg.name, "hello");
        assert_eq!(pkg.version, "1.0.0");
        assert_eq!(pkg.version_source, Some("direct".to_string()));
        assert_eq!(pkg.first_commit_hash, "abc123");
        assert_eq!(pkg.last_commit_hash, "abc123");
        assert_eq!(pkg.attribute_path, "hello");
        assert_eq!(pkg.description, Some("A test package".to_string()));
        assert!(pkg.store_paths.contains_key("x86_64-linux"));
    }

    #[test]
    fn test_index_result_default_state() {
        let result = IndexResult {
            commits_processed: 0,
            packages_found: 0,
            packages_upserted: 0,
            unique_names: 0,
            was_interrupted: false,
            extraction_failures: 0,
        };

        assert_eq!(result.commits_processed, 0);
        assert!(!result.was_interrupted);
        assert_eq!(result.extraction_failures, 0);
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
            worker_count: Some(1), // Sequential for tests
            max_memory_mib: 6 * 1024,
            verbose: false,
            gc_interval: 0, // Disable GC for tests
            gc_min_free_bytes: 0,
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
            worker_count: Some(1), // Sequential for tests
            max_memory_mib: 6 * 1024,
            verbose: false,
            gc_interval: 0, // Disable GC for tests
            gc_min_free_bytes: 0,
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
            db.set_meta("last_indexed_date", "2024-01-15T10:00:00Z")
                .unwrap();
        }

        // Verify checkpoint state is recoverable
        {
            let db = Database::open(&db_path).unwrap();
            let last_commit = db.get_meta("last_indexed_commit").unwrap();
            assert_eq!(last_commit, Some("abc123def456".to_string()));

            let last_date = db.get_meta("last_indexed_date").unwrap();
            assert_eq!(last_date, Some("2024-01-15T10:00:00Z".to_string()));
        }
    }

    #[test]
    fn test_upsert_batch_consistency() {
        // Test that the database UPSERT operations work correctly
        let db_dir = tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let packages = vec![
            PackageVersion {
                id: 0,
                name: "python".to_string(),
                version: "3.11.0".to_string(),
                version_source: None,
                first_commit_hash: "aaa111".to_string(),
                first_commit_date: Utc.timestamp_opt(1700000000, 0).unwrap(),
                last_commit_hash: "aaa111".to_string(),
                last_commit_date: Utc.timestamp_opt(1700000000, 0).unwrap(),
                attribute_path: "python311".to_string(),
                description: Some("Python".to_string()),
                license: Some(r#"["MIT"]"#.to_string()),
                homepage: Some("https://python.org".to_string()),
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            },
            PackageVersion {
                id: 0,
                name: "nodejs".to_string(),
                version: "20.0.0".to_string(),
                version_source: None,
                first_commit_hash: "ccc333".to_string(),
                first_commit_date: Utc.timestamp_opt(1700200000, 0).unwrap(),
                last_commit_hash: "ccc333".to_string(),
                last_commit_date: Utc.timestamp_opt(1700200000, 0).unwrap(),
                attribute_path: "nodejs_20".to_string(),
                description: Some("Node.js".to_string()),
                license: Some(r#"["MIT"]"#.to_string()),
                homepage: Some("https://nodejs.org".to_string()),
                maintainers: None,
                platforms: None,
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            },
        ];

        // UPSERT as batch
        {
            let mut db = Database::open(&db_path).unwrap();
            let upserted = db.upsert_packages_batch(&packages).unwrap();
            assert_eq!(upserted, 2);
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

            // UPSERT some packages
            let pkg = PackageVersion {
                id: 0,
                name: "firefox".to_string(),
                version: "120.0".to_string(),
                version_source: None,
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            };
            db.upsert_packages_batch(&[pkg]).unwrap();

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
                version_source: None,
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
            };
            db.upsert_packages_batch(&[pkg]).unwrap();

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

    #[test]
    fn test_extract_assignment_attr() {
        // Simple assignment
        assert_eq!(
            extract_assignment_attr("  hello = callPackage ../applications/misc/hello { };"),
            Some("hello".to_string())
        );

        // Assignment with hyphens
        assert_eq!(
            extract_assignment_attr("  gnome-shell = callPackage ../desktops/gnome/shell { };"),
            Some("gnome-shell".to_string())
        );

        // Assignment with underscore
        assert_eq!(
            extract_assignment_attr(
                "  node_20 = callPackage ../development/interpreters/node { };"
            ),
            Some("node_20".to_string())
        );

        // Assignment without leading spaces
        assert_eq!(
            extract_assignment_attr("firefox = wrapFirefox firefox-unwrapped { };"),
            Some("firefox".to_string())
        );

        // Non-assignment (comment)
        assert_eq!(extract_assignment_attr("  # hello = old version"), None);

        // Non-assignment (no equals sign)
        assert_eq!(extract_assignment_attr("  hello world"), None);

        // Invalid identifier start
        assert_eq!(extract_assignment_attr("  123abc = bad"), None);
    }

    #[test]
    fn test_extract_inherit_attrs() {
        // Simple inherit with source
        assert_eq!(
            extract_inherit_attrs("  inherit (prev) hello world;"),
            Some(vec!["hello".to_string(), "world".to_string()])
        );

        // Inherit with multiple attrs (order preserved from input)
        assert_eq!(
            extract_inherit_attrs("  inherit (gnome) gnome-shell mutter gjs;"),
            Some(vec![
                "gnome-shell".to_string(),
                "mutter".to_string(),
                "gjs".to_string()
            ])
        );

        // Inherit without source (plain inherit)
        assert_eq!(
            extract_inherit_attrs("  inherit foo bar baz;"),
            Some(vec![
                "foo".to_string(),
                "bar".to_string(),
                "baz".to_string()
            ])
        );

        // Not an inherit statement
        assert_eq!(extract_inherit_attrs("  hello = world;"), None);

        // Empty inherit
        assert_eq!(extract_inherit_attrs("  inherit;"), None);
    }

    #[test]
    fn test_extract_attrs_from_diff() {
        let diff = r#"diff --git a/pkgs/top-level/all-packages.nix b/pkgs/top-level/all-packages.nix
index abc123..def456 100644
--- a/pkgs/top-level/all-packages.nix
+++ b/pkgs/top-level/all-packages.nix
@@ -1234,7 +1234,7 @@
-  thunderbird = wrapThunderbird thunderbird-unwrapped { };
+  thunderbird = wrapThunderbird thunderbird-unwrapped { enableFoo = true; };
-  firefox = wrapFirefox firefox-unwrapped { };
+  firefox = wrapFirefox firefox-unwrapped { version = "120.0"; };
"#;

        let attrs = extract_attrs_from_diff(diff).expect("Should extract attrs");
        assert!(attrs.contains(&"thunderbird".to_string()));
        assert!(attrs.contains(&"firefox".to_string()));
        assert_eq!(attrs.len(), 2); // Should deduplicate
    }

    #[test]
    fn test_extract_attrs_from_diff_large_triggers_fallback() {
        // Create a diff with more than DIFF_FALLBACK_THRESHOLD lines
        let mut diff = String::from("diff --git a/test b/test\n--- a/test\n+++ b/test\n");
        for i in 0..150 {
            diff.push_str(&format!("+  pkg{} = callPackage {{ }};\n", i));
        }

        // Should return None to trigger fallback
        assert!(extract_attrs_from_diff(&diff).is_none());
    }

    #[test]
    fn test_is_non_package_attr() {
        // Non-package patterns
        assert!(is_non_package_attr("inherit"));
        assert!(is_non_package_attr("let"));
        assert!(is_non_package_attr("self"));
        assert!(is_non_package_attr("__private"));
        assert!(is_non_package_attr("callPackages")); // Note: plural

        // Package patterns (should return false)
        assert!(!is_non_package_attr("hello"));
        assert!(!is_non_package_attr("firefox"));
        assert!(!is_non_package_attr("callPackage")); // Note: singular is OK
        assert!(!is_non_package_attr("gnome-shell"));
    }

    // Phase 1 tests: Version extraction and version_source tracking

    #[test]
    fn test_package_aggregate_with_version() {
        let pkg = extractor::PackageInfo {
            name: "hello".to_string(),
            version: Some("2.12.1".to_string()),
            version_source: Some("direct".to_string()),
            attribute_path: "hello".to_string(),
            description: Some("A program that prints Hello, world".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            out_path: None,
        };

        let aggregate = PackageAggregate::new(pkg, "x86_64-linux");

        assert_eq!(aggregate.name, "hello");
        assert_eq!(aggregate.version, "2.12.1");
        assert_eq!(aggregate.version_source, Some("direct".to_string()));
    }

    #[test]
    fn test_package_aggregate_with_none_version() {
        let pkg = extractor::PackageInfo {
            name: "breakpointHook".to_string(),
            version: None,
            version_source: None,
            attribute_path: "breakpointHook".to_string(),
            description: Some("A build hook".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            out_path: None,
        };

        let aggregate = PackageAggregate::new(pkg, "x86_64-linux");

        assert_eq!(aggregate.name, "breakpointHook");
        assert_eq!(aggregate.version, ""); // Should default to empty string
        assert_eq!(aggregate.version_source, None);
    }

    #[test]
    fn test_package_aggregate_version_source_propagates() {
        // Test that version_source is properly propagated through the system
        let pkg = extractor::PackageInfo {
            name: "neovim".to_string(),
            version: Some("0.9.5".to_string()),
            version_source: Some("unwrapped".to_string()), // Version came from unwrapped
            attribute_path: "neovim".to_string(),
            description: Some("Vim-fork focused on extensibility".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            out_path: None,
        };

        let aggregate = PackageAggregate::new(pkg, "x86_64-linux");

        assert_eq!(aggregate.version_source, Some("unwrapped".to_string()));

        // Test conversion to PackageVersion preserves version_source
        let pkg_version = aggregate.to_package_version("ghi789", Utc::now());
        assert_eq!(pkg_version.version_source, Some("unwrapped".to_string()));
        assert_eq!(pkg_version.name, "neovim");
        assert_eq!(pkg_version.version, "0.9.5");
    }

    #[test]
    fn test_package_aggregate_name_extracted_version() {
        // Test that version extracted from name is tracked
        let pkg = extractor::PackageInfo {
            name: "python-3.11.7".to_string(),
            version: Some("3.11.7".to_string()),
            version_source: Some("name".to_string()), // Version came from name parsing
            attribute_path: "python311".to_string(),
            description: Some("Python interpreter".to_string()),
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: None,
            out_path: None,
        };

        let aggregate = PackageAggregate::new(pkg, "x86_64-linux");

        assert_eq!(aggregate.version, "3.11.7");
        assert_eq!(aggregate.version_source, Some("name".to_string()));
    }
}

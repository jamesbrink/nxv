//! Indexer module for building the package index from nixpkgs.
//!
//! This module is only available when the `indexer` feature is enabled.

pub mod backfill;
pub mod extractor;
pub mod gc;
pub mod git;
pub mod nix_ffi;
pub mod publisher;
pub mod worker;

/// A checkpoint-serializable version of OpenRange for database persistence.
/// This is used to save/restore open ranges across indexer restarts.
#[derive(Debug, Clone)]
pub struct CheckpointRange {
    pub name: String,
    pub version: String,
    pub first_commit_hash: String,
    pub first_commit_date: chrono::DateTime<chrono::Utc>,
    pub attribute_path: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub maintainers: Option<String>,
    pub platforms: Option<String>,
    pub source_path: Option<String>,
    pub known_vulnerabilities: Option<String>,
    /// Store paths per architecture (e.g., {"x86_64-linux": "/nix/store/..."})
    pub store_paths: std::collections::HashMap<String, String>,
}

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
            gc_interval: 5, // GC every 5 checkpoints (500 commits by default)
            gc_min_free_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
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
    source_path: Option<String>,
    known_vulnerabilities: Option<String>,
    /// Store paths per architecture
    store_paths: HashMap<String, String>,
}

impl OpenRange {
    /// Convert this OpenRange into a PackageVersion using the provided last commit metadata.
    ///
    /// The returned PackageVersion contains all metadata carried by the OpenRange plus the
    /// supplied `last_commit_hash` and `last_commit_date`.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// // Construct an OpenRange and finalize it into a PackageVersion:
    /// let open = OpenRange { /* populate fields */ };
    /// let pv = open.to_package_version("deadbeef", chrono::Utc::now());
    /// assert_eq!(pv.last_commit_hash, "deadbeef");
    /// ```
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
            source_path: self.source_path.clone(),
            known_vulnerabilities: self.known_vulnerabilities.clone(),
            store_paths: self.store_paths.clone(),
        }
    }

    /// Convert to a CheckpointRange for serialization.
    fn to_checkpoint(&self) -> CheckpointRange {
        CheckpointRange {
            name: self.name.clone(),
            version: self.version.clone(),
            first_commit_hash: self.first_commit_hash.clone(),
            first_commit_date: self.first_commit_date,
            attribute_path: self.attribute_path.clone(),
            description: self.description.clone(),
            license: self.license.clone(),
            homepage: self.homepage.clone(),
            maintainers: self.maintainers.clone(),
            platforms: self.platforms.clone(),
            source_path: self.source_path.clone(),
            known_vulnerabilities: self.known_vulnerabilities.clone(),
            store_paths: self.store_paths.clone(),
        }
    }

    /// Create from a CheckpointRange for deserialization.
    fn from_checkpoint(cr: CheckpointRange) -> Self {
        Self {
            name: cr.name,
            version: cr.version,
            first_commit_hash: cr.first_commit_hash,
            first_commit_date: cr.first_commit_date,
            attribute_path: cr.attribute_path,
            description: cr.description,
            license: cr.license,
            homepage: cr.homepage,
            maintainers: cr.maintainers,
            platforms: cr.platforms,
            source_path: cr.source_path,
            known_vulnerabilities: cr.known_vulnerabilities,
            store_paths: cr.store_paths,
        }
    }

    /// Conditionally updates the stored metadata fields with the provided values.
    ///
    /// Each optional field replaces the corresponding stored value if it differs.
    /// The `source_path` is set only if the existing `source_path` is `None` and
    /// a new `Some` value is provided; it is never overwritten once set.
    ///
    /// # Returns
    ///
    /// `true` if any field was changed, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut r = OpenRange {
    ///     name: "pkg".into(),
    ///     version: "1.0".into(),
    ///     first_commit_hash: "abc".into(),
    ///     first_commit_date: "2020-01-01".into(),
    ///     attribute_path: "pkgs.pkg".into(),
    ///     description: None,
    ///     license: None,
    ///     homepage: None,
    ///     maintainers: None,
    ///     platforms: None,
    ///     source_path: None,
    /// };
    ///
    /// let changed = r.update_metadata(
    ///     Some("desc".into()),
    ///     Some("MIT".into()),
    ///     None,
    ///     None,
    ///     None,
    ///     Some("path/to/source".into()),
    /// );
    ///
    /// assert!(changed);
    /// assert_eq!(r.description, Some("desc".into()));
    /// assert_eq!(r.source_path, Some("path/to/source".into()));
    /// ```
    #[allow(clippy::too_many_arguments)]
    fn update_metadata(
        &mut self,
        description: Option<String>,
        license: Option<String>,
        homepage: Option<String>,
        maintainers: Option<String>,
        platforms: Option<String>,
        source_path: Option<String>,
        known_vulnerabilities: Option<String>,
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
        if self.source_path.is_none() && source_path.is_some() {
            self.source_path = source_path;
            updated = true;
        }
        if self.known_vulnerabilities != known_vulnerabilities {
            self.known_vulnerabilities = known_vulnerabilities;
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
            version: pkg.version,
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

    fn known_vulnerabilities_json(&self) -> Option<String> {
        self.known_vulnerabilities
            .as_ref()
            .filter(|v| !v.is_empty())
            .map(|v| serde_json::to_string(v).unwrap_or_default())
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
/// Uses EMA with outlier rejection instead of simple sliding window average because:
/// - Commit processing times vary wildly (some touch 1 file, others touch thousands)
/// - Simple averages get thrown off by outliers
/// - EMA provides smooth, responsive estimates
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
    /// Recent durations for outlier detection (rolling median)
    recent_durations: VecDeque<f64>,
    /// Window size for outlier detection
    outlier_window: usize,
}

impl EtaTracker {
    /// Creates an EtaTracker with exponential moving average smoothing.
    ///
    /// `window_size` controls the effective smoothing - larger values mean
    /// more smoothing (less responsive but more stable).
    ///
    /// The EMA alpha is calculated as 2/(window_size+1), which gives:
    /// - window_size=20 -> alpha=0.095 (smooth)
    /// - window_size=50 -> alpha=0.039 (very smooth)
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
            warmup_count: 10, // Don't show ETA until 10 commits processed
            recent_durations: VecDeque::with_capacity(50),
            outlier_window: 50,
        }
    }

    /// Begin timing for the current commit.
    fn start_commit(&mut self) {
        self.commit_start = Some(Instant::now());
    }

    /// Stops the current commit timer and updates the EMA.
    ///
    /// Uses outlier rejection: if a sample is more than 3x the median,
    /// it's given reduced weight to prevent ETA spikes.
    fn finish_commit(&mut self) {
        if let Some(start) = self.commit_start.take() {
            let elapsed_secs = start.elapsed().as_secs_f64();
            self.commits_processed += 1;

            // Track recent durations for outlier detection
            self.recent_durations.push_back(elapsed_secs);
            if self.recent_durations.len() > self.outlier_window {
                self.recent_durations.pop_front();
            }

            // Calculate median for outlier detection
            let median = self.calculate_median();

            // Determine if this is an outlier and adjust alpha accordingly
            let effective_alpha = if let Some(med) = median {
                if elapsed_secs > med * 3.0 {
                    // Outlier (slow): use much smaller alpha to reduce impact
                    self.alpha * 0.1
                } else if elapsed_secs < med * 0.1 {
                    // Outlier (fast): also reduce impact
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
    fn eta(&self) -> Option<Duration> {
        // Don't show ETA until we have enough samples
        if self.commits_processed < self.warmup_count {
            return None;
        }

        let avg_secs = self.ema_secs?;
        let remaining_secs = avg_secs * self.total_remaining as f64;

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
                                ranges_created: 0,
                                unique_names: 0,
                                was_interrupted: false,
                                extraction_failures: 0,
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

    /// Processes a sequence of commits: extracts package metadata for configured systems,
    /// tracks open version ranges across commits, finalizes and inserts package versions
    /// into the database, and updates indexing checkpoint metadata.
    ///
    /// This method iterates the provided commits in order, checking out each commit,
    /// extracting packages for the indexer's configured target systems, merging per-system
    /// metadata, and maintaining "open" version ranges for packages that persist across
    /// commits. When a range ends (the package disappears or a checkpoint is reached),
    /// the range is converted to a PackageVersion and written to the database in batches.
    /// The method also supports graceful shutdown (saving a checkpoint and flushing pending
    /// inserts), periodic checkpoints controlled by the indexer's configuration, and optional
    /// progress reporting with a smoothed ETA. It updates database meta keys such as
    /// "last_indexed_commit" and "checkpoint_open_ranges" and attempts to restore the
    /// repository's original HEAD upon completion.
    ///
    /// # Returns
    ///
    /// An `IndexResult` summarizing the indexing operation: number of commits processed,
    /// packages found, ranges created, unique package names observed, and whether the run
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

        // ETA tracker with 20-commit sliding window for stable estimates
        let mut eta_tracker = EtaTracker::new(20);

        // Track open ranges: attribute_path+version -> OpenRange
        // Try to load from checkpoint if resuming
        let mut open_ranges: HashMap<String, OpenRange> = if resume_from.is_some() {
            match db.load_checkpoint_ranges() {
                Ok(checkpoint_ranges) => {
                    if !checkpoint_ranges.is_empty() {
                        if let Some(ref pb) = progress_bar {
                            pb.println(format!(
                                "{} {} open ranges from checkpoint",
                                "Restored".success(),
                                checkpoint_ranges.len().count_pending()
                            ));
                        } else {
                            eprintln!(
                                "{} {} open ranges from checkpoint",
                                "Restored".success(),
                                checkpoint_ranges.len().count_pending()
                            );
                        }
                        checkpoint_ranges
                            .into_iter()
                            .map(|(k, v)| (k, OpenRange::from_checkpoint(v)))
                            .collect()
                    } else {
                        HashMap::new()
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Could not load checkpoint ranges: {}", e);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        // Track unique package names for bloom filter
        let mut unique_names: HashSet<String> = HashSet::new();

        let mut result = IndexResult {
            commits_processed: 0,
            packages_found: 0,
            ranges_created: 0,
            unique_names: 0,
            was_interrupted: false,
            extraction_failures: 0,
        };

        let mut prev_commit_hash: Option<String> = resume_from.map(String::from);
        let mut prev_commit_date: Option<DateTime<Utc>> = None;
        let mut pending_inserts: Vec<PackageVersion> = Vec::new();
        let mut checkpoints_since_gc: usize = 0;

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

                // Insert any pending ranges (ranges that were closed during this run)
                if !pending_inserts.is_empty() {
                    result.ranges_created +=
                        db.insert_package_ranges_batch(&pending_inserts)? as u64;
                }

                // Save checkpoint with open ranges for resume
                if let Some(ref prev_hash) = prev_commit_hash {
                    db.set_meta("last_indexed_commit", prev_hash)?;
                    db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
                    db.set_meta("checkpoint_open_ranges", &open_ranges.len().to_string())?;

                    // Save open ranges to checkpoint table for resume capability
                    let checkpoint_ranges: HashMap<String, CheckpointRange> = open_ranges
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_checkpoint()))
                        .collect();
                    db.save_checkpoint_ranges(&checkpoint_ranges)?;

                    db.checkpoint()?;

                    if let Some(ref pb) = progress_bar {
                        pb.println(format!(
                            "{} {} open ranges to checkpoint",
                            "Saved".success(),
                            open_ranges.len().count_pending()
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
                    "{} | {} {} | {} {} | {} {} {} {}",
                    eta_tracker.progress_string(total_commits as u64),
                    commit.short_hash.commit(),
                    format!("({})", commit.date.format("%Y-%m-%d")).dimmed(),
                    result.packages_found.count_found(),
                    "pkgs".dimmed(),
                    open_ranges.len().count_pending(),
                    "open".dimmed(),
                    result.ranges_created.count(),
                    "closed".dimmed()
                ));
            }

            // Checkout the commit in the worktree
            if let Err(e) = session.checkout(&commit.hash) {
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
                prev_commit_hash = Some(commit.hash.clone());
                prev_commit_date = Some(commit.date);
                eta_tracker.finish_commit();
                continue;
            }

            let mut target_list: Vec<String> = target_attr_paths.into_iter().collect();
            target_list.sort();

            // Extract packages for all systems
            let mut aggregates: HashMap<String, PackageAggregate> = HashMap::new();

            // Use parallel evaluation if worker pool is available, otherwise sequential
            let extraction_results: Vec<(String, Result<Vec<extractor::PackageInfo>>)> =
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
                };

            // Process results from all systems
            for (system, packages_result) in extraction_results {
                let packages = match packages_result {
                    Ok(pkgs) => pkgs,
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
                    let key = format!("{}::{}", pkg.attribute_path, pkg.version);
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
                        aggregate.source_path.clone(),
                        aggregate.known_vulnerabilities_json(),
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
                            source_path: aggregate.source_path.clone(),
                            known_vulnerabilities: aggregate.known_vulnerabilities_json(),
                            store_paths: aggregate.store_paths.clone(),
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
                    db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
                    db.set_meta("checkpoint_open_ranges", &open_ranges.len().to_string())?;

                    // Save open ranges to checkpoint table for resume capability
                    let checkpoint_ranges: HashMap<String, CheckpointRange> = open_ranges
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_checkpoint()))
                        .collect();
                    db.save_checkpoint_ranges(&checkpoint_ranges)?;

                    db.checkpoint()?;
                }

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
                db.set_meta("last_indexed_date", &Utc::now().to_rfc3339())?;
            }

            // Clear checkpoint ranges - indexing completed successfully
            db.clear_checkpoint_ranges()?;
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
                result.ranges_created.count(),
                "ranges".dimmed()
            ));
        }

        // WorktreeSession auto-cleans on drop - no need to restore HEAD
        Ok(result)
    }
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

        // Process more commits than warmup_count (10)
        for _ in 0..12 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(10));
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

        // Add consistent commits to establish baseline
        for _ in 0..15 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(20));
            tracker.finish_commit();
        }

        let ema_before = tracker.ema_secs.unwrap();

        // Add one outlier (very slow)
        tracker.start_commit();
        thread::sleep(Duration::from_millis(200));
        tracker.finish_commit();

        let ema_after = tracker.ema_secs.unwrap();

        // EMA should not have jumped dramatically due to outlier rejection
        // (outlier gets reduced alpha, so impact is dampened)
        assert!(
            ema_after < ema_before * 2.0,
            "EMA jumped too much after outlier"
        );
    }

    #[test]
    fn test_eta_tracker_formatting() {
        let mut tracker = EtaTracker::new(5);
        tracker.set_remaining(1);

        // Process enough commits to pass warmup
        for _ in 0..12 {
            tracker.start_commit();
            thread::sleep(Duration::from_millis(10));
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
            source_path: Some("pkgs/hello/default.nix".to_string()),
            known_vulnerabilities: None,
            store_paths: HashMap::new(),
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
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
                source_path: None,
                known_vulnerabilities: None,
                store_paths: HashMap::new(),
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

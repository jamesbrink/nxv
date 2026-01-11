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
use crate::memory::{DEFAULT_MEMORY_BUDGET, MIN_WORKER_MEMORY, MemorySize};
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
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tracing::{debug, debug_span, info, instrument, trace, warn};

/// Calculate per-worker memory from total budget.
///
/// Divides the total memory budget among all workers (system_count × range_count),
/// ensuring each worker gets at least MIN_WORKER_MEMORY.
fn calculate_per_worker_memory(
    budget: MemorySize,
    system_count: usize,
    range_count: usize,
) -> Result<usize> {
    let total_workers = system_count * range_count;
    let per_worker = budget
        .divide_among(total_workers, MIN_WORKER_MEMORY)
        .map_err(|e| NxvError::Config(e.to_string()))?;
    Ok(per_worker.as_mib() as usize)
}

/// A year range for parallel indexing.
///
/// Represents a time range for partitioning commits during parallel indexing.
/// Each range is processed by a separate worker with its own worktree.
#[derive(Debug, Clone)]
pub struct YearRange {
    /// Human-readable label for checkpointing (e.g., "2017" or "2017-2018").
    pub label: String,
    /// Start date (inclusive) in ISO format: "YYYY-MM-DD".
    pub since: String,
    /// End date (exclusive) in ISO format: "YYYY-MM-DD".
    pub until: String,
}

impl YearRange {
    /// Create a range for a single year.
    pub fn new(start_year: u16, end_year: u16) -> Self {
        Self {
            label: if start_year == end_year - 1 {
                format!("{}", start_year)
            } else {
                format!("{}-{}", start_year, end_year - 1)
            },
            since: format!("{}-01-01", start_year),
            until: format!("{}-01-01", end_year),
        }
    }

    /// Create a range for a specific month (useful for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn new_month(year: u16, month: u8) -> Self {
        Self::new_months(year, month, year, month)
    }

    /// Create a range spanning from start month to end month (inclusive).
    ///
    /// # Arguments
    /// * `start_year` - Starting year
    /// * `start_month` - Starting month (1-12, inclusive)
    /// * `end_year` - Ending year
    /// * `end_month` - Ending month (1-12, inclusive)
    pub fn new_months(start_year: u16, start_month: u8, end_year: u16, end_month: u8) -> Self {
        // Calculate the month after end_month for exclusive end date
        let (next_year, next_month) = if end_month == 12 {
            (end_year + 1, 1)
        } else {
            (end_year, end_month + 1)
        };

        // Create a descriptive label
        let label = if start_year == end_year && start_month == end_month {
            format!("{}-{:02}", start_year, start_month)
        } else if start_year == end_year {
            format!("{}-{:02}-{:02}", start_year, start_month, end_month)
        } else {
            format!(
                "{}-{:02}_{}-{:02}",
                start_year, start_month, end_year, end_month
            )
        };

        Self {
            label,
            since: format!("{}-{:02}-01", start_year, start_month),
            until: format!("{}-{:02}-01", next_year, next_month),
        }
    }

    /// Create a half-year range (H1 = Jan-Jun, H2 = Jul-Dec).
    pub fn new_half(year: u16, half: u8) -> Self {
        let (start_month, end_month) = match half {
            1 => (1, 6),
            2 => (7, 12),
            _ => (1, 6), // Default to H1
        };
        let mut range = Self::new_months(year, start_month, year, end_month);
        range.label = format!("{}-H{}", year, half);
        range
    }

    /// Create a quarter range (Q1-Q4).
    pub fn new_quarter(year: u16, quarter: u8) -> Self {
        let (start_month, end_month) = match quarter {
            1 => (1, 3),
            2 => (4, 6),
            3 => (7, 9),
            4 => (10, 12),
            _ => (1, 3), // Default to Q1
        };
        let mut range = Self::new_months(year, start_month, year, end_month);
        range.label = format!("{}-Q{}", year, quarter);
        range
    }

    /// Parse a range specification string.
    ///
    /// Supports multiple formats:
    /// - `"4"` - Auto-partition into N equal ranges
    /// - `"2017"` - Single year
    /// - `"2017-2020"` - Year range (2017 through 2019, exclusive end)
    /// - `"2017,2018,2019"` - Multiple individual years
    /// - `"2017-2019,2020-2024"` - Multiple ranges
    /// - `"2018-H1,2018-H2"` - Half-year ranges (H1=Jan-Jun, H2=Jul-Dec)
    /// - `"2018-Q1,2018-Q2"` - Quarter ranges (Q1-Q4)
    ///
    /// # Arguments
    /// * `spec` - The range specification string
    /// * `min_year` - Minimum year to consider (e.g., 2017)
    /// * `max_year` - Maximum year (exclusive, e.g., 2026 for up to 2025)
    ///
    /// # Examples
    /// ```
    /// let ranges = YearRange::parse_ranges("4", 2017, 2025).unwrap();
    /// assert_eq!(ranges.len(), 4); // 8 years split into 4 ranges
    /// ```
    pub fn parse_ranges(spec: &str, min_year: u16, max_year: u16) -> Result<Vec<Self>> {
        let spec = spec.trim();

        // Check if it's a small number (auto-partition) - numbers < 100 are counts, >= 100 could be years
        // This distinguishes "4" (partition into 4 ranges) from "2017" (single year)
        if let Ok(count) = spec.parse::<usize>() {
            // If the number looks like a year (>= 1970), treat it as a single year spec
            if count >= 1970 {
                let year = count as u16;
                return Ok(vec![Self::new(year, year + 1)]);
            }
            if count == 0 {
                return Err(NxvError::Config(
                    "Range count must be greater than 0".into(),
                ));
            }
            let total_years = (max_year - min_year) as usize;
            if count > total_years {
                return Err(NxvError::Config(format!(
                    "Cannot split {} years into {} ranges",
                    total_years, count
                )));
            }
            let years_per_range = total_years / count;
            let remainder = total_years % count;

            let mut ranges = Vec::new();
            let mut current = min_year;
            for i in 0..count {
                // Distribute remainder among first ranges
                let extra = if i < remainder { 1 } else { 0 };
                let end = current + years_per_range as u16 + extra as u16;
                ranges.push(Self::new(current, end));
                current = end;
            }
            return Ok(ranges);
        }

        // Parse comma-separated ranges/years
        let mut ranges = Vec::new();
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((year_str, suffix)) = part.split_once('-') {
                let year_str = year_str.trim();
                let suffix = suffix.trim().to_uppercase();

                // Check for half-year format: "2018-H1" or "2018-H2"
                if suffix == "H1" || suffix == "H2" {
                    let year: u16 = year_str
                        .parse()
                        .map_err(|_| NxvError::Config(format!("Invalid year: {}", year_str)))?;
                    let half: u8 = suffix.chars().last().unwrap().to_digit(10).unwrap() as u8;
                    ranges.push(Self::new_half(year, half));
                    continue;
                }

                // Check for quarter format: "2018-Q1" through "2018-Q4"
                if suffix.starts_with('Q')
                    && suffix.len() == 2
                    && let Some(q) = suffix.chars().last().and_then(|c| c.to_digit(10))
                    && (1..=4).contains(&q)
                {
                    let year: u16 = year_str
                        .parse()
                        .map_err(|_| NxvError::Config(format!("Invalid year: {}", year_str)))?;
                    ranges.push(Self::new_quarter(year, q as u8));
                    continue;
                }

                // Range format: "2017-2020"
                let start: u16 = year_str
                    .parse()
                    .map_err(|_| NxvError::Config(format!("Invalid year: {}", year_str)))?;
                let end: u16 = suffix
                    .parse()
                    .map_err(|_| NxvError::Config(format!("Invalid year or suffix: {}", suffix)))?;
                if start >= end {
                    return Err(NxvError::Config(format!(
                        "Invalid range: {} must be less than {}",
                        start, end
                    )));
                }
                ranges.push(Self::new(start, end + 1)); // +1 because end is inclusive in input
            } else {
                // Single year: "2017"
                let year: u16 = part
                    .parse()
                    .map_err(|_| NxvError::Config(format!("Invalid year: {}", part)))?;
                ranges.push(Self::new(year, year + 1));
            }
        }

        if ranges.is_empty() {
            return Err(NxvError::Config("No valid ranges specified".into()));
        }

        Ok(ranges)
    }
}

/// Result from indexing a single year range (for parallel indexing).
#[derive(Debug, Default, Clone)]
pub struct RangeIndexResult {
    /// Label of the range that was processed.
    #[allow(dead_code)] // Used for logging and debugging
    pub range_label: String,
    /// Number of commits successfully processed in this range.
    pub commits_processed: u64,
    /// Total number of package extractions in this range.
    pub packages_found: u64,
    /// Number of packages upserted from this range.
    pub packages_upserted: u64,
    /// Number of extraction failures in this range.
    pub extraction_failures: u64,
    /// Whether this range's indexing was interrupted.
    pub was_interrupted: bool,
}

/// Configuration for the indexer.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Number of commits between checkpoints.
    pub checkpoint_interval: usize,
    /// Whether to show progress output (currently unused, reserved for future use).
    #[allow(dead_code)]
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
    /// Total memory budget for all workers combined.
    /// Automatically divided among workers (systems × range_workers).
    pub memory_budget: MemorySize,
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
    /// Interval for full package extraction (every N commits).
    /// This catches packages missed by incremental detection (e.g., firefox
    /// versions defined in packages.nix but assigned in all-packages.nix).
    /// Set to 0 to disable periodic full extraction.
    /// Default: 50
    pub full_extraction_interval: u32,
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
            memory_budget: DEFAULT_MEMORY_BUDGET,
            verbose: false,
            gc_interval: 20, // GC every 20 checkpoints (2000 commits by default)
            gc_min_free_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            full_extraction_interval: 50, // Full extraction every 50 commits
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

/// Simple progress tracker that tracks percentage completion.
///
/// This is a lightweight replacement for EtaTracker that only tracks
/// progress percentage without ETA calculations.
pub(super) struct ProgressTracker {
    /// Number of items processed
    processed: u64,
    /// Total items to process
    total: u64,
    /// Label for logging
    label: String,
    /// Interval for logging progress (percentage points)
    log_interval_pct: f64,
    /// Last logged percentage (to avoid duplicate logs)
    last_logged_pct: f64,
}

impl ProgressTracker {
    /// Creates a new progress tracker.
    ///
    /// # Arguments
    /// * `total` - Total number of items to process
    /// * `label` - Label for log messages
    fn new(total: u64, label: &str) -> Self {
        Self {
            processed: 0,
            total,
            label: label.to_string(),
            log_interval_pct: 5.0, // Log every 5%
            last_logged_pct: -1.0,
        }
    }

    /// Record that an item was processed.
    fn tick(&mut self) {
        self.processed += 1;
    }

    /// Get current percentage complete.
    fn percentage(&self) -> f64 {
        if self.total == 0 {
            return 100.0;
        }
        (self.processed as f64 / self.total as f64) * 100.0
    }

    /// Get number of items processed.
    #[allow(dead_code)]
    fn processed(&self) -> u64 {
        self.processed
    }

    /// Check if we should log progress at this point.
    fn should_log(&self) -> bool {
        let pct = self.percentage();
        let pct_floored = (pct / self.log_interval_pct).floor() * self.log_interval_pct;
        pct_floored > self.last_logged_pct
    }

    /// Mark progress as logged at current percentage.
    fn mark_logged(&mut self) {
        let pct = self.percentage();
        self.last_logged_pct = (pct / self.log_interval_pct).floor() * self.log_interval_pct;
    }

    /// Log progress if it's time.
    fn log_if_needed(&mut self, extra_info: &str) {
        if self.should_log() {
            info!(
                target: "nxv::index",
                "{}: {:.1}% ({}/{}) {}",
                self.label,
                self.percentage(),
                self.processed,
                self.total,
                extra_info
            );
            self.mark_logged();
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

        // Clean up all eval stores from previous runs
        info!(target: "nxv::index", "Cleaning up temporary eval stores from previous runs...");
        let temp_store_freed = gc::cleanup_all_eval_stores();
        if temp_store_freed > 0 {
            let freed_mb = temp_store_freed as f64 / 1024.0 / 1024.0;
            info!(
                target: "nxv::index",
                freed_mb = format!("{:.1}", freed_mb),
                "Freed space from temporary eval stores"
            );
        }

        // Check store health before starting
        if !gc::verify_store() {
            warn!(
                target: "nxv::index",
                "Nix store verification failed. Run 'nix-store --verify --repair' to fix."
            );
        }

        // Check available disk space
        if gc::is_store_low_on_space(self.config.gc_min_free_bytes) {
            warn!(target: "nxv::index", "Low disk space detected. Running garbage collection...");
            if let Some(duration) = gc::run_garbage_collection() {
                info!(
                    target: "nxv::index",
                    "Garbage collection completed in {:.1}s",
                    duration.as_secs_f64()
                );
            }
        }

        let mut db = Database::open(&db_path)?;

        info!(target: "nxv::index", "Performing full index rebuild...");
        debug!(
            target: "nxv::index",
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

        info!(
            target: "nxv::index",
            "Found {} indexable commits with package changes (starting from {})",
            total_commits,
            self.config
                .since
                .as_deref()
                .unwrap_or(git::MIN_INDEXABLE_DATE)
        );

        // Report temp store cleanup after "Found X commits"
        if temp_store_freed > 0 {
            info!(
                target: "nxv::index",
                "Cleaned up eval stores ({:.1} MB freed)",
                temp_store_freed as f64 / 1_000_000.0
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

        // Clean up all eval stores from previous runs
        info!(target: "nxv::index", "Cleaning up temporary eval stores from previous runs...");
        let temp_store_freed = gc::cleanup_all_eval_stores();
        if temp_store_freed > 0 {
            let freed_mb = temp_store_freed as f64 / 1024.0 / 1024.0;
            info!(
                target: "nxv::index",
                freed_mb = format!("{:.1}", freed_mb),
                "Freed space from temporary eval stores"
            );
        }

        // Check store health before starting
        if !gc::verify_store() {
            warn!(
                target: "nxv::index",
                "Nix store verification failed. Run 'nix-store --verify --repair' to fix."
            );
        }

        // Check available disk space
        if gc::is_store_low_on_space(self.config.gc_min_free_bytes) {
            warn!(target: "nxv::index", "Low disk space detected. Running garbage collection...");
            if let Some(duration) = gc::run_garbage_collection() {
                info!(
                    target: "nxv::index",
                    "Garbage collection completed in {:.1}s",
                    duration.as_secs_f64()
                );
            }
        }

        let mut db = Database::open(&db_path)?;

        // Log startup configuration
        let db_size_mb = std::fs::metadata(&db_path)
            .map(|m| m.len() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);
        let schema_version = db.get_meta("schema_version")?.unwrap_or_default();
        let package_count = db.get_package_count().unwrap_or(0);

        info!(
            target: "nxv::index",
            version = env!("CARGO_PKG_VERSION"),
            db_size_mb = format!("{:.1}", db_size_mb),
            schema_version = schema_version,
            packages = package_count,
            checkpoint_interval = self.config.checkpoint_interval,
            memory_budget = %self.config.memory_budget,
            systems = ?self.config.systems,
            gc_interval = self.config.gc_interval,
            "Indexer initialized"
        );

        // Check for last indexed commit across all checkpoint types
        // This unifies regular incremental and year-range checkpoints
        let last_commit = db.get_latest_checkpoint()?;

        match last_commit {
            Some(hash) => {
                info!(
                    target: "nxv::index",
                    commit = &hash[..7],
                    "Resuming from checkpoint"
                );

                // Get current HEAD
                let head_hash = repo.head_commit()?;

                // Check if HEAD is an ancestor of last_indexed_commit
                // This means the repo has been reset to an older state
                if head_hash != hash {
                    match repo.is_ancestor(&head_hash, &hash) {
                        Ok(true) => {
                            tracing::error!(
                                target: "nxv::index",
                                "Repository HEAD ({}) is older than last indexed commit ({}). \
                                This can happen if the repository was reset or the submodule is out of date. \
                                Update your nixpkgs repository or use --full to rebuild the index.",
                                &head_hash[..7],
                                &hash[..7]
                            );
                            return Err(NxvError::Git(git2::Error::from_str(
                                "Repository HEAD is behind last indexed commit. Run with --full to rebuild.",
                            )));
                        }
                        Ok(false) => {
                            // HEAD is not an ancestor, so it's either ahead or diverged - continue normally
                        }
                        Err(e) => {
                            // If we can't check ancestry, warn but continue
                            warn!(
                                target: "nxv::index",
                                "Could not verify commit ancestry: {}",
                                e
                            );
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
                            info!(target: "nxv::index", "Index is already up to date.");
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
                        info!(
                            target: "nxv::index",
                            "Found {} new commits to process",
                            commits.len()
                        );

                        // Report temp store cleanup after "Found X commits"
                        if temp_store_freed > 0 {
                            info!(
                                target: "nxv::index",
                                "Cleaned up eval stores ({:.1} MB freed)",
                                temp_store_freed as f64 / 1_000_000.0
                            );
                        }

                        self.process_commits(&mut db, &nixpkgs_path, &repo, commits, Some(&hash))
                    }
                    Err(_) => {
                        warn!(
                            target: "nxv::index",
                            "Last indexed commit {} not found in repository. \
                            This may indicate a rebase. Consider running with --full.",
                            &hash[..7]
                        );
                        Err(NxvError::Git(git2::Error::from_str(
                            "Last indexed commit not found. Run with --full to rebuild.",
                        )))
                    }
                }
            }
            None => {
                info!(target: "nxv::index", "No previous index found, performing full index.");
                self.index_full(nixpkgs_path, db_path)
            }
        }
    }

    /// Run parallel indexing across multiple year ranges.
    ///
    /// Each range is processed by a separate thread with its own git worktree.
    /// Results are merged into a single database via UPSERT semantics - the
    /// MIN/MAX bounds logic ensures correct merging regardless of processing order.
    ///
    /// # Arguments
    /// * `nixpkgs_path` - Path to nixpkgs repository
    /// * `db_path` - Path to database file
    /// * `ranges` - Year ranges to process in parallel
    ///
    /// # Example
    /// ```no_run
    /// let indexer = Indexer::new(IndexerConfig::default());
    /// let ranges = YearRange::parse_ranges("2017,2018,2019", 2017, 2025)?;
    /// let result = indexer.index_parallel_ranges("./nixpkgs", "./index.db", ranges, 4)?;
    /// ```
    pub fn index_parallel_ranges<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        nixpkgs_path: P,
        db_path: Q,
        ranges: Vec<YearRange>,
        max_range_workers: usize,
    ) -> Result<IndexResult> {
        use std::sync::Mutex;
        use std::thread;

        let nixpkgs_path = nixpkgs_path.as_ref();
        let db_path = db_path.as_ref();

        // Validate we have ranges to process
        if ranges.is_empty() {
            return Err(NxvError::Config(
                "No ranges specified for parallel indexing".into(),
            ));
        }

        let repo = NixpkgsRepo::open(nixpkgs_path)?;

        // Clean up orphaned worktrees from previous crashed runs
        repo.prune_worktrees()?;

        // Clean up all eval stores from previous runs
        info!(target: "nxv::index", "Cleaning up temporary eval stores from previous runs...");
        let temp_store_freed = gc::cleanup_all_eval_stores();
        if temp_store_freed > 0 {
            let freed_mb = temp_store_freed as f64 / 1024.0 / 1024.0;
            info!(
                target: "nxv::index",
                freed_mb = format!("{:.1}", freed_mb),
                "Freed space from temporary eval stores"
            );
        }

        // Check store health
        if !gc::verify_store() {
            warn!(
                target: "nxv::index",
                "Nix store verification failed. Run 'nix-store --verify --repair' to fix."
            );
        }

        // Check available disk space
        if gc::is_store_low_on_space(self.config.gc_min_free_bytes) {
            warn!(target: "nxv::index", "Low disk space detected. Running garbage collection...");
            if let Some(duration) = gc::run_garbage_collection() {
                info!(
                    target: "nxv::index",
                    "Garbage collection completed in {:.1}s",
                    duration.as_secs_f64()
                );
            }
        }

        // Open database with mutex for thread-safe access
        let db = Arc::new(Mutex::new(Database::open(db_path)?));

        info!(
            target: "nxv::index",
            "Starting parallel indexing with {} ranges",
            ranges.len()
        );
        for range in &ranges {
            info!(
                target: "nxv::index",
                "  Range {}: {} to {}",
                range.label, range.since, range.until
            );
        }

        // Shared shutdown flag
        let shutdown = self.shutdown.clone();

        // Collect results from all range workers
        let results: Arc<Mutex<Vec<RangeIndexResult>>> = Arc::new(Mutex::new(Vec::new()));
        let errors: Arc<Mutex<Vec<(String, NxvError)>>> = Arc::new(Mutex::new(Vec::new()));

        // Limit the number of concurrent range workers
        let effective_max_workers = max_range_workers.max(1).min(ranges.len());

        // Calculate per-worker memory: total budget / (ranges × systems)
        let worker_count = self
            .config
            .worker_count
            .unwrap_or(self.config.systems.len());
        let per_worker_memory_mib = calculate_per_worker_memory(
            self.config.memory_budget,
            worker_count,
            effective_max_workers,
        )?;

        info!(
            target: "nxv::index",
            range_workers = effective_max_workers,
            system_workers = worker_count,
            total_workers = effective_max_workers * worker_count,
            per_worker_mib = per_worker_memory_mib,
            total_budget = %self.config.memory_budget,
            "Memory allocation for parallel indexing"
        );

        // Process ranges in batches to limit concurrency
        for batch in ranges.chunks(effective_max_workers) {
            // Check for shutdown before starting new batch
            if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                info!(target: "nxv::index", "Shutdown requested, skipping remaining batches");
                break;
            }

            // Process this batch in parallel using scoped threads
            thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|range| {
                        let db = db.clone();
                        let nixpkgs_path = nixpkgs_path.to_path_buf();
                        let shutdown = shutdown.clone();
                        let results = results.clone();
                        let errors = errors.clone();
                        let config = self.config.clone();
                        let range = range.clone();

                        s.spawn(move || {
                            match process_range_worker(
                                &nixpkgs_path,
                                db,
                                range.clone(),
                                &config,
                                per_worker_memory_mib,
                                shutdown,
                            ) {
                                Ok(result) => {
                                    results.lock().unwrap().push(result);
                                }
                                Err(e) => {
                                    errors.lock().unwrap().push((range.label, e));
                                }
                            }
                        })
                    })
                    .collect();

                // Wait for all workers in this batch to complete
                for handle in handles {
                    let _ = handle.join();
                }
            });
        }

        // Check for errors
        let errors = errors.lock().unwrap();
        if !errors.is_empty() {
            for (label, error) in errors.iter() {
                tracing::error!(
                    target: "nxv::index",
                    "Error in range {}: {}",
                    label, error
                );
            }
            // Return first error
            if let Some((label, _)) = errors.first() {
                return Err(NxvError::Config(format!(
                    "Parallel indexing failed for range {}",
                    label
                )));
            }
        }

        // Aggregate results
        let mut final_result = IndexResult::default();
        let results = results.lock().unwrap();
        for range_result in results.iter() {
            final_result.merge(range_result.clone());
        }

        // Calculate unique names from database
        {
            let db_guard = db.lock().unwrap();
            final_result.unique_names = db_guard
                .connection()
                .query_row(
                    "SELECT COUNT(DISTINCT attribute_path) FROM package_versions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0) as u64;
        }

        // Sync global checkpoint to the latest across all ranges
        // This ensures regular incremental indexing picks up where parallel left off
        {
            let db_guard = db.lock().unwrap();
            if let Ok(Some(latest)) = db_guard.get_latest_checkpoint() {
                let _ = db_guard.set_meta("last_indexed_commit", &latest);
                let _ = db_guard.set_meta("last_indexed_date", &chrono::Utc::now().to_rfc3339());
                debug!(
                    target: "nxv::index",
                    "Synced global checkpoint to {}",
                    &latest[..7]
                );
            }
        }

        info!(
            target: "nxv::index",
            "Parallel indexing complete: {} commits, {} packages upserted",
            final_result.commits_processed, final_result.packages_upserted
        );

        Ok(final_result)
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
    #[instrument(level = "debug", skip(self, db, nixpkgs_path, repo, commits, resume_from), fields(total_commits = commits.len()))]
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
            // Calculate per-worker memory from total budget
            let per_worker_mib = calculate_per_worker_memory(
                self.config.memory_budget,
                worker_count,
                1, // single range mode
            )?;
            let pool_config = worker::WorkerPoolConfig {
                worker_count,
                per_worker_memory_mib: per_worker_mib,
                ..Default::default()
            };
            match worker::WorkerPool::new(pool_config) {
                Ok(pool) => Some(pool),
                Err(e) => {
                    warn!(
                        target: "nxv::index",
                        "Failed to create worker pool ({}), falling back to sequential",
                        e
                    );
                    None
                }
            }
        } else {
            if systems.len() > 1 {
                info!(target: "nxv::index", "Using sequential evaluation (--workers=1)");
            }
            None
        };

        // Progress tracking
        let mut progress = ProgressTracker::new(total_commits as u64, "Indexing");

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
                    warn!(
                        target: "nxv::index",
                        "Initial file-to-attribute map failed ({}), using empty map",
                        e
                    );
                    (HashMap::new(), String::new())
                }
            };

        // Log start of indexing
        info!(
            target: "nxv::index",
            total_commits = total_commits,
            first_commit = %first_commit.short_hash,
            "Starting commit processing"
        );

        // Process commits sequentially
        for (commit_idx, commit) in commits.iter().enumerate() {
            // Check for shutdown
            if self.is_shutdown_requested() {
                info!(target: "nxv::index", "Shutdown requested, saving checkpoint...");
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
                    info!(
                        target: "nxv::index",
                        "Saved checkpoint at {}",
                        &last_hash[..7]
                    );
                }

                break;
            }

            // Checkout the commit in the worktree
            if let Err(e) = session.checkout(&commit.hash) {
                warn!(
                    target: "nxv::index",
                    "Failed to checkout {}: {}",
                    &commit.short_hash, e
                );
                progress.tick();
                continue;
            }

            // Get changed paths
            let changed_paths = match repo.get_commit_changed_paths(&commit.hash) {
                Ok(paths) => paths,
                Err(e) => {
                    warn!(
                        target: "nxv::index",
                        "Failed to list changes for {}: {}",
                        &commit.short_hash, e
                    );
                    progress.tick();
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
            // Also trigger periodic full extraction every N commits to catch packages
            // that can't be detected from file paths (e.g., firefox versions in packages.nix)
            let periodic_full = self.config.full_extraction_interval > 0
                && (commit_idx + 1) % self.config.full_extraction_interval as usize == 0;
            let mut needs_full_extraction = commit_idx == 0 || periodic_full;
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

            // If full extraction is needed (first commit, periodic, or large infrastructure diff),
            // extract all packages from all-packages.nix
            // Track whether we should do full extraction with empty target list (triggers builtins.attrNames)
            let mut extract_all_packages = false;

            if needs_full_extraction {
                if let Some(all_attrs_list) = all_attrs {
                    for attr in all_attrs_list {
                        target_attr_paths.insert(attr.clone());
                    }
                    if commit_idx == 0 {
                        debug!(
                            commit = %commit.short_hash,
                            total_attrs = target_attr_paths.len(),
                            "Full extraction for first commit to capture baseline state"
                        );
                    } else if periodic_full {
                        debug!(
                            commit = %commit.short_hash,
                            total_attrs = target_attr_paths.len(),
                            interval = self.config.full_extraction_interval,
                            "Periodic full extraction to catch missed packages"
                        );
                    } else {
                        debug!(
                            commit = %commit.short_hash,
                            total_attrs = target_attr_paths.len(),
                            "Full extraction triggered due to large infrastructure diff"
                        );
                    }
                } else {
                    // all_attrs is None (file_attr_map failed or is empty)
                    // Pass empty list to extraction which triggers builtins.attrNames in Nix
                    extract_all_packages = true;
                    debug!(
                        commit = %commit.short_hash,
                        reason = if commit_idx == 0 { "first_commit" } else if periodic_full { "periodic" } else { "infrastructure_diff" },
                        "Full extraction with dynamic attr discovery (file_attr_map unavailable)"
                    );
                }
            }

            for path in &changed_paths {
                if let Some(attr_paths) = file_attr_map.get(path) {
                    // Path is in the file-to-attr map (built from HEAD)
                    for attr in attr_paths {
                        target_attr_paths.insert(attr.clone());
                    }
                } else if let Some(attr) = extract_attr_from_path(path) {
                    // Fallback: extract attr name from path structure
                    // No validation against all_attrs - this allows detecting changes
                    // to packages that have moved (e.g., to pkgs/by-name/) since HEAD
                    target_attr_paths.insert(attr);
                } else if path.ends_with(".nix") && path.starts_with("pkgs/") {
                    // Changed .nix file in pkgs/ but couldn't determine attribute
                    // This happens for files like firefox/packages.nix where the filename
                    // doesn't match the package name. Trigger full extraction to be safe.
                    if !needs_full_extraction {
                        debug!(
                            path = path,
                            commit = %commit.short_hash,
                            "Unknown package file changed, triggering full extraction"
                        );
                        needs_full_extraction = true;
                        // Add all packages since we now need full extraction
                        if let Some(all_attrs_list) = all_attrs {
                            for attr in all_attrs_list {
                                target_attr_paths.insert(attr.clone());
                            }
                        }
                    }
                }
            }

            // Skip commits with no targets UNLESS we're doing full extraction with dynamic discovery
            if target_attr_paths.is_empty() && !extract_all_packages {
                result.commits_processed += 1;
                last_processed_commit = Some(commit.hash.clone());
                progress.tick();
                continue;
            }

            // When extract_all_packages is true, pass empty list to trigger builtins.attrNames in Nix
            let mut target_list: Vec<String> = target_attr_paths.into_iter().collect();
            target_list.sort();

            // Log progress at INFO level periodically (every 50 commits or at milestones)
            let progress_pct = ((commit_idx + 1) as f64 / total_commits as f64 * 100.0) as u32;
            if commit_idx == 0
                || (commit_idx + 1) % 50 == 0
                || commit_idx + 1 == total_commits
                || progress_pct.is_multiple_of(10)
                    && progress_pct != ((commit_idx) as f64 / total_commits as f64 * 100.0) as u32
            {
                info!(
                    target: "nxv::index",
                    commit = %commit.short_hash,
                    date = %commit.date.format("%Y-%m-%d"),
                    progress = %format!("{}/{}", commit_idx + 1, total_commits),
                    percent = progress_pct,
                    targets = target_list.len(),
                    "Processing commit"
                );
            }

            debug!(
                commit = %commit.short_hash,
                target_count = target_list.len(),
                "Processing commit details"
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
                let _extract_span = debug_span!(
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
                        if self.config.verbose {
                            warn!(
                                target: "nxv::index",
                                "Extraction failed at {} ({}): {}",
                                &commit.short_hash, system, e
                            );
                        } else {
                            debug!(
                                commit = %commit.short_hash,
                                system = %system,
                                error = %e,
                                "Extraction failed for system"
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

            // Update progress and log if needed
            progress.tick();
            progress.log_if_needed(&format!(
                "pkgs={} upserted={}",
                result.packages_found,
                result.packages_upserted + pending_upserts.len() as u64
            ));

            // Checkpoint if needed
            if (commit_idx + 1).is_multiple_of(self.config.checkpoint_interval)
                || commit_idx + 1 == commits.len()
            {
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
                    debug!(target: "nxv::index", "Running garbage collection...");

                    // Clean up all eval stores to free disk space
                    let bytes = gc::cleanup_all_eval_stores();
                    if bytes > 0 {
                        info!(
                            target: "nxv::index",
                            "Cleaned up eval stores ({:.1} MB freed)",
                            bytes as f64 / 1_000_000.0
                        );
                    }

                    if let Some(duration) = gc::run_garbage_collection() {
                        info!(
                            target: "nxv::index",
                            "Completed garbage collection in {:.1}s",
                            duration.as_secs_f64()
                        );
                    } else {
                        debug!(target: "nxv::index", "Skipped garbage collection (GC command failed)");
                    }
                    checkpoints_since_gc = 0;
                }

                info!(
                    target: "nxv::index",
                    commit = %commit.short_hash,
                    progress = %format!("{}/{}", commit_idx + 1, total_commits),
                    upserted = result.packages_upserted,
                    "Checkpoint saved"
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

        // Log final summary
        info!(
            target: "nxv::index",
            "Indexing complete: {} commits, {} pkgs found, {} upserted",
            result.commits_processed,
            result.packages_found,
            result.packages_upserted
        );

        // WorktreeSession auto-cleans on drop - no need to restore HEAD

        // Clean up all eval stores on exit
        let bytes = gc::cleanup_all_eval_stores();
        if bytes > 0 {
            info!(
                target: "nxv::index",
                "Cleaned up eval stores ({:.1} MB freed)",
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

/// Directories under pkgs/ that contain infrastructure, not packages.
/// Files in these directories should not be treated as package definitions.
const NON_PACKAGE_PREFIXES: &[&str] = &[
    "pkgs/build-support/",
    "pkgs/stdenv/",
    "pkgs/top-level/",
    "pkgs/test/",
    "pkgs/pkgs-lib/",
];

/// Filenames (without .nix) that are clearly NOT package attribute names.
/// When these files change, we can't determine the affected package from the path alone,
/// so we need to trigger a full extraction for that commit.
/// Examples: firefox/packages.nix defines firefox but "packages" isn't the attr name.
const AMBIGUOUS_FILENAMES: &[&str] = &[
    "packages",
    "common",
    "wrapper",
    "update",
    "generated",
    "sources",
    "versions",
    "metadata",
    "overrides",
    "extensions",
    "browser",
    "bin",
    "unwrapped",
];

/// Extract an attribute name from a nixpkgs file path.
///
/// This function handles both modern `pkgs/by-name/` structure and traditional
/// paths like `pkgs/tools/graphics/jhead/default.nix`.
///
/// Returns `None` for:
/// - Infrastructure files (build-support, stdenv, etc.)
/// - Files that don't match expected patterns
/// - Empty or invalid names
/// - Ambiguous filenames like `packages.nix`, `common.nix` that don't correspond
///   to attribute names (triggers full extraction fallback)
///
/// # Examples
/// - `pkgs/by-name/jh/jhead/package.nix` → `Some("jhead")`
/// - `pkgs/tools/graphics/jhead/default.nix` → `Some("jhead")`
/// - `pkgs/development/python-modules/requests/default.nix` → `Some("requests")`
/// - `pkgs/build-support/fetchurl/default.nix` → `None`
/// - `pkgs/applications/networking/browsers/firefox/packages.nix` → `None`
fn extract_attr_from_path(path: &str) -> Option<String> {
    // Must be a .nix file under pkgs/
    if !path.starts_with("pkgs/") || !path.ends_with(".nix") {
        return None;
    }

    // Skip infrastructure directories
    if NON_PACKAGE_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return None;
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return None;
    }

    // pkgs/by-name/XX/pkgname/package.nix → pkgname
    if path.starts_with("pkgs/by-name/") && parts.len() >= 4 {
        let pkg_name = parts[3];
        if !pkg_name.is_empty() {
            return Some(pkg_name.to_string());
        }
        return None;
    }

    // Traditional paths: extract from directory or filename
    let potential_name = if parts.last() == Some(&"default.nix") && parts.len() >= 2 {
        // pkgs/.../something/default.nix → something
        parts[parts.len() - 2]
    } else {
        // pkgs/.../something.nix → something
        parts
            .last()
            .map(|f| f.trim_end_matches(".nix"))
            .unwrap_or("")
    };

    if potential_name.is_empty() {
        return None;
    }

    // Reject ambiguous filenames that are clearly not package attribute names.
    // When firefox/packages.nix changes, we can't know the affected package is "firefox"
    // from the path alone. Returning None signals the caller to trigger full extraction.
    if AMBIGUOUS_FILENAMES.contains(&potential_name) {
        return None;
    }

    Some(potential_name.to_string())
}

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
#[derive(Debug, Default)]
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

impl IndexResult {
    /// Merge results from a range worker into this aggregate result.
    ///
    /// Used in parallel indexing to combine results from multiple year range workers.
    pub fn merge(&mut self, other: RangeIndexResult) {
        self.commits_processed += other.commits_processed;
        self.packages_found += other.packages_found;
        self.packages_upserted += other.packages_upserted;
        self.extraction_failures += other.extraction_failures;
        self.was_interrupted |= other.was_interrupted;
        // Note: unique_names is not tracked per-range, will be calculated at the end
    }
}

/// Worker function for processing a single year range (runs in its own thread).
///
/// This function:
/// 1. Opens its own repo handle and creates a dedicated worktree
/// 2. Fetches commits for the specified date range
/// 3. Checks for and resumes from any existing checkpoint
/// 4. Processes commits and UPSERTs packages to the shared database
/// 5. Saves checkpoints periodically
fn process_range_worker(
    nixpkgs_path: &Path,
    db: Arc<std::sync::Mutex<Database>>,
    range: YearRange,
    config: &IndexerConfig,
    per_worker_memory_mib: usize,
    shutdown: Arc<AtomicBool>,
) -> Result<RangeIndexResult> {
    use crate::db::queries::PackageVersion;

    let mut result = RangeIndexResult {
        range_label: range.label.clone(),
        ..Default::default()
    };

    // Open repo and create worktree for this range
    let repo = NixpkgsRepo::open(nixpkgs_path)?;

    // Get commits for this range
    let commits = repo.get_indexable_commits_touching_paths(
        &["pkgs"],
        Some(&range.since),
        Some(&range.until),
    )?;

    if commits.is_empty() {
        debug!(target: "nxv::index", "Range {}: no commits", range.label);
        return Ok(result);
    }

    // Check for resume point
    let resume_from = {
        let db_guard = db.lock().unwrap();
        db_guard.get_range_checkpoint(&range.label)?
    };

    // Filter commits if resuming
    let commits: Vec<_> = if let Some(ref resume_hash) = resume_from {
        // Find the index of the resume commit and skip everything before it
        if let Some(pos) = commits.iter().position(|c| c.hash == *resume_hash) {
            commits.into_iter().skip(pos + 1).collect()
        } else {
            // Resume commit not found, process all
            commits
        }
    } else {
        commits
    };

    let total_commits = commits.len();
    if total_commits == 0 {
        debug!(target: "nxv::index", "Range {}: already complete", range.label);
        return Ok(result);
    }

    // Log progress for this range
    let mut progress =
        ProgressTracker::new(total_commits as u64, &format!("Range {}", range.label));
    if resume_from.is_some() {
        info!(target: "nxv::index", "Range {}: resuming ({} commits)", range.label, total_commits);
    } else {
        info!(target: "nxv::index", "Range {}: processing {} commits", range.label, total_commits);
    }

    // Get first commit for worktree creation
    let first_commit = commits
        .first()
        .ok_or_else(|| NxvError::Git(git2::Error::from_str("No commits to process in range")))?;

    // Create dedicated worktree for this range
    let worktree = WorktreeSession::new(&repo, &first_commit.hash)?;
    let worktree_path = worktree.path();

    // Determine if we should use parallel evaluation for systems
    let systems = &config.systems;
    let use_parallel = config.worker_count != Some(1) && systems.len() > 1;
    let worker_count = config.worker_count.unwrap_or(systems.len());

    // Create worker pool for parallel system evaluation (if enabled)
    // Each range gets its own eval store to avoid SQLite contention
    let worker_pool = if use_parallel && worker_count > 1 {
        let eval_store_path = format!("{}-{}", gc::TEMP_EVAL_STORE_PATH, range.label);
        let pool_config = worker::WorkerPoolConfig {
            worker_count,
            per_worker_memory_mib,
            eval_store_path: Some(eval_store_path),
            ..Default::default()
        };
        worker::WorkerPool::new(pool_config).ok()
    } else {
        None
    };

    // Build initial file-to-attribute map
    let (mut file_attr_map, mut mapping_commit) =
        match build_file_attr_map(worktree_path, systems, worker_pool.as_ref()) {
            Ok(map) => (map, first_commit.hash.clone()),
            Err(_) => (HashMap::new(), String::new()),
        };

    // Buffer for batch UPSERT operations
    let mut pending_upserts: Vec<PackageVersion> = Vec::new();

    // Process commits
    for (commit_idx, commit) in commits.iter().enumerate() {
        let commit_start = std::time::Instant::now();

        // Check for shutdown
        if shutdown.load(Ordering::SeqCst) {
            result.was_interrupted = true;

            // Save checkpoint and flush pending
            if !pending_upserts.is_empty() {
                let mut db_guard = db.lock().unwrap();
                result.packages_upserted +=
                    db_guard.upsert_packages_batch(&pending_upserts)? as u64;
            }
            if let Some(last) = commits.get(commit_idx.saturating_sub(1)) {
                let db_guard = db.lock().unwrap();
                db_guard.set_range_checkpoint(&range.label, &last.hash)?;
            }

            info!(target: "nxv::index", "Range {}: interrupted", range.label);
            return Ok(result);
        }

        // Checkout commit
        let checkout_start = std::time::Instant::now();
        worktree.checkout(&commit.hash)?;
        let checkout_ms = checkout_start.elapsed().as_millis();
        tracing::trace!(
            range = %range.label,
            commit = %&commit.hash[..8],
            checkout_ms = checkout_ms,
            "git checkout"
        );

        // Get changed files for this commit
        let changed_paths = repo.get_commit_changed_paths(&commit.hash)?;

        // Check if we need to rebuild the file-to-attribute map
        let need_refresh = file_attr_map.is_empty() || should_refresh_file_map(&changed_paths);
        if need_refresh && mapping_commit != commit.hash {
            let map_start = std::time::Instant::now();
            if let Ok(new_map) = build_file_attr_map(worktree_path, systems, worker_pool.as_ref()) {
                let map_ms = map_start.elapsed().as_millis();
                tracing::trace!(
                    range = %range.label,
                    commit = %&commit.hash[..8],
                    map_entries = new_map.len(),
                    map_ms = map_ms,
                    "rebuilt file-attr map"
                );
                file_attr_map = new_map;
                mapping_commit = commit.hash.clone();
            }
        }

        // Determine target attributes
        let mut target_attr_paths: HashSet<String> = HashSet::new();
        let all_attrs: Option<&Vec<String>> = file_attr_map.get("pkgs/top-level/all-packages.nix");

        // Check for infrastructure files and parse their diffs
        // Also trigger periodic full extraction every N commits to catch packages
        // that can't be detected from file paths (e.g., firefox versions in packages.nix)
        let periodic_full = config.full_extraction_interval > 0
            && (commit_idx + 1) % config.full_extraction_interval as usize == 0;
        let mut needs_full_extraction = commit_idx == 0 || periodic_full;
        for infra_file in INFRASTRUCTURE_FILES {
            if changed_paths.contains(&infra_file.to_string())
                && let Ok(diff) = repo.get_file_diff(&commit.hash, infra_file)
            {
                if let Some(extracted_attrs) = extract_attrs_from_diff(&diff) {
                    for attr in extracted_attrs {
                        if let Some(all_attrs_list) = all_attrs {
                            if all_attrs_list.contains(&attr) {
                                target_attr_paths.insert(attr);
                            }
                        } else {
                            target_attr_paths.insert(attr);
                        }
                    }
                } else {
                    needs_full_extraction = true;
                }
            }
        }

        // Full extraction for first commit, periodic interval, or large infrastructure diff
        // Track whether we should do full extraction with empty target list (triggers builtins.attrNames)
        let mut extract_all_packages = false;

        if needs_full_extraction {
            if let Some(all_attrs_list) = all_attrs {
                for attr in all_attrs_list {
                    target_attr_paths.insert(attr.clone());
                }
            } else {
                // all_attrs is None (file_attr_map failed or is empty)
                // Pass empty list to extraction which triggers builtins.attrNames in Nix
                extract_all_packages = true;
                tracing::debug!(
                    range = %range.label,
                    commit = %&commit.hash[..8],
                    reason = if commit_idx == 0 { "first_commit" } else if periodic_full { "periodic" } else { "infrastructure_diff" },
                    "Full extraction with dynamic attr discovery (file_attr_map unavailable)"
                );
            }
        }

        // Add attrs from changed package files
        for path in &changed_paths {
            if let Some(attr_paths) = file_attr_map.get(path) {
                // Path is in the file-to-attr map (built from HEAD)
                for attr in attr_paths {
                    target_attr_paths.insert(attr.clone());
                }
            } else if let Some(attr) = extract_attr_from_path(path) {
                // Fallback: extract attr name from path structure
                // No validation against all_attrs - this allows detecting changes
                // to packages that have moved (e.g., to pkgs/by-name/) since HEAD
                target_attr_paths.insert(attr);
            } else if path.ends_with(".nix") && path.starts_with("pkgs/") {
                // Changed .nix file in pkgs/ but couldn't determine attribute
                // This happens for files like firefox/packages.nix where the filename
                // doesn't match the package name. Trigger full extraction to be safe.
                if !needs_full_extraction {
                    tracing::debug!(
                        range = %range.label,
                        path = path,
                        commit = %&commit.hash[..8],
                        "Unknown package file changed, triggering full extraction"
                    );
                    needs_full_extraction = true;
                    // Add all packages since we now need full extraction
                    if let Some(all_attrs_list) = all_attrs {
                        for attr in all_attrs_list {
                            target_attr_paths.insert(attr.clone());
                        }
                    }
                }
            }
        }

        let target_attrs: Vec<String> = target_attr_paths.into_iter().collect();

        // Log progress at INFO level periodically (every 50 commits or at milestones)
        let progress_pct = ((commit_idx + 1) as f64 / total_commits as f64 * 100.0) as u32;
        if commit_idx == 0
            || (commit_idx + 1) % 50 == 0
            || commit_idx + 1 == total_commits
            || progress_pct.is_multiple_of(10)
                && progress_pct != ((commit_idx) as f64 / total_commits as f64 * 100.0) as u32
        {
            info!(
                target: "nxv::index",
                range = %range.label,
                commit = %commit.short_hash,
                date = %commit.date.format("%Y-%m-%d"),
                progress = %format!("{}/{}", commit_idx + 1, total_commits),
                percent = progress_pct,
                targets = target_attrs.len(),
                "Processing commit"
            );
        }

        // Skip commits with no targets UNLESS we're doing full extraction with dynamic discovery
        if target_attrs.is_empty() && !extract_all_packages {
            // No packages to extract for this commit
            result.commits_processed += 1;
            progress.tick();
            continue;
        }

        // Extract packages for all systems
        // When extract_all_packages is true, empty target_attrs triggers builtins.attrNames in Nix
        let extract_start = std::time::Instant::now();
        let extraction_results: Vec<(
            String,
            std::result::Result<Vec<extractor::PackageInfo>, NxvError>,
        )> = if let Some(ref pool) = worker_pool {
            let results = pool.extract_parallel(worktree_path, systems, &target_attrs);
            systems.iter().cloned().zip(results).collect()
        } else {
            systems
                .iter()
                .map(|system| {
                    let result =
                        extractor::extract_packages_for_attrs(worktree_path, system, &target_attrs);
                    (system.clone(), result)
                })
                .collect()
        };
        let extract_ms = extract_start.elapsed().as_millis();
        tracing::trace!(
            range = %range.label,
            commit = %&commit.hash[..8],
            target_attrs = target_attrs.len(),
            systems = systems.len(),
            extract_ms = extract_ms,
            "nix extraction"
        );

        // Aggregate results across systems
        let mut aggregates: HashMap<String, PackageAggregate> = HashMap::new();

        for (system, packages_result) in extraction_results {
            match packages_result {
                Ok(packages) => {
                    result.packages_found += packages.len() as u64;
                    for pkg in packages {
                        let key = format!(
                            "{}::{}",
                            pkg.attribute_path,
                            pkg.version.as_deref().unwrap_or("")
                        );
                        if let Some(existing) = aggregates.get_mut(&key) {
                            existing.merge(pkg, &system);
                        } else {
                            aggregates.insert(key, PackageAggregate::new(pkg, &system));
                        }
                    }
                }
                Err(_) => {
                    result.extraction_failures += 1;
                }
            }
        }

        // Convert aggregates to PackageVersion records
        for aggregate in aggregates.values() {
            pending_upserts.push(aggregate.to_package_version(&commit.hash, commit.date));
        }

        result.commits_processed += 1;
        progress.tick();
        progress.log_if_needed(&format!("pkgs={}", result.packages_found));

        // Checkpoint periodically
        if (commit_idx + 1) % config.checkpoint_interval == 0 {
            let upsert_start = std::time::Instant::now();
            let mut db_guard = db.lock().unwrap();
            let upsert_count = db_guard.upsert_packages_batch(&pending_upserts)?;
            result.packages_upserted += upsert_count as u64;
            db_guard.set_range_checkpoint(&range.label, &commit.hash)?;
            db_guard.checkpoint()?;
            let upsert_ms = upsert_start.elapsed().as_millis();
            drop(db_guard);
            tracing::debug!(
                range = %range.label,
                commit = %&commit.hash[..8],
                packages = upsert_count,
                upsert_ms = upsert_ms,
                "checkpoint upsert"
            );
            pending_upserts.clear();
        }

        // Log overall commit timing for slow commits
        let commit_ms = commit_start.elapsed().as_millis();
        if commit_ms > 5000 {
            tracing::debug!(
                range = %range.label,
                commit = %&commit.hash[..8],
                total_ms = commit_ms,
                packages = aggregates.len(),
                "slow commit"
            );
        }
    }

    // Final flush
    if !pending_upserts.is_empty() {
        let mut db_guard = db.lock().unwrap();
        result.packages_upserted += db_guard.upsert_packages_batch(&pending_upserts)? as u64;
        if let Some(last) = commits.last() {
            db_guard.set_range_checkpoint(&range.label, &last.hash)?;
        }
        db_guard.checkpoint()?;
    }

    info!(
        target: "nxv::index",
        "Range {}: complete ({} commits, {} pkgs)",
        range.label,
        result.commits_processed,
        result.packages_found
    );

    Ok(result)
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
    use tempfile::tempdir;

    #[test]
    fn test_progress_tracker_new() {
        let tracker = ProgressTracker::new(100, "Test");
        assert_eq!(tracker.processed(), 0);
        assert!((tracker.percentage() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_progress_tracker_tick() {
        let mut tracker = ProgressTracker::new(100, "Test");
        tracker.tick();
        assert_eq!(tracker.processed(), 1);
        assert!((tracker.percentage() - 1.0).abs() < f64::EPSILON);

        for _ in 0..49 {
            tracker.tick();
        }
        assert_eq!(tracker.processed(), 50);
        assert!((tracker.percentage() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_progress_tracker_percentage_complete() {
        let mut tracker = ProgressTracker::new(100, "Test");
        for _ in 0..100 {
            tracker.tick();
        }
        assert_eq!(tracker.processed(), 100);
        assert!((tracker.percentage() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_progress_tracker_empty_total() {
        let tracker = ProgressTracker::new(0, "Test");
        // Empty total should return 100% to avoid division by zero
        assert!((tracker.percentage() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_progress_tracker_should_log() {
        let mut tracker = ProgressTracker::new(100, "Test");

        // Initially should log at 0%
        assert!(tracker.should_log());
        tracker.mark_logged();
        assert!(!tracker.should_log()); // Not until next interval

        // Advance to just under 5%
        for _ in 0..4 {
            tracker.tick();
        }
        assert!(!tracker.should_log());

        // Hit 5%
        tracker.tick();
        assert!(tracker.should_log());
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
    fn test_extract_attr_from_path_by_name() {
        // pkgs/by-name structure
        assert_eq!(
            extract_attr_from_path("pkgs/by-name/jh/jhead/package.nix"),
            Some("jhead".to_string())
        );
        assert_eq!(
            extract_attr_from_path("pkgs/by-name/he/hello/package.nix"),
            Some("hello".to_string())
        );
        assert_eq!(
            extract_attr_from_path("pkgs/by-name/fi/firefox/package.nix"),
            Some("firefox".to_string())
        );
    }

    #[test]
    fn test_extract_attr_from_path_traditional() {
        // Traditional paths with default.nix
        assert_eq!(
            extract_attr_from_path("pkgs/tools/graphics/jhead/default.nix"),
            Some("jhead".to_string())
        );
        assert_eq!(
            extract_attr_from_path("pkgs/applications/editors/vim/default.nix"),
            Some("vim".to_string())
        );
        assert_eq!(
            extract_attr_from_path("pkgs/development/python-modules/requests/default.nix"),
            Some("requests".to_string())
        );
        // Traditional paths with named .nix file
        assert_eq!(
            extract_attr_from_path("pkgs/servers/nginx.nix"),
            Some("nginx".to_string())
        );
    }

    #[test]
    fn test_extract_attr_from_path_infrastructure_excluded() {
        // Infrastructure directories should return None
        assert_eq!(
            extract_attr_from_path("pkgs/build-support/fetchurl/default.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/stdenv/linux/default.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/top-level/all-packages.nix"),
            None
        );
        assert_eq!(extract_attr_from_path("pkgs/test/simple/default.nix"), None);
        assert_eq!(extract_attr_from_path("pkgs/pkgs-lib/formats.nix"), None);
    }

    #[test]
    fn test_extract_attr_from_path_invalid() {
        // Non-pkgs paths
        assert_eq!(extract_attr_from_path("lib/something.nix"), None);
        assert_eq!(extract_attr_from_path("nixos/modules/foo.nix"), None);
        // Non-.nix files
        assert_eq!(
            extract_attr_from_path("pkgs/tools/misc/hello/README.md"),
            None
        );
        // Empty/malformed
        assert_eq!(extract_attr_from_path(""), None);
        assert_eq!(extract_attr_from_path("pkgs/"), None);
    }

    #[test]
    fn test_extract_attr_from_path_ambiguous_rejected() {
        // Ambiguous filenames that don't correspond to package attribute names
        // should return None to trigger full extraction fallback.
        // This catches cases like firefox/packages.nix where the version is
        // defined in packages.nix but the attribute is "firefox" in all-packages.nix.
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/firefox/packages.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/firefox/common.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/chromium/browser.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/firefox/wrapper.nix"),
            None
        );
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/firefox/update.nix"),
            None
        );
        // But specific package files should still work
        assert_eq!(
            extract_attr_from_path("pkgs/applications/networking/browsers/firefox/firefox.nix"),
            Some("firefox".to_string())
        );
        assert_eq!(
            extract_attr_from_path("pkgs/servers/nginx.nix"),
            Some("nginx".to_string())
        );
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
            memory_budget: DEFAULT_MEMORY_BUDGET,
            verbose: false,
            gc_interval: 0, // Disable GC for tests
            gc_min_free_bytes: 0,
            full_extraction_interval: 0, // Disable periodic full extraction for tests
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
            memory_budget: DEFAULT_MEMORY_BUDGET,
            verbose: false,
            gc_interval: 0, // Disable GC for tests
            gc_min_free_bytes: 0,
            full_extraction_interval: 0, // Disable periodic full extraction for tests
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

    // =============================================
    // YearRange parsing tests
    // =============================================

    #[test]
    fn test_year_range_new_single_year() {
        let range = YearRange::new(2017, 2018);
        assert_eq!(range.label, "2017");
        assert_eq!(range.since, "2017-01-01");
        assert_eq!(range.until, "2018-01-01");
    }

    #[test]
    fn test_year_range_new_multi_year() {
        let range = YearRange::new(2017, 2020);
        assert_eq!(range.label, "2017-2019");
        assert_eq!(range.since, "2017-01-01");
        assert_eq!(range.until, "2020-01-01");
    }

    #[test]
    fn test_year_range_parse_single_year() {
        let ranges = YearRange::parse_ranges("2017", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].since, "2017-01-01");
        assert_eq!(ranges[0].until, "2018-01-01");
    }

    #[test]
    fn test_year_range_parse_range() {
        // Note: "2017-2020" means years 2017, 2018, 2019, 2020 (inclusive end)
        // So until is 2021-01-01 (exclusive)
        let ranges = YearRange::parse_ranges("2017-2020", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].since, "2017-01-01");
        assert_eq!(ranges[0].until, "2021-01-01"); // 2020 inclusive means until 2021
    }

    #[test]
    fn test_year_range_parse_multiple() {
        let ranges = YearRange::parse_ranges("2017,2018,2019", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].label, "2017");
        assert_eq!(ranges[1].label, "2018");
        assert_eq!(ranges[2].label, "2019");
    }

    #[test]
    fn test_year_range_auto_partition() {
        // 8 years (2017-2024) divided into 4 ranges = 2 years each
        let ranges = YearRange::parse_ranges("4", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges[0].label, "2017-2018");
        assert_eq!(ranges[0].since, "2017-01-01");
        assert_eq!(ranges[0].until, "2019-01-01");
    }

    #[test]
    fn test_year_range_auto_partition_uneven() {
        // 9 years (2017-2025) divided into 4 ranges
        let ranges = YearRange::parse_ranges("4", 2017, 2026).unwrap();
        assert_eq!(ranges.len(), 4);
        // First ranges get 2 years each, last may get more
    }

    #[test]
    fn test_year_range_parse_invalid_year() {
        let result = YearRange::parse_ranges("1900", 2017, 2025);
        assert!(result.is_err());
    }

    #[test]
    fn test_year_range_parse_invalid_format() {
        let result = YearRange::parse_ranges("abc", 2017, 2025);
        assert!(result.is_err());
    }

    #[test]
    fn test_year_range_parse_half_year() {
        let ranges = YearRange::parse_ranges("2018-H1,2018-H2", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 2);

        // H1 = Jan-Jun
        assert_eq!(ranges[0].label, "2018-H1");
        assert_eq!(ranges[0].since, "2018-01-01");
        assert_eq!(ranges[0].until, "2018-07-01");

        // H2 = Jul-Dec
        assert_eq!(ranges[1].label, "2018-H2");
        assert_eq!(ranges[1].since, "2018-07-01");
        assert_eq!(ranges[1].until, "2019-01-01");
    }

    #[test]
    fn test_year_range_parse_quarter() {
        let ranges =
            YearRange::parse_ranges("2019-Q1,2019-Q2,2019-Q3,2019-Q4", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 4);

        // Q1 = Jan-Mar
        assert_eq!(ranges[0].label, "2019-Q1");
        assert_eq!(ranges[0].since, "2019-01-01");
        assert_eq!(ranges[0].until, "2019-04-01");

        // Q2 = Apr-Jun
        assert_eq!(ranges[1].label, "2019-Q2");
        assert_eq!(ranges[1].since, "2019-04-01");
        assert_eq!(ranges[1].until, "2019-07-01");

        // Q3 = Jul-Sep
        assert_eq!(ranges[2].label, "2019-Q3");
        assert_eq!(ranges[2].since, "2019-07-01");
        assert_eq!(ranges[2].until, "2019-10-01");

        // Q4 = Oct-Dec
        assert_eq!(ranges[3].label, "2019-Q4");
        assert_eq!(ranges[3].since, "2019-10-01");
        assert_eq!(ranges[3].until, "2020-01-01");
    }

    #[test]
    fn test_year_range_parse_mixed() {
        // Mix of years, halves, and quarters
        let ranges = YearRange::parse_ranges("2017,2018-H1,2019-Q1", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].label, "2017");
        assert_eq!(ranges[1].label, "2018-H1");
        assert_eq!(ranges[2].label, "2019-Q1");
    }

    #[test]
    fn test_year_range_parse_lowercase_suffix() {
        // Should accept lowercase h1/q1
        let ranges = YearRange::parse_ranges("2018-h1,2019-q2", 2017, 2025).unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].label, "2018-H1");
        assert_eq!(ranges[1].label, "2019-Q2");
    }

    // =============================================
    // IndexResult merge tests
    // =============================================

    #[test]
    fn test_index_result_merge_single() {
        let mut total = IndexResult::default();
        let range1 = RangeIndexResult {
            range_label: "2017".to_string(),
            commits_processed: 100,
            packages_found: 1000,
            packages_upserted: 500,
            extraction_failures: 10,
            was_interrupted: false,
        };
        total.merge(range1);
        assert_eq!(total.commits_processed, 100);
        assert_eq!(total.packages_found, 1000);
        assert_eq!(total.packages_upserted, 500);
        assert_eq!(total.extraction_failures, 10);
        assert!(!total.was_interrupted);
    }

    #[test]
    fn test_index_result_merge_multiple() {
        let mut total = IndexResult::default();
        let range1 = RangeIndexResult {
            range_label: "2017".to_string(),
            commits_processed: 100,
            packages_found: 1000,
            packages_upserted: 500,
            extraction_failures: 10,
            was_interrupted: false,
        };
        let range2 = RangeIndexResult {
            range_label: "2018".to_string(),
            commits_processed: 150,
            packages_found: 1500,
            packages_upserted: 750,
            extraction_failures: 5,
            was_interrupted: false,
        };
        total.merge(range1);
        total.merge(range2);
        assert_eq!(total.commits_processed, 250);
        assert_eq!(total.packages_found, 2500);
        assert_eq!(total.packages_upserted, 1250);
        assert_eq!(total.extraction_failures, 15);
        assert!(!total.was_interrupted);
    }

    #[test]
    fn test_index_result_merge_interrupted() {
        let mut total = IndexResult::default();
        let range1 = RangeIndexResult {
            range_label: "2017".to_string(),
            commits_processed: 100,
            packages_found: 1000,
            packages_upserted: 500,
            extraction_failures: 0,
            was_interrupted: false,
        };
        let range2 = RangeIndexResult {
            range_label: "2018".to_string(),
            commits_processed: 50,
            packages_found: 500,
            packages_upserted: 250,
            extraction_failures: 0,
            was_interrupted: true, // This one was interrupted
        };
        total.merge(range1);
        total.merge(range2);
        assert_eq!(total.commits_processed, 150);
        assert!(total.was_interrupted); // Interrupt flag propagates
    }
}

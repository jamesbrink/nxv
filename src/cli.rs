//! Command-line interface definitions using clap.

use crate::logging::{LogConfig, LogFormat, LogRotation};
use crate::output::OutputFormat;
use crate::paths;
use crate::search::SortOrder;
use crate::version;
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;
use tracing::Level;

/// nxv - Nix Version Index
#[derive(Parser, Debug)]
#[command(name = "nxv")]
#[command(author, version = version::clap_version(), long_version = version::long_version(), about, long_about = None)]
pub struct Cli {
    /// Path to the index database.
    #[arg(long, env = "NXV_DB_PATH", default_value_os_t = paths::get_index_path())]
    pub db_path: PathBuf,

    /// Enable verbose output (-v for debug, -vv for trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress all output except errors.
    #[arg(short, long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Disable colored output.
    #[arg(long, env = "NO_COLOR")]
    pub no_color: bool,

    /// API request timeout in seconds (when using remote backend).
    #[arg(long, env = "NXV_API_TIMEOUT", default_value_t = 30)]
    pub api_timeout: u64,

    /// Log level: error, warn, info, debug, trace.
    #[arg(long, env = "NXV_LOG_LEVEL", global = true)]
    pub log_level: Option<String>,

    /// Log format: pretty, compact, json.
    #[arg(long, env = "NXV_LOG_FORMAT", global = true)]
    pub log_format: Option<String>,

    /// Log to file (in addition to stderr).
    #[arg(long, env = "NXV_LOG_FILE", global = true)]
    pub log_file: Option<PathBuf>,

    /// Log rotation: hourly, daily, never.
    #[arg(long, env = "NXV_LOG_ROTATION", default_value = "daily", global = true)]
    pub log_rotation: String,

    #[command(subcommand)]
    pub command: Commands,
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Search for package versions.
    Search(SearchArgs),

    /// Download or update the package index.
    Update(UpdateArgs),

    /// Show detailed information about a package.
    Info(InfoArgs),

    /// Show index statistics.
    Stats,

    /// Show version history for a package.
    History(HistoryArgs),

    /// Build the index from a local nixpkgs repository.
    #[cfg(feature = "indexer")]
    Index(IndexArgs),

    /// Backfill missing metadata (source_path, homepage) from current nixpkgs.
    #[cfg(feature = "indexer")]
    Backfill(BackfillArgs),

    /// Reset/clean the nixpkgs repository to a known state.
    #[cfg(feature = "indexer")]
    Reset(ResetArgs),

    /// Generate publishable index artifacts (compressed DB, bloom filter, manifest).
    #[cfg(feature = "indexer")]
    Publish(PublishArgs),

    /// Generate a new minisign keypair for signing manifests.
    #[cfg(feature = "indexer")]
    Keygen(KeygenArgs),

    /// Start the API server.
    Serve(ServeArgs),

    /// Generate shell completions.
    Completions(CompletionsArgs),

    /// Complete package names (for shell completion scripts).
    #[command(hide = true)]
    CompletePackage(CompletePackageArgs),
}

/// Arguments for shell completions.
#[derive(Parser, Debug)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: Shell,
}

impl CompletionsArgs {
    /// Generate and print completions to stdout.
    ///
    /// For bash, zsh, and fish, this includes custom completion functions
    /// that provide dynamic package name completion using `nxv complete-package`.
    pub fn generate(&self) {
        // Silently ignore broken pipe errors (e.g., when piped to head or closed early)
        let _ = crate::completions::generate_completions(self.shell, &mut std::io::stdout());
    }
}

/// Arguments for package name completion (used by shell completion scripts).
#[derive(Parser, Debug)]
pub struct CompletePackageArgs {
    /// Prefix to complete (what the user has typed so far).
    #[arg(default_value = "")]
    pub prefix: String,

    /// Maximum number of completions to return.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
}

/// Arguments for the search command.
#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// Package name or attribute path to search for.
    pub package: String,

    /// Version to filter by (positional, prefix match).
    #[arg(conflicts_with = "version_opt")]
    pub version: Option<String>,

    /// Filter by version (prefix match, alternative to positional).
    #[arg(short = 'V', long = "version", conflicts_with = "version")]
    pub version_opt: Option<String>,

    /// Search in package descriptions (FTS).
    #[arg(long)]
    pub desc: bool,

    /// Filter by license.
    #[arg(long)]
    pub license: Option<String>,

    /// Filter by platform (e.g., "x86_64-linux", "aarch64-darwin").
    #[arg(long)]
    pub platform: Option<String>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormatArg::Table)]
    pub format: OutputFormatArg,

    /// Show platforms column in output.
    #[arg(long)]
    pub show_platforms: bool,

    /// Show store path column in output (for fetchClosure support).
    #[arg(long)]
    pub show_store_path: bool,

    /// Sort results.
    #[arg(long, value_enum, default_value_t = SortOrder::Relevance)]
    pub sort: SortOrder,

    /// Reverse sort order.
    #[arg(short, long)]
    pub reverse: bool,

    /// Limit number of results (0 for unlimited).
    #[arg(short = 'n', long, default_value_t = 50)]
    pub limit: usize,

    /// Perform exact name match only.
    #[arg(short, long)]
    pub exact: bool,

    /// Show all commits (by default, only most recent per package+version is shown).
    #[arg(long)]
    pub full: bool,

    /// Use ASCII table borders instead of Unicode.
    #[arg(long)]
    pub ascii: bool,
}

impl SearchArgs {
    /// Get the version filter from either positional or option argument.
    pub fn get_version(&self) -> Option<&str> {
        self.version.as_deref().or(self.version_opt.as_deref())
    }
}

/// Arguments for the update command.
#[derive(Parser, Debug)]
pub struct UpdateArgs {
    /// Force full re-download of the index.
    #[arg(short, long)]
    pub force: bool,

    /// Custom manifest URL (for testing or alternate index sources).
    #[arg(long, env = "NXV_MANIFEST_URL", hide = true)]
    pub manifest_url: Option<String>,

    /// Skip manifest signature verification (INSECURE - use only for development/testing).
    #[arg(long, env = "NXV_SKIP_VERIFY")]
    pub skip_verify: bool,

    /// Custom public key for manifest signature verification (for self-hosted indexes).
    /// Can be the raw key (RW...) or path to a .pub file.
    #[arg(long, env = "NXV_PUBLIC_KEY")]
    pub public_key: Option<String>,
}

/// Arguments for the history command.
#[derive(Parser, Debug)]
pub struct HistoryArgs {
    /// Package name to show history for.
    pub package: String,

    /// Specific version to show availability for.
    pub version: Option<String>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormatArg::Table)]
    pub format: OutputFormatArg,

    /// Show full details (commits, description, license, homepage, etc.).
    #[arg(long)]
    pub full: bool,

    /// Use ASCII table borders instead of Unicode.
    #[arg(long)]
    pub ascii: bool,
}

/// Arguments for the info command.
#[derive(Parser, Debug)]
pub struct InfoArgs {
    /// Package name to show info for.
    pub package: String,

    /// Specific version to show info for (positional).
    #[arg(conflicts_with = "version_opt")]
    pub version: Option<String>,

    /// Specific version to show info for (alternative to positional).
    #[arg(short = 'V', long = "version", conflicts_with = "version")]
    pub version_opt: Option<String>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormatArg::Table)]
    pub format: OutputFormatArg,
}

impl InfoArgs {
    /// Selects the version string provided by the positional argument or the version option.
    ///
    /// Prefers the positional `version` field and falls back to `version_opt` if the positional is `None`.
    ///
    /// # Returns
    ///
    /// `Some(&str)` with the chosen version, or `None` if neither field is set.
    ///
    /// # Examples
    ///
    /// ```
    /// let args = InfoArgs {
    ///     package: "pkg".to_string(),
    ///     version: Some("1.2.3".to_string()),
    ///     version_opt: None,
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args.get_version(), Some("1.2.3"));
    ///
    /// let args2 = InfoArgs {
    ///     package: "pkg".to_string(),
    ///     version: None,
    ///     version_opt: Some("2.0.0".to_string()),
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args2.get_version(), Some("2.0.0"));
    ///
    /// let args3 = InfoArgs {
    ///     package: "pkg".to_string(),
    ///     version: None,
    ///     version_opt: None,
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args3.get_version(), None);
    /// ```
    pub fn get_version(&self) -> Option<&str> {
        self.version.as_deref().or(self.version_opt.as_deref())
    }
}

/// Arguments for the serve command.
#[derive(Parser, Debug)]
pub struct ServeArgs {
    /// Host address to bind to.
    #[arg(short = 'H', long, default_value = "127.0.0.1", env = "NXV_HOST")]
    pub host: String,

    /// Port to listen on.
    #[arg(short, long, default_value_t = 8080, env = "NXV_PORT")]
    pub port: u16,

    /// Enable CORS for all origins (insecure, use --cors-origins for production).
    #[arg(long)]
    pub cors: bool,

    /// Specific CORS origins (comma-separated, recommended for production).
    #[arg(long, value_delimiter = ',')]
    pub cors_origins: Option<Vec<String>>,

    /// Enable rate limiting per IP address (requests per second).
    /// When set, limits each IP to this many requests per second.
    #[arg(long, env = "NXV_RATE_LIMIT")]
    pub rate_limit: Option<u64>,

    /// Burst size for rate limiting (default: 2x rate_limit).
    /// Allows temporary bursts above the sustained rate.
    #[arg(long, env = "NXV_RATE_LIMIT_BURST")]
    pub rate_limit_burst: Option<u32>,
}

/// Arguments for the index command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct IndexArgs {
    /// Path to the nixpkgs repository.
    #[arg(long)]
    pub nixpkgs_path: PathBuf,

    /// Force full rebuild (ignore last indexed commit).
    #[arg(long)]
    pub full: bool,

    /// Commits between checkpoints.
    #[arg(long, default_value_t = 100)]
    pub checkpoint_interval: usize,

    /// Comma-separated list of systems to evaluate (e.g. x86_64-linux,aarch64-linux).
    #[arg(long, value_delimiter = ',')]
    pub systems: Option<Vec<String>>,

    /// Limit commits to those after this date (YYYY-MM-DD) or git date string.
    #[arg(long)]
    pub since: Option<String>,

    /// Limit commits to those before this date (YYYY-MM-DD) or git date string.
    #[arg(long)]
    pub until: Option<String>,

    /// Limit the number of commits processed.
    #[arg(long)]
    pub max_commits: Option<usize>,

    /// Number of parallel worker processes for evaluation (default: number of systems).
    #[arg(long)]
    pub workers: Option<usize>,

    /// Memory threshold (MiB) before worker restart (default: 6144).
    #[arg(long, default_value_t = 6144)]
    pub max_memory: usize,

    /// Show verbose output including extraction warnings.
    #[arg(long, short = 'v')]
    pub verbose: bool,

    /// Number of checkpoints between garbage collection runs.
    /// Set to 0 to disable automatic garbage collection.
    /// Default: 5 (GC every 500 commits with default checkpoint_interval of 100)
    #[arg(long, default_value_t = 5)]
    pub gc_interval: usize,

    /// Minimum free disk space (GB) before triggering emergency GC.
    /// If available space falls below this, GC runs at next checkpoint.
    #[arg(long, default_value_t = 10)]
    pub gc_min_free_gb: u64,

    /// Internal flag for worker subprocess mode (hidden from help).
    #[arg(long, hide = true)]
    pub internal_worker: bool,

    /// Process year ranges in parallel for faster indexing.
    /// Formats:
    ///   "4"             - Auto-partition into 4 equal year ranges
    ///   "2017,2018"     - Process years 2017 and 2018 in parallel
    ///   "2017-2020"     - Single range from 2017 to 2019 (end exclusive)
    ///   "2017-2019,2020-2024" - Multiple explicit ranges
    #[arg(long, value_name = "RANGES")]
    pub parallel_ranges: Option<String>,

    /// Maximum concurrent range workers for parallel indexing.
    /// Each range gets its own worktree and worker pool.
    /// Default: min(num_cpus / 4, number of ranges)
    #[arg(long, default_value_t = 4)]
    pub max_range_workers: usize,
}

/// Fields that can be backfilled from nixpkgs.
#[cfg(feature = "indexer")]
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackfillField {
    /// Source path (e.g., pkgs/development/python-modules/foo/default.nix)
    SourcePath,
    /// Homepage URL
    Homepage,
    /// Known security vulnerabilities (CVEs)
    KnownVulnerabilities,
}

#[cfg(feature = "indexer")]
impl BackfillField {
    /// Convert to the string representation used in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            BackfillField::SourcePath => "source_path",
            BackfillField::Homepage => "homepage",
            BackfillField::KnownVulnerabilities => "known_vulnerabilities",
        }
    }
}

/// Arguments for the backfill command (feature-gated).
///
/// Backfill updates existing database records with metadata extracted from nixpkgs.
/// This is useful for populating fields that weren't captured during initial indexing.
///
/// Two modes are available:
///
/// HEAD MODE (default):
///   Extracts metadata from the current nixpkgs checkout. Fast (single nix eval),
///   but can only update packages that still exist in that checkout. Packages that
///   have been removed or renamed won't be found.
///
/// HISTORICAL MODE (--history):
///   For each package, checks out the original commit where it was indexed and
///   extracts metadata from there. Much slower (many git checkouts + nix evals),
///   but can update old/removed packages like Python 2.7.
///
/// Examples:
///   # Fast backfill from current HEAD (only updates packages that still exist)
///   nxv backfill --nixpkgs-path ./nixpkgs --fields known-vulnerabilities
///
///   # Full historical backfill (slower, but updates old/removed packages)
///   nxv backfill --nixpkgs-path ./nixpkgs --fields known-vulnerabilities --history
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct BackfillArgs {
    /// Path to the nixpkgs repository checkout.
    #[arg(long)]
    pub nixpkgs_path: PathBuf,

    /// Comma-separated list of fields to backfill.
    /// Default: all fields.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub fields: Option<Vec<BackfillField>>,

    /// Limit the number of packages to backfill (for testing).
    #[arg(long)]
    pub limit: Option<usize>,

    /// Dry run - show what would be updated without making changes.
    #[arg(long)]
    pub dry_run: bool,

    /// Use historical mode: traverse git to each package's original commit.
    /// Slower but can update packages that no longer exist in current nixpkgs.
    /// Without this flag, only packages in the current checkout can be updated.
    #[arg(long)]
    pub history: bool,

    /// Filter to packages first seen after this date (YYYY-MM-DD).
    /// Only applies in historical mode.
    #[arg(long)]
    pub since: Option<String>,

    /// Filter to packages first seen before this date (YYYY-MM-DD).
    /// Only applies in historical mode.
    #[arg(long)]
    pub until: Option<String>,

    /// Maximum number of commits to process (historical mode only).
    #[arg(long)]
    pub max_commits: Option<usize>,
}

/// Arguments for the reset command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct ResetArgs {
    /// Path to the nixpkgs repository.
    #[arg(long)]
    pub nixpkgs_path: PathBuf,

    /// Reset to a specific commit or ref (default: origin/nixpkgs-unstable).
    #[arg(long)]
    pub to: Option<String>,

    /// Also fetch from origin before resetting.
    #[arg(long)]
    pub fetch: bool,
}

/// Arguments for the publish command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct PublishArgs {
    /// Output directory for generated artifacts.
    #[arg(short, long, default_value = "./publish")]
    pub output: PathBuf,

    /// Base URL prefix for manifest URLs (e.g., https://github.com/user/repo/releases/download/index-latest).
    #[arg(long)]
    pub url_prefix: Option<String>,

    /// Sign the manifest with a minisign secret key.
    #[arg(long)]
    pub sign: bool,

    /// Secret key for signing (file path or raw key content).
    /// Can also be set via NXV_SECRET_KEY environment variable.
    #[arg(long, env = "NXV_SECRET_KEY", required_if_eq("sign", "true"))]
    pub secret_key: Option<String>,

    /// Minimum schema version required to read this index.
    /// Set this lower than the schema version for backward-compatible changes.
    /// If not set, defaults to the schema version (breaking change).
    #[arg(long)]
    pub min_version: Option<u32>,

    /// Zstd compression level (1-22, default 19). Lower = faster, higher = smaller.
    #[arg(long, default_value_t = 19, value_parser = clap::value_parser!(i32).range(1..=22))]
    pub compression_level: i32,
}

/// Arguments for the keygen command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct KeygenArgs {
    /// Output path for the secret key file.
    #[arg(short, long, default_value = "./nxv.key")]
    pub secret_key: PathBuf,

    /// Output path for the public key file.
    #[arg(short, long, default_value = "./nxv.pub")]
    pub public_key: PathBuf,

    /// Comment to embed in the key files.
    #[arg(short, long, default_value = "nxv signing key")]
    pub comment: String,

    /// Overwrite existing key files if they exist.
    #[arg(short, long)]
    pub force: bool,
}

/// Output format argument.
#[derive(ValueEnum, Clone, Copy, Debug, Default)]
pub enum OutputFormatArg {
    /// Colored table output.
    #[default]
    Table,
    /// JSON output.
    Json,
    /// Plain text output (no colors).
    Plain,
}

impl From<OutputFormatArg> for OutputFormat {
    /// Convert a CLI-level `OutputFormatArg` into the corresponding internal `OutputFormat`.
    ///
    /// # Returns
    ///
    /// The matching `OutputFormat` variant for the provided `OutputFormatArg`.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::cli::OutputFormatArg;
    /// use crate::output::OutputFormat;
    ///
    /// let arg = OutputFormatArg::Json;
    /// let fmt: OutputFormat = arg.into();
    /// assert_eq!(fmt, OutputFormat::Json);
    /// ```
    fn from(arg: OutputFormatArg) -> Self {
        match arg {
            OutputFormatArg::Table => OutputFormat::Table,
            OutputFormatArg::Json => OutputFormat::Json,
            OutputFormatArg::Plain => OutputFormat::Plain,
        }
    }
}

// SortOrder is imported from crate::search

/// Verbosity level for output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// Default: info level (natural CLI output).
    Normal,
    /// -v: debug level for more detail.
    Debug,
    /// -vv: trace level for full detail.
    Trace,
}

impl From<u8> for Verbosity {
    fn from(count: u8) -> Self {
        match count {
            0 => Verbosity::Normal,
            1 => Verbosity::Debug,
            _ => Verbosity::Trace,
        }
    }
}

impl Cli {
    /// Get the verbosity level based on -v flags.
    pub fn verbosity(&self) -> Verbosity {
        Verbosity::from(self.verbose)
    }

    /// Build a LogConfig from CLI arguments.
    ///
    /// Applies CLI args first, then environment variable overrides.
    pub fn log_config(&self) -> LogConfig {
        let mut config = LogConfig::default();

        // Apply log level from CLI or verbose flags
        if let Some(ref level_str) = self.log_level {
            config.level = parse_log_level(level_str).unwrap_or(Level::INFO);
        } else {
            // Fall back to verbose flags if no explicit log level
            config.level = match self.verbose {
                0 => Level::INFO,  // Default: info for natural CLI output
                1 => Level::DEBUG, // -v: debug for more detail
                _ => Level::TRACE, // -vv+: trace for full detail
            };
        }

        // Apply log format from CLI
        if let Some(ref format_str) = self.log_format {
            config.format = format_str.parse().unwrap_or(LogFormat::Pretty);
        }

        // Apply log file from CLI
        if let Some(ref path) = self.log_file {
            config.file_path = Some(path.clone());
        }

        // Apply log rotation from CLI
        config.rotation = self.log_rotation.parse().unwrap_or(LogRotation::Daily);

        // Apply environment variable overrides (NXV_LOG, RUST_LOG, etc.)
        config.with_env_overrides()
    }
}

/// Parse a log level string to tracing Level.
fn parse_log_level(s: &str) -> Option<Level> {
    match s.to_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" | "warning" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use serial_test::serial;

    #[test]
    fn test_cli_parsing() {
        // Verify the CLI definition is valid
        Cli::command().debug_assert();
    }

    #[test]
    fn test_search_command() {
        let args = Cli::try_parse_from(["nxv", "search", "python"]).unwrap();
        match args.command {
            Commands::Search(search) => {
                assert_eq!(search.package, "python");
                assert!(!search.exact);
                assert_eq!(search.limit, 50);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_search_with_version_option() {
        let args = Cli::try_parse_from(["nxv", "search", "python", "--version", "3.11", "--exact"])
            .unwrap();
        match args.command {
            Commands::Search(search) => {
                assert_eq!(search.package, "python");
                assert_eq!(search.get_version(), Some("3.11"));
                assert!(search.exact);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_search_with_version_positional() {
        let args = Cli::try_parse_from(["nxv", "search", "python", "2.7"]).unwrap();
        match args.command {
            Commands::Search(search) => {
                assert_eq!(search.package, "python");
                assert_eq!(search.get_version(), Some("2.7"));
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_search_version_option_and_positional_conflict() {
        // Cannot use both positional version and -V/--version option
        let result = Cli::try_parse_from(["nxv", "search", "python", "2.7", "-V", "3.11"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_command() {
        let args = Cli::try_parse_from(["nxv", "update"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert!(!update.force);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_force() {
        let args = Cli::try_parse_from(["nxv", "update", "--force"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert!(update.force);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    #[serial(env)]
    fn test_update_public_key() {
        let args = Cli::try_parse_from(["nxv", "update", "--public-key", "RWTest123"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert_eq!(update.public_key, Some("RWTest123".to_string()));
                assert!(!update.skip_verify);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_public_key_file() {
        let args =
            Cli::try_parse_from(["nxv", "update", "--public-key", "/path/to/key.pub"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert_eq!(update.public_key, Some("/path/to/key.pub".to_string()));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_with_force() {
        let args = Cli::try_parse_from(["nxv", "update", "--force"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert!(update.force);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_info_command() {
        let args = Cli::try_parse_from(["nxv", "info", "python"]).unwrap();
        match args.command {
            Commands::Info(info) => {
                assert_eq!(info.package, "python");
                assert!(info.version.is_none());
            }
            _ => panic!("Expected Info command"),
        }
    }

    #[test]
    fn test_info_with_version() {
        let args = Cli::try_parse_from(["nxv", "info", "python", "3.11"]).unwrap();
        match args.command {
            Commands::Info(info) => {
                assert_eq!(info.package, "python");
                assert_eq!(info.version, Some("3.11".to_string()));
            }
            _ => panic!("Expected Info command"),
        }
    }

    #[test]
    fn test_stats_command() {
        let args = Cli::try_parse_from(["nxv", "stats"]).unwrap();
        assert!(matches!(args.command, Commands::Stats));
    }

    #[test]
    fn test_history_command() {
        let args = Cli::try_parse_from(["nxv", "history", "python"]).unwrap();
        match args.command {
            Commands::History(history) => {
                assert_eq!(history.package, "python");
                assert!(history.version.is_none());
            }
            _ => panic!("Expected History command"),
        }
    }

    #[test]
    fn test_history_with_version() {
        let args = Cli::try_parse_from(["nxv", "history", "python", "3.11.0"]).unwrap();
        match args.command {
            Commands::History(history) => {
                assert_eq!(history.package, "python");
                assert_eq!(history.version, Some("3.11.0".to_string()));
            }
            _ => panic!("Expected History command"),
        }
    }

    #[test]
    fn test_global_options() {
        let args = Cli::try_parse_from(["nxv", "-vv", "--no-color", "stats"]).unwrap();
        assert_eq!(args.verbose, 2);
        assert!(args.no_color);
    }

    #[test]
    fn test_quiet_conflicts_with_verbose() {
        let result = Cli::try_parse_from(["nxv", "-v", "-q", "stats"]);
        assert!(result.is_err());
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_index_systems_parsing() {
        let args = Cli::try_parse_from([
            "nxv",
            "index",
            "--nixpkgs-path",
            "./nixpkgs",
            "--systems",
            "x86_64-linux,aarch64-linux",
        ])
        .unwrap();

        match args.command {
            Commands::Index(index) => {
                let systems = index.systems.unwrap();
                assert_eq!(systems, vec!["x86_64-linux", "aarch64-linux"]);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_publish_command() {
        let args = Cli::try_parse_from(["nxv", "publish", "--output", "/tmp/publish"]).unwrap();
        match args.command {
            Commands::Publish(publish) => {
                assert_eq!(publish.output.to_string_lossy(), "/tmp/publish");
                assert!(!publish.sign);
                assert!(publish.secret_key.is_none());
            }
            _ => panic!("Expected Publish command"),
        }
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_publish_with_signing() {
        let args = Cli::try_parse_from([
            "nxv",
            "publish",
            "--sign",
            "--secret-key",
            "/path/to/key.key",
        ])
        .unwrap();
        match args.command {
            Commands::Publish(publish) => {
                assert!(publish.sign);
                assert_eq!(publish.secret_key.unwrap(), "/path/to/key.key");
            }
            _ => panic!("Expected Publish command"),
        }
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_publish_sign_requires_secret_key() {
        // --sign without --secret-key should fail
        let result = Cli::try_parse_from(["nxv", "publish", "--sign"]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("secret-key"));
    }

    #[test]
    fn test_log_level_argument() {
        let args = Cli::try_parse_from(["nxv", "--log-level", "debug", "stats"]).unwrap();
        assert_eq!(args.log_level, Some("debug".to_string()));
    }

    #[test]
    fn test_log_format_argument() {
        let args = Cli::try_parse_from(["nxv", "--log-format", "json", "stats"]).unwrap();
        assert_eq!(args.log_format, Some("json".to_string()));
    }

    #[test]
    fn test_log_file_argument() {
        let args = Cli::try_parse_from(["nxv", "--log-file", "/tmp/nxv.log", "stats"]).unwrap();
        assert_eq!(
            args.log_file,
            Some(std::path::PathBuf::from("/tmp/nxv.log"))
        );
    }

    #[test]
    fn test_log_rotation_argument() {
        let args = Cli::try_parse_from(["nxv", "--log-rotation", "hourly", "stats"]).unwrap();
        assert_eq!(args.log_rotation, "hourly");
    }

    #[test]
    fn test_log_rotation_default() {
        let args = Cli::try_parse_from(["nxv", "stats"]).unwrap();
        assert_eq!(args.log_rotation, "daily");
    }

    #[test]
    #[serial(env)]
    fn test_log_config_from_cli_args() {
        // Clear any logging env vars that could interfere
        // SAFETY: Test runs serially via #[serial(env)] to avoid env var races
        unsafe {
            std::env::remove_var("NXV_LOG");
            std::env::remove_var("NXV_LOG_LEVEL");
            std::env::remove_var("NXV_LOG_FORMAT");
            std::env::remove_var("RUST_LOG");
        }

        let args = Cli::try_parse_from([
            "nxv",
            "--log-level",
            "debug",
            "--log-format",
            "json",
            "--log-rotation",
            "hourly",
            "stats",
        ])
        .unwrap();

        let config = args.log_config();
        assert_eq!(config.level, Level::DEBUG);
        assert_eq!(config.format, LogFormat::Json);
        assert_eq!(config.rotation, LogRotation::Hourly);
    }

    #[test]
    #[serial(env)]
    fn test_log_config_verbose_flags() {
        // Clear any logging env vars that could interfere
        // SAFETY: Test runs serially via #[serial(env)] to avoid env var races
        unsafe {
            std::env::remove_var("NXV_LOG");
            std::env::remove_var("NXV_LOG_LEVEL");
            std::env::remove_var("RUST_LOG");
        }

        // No verbose flags = INFO level (natural CLI output)
        let args = Cli::try_parse_from(["nxv", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::INFO);

        // -v = DEBUG level (more detail)
        let args = Cli::try_parse_from(["nxv", "-v", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::DEBUG);

        // -vv = TRACE level (full detail)
        let args = Cli::try_parse_from(["nxv", "-vv", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::TRACE);

        // -vvv = TRACE level (same as -vv)
        let args = Cli::try_parse_from(["nxv", "-vvv", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::TRACE);
    }

    #[test]
    #[serial(env)]
    fn test_log_level_overrides_verbose() {
        // Clear any logging env vars that could interfere
        // SAFETY: Test runs serially via #[serial(env)] to avoid env var races
        unsafe {
            std::env::remove_var("NXV_LOG");
            std::env::remove_var("NXV_LOG_LEVEL");
            std::env::remove_var("RUST_LOG");
        }

        // Explicit --log-level should override -v flags
        let args = Cli::try_parse_from(["nxv", "-vvv", "--log-level", "warn", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::WARN);
    }

    #[test]
    #[serial(env)]
    fn test_log_config_env_override() {
        // SAFETY: Test runs serially via #[serial(env)] to avoid env var races
        unsafe {
            std::env::set_var("NXV_LOG_LEVEL", "trace");
        }
        let args = Cli::try_parse_from(["nxv", "stats"]).unwrap();
        let config = args.log_config();
        assert_eq!(config.level, Level::TRACE);
        // SAFETY: Test runs serially via #[serial(env)] to avoid env var races
        unsafe {
            std::env::remove_var("NXV_LOG_LEVEL");
        }
    }
}

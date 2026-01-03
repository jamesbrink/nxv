//! Command-line interface definitions using clap.

use crate::output::OutputFormat;
use crate::paths;
use crate::search::SortOrder;
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

/// nxv - Nix Version Index
#[derive(Parser, Debug)]
#[command(name = "nxv")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to the index database.
    #[arg(long, env = "NXV_DB_PATH", default_value_os_t = paths::get_index_path())]
    pub db_path: PathBuf,

    /// Enable verbose output (-v for info, -vv for debug).
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

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormatArg::Table)]
    pub format: OutputFormatArg,

    /// Show platforms column in output.
    #[arg(long)]
    pub show_platforms: bool,

    /// Sort results.
    #[arg(long, value_enum, default_value_t = SortOrder::Date)]
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
}

/// Arguments for the reset command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct ResetArgs {
    /// Path to the nixpkgs repository.
    #[arg(long)]
    pub nixpkgs_path: PathBuf,

    /// Reset to a specific commit or ref (default: origin/master).
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
    /// Default: errors and results only.
    Normal,
    /// -v: include warnings and progress info.
    Info,
    /// -vv: include debug info (SQL queries, HTTP requests).
    Debug,
}

impl From<u8> for Verbosity {
    fn from(count: u8) -> Self {
        match count {
            0 => Verbosity::Normal,
            1 => Verbosity::Info,
            _ => Verbosity::Debug,
        }
    }
}

impl Cli {
    /// Get the verbosity level based on -v flags.
    pub fn verbosity(&self) -> Verbosity {
        Verbosity::from(self.verbose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

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
}

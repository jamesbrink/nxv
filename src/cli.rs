//! Command-line interface definitions using clap.

use crate::output::OutputFormat;
use crate::paths;
use crate::search::SortOrder;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

/// CLI tool for finding specific versions of Nix packages.
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

    /// Start the API server.
    Serve(ServeArgs),

    /// Generate shell completions.
    Completions(CompletionsArgs),
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
    pub fn generate(&self) {
        clap_complete::generate(
            self.shell,
            &mut Cli::command(),
            "nxv",
            &mut std::io::stdout(),
        );
    }
}

/// Arguments for the search command.
#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// Package name or attribute path to search for.
    pub package: String,

    /// Filter by version (prefix match).
    #[arg(short = 'V', long)]
    pub version: Option<String>,

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

/// Arguments for the update command.
#[derive(Parser, Debug)]
pub struct UpdateArgs {
    /// Force full re-download of the index.
    #[arg(short, long)]
    pub force: bool,

    /// Custom manifest URL (for testing or alternate index sources).
    #[arg(long, env = "NXV_MANIFEST_URL", hide = true)]
    pub manifest_url: Option<String>,
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
    #[arg(conflicts_with = "version_flag")]
    pub version: Option<String>,

    /// Specific version to show info for (flag).
    #[arg(short = 'V', long = "version", conflicts_with = "version")]
    pub version_flag: Option<String>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormatArg::Table)]
    pub format: OutputFormatArg,
}

impl InfoArgs {
    /// Selects the version string provided by the positional argument or the version flag.
    ///
    /// Prefers the positional `version` field and falls back to `version_flag` if the positional is `None`.
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
    ///     version_flag: None,
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args.get_version(), Some("1.2.3"));
    ///
    /// let args2 = InfoArgs {
    ///     package: "pkg".to_string(),
    ///     version: None,
    ///     version_flag: Some("2.0.0".to_string()),
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args2.get_version(), Some("2.0.0"));
    ///
    /// let args3 = InfoArgs {
    ///     package: "pkg".to_string(),
    ///     version: None,
    ///     version_flag: None,
    ///     format: OutputFormatArg::Table,
    /// };
    /// assert_eq!(args3.get_version(), None);
    /// ```
    pub fn get_version(&self) -> Option<&str> {
        self.version.as_deref().or(self.version_flag.as_deref())
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

    /// Enable CORS for all origins.
    #[arg(long)]
    pub cors: bool,

    /// Specific CORS origins (comma-separated). Implies --cors.
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

/// Arguments for the backfill command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct BackfillArgs {
    /// Path to the nixpkgs repository.
    #[arg(long)]
    pub nixpkgs_path: PathBuf,

    /// Only backfill these specific fields.
    #[arg(long, value_delimiter = ',')]
    pub fields: Option<Vec<String>>,

    /// Limit the number of packages to backfill (for testing).
    #[arg(long)]
    pub limit: Option<usize>,

    /// Dry run - show what would be updated without making changes.
    #[arg(long)]
    pub dry_run: bool,

    /// Traverse git history to extract metadata from original commits.
    /// Without this flag, metadata is extracted from current nixpkgs HEAD only.
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
    fn test_search_with_options() {
        let args = Cli::try_parse_from(["nxv", "search", "python", "--version", "3.11", "--exact"])
            .unwrap();
        match args.command {
            Commands::Search(search) => {
                assert_eq!(search.package, "python");
                assert_eq!(search.version, Some("3.11".to_string()));
                assert!(search.exact);
            }
            _ => panic!("Expected Search command"),
        }
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
}
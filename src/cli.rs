//! Command-line interface definitions using clap.

use crate::output::OutputFormat;
use crate::paths;
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

    /// Show index statistics.
    Info,

    /// Show version history for a package.
    History(HistoryArgs),

    /// Build the index from a local nixpkgs repository.
    #[cfg(feature = "indexer")]
    Index(IndexArgs),

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

    /// Number of parallel workers.
    #[arg(short, long, default_value_t = num_cpus())]
    pub jobs: usize,

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

#[cfg(feature = "indexer")]
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
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
    fn from(arg: OutputFormatArg) -> Self {
        match arg {
            OutputFormatArg::Table => OutputFormat::Table,
            OutputFormatArg::Json => OutputFormat::Json,
            OutputFormatArg::Plain => OutputFormat::Plain,
        }
    }
}

/// Sort order for search results.
#[derive(ValueEnum, Clone, Copy, Debug, Default)]
pub enum SortOrder {
    /// Sort by date (newest first).
    #[default]
    Date,
    /// Sort by version (semver-aware).
    Version,
    /// Sort by name (alphabetical).
    Name,
}

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
        let args = Cli::try_parse_from(["nxv", "info"]).unwrap();
        assert!(matches!(args.command, Commands::Info));
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
        let args = Cli::try_parse_from(["nxv", "-vv", "--no-color", "info"]).unwrap();
        assert_eq!(args.verbose, 2);
        assert!(args.no_color);
    }

    #[test]
    fn test_quiet_conflicts_with_verbose() {
        let result = Cli::try_parse_from(["nxv", "-v", "-q", "info"]);
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

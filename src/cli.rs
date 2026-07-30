//! Command-line interface definitions using clap.

use crate::output::OutputFormat;
use crate::paths;
use crate::run::PackageSpec;
use crate::search::SortOrder;
use crate::skill::Agent;
use crate::version;
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

/// nxv - Nix Version Index
#[derive(Parser, Debug)]
#[command(name = "nxv")]
#[command(author, version = version::clap_version(), long_version = version::long_version(), about, long_about = None)]
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

    /// Open a shell with resolved package versions.
    Run(RunArgs),

    /// Update nxv to the latest application release.
    ///
    /// On a local install (install.sh, manual download), the latest binary
    /// is downloaded, verified, and replaced atomically. Managed installs
    /// (Nix, cargo, Homebrew) print the matching upgrade command instead.
    /// Use `nxv sync` to download or refresh the package index.
    Update(UpdateArgs),

    /// Download or refresh the local package index.
    ///
    /// Synchronizes the SQLite index and bloom filter from the configured
    /// manifest. This command never checks for or installs nxv releases.
    Sync(SyncArgs),

    /// Show detailed information about a package.
    ///
    /// The package name is resolved as an exact attribute path first, so
    /// `nxv info python311` reports on python311 itself and never on
    /// python311Full or python311Packages.*. A known package that never
    /// had the requested version reports not found rather than falling
    /// back to unrelated prefix matches.
    ///
    /// An unknown attribute path is widened to a prefix search, but what
    /// gets prefix-matched differs: with a version it matches attribute
    /// paths (so `nxv info python311Packages.req 2.32` resolves), and
    /// without a version it matches the package name field instead (values
    /// like `python-3.11.0`), so partial attribute paths generally do not
    /// resolve unversioned. Use `nxv search` for prefix matching.
    Info(InfoArgs),

    /// Show index statistics.
    Stats,

    /// Show version history for a package.
    History(HistoryArgs),

    /// Build the index from nixpkgs channel-release snapshots.
    #[cfg(feature = "indexer")]
    Index(IndexArgs),

    /// Retired: metadata now comes from channel snapshots (see `nxv index`).
    #[cfg(feature = "indexer")]
    #[command(hide = true)]
    Backfill(RetiredArgs),

    /// Retired: the snapshot indexer does not use a nixpkgs checkout.
    #[cfg(feature = "indexer")]
    #[command(hide = true)]
    Reset(RetiredArgs),

    /// Collapse duplicate `(attribute_path, version)` rows in the index.
    ///
    /// Repairs databases bloated by the pre-0.1.5 incremental indexer bug
    /// (see CHANGELOG). Keeps one row per unique pair with the earliest
    /// `first_commit_*` and the latest `last_commit_*` across the duplicates,
    /// then VACUUMs.
    #[cfg(feature = "indexer")]
    Dedupe(DedupeArgs),

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

    /// Install the nxv agent skill for AI coding agents.
    ///
    /// Generates a SKILL.md following the Agent Skills standard
    /// (https://agentskills.io) and installs it where each supported agent
    /// looks: Claude Code, OpenAI Codex CLI, Pi, OpenClaw, GitHub Copilot
    /// CLI, Cursor, Gemini CLI, Amp, and Goose, plus the generic
    /// cross-agent `.agents/skills` directory. Supports user-wide
    /// (default) and project-level (--project) installs.
    Skill(SkillArgs),

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

/// Arguments for the skill command.
#[derive(Parser, Debug)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommands,
}

/// Skill subcommands.
#[derive(Subcommand, Debug)]
pub enum SkillCommands {
    /// List supported agents, their skill paths, and install status.
    List(SkillListArgs),

    /// Install the nxv skill for explicitly selected agents.
    ///
    /// Name one or more agents, or explicitly pass `--detected` or `--all`.
    /// Nothing is installed when no target mode is supplied. User-wide is
    /// the default scope; use `--project` or `--dir` for project-level paths.
    Install(SkillInstallArgs),

    /// Remove installed nxv skills.
    ///
    /// With no agent arguments, removes the skill from every known agent
    /// path in the selected scope. Only `nxv/SKILL.md` is deleted; the
    /// directory is kept if it contains other files.
    Uninstall(SkillUninstallArgs),

    /// Print the generated SKILL.md to stdout.
    Show,
}

/// Arguments for `skill list`.
#[derive(Parser, Debug)]
pub struct SkillListArgs {
    /// Use ASCII table borders instead of Unicode.
    #[arg(long)]
    pub ascii: bool,
}

/// Arguments for `skill install`.
#[derive(Parser, Debug)]
#[command(group(
    ArgGroup::new("targets")
        .required(true)
        .multiple(false)
        .args(["agents", "detected", "all"])
))]
pub struct SkillInstallArgs {
    /// Agents to install for.
    #[arg(value_enum)]
    pub agents: Vec<Agent>,

    /// Install into project-level skill directories under the current
    /// directory instead of user-wide.
    #[arg(long)]
    pub project: bool,

    /// Project directory to install into (implies --project).
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Install for agents detected from user configuration directories.
    ///
    /// With --project or --dir, detected agents are mapped to their
    /// corresponding project-level skill paths.
    #[arg(long)]
    pub detected: bool,

    /// Install for all supported agents regardless of detection.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `skill uninstall`.
#[derive(Parser, Debug)]
pub struct SkillUninstallArgs {
    /// Agents to uninstall from (default: every agent path in scope).
    #[arg(value_enum)]
    pub agents: Vec<Agent>,

    /// Remove from project-level skill directories under the current
    /// directory instead of user-wide.
    #[arg(long)]
    pub project: bool,

    /// Project directory to uninstall from (implies --project).
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,
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

    /// Include nested attribute-path depths in a version search.
    #[arg(long, conflicts_with_all = ["exact", "desc"])]
    pub all_depths: bool,

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

/// Arguments for the run command.
#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Package name or attribute path to resolve.
    pub package: String,

    /// Version to filter by (positional, prefix match).
    #[arg(conflicts_with = "version_opt")]
    pub version: Option<String>,

    /// Filter by version (prefix match, alternative to positional).
    #[arg(short = 'V', long = "version", conflicts_with = "version")]
    pub version_opt: Option<String>,

    /// Add another package, optionally with an @VERSION suffix.
    #[arg(long = "with", value_name = "PACKAGE[@VERSION]")]
    pub additional: Vec<PackageSpec>,

    /// Perform exact attribute-path matching.
    #[arg(short, long)]
    pub exact: bool,

    /// Include nested attribute-path depths in version searches.
    #[arg(long, conflicts_with = "exact")]
    pub all_depths: bool,
}

impl RunArgs {
    /// Get the primary package version from either positional or option argument.
    pub fn get_version(&self) -> Option<&str> {
        self.version.as_deref().or(self.version_opt.as_deref())
    }

    /// Return the primary query followed by all `--with` queries.
    pub fn package_specs(&self) -> Vec<PackageSpec> {
        let mut specs = Vec::with_capacity(1 + self.additional.len());
        specs.push(PackageSpec::new(
            self.package.clone(),
            self.get_version().map(str::to_string),
        ));
        specs.extend(self.additional.iter().cloned());
        specs
    }
}

/// Arguments retained only to give users of the former combined update
/// command an actionable migration error. They stay hidden from help and
/// never trigger index or application updates.
#[derive(Parser, Debug)]
pub struct UpdateArgs {
    #[arg(short = 'f', long = "force", hide = true)]
    pub legacy_force: bool,

    #[arg(long = "manifest-url", hide = true)]
    pub legacy_manifest_url: Option<String>,

    #[arg(long = "skip-verify", hide = true)]
    pub legacy_skip_verify: bool,

    #[arg(long = "public-key", hide = true)]
    pub legacy_public_key: Option<String>,

    #[arg(long = "no-self-update", hide = true)]
    pub legacy_no_self_update: bool,
}

impl UpdateArgs {
    pub fn has_legacy_index_options(&self) -> bool {
        self.legacy_force
            || self.legacy_manifest_url.is_some()
            || self.legacy_skip_verify
            || self.legacy_public_key.is_some()
            || self.legacy_no_self_update
    }
}

/// Arguments for the sync command.
#[derive(Parser, Debug)]
pub struct SyncArgs {
    /// Force full re-download of the index.
    #[arg(short, long)]
    pub force: bool,

    /// Custom manifest URL for alternate or self-hosted indexes.
    #[arg(long, env = "NXV_MANIFEST_URL")]
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
    /// Package name to show info for (matched as an exact attribute path first).
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
///
/// The indexer ingests channel-release snapshots from releases.nixos.org:
/// `packages.json.br` where it exists (2020-03-27 onward; no Nix evaluation),
/// and optionally `nix-env` over `nixexprs.tar.xz` for the pre-2020 era
/// (`--backfill-evals`, requires `nix`).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct IndexArgs {
    /// Channels to ingest (repeatable). Defaults to nixpkgs-unstable (history
    /// spine) plus nixos-unstable-small (currency).
    #[arg(long = "channel", value_delimiter = ',')]
    pub channels: Option<Vec<String>>,

    /// Only ingest releases dated on/after this date (YYYY-MM-DD).
    #[arg(long)]
    pub since: Option<String>,

    /// Only ingest releases dated on/before this date (YYYY-MM-DD).
    #[arg(long)]
    pub until: Option<String>,

    /// Parallel snapshot download/parse workers.
    #[arg(long)]
    pub jobs: Option<usize>,

    /// Treat monitor warnings (count floors, sentinels, head lag) as fatal.
    #[arg(long)]
    pub strict: bool,

    /// Write the end-of-run coverage report as JSON to this path.
    #[arg(long)]
    pub report: Option<PathBuf>,

    /// Retry releases that were parked as failed/skipped.
    #[arg(long)]
    pub retry_failed: bool,

    /// Also ingest the pre-2020 era by evaluating nixexprs.tar.xz with
    /// nix-env (requires `nix`; one-time, ~1.5-3h).
    #[arg(long)]
    pub backfill_evals: bool,

    /// Evaluate nixpkgs master HEAD directly (GitHub tarball) when channel
    /// observations lag behind; requires `nix`.
    #[arg(long)]
    pub head_eval: bool,

    /// Re-plan every known release instead of only new ones.
    #[arg(long)]
    pub full: bool,

    /// Limit the number of releases ingested this run (for testing).
    #[arg(long)]
    pub max_releases: Option<usize>,

    /// Deprecated: the snapshot indexer does not read a nixpkgs checkout.
    /// Accepted (with a warning) for one release cycle.
    #[arg(long, hide = true)]
    pub nixpkgs_path: Option<PathBuf>,
}

/// Catch-all arguments for retired subcommands (hidden deprecation stubs).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct RetiredArgs {
    /// Ignored.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

/// Arguments for the dedupe command (feature-gated).
#[cfg(feature = "indexer")]
#[derive(Parser, Debug)]
pub struct DedupeArgs {
    /// Report what would change without modifying the database.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the trailing VACUUM (faster, but the DB file won't shrink).
    #[arg(long)]
    pub no_vacuum: bool,
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

    /// Prefix to add to index and bloom artifact names in the manifest.
    ///
    /// This is useful for GitHub release publishing: payload artifacts can be
    /// uploaded under immutable run-specific names while manifest.json remains
    /// the stable pointer clients fetch.
    #[arg(long)]
    pub artifact_name_prefix: Option<String>,

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
                assert_eq!(search.sort, SortOrder::Relevance);
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
    fn test_run_command_with_additional_packages() {
        let args = Cli::try_parse_from([
            "nxv",
            "run",
            "python",
            "2.7",
            "--with",
            "nodejs@15",
            "--with",
            "jq",
            "--all-depths",
        ])
        .unwrap();

        match args.command {
            Commands::Run(run) => {
                assert_eq!(run.package, "python");
                assert_eq!(run.get_version(), Some("2.7"));
                assert_eq!(
                    run.additional,
                    [
                        PackageSpec::new("nodejs".to_string(), Some("15".to_string())),
                        PackageSpec::new("jq".to_string(), None),
                    ]
                );
                assert!(run.all_depths);
                assert!(!run.exact);
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_run_version_option_and_positional_conflict() {
        assert!(Cli::try_parse_from(["nxv", "run", "python", "2.7", "-V", "3.11"]).is_err());
    }

    #[test]
    fn test_run_rejects_malformed_additional_specs() {
        assert!(Cli::try_parse_from(["nxv", "run", "python", "--with", "@15"]).is_err());
        assert!(Cli::try_parse_from(["nxv", "run", "python", "--with", "nodejs@"]).is_err());
    }

    #[test]
    fn test_update_command() {
        let args = Cli::try_parse_from(["nxv", "update"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert!(!update.has_legacy_index_options());
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_retains_legacy_force_for_migration_error() {
        let args = Cli::try_parse_from(["nxv", "update", "--force"]).unwrap();
        match args.command {
            Commands::Update(update) => {
                assert!(update.has_legacy_index_options());
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_sync_command() {
        let args = Cli::try_parse_from(["nxv", "sync"]).unwrap();
        match args.command {
            Commands::Sync(sync) => {
                assert!(!sync.force);
                assert!(!sync.skip_verify);
            }
            _ => panic!("Expected Sync command"),
        }
    }

    #[test]
    fn test_sync_force() {
        let args = Cli::try_parse_from(["nxv", "sync", "--force"]).unwrap();
        match args.command {
            Commands::Sync(sync) => {
                assert!(sync.force);
            }
            _ => panic!("Expected Sync command"),
        }
    }

    #[test]
    fn test_sync_public_key() {
        let args = Cli::try_parse_from(["nxv", "sync", "--public-key", "RWTest123"]).unwrap();
        match args.command {
            Commands::Sync(sync) => {
                assert_eq!(sync.public_key, Some("RWTest123".to_string()));
                assert!(!sync.skip_verify);
            }
            _ => panic!("Expected Sync command"),
        }
    }

    #[test]
    fn test_skill_install_requires_explicit_target_mode() {
        assert!(Cli::try_parse_from(["nxv", "skill", "install"]).is_err());
    }

    #[test]
    fn test_skill_install_detected_mode() {
        let args = Cli::try_parse_from(["nxv", "skill", "install", "--detected"]).unwrap();
        match args.command {
            Commands::Skill(skill) => match skill.command {
                SkillCommands::Install(install) => {
                    assert!(install.detected);
                    assert!(!install.all);
                    assert!(install.agents.is_empty());
                }
                _ => panic!("Expected Install command"),
            },
            _ => panic!("Expected Skill command"),
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

    #[test]
    fn test_skill_install_parsing() {
        let args = Cli::try_parse_from(["nxv", "skill", "install", "claude", "codex", "--project"])
            .unwrap();
        match args.command {
            Commands::Skill(skill) => match skill.command {
                SkillCommands::Install(install) => {
                    assert_eq!(install.agents, vec![Agent::Claude, Agent::Codex]);
                    assert!(install.project);
                    assert!(install.dir.is_none());
                    assert!(!install.detected);
                    assert!(!install.all);
                }
                _ => panic!("Expected Install subcommand"),
            },
            _ => panic!("Expected Skill command"),
        }
    }

    #[test]
    fn test_skill_install_all_conflicts_with_agents() {
        let result = Cli::try_parse_from(["nxv", "skill", "install", "claude", "--all"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_skill_install_detected_conflicts_with_other_targets() {
        assert!(Cli::try_parse_from(["nxv", "skill", "install", "claude", "--detected"]).is_err());
        assert!(Cli::try_parse_from(["nxv", "skill", "install", "--detected", "--all"]).is_err());
    }

    #[test]
    fn test_skill_uninstall_dir() {
        let args =
            Cli::try_parse_from(["nxv", "skill", "uninstall", "--dir", "/tmp/proj"]).unwrap();
        match args.command {
            Commands::Skill(skill) => match skill.command {
                SkillCommands::Uninstall(uninstall) => {
                    assert!(uninstall.agents.is_empty());
                    assert_eq!(uninstall.dir, Some(PathBuf::from("/tmp/proj")));
                }
                _ => panic!("Expected Uninstall subcommand"),
            },
            _ => panic!("Expected Skill command"),
        }
    }

    #[test]
    fn test_skill_show_parsing() {
        let args = Cli::try_parse_from(["nxv", "skill", "show"]).unwrap();
        match args.command {
            Commands::Skill(skill) => assert!(matches!(skill.command, SkillCommands::Show)),
            _ => panic!("Expected Skill command"),
        }
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_index_channel_parsing() {
        let args = Cli::try_parse_from([
            "nxv",
            "index",
            "--channel",
            "nixpkgs-unstable,nixos-unstable-small",
            "--strict",
        ])
        .unwrap();

        match args.command {
            Commands::Index(index) => {
                let channels = index.channels.unwrap();
                assert_eq!(channels, vec!["nixpkgs-unstable", "nixos-unstable-small"]);
                assert!(index.strict);
                assert!(!index.backfill_evals);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[cfg(feature = "indexer")]
    #[test]
    fn test_index_accepts_deprecated_nixpkgs_path() {
        let args = Cli::try_parse_from(["nxv", "index", "--nixpkgs-path", "./nixpkgs"]).unwrap();
        match args.command {
            Commands::Index(index) => {
                assert!(index.nixpkgs_path.is_some());
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

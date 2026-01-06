//! nxv - Nix Version Index

mod backend;
mod bloom;
mod cli;
mod client;
mod completions;
mod db;
mod error;
mod output;
mod paths;
mod remote;
mod search;
mod theme;
pub mod version;

#[cfg(feature = "indexer")]
mod index;

mod server;

use anyhow::Result;
use clap::Parser;
use owo_colors::{OwoColorize, Stream::Stderr};

use cli::{Cli, Commands};

/// Program entry point: parses CLI arguments, dispatches the selected command, and handles top-level errors.
///
/// This function configures global color behavior based on the CLI, invokes the appropriate command handler
/// for the parsed subcommand, and on error prints a colored error header followed by each cause in the error
/// chain before exiting with status code 1.
///
/// # Examples
///
/// ```
/// // Running the binary will invoke `main`. In doctests this demonstrates that `main` is callable.
/// // Note: calling `main()` will execute the program logic and may exit the process on error.
/// nxv::main();
/// ```
fn main() {
    let cli = Cli::parse();

    // Handle no-color flag - affects both owo_colors and comfy_table
    if cli.no_color {
        theme::disable_colors();
    }

    let result = match &cli.command {
        Commands::Search(args) => cmd_search(&cli, args),
        Commands::Update(args) => cmd_update(&cli, args),
        Commands::Info(args) => cmd_pkg_info(&cli, args),
        Commands::Stats => cmd_stats(&cli),
        Commands::History(args) => cmd_history(&cli, args),
        Commands::Completions(args) => {
            args.generate();
            Ok(())
        }
        Commands::CompletePackage(args) => cmd_complete_package(&cli, args),
        #[cfg(feature = "indexer")]
        Commands::Index(args) => cmd_index(&cli, args),
        #[cfg(feature = "indexer")]
        Commands::Backfill(args) => cmd_backfill(&cli, args),
        #[cfg(feature = "indexer")]
        Commands::Reset(args) => cmd_reset(&cli, args),
        #[cfg(feature = "indexer")]
        Commands::Publish(args) => cmd_publish(&cli, args),
        #[cfg(feature = "indexer")]
        Commands::Keygen(args) => cmd_keygen(&cli, args),
        Commands::Serve(args) => cmd_serve(&cli, args),
    };

    if let Err(e) = result {
        eprintln!(
            "{}: {}",
            "error"
                .if_supports_color(Stderr, |text| text.red())
                .if_supports_color(Stderr, |text| text.bold()),
            e
        );
        // Print the error chain if there are causes
        for cause in e.chain().skip(1) {
            eprintln!(
                "  {}: {}",
                "caused by".if_supports_color(Stderr, |text| text.yellow()),
                cause
            );
        }
        std::process::exit(1);
    }
}

/// Selects and initializes the appropriate backend based on the `NXV_API_URL` environment variable.
///
/// If `NXV_API_URL` is set, constructs an `ApiClient` for that URL and returns `Backend::Remote`.
/// Otherwise opens the local database at `cli.db_path` in read-only mode and returns `Backend::Local`.
///
/// # Errors
///
/// Returns any error produced while creating the API client or opening the local database.
///
/// # Examples
///
/// ```
/// // chooses remote when NXV_API_URL is set
/// std::env::set_var("NXV_API_URL", "https://example.com");
/// let cli = crate::cli::Cli::default(); // adjust as needed for test context
/// let backend = crate::main::get_backend(&cli).unwrap();
/// match backend {
///     crate::backend::Backend::Remote(_) => {}
///     _ => panic!("expected remote backend"),
/// }
/// ```
fn get_backend(cli: &Cli) -> Result<backend::Backend> {
    use backend::Backend;

    if let Ok(url) = std::env::var("NXV_API_URL") {
        let client = client::ApiClient::new_with_timeout(&url, cli.api_timeout)?;
        Ok(Backend::Remote(client))
    } else {
        let db = db::Database::open_readonly(&cli.db_path)?;
        Ok(Backend::Local(db))
    }
}

/// Selects the runtime backend (remote API client or local readonly database) and
/// offers an interactive first-run flow to download the local index when none is found.
///
/// If the `NXV_API_URL` environment variable is set, a remote API client backend is created.
/// Otherwise the function attempts to open the local database at `cli.db_path`.
/// If no local index exists and the process is running in interactive terminals and `cli.quiet` is false,
/// the user is prompted to download the index; consenting runs the update flow and the database is reopened.
/// All other errors are propagated.
///
/// # Returns
///
/// A configured `backend::Backend` instance: either a remote API client or a local readonly database.
///
/// # Examples
///
/// ```no_run
/// let cli = Cli::parse();
/// let backend = get_backend_with_prompt(&cli).expect("failed to initialize backend");
/// match backend {
///     backend::Backend::Local(_) => println!("using local database"),
///     backend::Backend::Remote(_) => println!("using remote API"),
/// }
/// ```
fn get_backend_with_prompt(cli: &Cli) -> Result<backend::Backend> {
    use backend::Backend;
    use std::io::{IsTerminal, Write};

    // If using remote API, no need for local database
    if let Ok(url) = std::env::var("NXV_API_URL") {
        let client = client::ApiClient::new_with_timeout(&url, cli.api_timeout)?;
        return Ok(Backend::Remote(client));
    }

    // Try to open local database
    match db::Database::open_readonly(&cli.db_path) {
        Ok(db) => Ok(Backend::Local(db)),
        Err(error::NxvError::NoIndex) => {
            // First-run experience: offer to download the index
            if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() && !cli.quiet {
                eprintln!("No package index found.");
                eprint!("Would you like to download it now? [Y/n] ");
                std::io::stderr().flush()?;

                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let input = input.trim().to_lowercase();

                if input.is_empty() || input == "y" || input == "yes" {
                    // Run the update command
                    let update_args = cli::UpdateArgs {
                        force: false,
                        manifest_url: None,
                        skip_verify: false,
                        public_key: None,
                    };
                    cmd_update(cli, &update_args)?;

                    // Try to open the database again
                    let db = db::Database::open_readonly(&cli.db_path)?;
                    Ok(Backend::Local(db))
                } else {
                    Err(error::NxvError::NoIndex.into())
                }
            } else {
                Err(error::NxvError::NoIndex.into())
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Searches for packages using the configured backend and prints results according to CLI options.
///
/// This runs the search described by `args` against either the local database or a remote API,
/// may consult a local bloom filter to short-circuit exact-name lookups, and emits results in
/// the output format selected on the CLI. Respects verbosity and quiet flags and will print a
/// "more results" hint when the backend reports additional matches beyond the returned page.
///
/// # Examples
///
/// ```no_run
/// # use crate::cli::{Cli, SearchArgs};
/// # fn make_cli() -> Cli { unimplemented!() }
/// # fn make_args() -> SearchArgs { unimplemented!() }
/// let cli = make_cli();
/// let args = make_args();
/// // Call from a CLI command handler context
/// let _ = crate::cmd_search(&cli, &args);
/// ```
///
/// # Returns
///
/// `Ok(())` on success, or an error if the backend operation fails.
fn cmd_search(cli: &Cli, args: &cli::SearchArgs) -> Result<()> {
    use crate::bloom::PackageBloomFilter;
    use crate::cli::Verbosity;
    use crate::output::{OutputFormat, TableOptions, print_results};
    use crate::search::SearchOptions;

    let verbosity = cli.verbosity();

    // Debug: show search parameters
    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Search parameters:");
        eprintln!("[debug]   package: {:?}", args.package);
        eprintln!("[debug]   version: {:?}", args.get_version());
        eprintln!("[debug]   exact: {}", args.exact);
        eprintln!("[debug]   desc: {}", args.desc);
        eprintln!("[debug]   license: {:?}", args.license);
        if std::env::var("NXV_API_URL").is_ok() {
            eprintln!("[debug]   backend: remote API");
        } else {
            eprintln!("[debug]   db_path: {:?}", cli.db_path);
        }
    }

    // Get backend (local DB or remote API)
    let backend = get_backend_with_prompt(cli)?;

    // For exact package searches with local backend, check bloom filter first
    // This only applies to exact searches (not prefix or description searches)
    if args.exact && !args.desc && !backend.is_remote() {
        let bloom_path = paths::get_bloom_path_for_db(&cli.db_path);

        // Check if bloom filter is missing or stale (older than database)
        // This handles the case where the database is being actively indexed
        let bloom_is_stale = || -> bool {
            // If bloom doesn't exist, it's "stale" (needs creation)
            if !bloom_path.exists() {
                return true;
            }

            // Get bloom filter modification time
            let bloom_mtime = match std::fs::metadata(&bloom_path) {
                Ok(m) => m.modified().ok(),
                Err(_) => return true,
            };

            // Get database modification time (check WAL file too for active writes)
            let db_mtime = std::fs::metadata(&cli.db_path)
                .ok()
                .and_then(|m| m.modified().ok());
            let wal_path = cli.db_path.with_extension("db-wal");
            let wal_mtime = std::fs::metadata(&wal_path)
                .ok()
                .and_then(|m| m.modified().ok());

            // Bloom is stale if database or WAL is newer
            match (bloom_mtime, db_mtime, wal_mtime) {
                (Some(bloom), Some(db), _) if db > bloom => true,
                (Some(bloom), _, Some(wal)) if wal > bloom => true,
                _ => false,
            }
        };

        // Regenerate bloom filter if missing or stale
        if bloom_is_stale()
            && cli.db_path.exists()
            && let Ok(db) = crate::db::Database::open_readonly(&cli.db_path)
        {
            if !cli.quiet {
                eprintln!("Generating bloom filter...");
            }
            if let Err(e) = PackageBloomFilter::regenerate_from_db(&db, &bloom_path) {
                // Non-fatal: just log and continue without bloom filter
                if verbosity >= Verbosity::Debug {
                    eprintln!("[debug] Failed to generate bloom filter: {}", e);
                }
            }
        }

        if bloom_path.exists()
            && let Ok(filter) = PackageBloomFilter::load(&bloom_path)
            && !filter.contains(&args.package)
        {
            // Bloom filter says definitely not present
            if !cli.quiet {
                eprintln!("No packages found matching '{}'", args.package);
            }
            return Ok(());
        }
        // If bloom filter says maybe present or couldn't load, continue with query
    }

    // Build search options from CLI args
    let opts = SearchOptions {
        query: args.package.clone(),
        version: args.get_version().map(|s| s.to_string()),
        exact: args.exact,
        desc: args.desc,
        license: args.license.clone(),
        sort: args.sort,
        reverse: args.reverse,
        full: args.full,
        limit: args.limit,
        offset: 0, // CLI doesn't support offset yet
    };

    // Determine query type for logging
    let query_type = if args.desc {
        "FTS description search"
    } else if args.get_version().is_some() {
        "package+version search"
    } else if args.exact {
        "exact package search"
    } else {
        "prefix package search"
    };

    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Query type: {}", query_type);
    } else if verbosity >= Verbosity::Info {
        eprintln!("Searching for '{}'...", args.package);
    }

    // Execute search using backend
    let result = backend.search(&opts)?;

    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Total results: {}", result.total);
    }

    if result.data.is_empty() {
        if !cli.quiet {
            eprintln!("No packages found matching '{}'", args.package);
        }
        return Ok(());
    }

    // Print results
    let format: OutputFormat = args.format.into();
    let options = TableOptions {
        show_platforms: args.show_platforms,
        show_store_path: args.show_store_path,
        ascii: args.ascii,
    };
    print_results(&result.data, format, options);

    // Show "more results" message after the table
    if result.has_more && !cli.quiet {
        let remaining = result.total - result.data.len();
        // Check if using remote API (applied_limit is set)
        if let Some(applied) = result.applied_limit {
            // Remote API mode - always show server limit since --limit 0 won't help
            eprintln!(
                "{} more results. Server limits responses to {} results per request.",
                remaining, applied
            );
        } else {
            // Local mode - user can request unlimited results
            eprintln!("{} more results. Use --limit 0 for all.", remaining);
        }
    }

    Ok(())
}

/// Updates the local package index from the configured manifest or remote API.
///
/// Performs an update using the provided CLI context and update arguments, and prints
/// user-facing progress and the final outcome (up-to-date, downloaded, delta applied,
/// or full download). Verbosity and quiet flags on `cli` control additional output.
///
/// # Returns
///
/// `Ok(())` on success, or an error if the update process fails.
///
/// # Examples
///
/// ```no_run
/// # use anyhow::Result;
/// # use crate::cli::{Cli, UpdateArgs};
/// # fn example() -> Result<()> {
/// // `cli` and `args` would typically come from CLI parsing.
/// let cli = Cli::parse();
/// let args = UpdateArgs { force: false, manifest_url: None };
/// cmd_update(&cli, &args)?;
/// # Ok(())
/// # }
/// ```
fn cmd_update(cli: &Cli, args: &cli::UpdateArgs) -> Result<()> {
    use crate::cli::Verbosity;
    use crate::remote::update::{UpdateStatus, perform_update};

    let verbosity = cli.verbosity();
    let show_progress = !cli.quiet;
    let manifest_url = args.manifest_url.as_deref();

    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Update parameters:");
        eprintln!("[debug]   force: {}", args.force);
        eprintln!("[debug]   db_path: {:?}", cli.db_path);
        if let Some(url) = manifest_url {
            eprintln!("[debug]   manifest_url: {}", url);
        }
    }

    if args.force {
        eprintln!("Forcing full re-download of index...");
    } else {
        eprintln!("Checking for updates...");
    }

    let status = perform_update(
        manifest_url,
        &cli.db_path,
        args.force,
        show_progress,
        args.skip_verify,
        args.public_key.as_deref(),
        Some(cli.api_timeout),
    )?;

    match status {
        UpdateStatus::UpToDate { commit } => {
            eprintln!(
                "Index is up to date (commit {}).",
                &commit[..7.min(commit.len())]
            );
            if verbosity >= Verbosity::Info {
                eprintln!("Local index commit: {}", commit);
            }
        }
        UpdateStatus::NoLocalIndex { .. } => {
            eprintln!("Index downloaded successfully.");
        }
        UpdateStatus::DeltaAvailable { to_commit, .. } => {
            eprintln!(
                "Delta update applied successfully (now at commit {}).",
                &to_commit[..7.min(to_commit.len())]
            );
        }
        UpdateStatus::FullDownloadNeeded { commit, .. } => {
            eprintln!(
                "Full index downloaded successfully (commit {}).",
                &commit[..7.min(commit.len())]
            );
            if verbosity >= Verbosity::Info {
                eprintln!("Full commit hash: {}", commit);
            }
        }
    }

    Ok(())
}

/// Display detailed information for a package in the format requested by the CLI.
///
/// Obtains the configured backend (local database or remote API), looks up the package
/// by attribute path or name (optionally restricted to a version), and writes the
/// package information to stdout using the chosen output format (JSON, plain key/value
/// lines, or a human-friendly table view).
///
/// The table view presents a detailed single-package summary (description, availability,
/// maintainers, platforms, usage examples) and lists other attribute paths when multiple
/// results are found. If no matching package is found, a not-found message is printed.
///
/// # Returns
///
/// `Ok(())` on success, error otherwise.
///
/// # Examples
///
/// ```no_run
/// // Example (no runtime guarantees): construct CLI/args and print package info.
/// # use crate::cli;
/// # use crate::Cli;
/// # use crate::cmd_pkg_info;
/// let cli = Cli::parse_from(&["nxv"]);
/// let args = cli::InfoArgs {
///     package: "hello".to_string(),
///     version: None,
///     format: cli::OutputFormatArg::Table,
/// };
/// cmd_pkg_info(&cli, &args).unwrap();
/// ```
fn cmd_pkg_info(cli: &Cli, args: &cli::InfoArgs) -> Result<()> {
    use output::components;

    // Get backend (local DB or remote API)
    let backend = get_backend_with_prompt(cli)?;

    // Get package info - search by attribute path first (what users install with),
    // then fall back to name prefix search
    let version = args.get_version();
    let packages = backend.search_by_name_version(&args.package, version)?;

    if packages.is_empty() {
        println!(
            "Package '{}' not found{}.",
            args.package,
            version
                .map(|v| format!(" version {}", v))
                .unwrap_or_default()
        );
        return Ok(());
    }

    match args.format {
        cli::OutputFormatArg::Json => {
            println!("{}", serde_json::to_string_pretty(&packages)?);
        }
        cli::OutputFormatArg::Plain => {
            for pkg in &packages {
                println!("name\t{}", pkg.name);
                println!("version\t{}", pkg.version);
                println!("attribute_path\t{}", pkg.attribute_path);
                println!("first_commit\t{}", pkg.first_commit_hash);
                println!("first_date\t{}", pkg.first_commit_date.format("%Y-%m-%d"));
                println!("last_commit\t{}", pkg.last_commit_hash);
                println!("last_date\t{}", pkg.last_commit_date.format("%Y-%m-%d"));
                println!("description\t{}", pkg.description.as_deref().unwrap_or("-"));
                println!("license\t{}", pkg.license.as_deref().unwrap_or("-"));
                println!("homepage\t{}", pkg.homepage.as_deref().unwrap_or("-"));
                println!("maintainers\t{}", pkg.maintainers.as_deref().unwrap_or("-"));
                println!("platforms\t{}", pkg.platforms.as_deref().unwrap_or("-"));
                println!("insecure\t{}", if pkg.is_insecure() { "yes" } else { "no" });
                if pkg.is_insecure() {
                    println!(
                        "known_vulnerabilities\t{}",
                        pkg.known_vulnerabilities.as_deref().unwrap_or("[]")
                    );
                }
                println!();
            }
        }
        cli::OutputFormatArg::Table => {
            // For table format, show a detailed view for the first/most relevant result
            // and summarize if there are multiple attribute paths
            let pkg = &packages[0];

            components::print_package_header(pkg);
            components::print_details(pkg);
            components::print_availability(pkg);
            components::print_maintainers(pkg);
            components::print_platforms(pkg);
            components::print_security_warning(pkg);
            components::print_usage(pkg);
            components::print_store_paths(pkg);
            components::print_preflakes_warning(pkg);
            components::print_other_attr_paths(&packages, pkg);
        }
    }

    Ok(())
}

/// Prints index metadata and aggregate statistics for the configured backend to stdout.
///
/// Obtains a backend (remote when `NXV_API_URL` is set, otherwise the local database) and prints
/// index information (API endpoint or database path, index version, last indexed commit) and
/// aggregate statistics (total version ranges, unique package names/versions, oldest/newest commit
/// dates). If using a local backend, also prints the database file size and bloom filter status.
/// If a local index is missing, prints guidance to run `nxv update` and returns without error.
///
/// # Examples
///
/// ```no_run
/// // Typical invocation from the CLI entry point:
/// // let cli = Cli::parse();
/// // cmd_stats(&cli).unwrap();
/// ```
fn cmd_stats(cli: &Cli) -> Result<()> {
    use theme::Themed;

    // Check if using remote API
    let is_remote = std::env::var("NXV_API_URL").is_ok();

    let backend = match get_backend(cli) {
        Ok(b) => b,
        Err(e) => {
            // Check if it's a NoIndex error for local backend
            if !is_remote
                && e.downcast_ref::<error::NxvError>()
                    .is_some_and(|e| matches!(e, error::NxvError::NoIndex))
            {
                println!("No index found at {:?}", cli.db_path);
                println!("Run 'nxv update' to download the package index.");
                return Ok(());
            }
            return Err(e);
        }
    };

    let stats = backend.get_stats()?;

    // Get meta info
    let last_commit = backend.get_meta("last_indexed_commit")?;
    let last_indexed_date = backend.get_meta("last_indexed_date")?;
    let index_version = backend.get_meta("index_version")?;

    println!("{}", "Index Information".section_header());

    if is_remote {
        println!(
            "{} {}",
            "API endpoint:".label(),
            std::env::var("NXV_API_URL").unwrap_or_default()
        );
    } else {
        println!("{} {:?}", "Database path:".label(), cli.db_path);
    }

    if let Some(version) = index_version {
        println!("{} {}", "Index version:".label(), version);
    }

    if let Some(commit) = last_commit {
        println!(
            "{} {}",
            "Last indexed commit:".label(),
            (&commit[..7.min(commit.len())]).commit()
        );
    }

    if let Some(date_str) = last_indexed_date {
        // Parse the RFC3339 date and display in a friendly format
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(&date_str) {
            println!(
                "{} {}",
                "Last updated:".label(),
                date.format("%Y-%m-%d %H:%M:%S UTC")
            );
        } else {
            println!("{} {}", "Last updated:".label(), date_str);
        }
    }

    println!();
    println!("{}", "Statistics".section_header());
    println!(
        "{} {}",
        "Total version ranges:".label(),
        stats.total_ranges.count()
    );
    println!(
        "{} {}",
        "Unique package names:".label(),
        stats.unique_names.count()
    );
    println!(
        "{} {}",
        "Unique versions:".label(),
        stats.unique_versions.count()
    );

    if let Some(oldest) = stats.oldest_commit_date {
        println!(
            "{} {}",
            "Oldest package date:".label(),
            oldest.format("%Y-%m-%d")
        );
    }

    if let Some(newest) = stats.newest_commit_date {
        println!(
            "{} {}",
            "Latest package change:".label(),
            newest.format("%Y-%m-%d")
        );
    }

    // Local-only info: file sizes
    if !is_remote {
        if cli.db_path.exists()
            && let Ok(metadata) = std::fs::metadata(&cli.db_path)
        {
            let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
            println!("{} {:.2} MB", "Database size:".label(), size_mb);
        }

        // Bloom filter status
        let bloom_path = paths::get_bloom_path_for_db(&cli.db_path);
        if bloom_path.exists()
            && let Ok(metadata) = std::fs::metadata(&bloom_path)
        {
            let size_kb = metadata.len() as f64 / 1024.0;
            println!(
                "{} {} ({:.2} KB)",
                "Bloom filter:".label(),
                "present".success(),
                size_kb
            );
        } else if !bloom_path.exists() {
            println!("{} {}", "Bloom filter:".label(), "not found".warning());
        }
    }

    Ok(())
}

/// Outputs package name completions for shell tab completion.
///
/// This is a hidden subcommand used by shell completion scripts to provide
/// dynamic package name completions. It queries the local database for
/// attribute paths matching the given prefix and outputs them one per line.
///
/// Designed to be fast and silent on errors (shell completions should not
/// produce error messages that interfere with the user experience).
///
/// # Examples
///
/// ```no_run
/// // Called by shell completion scripts:
/// // nxv complete-package pyth --limit 20
/// // Outputs:
/// // python
/// // python2
/// // python3
/// // pythonPackages.foo
/// // ...
/// ```
fn cmd_complete_package(cli: &Cli, args: &cli::CompletePackageArgs) -> Result<()> {
    // Silently fail if database doesn't exist - completions should not produce errors
    let db = match db::Database::open_readonly(&cli.db_path) {
        Ok(db) => db,
        Err(_) => return Ok(()),
    };

    // Get completions matching the prefix
    let completions =
        match db::queries::complete_package_prefix(db.connection(), &args.prefix, args.limit) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

    // Output one completion per line (standard format for shell completions)
    for name in completions {
        println!("{}", name);
    }

    Ok(())
}

/// Display version history for a package in one of several formats.
///
/// When `args.version` is provided, shows when that specific version first appeared and was last seen,
/// including short commit hashes, dates, and a usage hint. When `args.full` is set, prints detailed
/// rows for every matching package/version. Otherwise prints a summary list of versions with first
/// and last seen dates. Output format is selected via `args.format` (JSON, plain, or table) and
/// table rendering respects `args.ascii`.
///
/// # Returns
///
/// `Ok(())` if the command completed successfully, or an error if data retrieval or rendering failed.
///
/// # Examples
///
/// ```ignore
/// // Typical usage from a CLI entry point:
/// let cli = Cli::parse();
/// let args = cli::HistoryArgs { package: "foo".into(), ..Default::default() };
/// cmd_history(&cli, &args)?;
/// ```
fn cmd_history(cli: &Cli, args: &cli::HistoryArgs) -> Result<()> {
    use comfy_table::{
        Cell, ContentArrangement, Table,
        presets::{ASCII_FULL, UTF8_FULL},
    };
    use output::components;
    use theme::{Semantic, ThemedCell};

    // Get backend (local DB or remote API)
    let backend = get_backend_with_prompt(cli)?;

    if let Some(ref version) = args.version {
        // DEPRECATED: Show when a specific version was available
        // This is redundant with `nxv info <pkg> <version>` which shows more details
        components::print_history_deprecation_warning();

        // Use prefix search (like info command) to find best matching package
        let packages = backend.search_by_name_version(&args.package, Some(version.as_str()))?;

        if packages.is_empty() {
            println!("Version {} of {} not found.", version, args.package);
        } else {
            // Use the first (most recent) match
            let pkg = &packages[0];
            components::print_package_header_with_attr(&pkg.attribute_path, &pkg.version);
            components::print_availability_compact(pkg);
            components::print_security_warning(pkg);
            components::print_usage_compact(pkg);
        }
    } else if args.full {
        // Show full details for all versions
        let packages = backend.search_by_name(&args.package, true)?;

        if packages.is_empty() {
            println!("No history found for package '{}'", args.package);
            return Ok(());
        }

        println!("Version history for: {}", args.package);
        println!();

        match args.format {
            cli::OutputFormatArg::Json => {
                println!("{}", serde_json::to_string_pretty(&packages)?);
            }
            cli::OutputFormatArg::Plain => {
                println!(
                    "VERSION\tATTR_PATH\tFIRST_COMMIT\tFIRST_DATE\tLAST_COMMIT\tLAST_DATE\tDESCRIPTION\tLICENSE\tHOMEPAGE"
                );
                for pkg in packages {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        pkg.version,
                        pkg.attribute_path,
                        pkg.first_commit_short(),
                        pkg.first_commit_date.format("%Y-%m-%d"),
                        pkg.last_commit_short(),
                        pkg.last_commit_date.format("%Y-%m-%d"),
                        pkg.description.as_deref().unwrap_or("-"),
                        pkg.license.as_deref().unwrap_or("-"),
                        pkg.homepage.as_deref().unwrap_or("-"),
                    );
                }
            }
            cli::OutputFormatArg::Table => {
                let mut table = Table::new();
                table
                    .load_preset(if args.ascii { ASCII_FULL } else { UTF8_FULL })
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec![
                        "Version",
                        "Attr Path",
                        "First Commit",
                        "Last Commit",
                        "Description",
                    ]);

                for pkg in packages {
                    let desc = pkg.description.as_deref().unwrap_or("-");
                    let version_display = if pkg.is_insecure() {
                        format!("{} \u{26a0}", pkg.version)
                    } else {
                        pkg.version.clone()
                    };
                    let version_semantic = if pkg.is_insecure() {
                        Semantic::VersionInsecure
                    } else {
                        Semantic::Version
                    };
                    table.add_row(vec![
                        Cell::new(&version_display).themed(version_semantic),
                        Cell::new(&pkg.attribute_path).themed(Semantic::AttrPath),
                        Cell::new(format!(
                            "{} ({})",
                            pkg.first_commit_short(),
                            pkg.first_commit_date.format("%Y-%m-%d")
                        ))
                        .themed(Semantic::Commit),
                        Cell::new(format!(
                            "{} ({})",
                            pkg.last_commit_short(),
                            pkg.last_commit_date.format("%Y-%m-%d")
                        ))
                        .themed(Semantic::Commit),
                        Cell::new(desc).themed(Semantic::Description),
                    ]);
                }

                println!("{table}");
            }
        }
    } else {
        // Show summary (versions only)
        let history = backend.get_version_history(&args.package)?;

        if history.is_empty() {
            println!("No history found for package '{}'", args.package);
            return Ok(());
        }

        println!("Version history for: {}", args.package);
        println!();

        match args.format {
            cli::OutputFormatArg::Json => {
                let json_history: Vec<_> = history
                    .iter()
                    .map(|(v, first, last, is_insecure)| {
                        serde_json::json!({
                            "version": v,
                            "first_seen": first.to_rfc3339(),
                            "last_seen": last.to_rfc3339(),
                            "is_insecure": is_insecure,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json_history)?);
            }
            cli::OutputFormatArg::Plain => {
                println!("VERSION\tFIRST_SEEN\tLAST_SEEN\tINSECURE");
                for (version, first, last, is_insecure) in history {
                    println!(
                        "{}\t{}\t{}\t{}",
                        version,
                        first.format("%Y-%m-%d"),
                        last.format("%Y-%m-%d"),
                        if is_insecure { "yes" } else { "no" }
                    );
                }
            }
            cli::OutputFormatArg::Table => {
                let mut table = Table::new();
                table
                    .load_preset(if args.ascii { ASCII_FULL } else { UTF8_FULL })
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec!["Version", "First Seen", "Last Seen"]);

                for (version, first, last, is_insecure) in history {
                    let version_display = if is_insecure {
                        format!("{} \u{26a0}", version)
                    } else {
                        version
                    };
                    let version_semantic = if is_insecure {
                        Semantic::VersionInsecure
                    } else {
                        Semantic::Version
                    };
                    table.add_row(vec![
                        Cell::new(&version_display).themed(version_semantic),
                        Cell::new(first.format("%Y-%m-%d").to_string()).themed(Semantic::Date),
                        Cell::new(last.format("%Y-%m-%d").to_string()).themed(Semantic::Date),
                    ]);
                }

                println!("{table}");
            }
        }
    }

    Ok(())
}

/// Validate a date argument in YYYY-MM-DD format.
/// Returns the date string if valid, or an error message.
#[cfg(feature = "indexer")]
fn validate_date_format(date: &str, arg_name: &str) -> Result<()> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        anyhow::bail!("--{} must be in YYYY-MM-DD format, got: {}", arg_name, date);
    }
    let year = parts[0].parse::<u32>();
    let month = parts[1].parse::<u32>();
    let day = parts[2].parse::<u32>();
    match (year, month, day) {
        (Ok(y), Ok(m), Ok(d))
            if (1970..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) =>
        {
            Ok(())
        }
        _ => anyhow::bail!(
            "--{} must be a valid date in YYYY-MM-DD format, got: {}",
            arg_name,
            date
        ),
    }
}

/// Validate date range against MIN_INDEXABLE_DATE.
/// Returns error if --until is before MIN_INDEXABLE_DATE.
/// Warns if --since is before MIN_INDEXABLE_DATE.
#[cfg(feature = "indexer")]
fn validate_date_range(since: Option<&str>, until: Option<&str>) -> Result<()> {
    use crate::index::git::MIN_INDEXABLE_DATE;

    // Validate format first
    if let Some(since) = since {
        validate_date_format(since, "since")?;
    }
    if let Some(until) = until {
        validate_date_format(until, "until")?;
    }

    // Check that --until is not before MIN_INDEXABLE_DATE
    if let Some(until) = until
        && until < MIN_INDEXABLE_DATE
    {
        anyhow::bail!(
            "--until date {} is before the minimum indexable date {}.\n\
             nixpkgs commits before {} have a different structure that \
             doesn't work with modern Nix evaluation.",
            until,
            MIN_INDEXABLE_DATE,
            MIN_INDEXABLE_DATE
        );
    }

    // Warn if --since is before MIN_INDEXABLE_DATE (it will be clamped)
    if let Some(since) = since
        && since < MIN_INDEXABLE_DATE
    {
        eprintln!(
            "Note: --since {} is before the minimum indexable date {}. \
             Starting from {} instead.",
            since, MIN_INDEXABLE_DATE, MIN_INDEXABLE_DATE
        );
    }

    // Check that --since is not after --until
    if let (Some(since), Some(until)) = (since, until)
        && since > until
    {
        anyhow::bail!(
            "--since {} is after --until {}. No commits in this range.",
            since,
            until
        );
    }

    Ok(())
}

/// Run the indexer against a nixpkgs repository and update the local index database.
///
/// This performs either a full rebuild or an incremental index (depending on `args.full`),
/// registers a Ctrl+C handler for graceful shutdown, prints progress and a summary of results,
/// and always (even after interruption) builds and saves a bloom filter from the resulting
/// database state.
///
/// On success the function prints indexing metrics and returns normally; on failure it returns
/// an error describing what went wrong.
///
/// # Examples
///
/// ```rust,no_run
/// # use crate::cli::{Cli, IndexArgs};
/// # use crate::cmd_index;
/// // Build or parse CLI structures (example placeholders)
/// let cli: Cli = unimplemented!();
/// let args: IndexArgs = unimplemented!();
///
/// // Run the index command (no_run prevents execution in doctests)
/// let _ = cmd_index(&cli, &args);
/// ```
#[cfg(feature = "indexer")]
fn cmd_index(cli: &Cli, args: &cli::IndexArgs) -> Result<()> {
    use crate::db::Database;
    use crate::index::{Indexer, IndexerConfig, save_bloom_filter};
    use std::sync::atomic::Ordering;

    // Check for internal worker mode first
    if args.internal_worker {
        // Set memory threshold from CLI args
        crate::index::worker::worker_main::set_max_memory(args.max_memory);
        // Run worker subprocess loop (never returns)
        crate::index::worker::run_worker_main();
    }

    // Validate date arguments
    validate_date_range(args.since.as_deref(), args.until.as_deref())?;

    // Ensure data directory exists before opening database
    paths::ensure_data_dir()?;

    let nixpkgs_path = paths::expand_tilde(&args.nixpkgs_path);
    eprintln!("Indexing nixpkgs from {:?}", nixpkgs_path);
    eprintln!("Checkpoint interval: {} commits", args.checkpoint_interval);

    let config = IndexerConfig {
        checkpoint_interval: args.checkpoint_interval,
        show_progress: !cli.quiet,
        systems: args
            .systems
            .clone()
            .unwrap_or_else(|| IndexerConfig::default().systems),
        since: args.since.clone(),
        until: args.until.clone(),
        max_commits: args.max_commits,
        worker_count: args.workers,
        max_memory_mib: args.max_memory,
        verbose: args.verbose,
        gc_interval: args.gc_interval,
        gc_min_free_bytes: args.gc_min_free_gb * 1024 * 1024 * 1024,
    };

    let indexer = Indexer::new(config);

    // Set up Ctrl+C handler
    let shutdown_flag = indexer.shutdown_flag();
    ctrlc::set_handler(move || {
        eprintln!("\nReceived Ctrl+C, requesting graceful shutdown...");
        shutdown_flag.store(true, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    let result = if args.full {
        eprintln!("Performing full rebuild...");
        indexer.index_full(&nixpkgs_path, &cli.db_path)?
    } else {
        eprintln!("Performing incremental index...");
        indexer.index_incremental(&nixpkgs_path, &cli.db_path)?
    };

    // Print results
    eprintln!();
    eprintln!("Indexing complete!");
    eprintln!("  Commits processed: {}", result.commits_processed);
    eprintln!("  Total packages found: {}", result.packages_found);
    eprintln!("  Version ranges created: {}", result.ranges_created);
    eprintln!("  Unique package names: {}", result.unique_names);

    // Build and save bloom filter from current database state
    // We do this even after interruption since the DB is consistent via checkpoints
    {
        eprintln!();
        eprintln!("Building bloom filter...");
        let db = Database::open_readonly(&cli.db_path)?;
        let bloom_path = paths::get_bloom_path_for_db(&cli.db_path);
        save_bloom_filter(&db, &bloom_path)?;
        eprintln!("Bloom filter saved to {:?}", bloom_path);
    }

    if result.was_interrupted {
        eprintln!();
        eprintln!("Note: Indexing was interrupted. Run again to continue from checkpoint.");
    }

    Ok(())
}

/// Runs a backfill process to populate or repair package metadata in the local index.
///
/// This command executes a backfill over the repository at `args.nixpkgs_path`, updating fields
/// in the database at `cli.db_path` according to `args` (fields, limit, dry-run, and whether to
/// operate in historical mode). It prints progress and a summary of metrics to stderr, and installs
/// a Ctrl+C handler to request a graceful shutdown; if interrupted, the run stops cleanly and the
/// summary indicates that it was interrupted.
///
/// The command reports:
/// - whether it ran in historical or HEAD mode,
/// - packages checked,
/// - commits processed (only in historical mode),
/// - records updated,
/// - number of source_path fields filled,
/// - number of homepage fields filled.
///
/// Also prints a note advising to re-run if it was interrupted.
///
/// # Examples
///
/// ```no_run
/// // Typical invocation from main command dispatch:
/// // cmd_backfill(&cli, &cli.backfill_args)?;
/// ```
#[cfg(feature = "indexer")]
fn cmd_backfill(cli: &Cli, args: &cli::BackfillArgs) -> Result<()> {
    use crate::index::backfill::{BackfillConfig, create_shutdown_flag, run_backfill};
    use std::sync::atomic::Ordering;

    // Validate date arguments
    validate_date_range(args.since.as_deref(), args.until.as_deref())?;

    let nixpkgs_path = paths::expand_tilde(&args.nixpkgs_path);

    if args.history {
        eprintln!(
            "Backfilling metadata from {:?} (historical mode)",
            nixpkgs_path
        );
        eprintln!("  This will check out each package's original commit to extract metadata.");
        eprintln!("  Slower but can update old/removed packages.");
    } else {
        eprintln!("Backfilling metadata from {:?} (HEAD mode)", nixpkgs_path);
        eprintln!("  This extracts metadata from the current nixpkgs checkout only.");
        eprintln!("  Fast, but packages not in this checkout won't be updated.");
    }
    eprintln!();

    let config = BackfillConfig {
        fields: args
            .fields
            .as_ref()
            .map(|f| f.iter().map(|field| field.as_str().to_string()).collect())
            .unwrap_or_default(),
        limit: args.limit,
        dry_run: args.dry_run,
        use_history: args.history,
        since: args.since.clone(),
        until: args.until.clone(),
        max_commits: args.max_commits,
    };

    // Set up Ctrl+C handler
    let shutdown_flag = create_shutdown_flag();
    let flag_clone = shutdown_flag.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nReceived Ctrl+C, requesting graceful shutdown...");
        flag_clone.store(true, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    let result = run_backfill(&nixpkgs_path, &cli.db_path, config, shutdown_flag)?;

    eprintln!();
    if result.was_interrupted {
        eprintln!("Backfill interrupted!");
    } else {
        eprintln!("Backfill complete!");
    }
    eprintln!("  Packages checked: {}", result.packages_checked);
    if args.history {
        eprintln!("  Commits processed: {}", result.commits_processed);
    }
    eprintln!("  Records updated: {}", result.records_updated);
    eprintln!(
        "  source_path fields filled: {}",
        result.source_paths_filled
    );
    eprintln!("  homepage fields filled: {}", result.homepages_filled);
    eprintln!(
        "  known_vulnerabilities fields filled: {}",
        result.vulnerabilities_filled
    );

    // Show helpful tips based on results
    if result.was_interrupted {
        eprintln!();
        eprintln!("Note: Backfill was interrupted. Run again to continue.");
    } else if !args.history && result.records_updated == 0 && result.packages_checked > 0 {
        eprintln!();
        eprintln!("Tip: No records were updated. This can happen if:");
        eprintln!("  - All packages already have the requested metadata, or");
        eprintln!("  - The packages no longer exist in your nixpkgs checkout.");
        eprintln!();
        eprintln!("To update old/removed packages, use historical mode:");
        eprintln!(
            "  nxv backfill --nixpkgs-path {:?} --history",
            args.nixpkgs_path
        );
    }

    Ok(())
}

/// Resets the local nixpkgs git repository to a given reference, optionally fetching from origin first.
///
/// If `args.fetch` is true, the repository will be fetched from origin before performing a hard reset.
/// The repository is reset to `args.to` when provided, otherwise to `origin/nixpkgs-unstable`. Progress and the
/// resulting HEAD short hash are printed to stderr. Errors from repository operations are propagated.
///
/// # Examples
///
/// ```no_run
/// # use crate::cli::{ResetArgs, Cli};
/// // Construct `args` with the desired path and options.
/// let cli = Cli::parse(); // placeholder for context where Cli is available
/// let args = ResetArgs { nixpkgs_path: "/path/to/nixpkgs".into(), fetch: true, to: None };
/// cmd_reset(&cli, &args).unwrap();
/// ```
#[cfg(feature = "indexer")]
fn cmd_reset(_cli: &Cli, args: &cli::ResetArgs) -> Result<()> {
    use crate::index::git::NixpkgsRepo;

    let nixpkgs_path = paths::expand_tilde(&args.nixpkgs_path);
    eprintln!("Resetting nixpkgs repository at {:?}", nixpkgs_path);

    let repo = NixpkgsRepo::open(&nixpkgs_path)?;

    if args.fetch {
        eprintln!("Fetching from origin...");
        repo.fetch_origin()?;
        eprintln!("Fetch complete.");
    }

    let target = args.to.as_deref();
    let target_display = target.unwrap_or("origin/nixpkgs-unstable");
    eprintln!("Resetting to {}...", target_display);

    repo.reset_hard(target)?;

    eprintln!("Reset complete.");
    eprintln!("  Repository is now at: {}", target_display);

    // Show current HEAD
    if let Ok(head) = repo.head_commit() {
        eprintln!("  HEAD: {}", &head[..12.min(head.len())]);
    }

    Ok(())
}

/// Generates publishable index artifacts (compressed database, bloom filter, manifest).
///
/// Creates distribution-ready files in the specified output directory:
/// - `index.db.zst` - zstd-compressed SQLite database
/// - `bloom.bin` - Bloom filter for fast negative lookups
/// - `manifest.json` - Manifest with URLs and checksums
///
/// The `--url-prefix` option sets the base URL for manifest download URLs.
///
/// # Examples
///
/// ```no_run
/// // Generate artifacts with custom URL prefix:
/// // nxv publish --output ./publish --url-prefix https://example.com/releases
/// ```
#[cfg(feature = "indexer")]
fn cmd_publish(cli: &Cli, args: &cli::PublishArgs) -> Result<()> {
    use crate::index::publisher::publish_index;
    use crate::paths::expand_tilde;

    if !cli.quiet && args.url_prefix.is_none() {
        eprintln!("Warning: --url-prefix not set; manifest URLs will be local file names.");
    }

    let output = expand_tilde(&args.output);

    publish_index(
        &cli.db_path,
        &output,
        args.url_prefix.as_deref(),
        !cli.quiet,
        args.secret_key.as_deref(),
    )?;

    Ok(())
}

/// Generate a new minisign keypair for signing manifests.
#[cfg(feature = "indexer")]
fn cmd_keygen(cli: &Cli, args: &cli::KeygenArgs) -> Result<()> {
    use crate::index::publisher::generate_keypair;

    let secret_key = paths::expand_tilde(&args.secret_key);
    let public_key = paths::expand_tilde(&args.public_key);

    // generate_keypair handles force check atomically to avoid TOCTOU race
    let pk_base64 = generate_keypair(&secret_key, &public_key, &args.comment, args.force)?;

    if !cli.quiet {
        eprintln!("Generated keypair:");
        eprintln!("  Secret key: {}", secret_key.display());
        eprintln!("  Public key: {}", public_key.display());
        eprintln!();
        eprintln!("Public key (for embedding in manifest.rs):");
        eprintln!("  {}", pk_base64);
        eprintln!();
        eprintln!("Keep the secret key safe! You'll need it to sign manifests.");
    }

    Ok(())
}

/// Starts the HTTP server using the provided CLI configuration and serve arguments.
///
/// Builds a ServerConfig from `cli` and `args`, creates a Tokio runtime, and runs the HTTP server.
///
/// # Examples
///
/// ```no_run
/// // Construct `Cli` and `cli::ServeArgs` per your application, then start the server:
/// // cmd_serve(&cli, &args).unwrap();
/// ```
fn cmd_serve(cli: &Cli, args: &cli::ServeArgs) -> Result<()> {
    use crate::server::{ServerConfig, run_server};

    let config = ServerConfig {
        host: args.host.clone(),
        port: args.port,
        db_path: cli.db_path.clone(),
        cors: args.cors || args.cors_origins.is_some(),
        cors_origins: args.cors_origins.clone(),
        rate_limit: args.rate_limit,
        rate_limit_burst: args.rate_limit_burst,
    };

    // Create tokio runtime and run the server
    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

    rt.block_on(run_server(config))?;

    Ok(())
}

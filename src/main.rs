//! nxv - CLI tool for finding specific versions of Nix packages.

mod backend;
mod bloom;
mod cli;
mod client;
mod db;
mod error;
mod output;
mod paths;
mod remote;
mod search;

#[cfg(feature = "indexer")]
mod index;

#[cfg(feature = "server")]
mod server;

use anyhow::Result;
use clap::Parser;
use owo_colors::{OwoColorize, Stream::Stderr};

use cli::{Cli, Commands};

fn main() {
    let cli = Cli::parse();

    // Handle no-color flag
    if cli.no_color {
        // Disable colors globally - this affects if_supports_color() calls
        owo_colors::set_override(false);
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
        #[cfg(feature = "indexer")]
        Commands::Index(args) => cmd_index(&cli, args),
        #[cfg(feature = "server")]
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

/// Get the backend based on NXV_API_URL environment variable.
///
/// If NXV_API_URL is set, uses a remote API client.
/// Otherwise, uses the local database.
fn get_backend(cli: &Cli) -> Result<backend::Backend> {
    use backend::Backend;

    if let Ok(url) = std::env::var("NXV_API_URL") {
        let client = client::ApiClient::new(&url)?;
        Ok(Backend::Remote(client))
    } else {
        let db = db::Database::open_readonly(&cli.db_path)?;
        Ok(Backend::Local(db))
    }
}

/// Get backend with first-run experience (prompt to download index).
fn get_backend_with_prompt(cli: &Cli) -> Result<backend::Backend> {
    use backend::Backend;
    use std::io::{IsTerminal, Write};

    // If using remote API, no need for local database
    if let Ok(url) = std::env::var("NXV_API_URL") {
        let client = client::ApiClient::new(&url)?;
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
        eprintln!("[debug]   version: {:?}", args.version);
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
        let bloom_path = paths::get_bloom_path();
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
        version: args.version.clone(),
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
    } else if args.version.is_some() {
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
        ascii: args.ascii,
    };
    print_results(&result.data, format, options);

    // Show "more results" message after the table
    if result.has_more && !cli.quiet {
        eprintln!(
            "{} more results. Use --limit 0 for all.",
            result.total - result.data.len()
        );
    }

    Ok(())
}

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

    let status = perform_update(manifest_url, &cli.db_path, args.force, show_progress)?;

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

fn cmd_pkg_info(cli: &Cli, args: &cli::InfoArgs) -> Result<()> {
    use owo_colors::OwoColorize;

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
                println!();
            }
        }
        cli::OutputFormatArg::Table => {
            // For table format, show a detailed view for the first/most relevant result
            // and summarize if there are multiple attribute paths
            let pkg = &packages[0];

            println!(
                "{}: {} {}",
                "Package".bold(),
                pkg.name.cyan(),
                pkg.version.green()
            );
            println!();

            println!("{}", "Details".bold().underline());
            println!(
                "  {:<16} {}",
                "Attribute:".yellow(),
                pkg.attribute_path.cyan()
            );
            println!(
                "  {:<16} {}",
                "Description:",
                pkg.description.as_deref().unwrap_or("-")
            );
            println!(
                "  {:<16} {}",
                "Homepage:",
                pkg.homepage.as_deref().unwrap_or("-")
            );
            println!(
                "  {:<16} {}",
                "License:",
                pkg.license.as_deref().unwrap_or("-")
            );
            println!();

            println!("{}", "Availability".bold().underline());
            println!(
                "  {:<16} {} ({})",
                "First seen:".yellow(),
                pkg.first_commit_short(),
                pkg.first_commit_date.format("%Y-%m-%d")
            );
            println!(
                "  {:<16} {} ({})",
                "Last seen:".yellow(),
                pkg.last_commit_short(),
                pkg.last_commit_date.format("%Y-%m-%d")
            );
            println!();

            if let Some(ref maintainers) = pkg.maintainers {
                println!("{}", "Maintainers".bold().underline());
                // Parse JSON array and display
                if let Ok(list) = serde_json::from_str::<Vec<String>>(maintainers) {
                    for m in list {
                        println!("  • {}", m);
                    }
                } else {
                    println!("  {}", maintainers);
                }
                println!();
            }

            if let Some(ref platforms) = pkg.platforms {
                println!("{}", "Platforms".bold().underline());
                if let Ok(list) = serde_json::from_str::<Vec<String>>(platforms) {
                    // Detect current platform
                    let current_platform = format!(
                        "{}-{}",
                        std::env::consts::ARCH,
                        if std::env::consts::OS == "macos" {
                            "darwin"
                        } else {
                            std::env::consts::OS
                        }
                    );

                    // Helper to format platform with highlighting
                    let format_platform = |p: &str| -> String {
                        if p == current_platform {
                            format!("{}", p.green().bold())
                        } else {
                            p.to_string()
                        }
                    };

                    // Group by OS
                    let mut linux: Vec<&str> = Vec::new();
                    let mut darwin: Vec<&str> = Vec::new();
                    let mut other: Vec<&str> = Vec::new();

                    for p in &list {
                        if p.contains("linux") {
                            linux.push(p);
                        } else if p.contains("darwin") {
                            darwin.push(p);
                        } else {
                            other.push(p);
                        }
                    }

                    if !linux.is_empty() {
                        let formatted: Vec<_> = linux.iter().map(|p| format_platform(p)).collect();
                        println!("  Linux:  {}", formatted.join(", "));
                    }
                    if !darwin.is_empty() {
                        let formatted: Vec<_> = darwin.iter().map(|p| format_platform(p)).collect();
                        println!("  Darwin: {}", formatted.join(", "));
                    }
                    if !other.is_empty() {
                        let formatted: Vec<_> = other.iter().map(|p| format_platform(p)).collect();
                        println!("  Other:  {}", formatted.join(", "));
                    }
                } else {
                    println!("  {}", platforms);
                }
                println!();
            }

            println!("{}", "Usage".bold().underline());
            println!(
                "  nix shell nixpkgs/{}#{}",
                pkg.last_commit_short(),
                pkg.attribute_path
            );
            println!(
                "  nix run nixpkgs/{}#{}",
                pkg.last_commit_short(),
                pkg.attribute_path
            );

            // Show other attribute paths if there are multiple (deduplicated)
            if packages.len() > 1 {
                use std::collections::HashSet;
                let mut seen: HashSet<(&str, &str)> = HashSet::new();
                seen.insert((&pkg.attribute_path, &pkg.version));

                let others: Vec<_> = packages
                    .iter()
                    .skip(1)
                    .filter(|p| seen.insert((&p.attribute_path, &p.version)))
                    .collect();

                if !others.is_empty() {
                    println!();
                    println!("{}", "Other Attribute Paths".bold().underline());
                    for other_pkg in others {
                        println!(
                            "  • {} ({})",
                            other_pkg.attribute_path.cyan(),
                            other_pkg.version.green()
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

fn cmd_stats(cli: &Cli) -> Result<()> {
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
    let index_version = backend.get_meta("index_version")?;

    println!("Index Information");
    println!("=================");

    if is_remote {
        println!(
            "API endpoint: {}",
            std::env::var("NXV_API_URL").unwrap_or_default()
        );
    } else {
        println!("Database path: {:?}", cli.db_path);
    }

    if let Some(version) = index_version {
        println!("Index version: {}", version);
    }

    if let Some(commit) = last_commit {
        println!("Last indexed commit: {}", &commit[..7.min(commit.len())]);
    }

    println!();
    println!("Statistics");
    println!("----------");
    println!("Total version ranges: {}", stats.total_ranges);
    println!("Unique package names: {}", stats.unique_names);
    println!("Unique versions: {}", stats.unique_versions);

    if let Some(oldest) = stats.oldest_commit_date {
        println!("Oldest commit: {}", oldest.format("%Y-%m-%d"));
    }

    if let Some(newest) = stats.newest_commit_date {
        println!("Newest commit: {}", newest.format("%Y-%m-%d"));
    }

    // Local-only info: file sizes
    if !is_remote {
        if cli.db_path.exists()
            && let Ok(metadata) = std::fs::metadata(&cli.db_path)
        {
            let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
            println!("Database size: {:.2} MB", size_mb);
        }

        // Bloom filter status
        let bloom_path = paths::get_bloom_path();
        if bloom_path.exists()
            && let Ok(metadata) = std::fs::metadata(&bloom_path)
        {
            let size_kb = metadata.len() as f64 / 1024.0;
            println!("Bloom filter: present ({:.2} KB)", size_kb);
        } else if !bloom_path.exists() {
            println!("Bloom filter: not found");
        }
    }

    Ok(())
}

fn cmd_history(cli: &Cli, args: &cli::HistoryArgs) -> Result<()> {
    // Get backend (local DB or remote API)
    let backend = get_backend_with_prompt(cli)?;

    if let Some(ref version) = args.version {
        // Show when a specific version was available
        let first = backend.get_first_occurrence(&args.package, version)?;
        let last = backend.get_last_occurrence(&args.package, version)?;

        match (first, last) {
            (Some(first_pkg), Some(last_pkg)) => {
                println!("Package: {} {}", args.package, version);
                println!();
                println!(
                    "First appeared: {} ({})",
                    first_pkg.first_commit_short(),
                    first_pkg.first_commit_date.format("%Y-%m-%d")
                );
                println!(
                    "Last seen: {} ({})",
                    last_pkg.last_commit_short(),
                    last_pkg.last_commit_date.format("%Y-%m-%d")
                );
                println!();
                println!("To use this version:");
                println!(
                    "  nix run nixpkgs/{}#{}",
                    last_pkg.last_commit_short(),
                    last_pkg.attribute_path
                );
            }
            _ => {
                println!("Version {} of {} not found.", version, args.package);
            }
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
                use comfy_table::{
                    Cell, Color, ContentArrangement, Table, presets::ASCII_FULL, presets::UTF8_FULL,
                };

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
                    table.add_row(vec![
                        Cell::new(&pkg.version).fg(Color::Green),
                        Cell::new(&pkg.attribute_path).fg(Color::Cyan),
                        Cell::new(format!(
                            "{} ({})",
                            pkg.first_commit_short(),
                            pkg.first_commit_date.format("%Y-%m-%d")
                        ))
                        .fg(Color::Yellow),
                        Cell::new(format!(
                            "{} ({})",
                            pkg.last_commit_short(),
                            pkg.last_commit_date.format("%Y-%m-%d")
                        ))
                        .fg(Color::Yellow),
                        Cell::new(desc),
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
                    .map(|(v, first, last)| {
                        serde_json::json!({
                            "version": v,
                            "first_seen": first.to_rfc3339(),
                            "last_seen": last.to_rfc3339(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json_history)?);
            }
            cli::OutputFormatArg::Plain => {
                println!("VERSION\tFIRST_SEEN\tLAST_SEEN");
                for (version, first, last) in history {
                    println!(
                        "{}\t{}\t{}",
                        version,
                        first.format("%Y-%m-%d"),
                        last.format("%Y-%m-%d")
                    );
                }
            }
            cli::OutputFormatArg::Table => {
                use comfy_table::{
                    Cell, Color, ContentArrangement, Table, presets::ASCII_FULL, presets::UTF8_FULL,
                };

                let mut table = Table::new();
                table
                    .load_preset(if args.ascii { ASCII_FULL } else { UTF8_FULL })
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec!["Version", "First Seen", "Last Seen"]);

                for (version, first, last) in history {
                    table.add_row(vec![
                        Cell::new(&version).fg(Color::Green),
                        Cell::new(first.format("%Y-%m-%d").to_string()).fg(Color::White),
                        Cell::new(last.format("%Y-%m-%d").to_string()).fg(Color::White),
                    ]);
                }

                println!("{table}");
            }
        }
    }

    Ok(())
}

#[cfg(feature = "indexer")]
fn cmd_index(cli: &Cli, args: &cli::IndexArgs) -> Result<()> {
    use crate::db::Database;
    use crate::index::{Indexer, IndexerConfig, save_bloom_filter};
    use std::sync::atomic::Ordering;

    eprintln!("Indexing nixpkgs from {:?}", args.nixpkgs_path);
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
        indexer.index_full(&args.nixpkgs_path, &cli.db_path)?
    } else {
        eprintln!("Performing incremental index...");
        indexer.index_incremental(&args.nixpkgs_path, &cli.db_path)?
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
        save_bloom_filter(&db)?;
        eprintln!("Bloom filter saved to {:?}", paths::get_bloom_path());
    }

    if result.was_interrupted {
        eprintln!();
        eprintln!("Note: Indexing was interrupted. Run again to continue from checkpoint.");
    }

    Ok(())
}

#[cfg(feature = "server")]
fn cmd_serve(cli: &Cli, args: &cli::ServeArgs) -> Result<()> {
    use crate::server::{ServerConfig, run_server};

    let config = ServerConfig {
        host: args.host.clone(),
        port: args.port,
        db_path: cli.db_path.clone(),
        cors: args.cors || args.cors_origins.is_some(),
        cors_origins: args.cors_origins.clone(),
    };

    // Create tokio runtime and run the server
    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");

    rt.block_on(run_server(config))?;

    Ok(())
}

//! nxv - CLI tool for finding specific versions of Nix packages.

mod bloom;
mod cli;
mod db;
mod error;
mod output;
mod paths;
mod remote;

#[cfg(feature = "indexer")]
mod index;

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
        Commands::Info => cmd_info(&cli),
        Commands::History(args) => cmd_history(&cli, args),
        Commands::Completions(args) => {
            args.generate();
            Ok(())
        }
        #[cfg(feature = "indexer")]
        Commands::Index(args) => cmd_index(&cli, args),
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

fn cmd_search(cli: &Cli, args: &cli::SearchArgs) -> Result<()> {
    use crate::bloom::PackageBloomFilter;
    use crate::cli::Verbosity;
    use crate::db::Database;
    use crate::db::queries;
    use crate::output::{OutputFormat, TableOptions, print_results};
    use std::io::{IsTerminal, Write};

    let verbosity = cli.verbosity();

    // Debug: show search parameters
    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Search parameters:");
        eprintln!("[debug]   package: {:?}", args.package);
        eprintln!("[debug]   version: {:?}", args.version);
        eprintln!("[debug]   exact: {}", args.exact);
        eprintln!("[debug]   desc: {}", args.desc);
        eprintln!("[debug]   license: {:?}", args.license);
        eprintln!("[debug]   db_path: {:?}", cli.db_path);
    }

    // Open database - handle first-run experience
    let db = match Database::open_readonly(&cli.db_path) {
        Ok(db) => db,
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
                    let update_args = cli::UpdateArgs { force: false, manifest_url: None };
                    cmd_update(cli, &update_args)?;

                    // Try to open the database again
                    Database::open_readonly(&cli.db_path)?
                } else {
                    return Err(error::NxvError::NoIndex.into());
                }
            } else {
                return Err(error::NxvError::NoIndex.into());
            }
        }
        Err(e) => return Err(e.into()),
    };

    // For exact name searches, check bloom filter first for instant "not found"
    // This only applies to exact name searches (not prefix, attribute path, or description)
    if args.exact && !args.desc && !args.package.contains('.') {
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
        // If bloom filter says maybe present or couldn't load, continue with DB query
    }

    // Perform search
    let query_type = if args.desc {
        "FTS description search"
    } else if args.package.contains('.') {
        "attribute path search"
    } else if args.version.is_some() {
        "name+version search"
    } else if args.exact {
        "exact name search"
    } else {
        "prefix name search"
    };

    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Query type: {}", query_type);
    } else if verbosity >= Verbosity::Info {
        eprintln!("Searching for '{}'...", args.package);
    }

    let results = if args.desc {
        // FTS search on description
        queries::search_by_description(db.connection(), &args.package)?
    } else if args.package.contains('.') {
        // Looks like an attribute path
        queries::search_by_attr(db.connection(), &args.package)?
    } else if let Some(ref version) = args.version {
        // Search by name and version
        queries::search_by_name_version(db.connection(), &args.package, version)?
    } else {
        // Search by name
        queries::search_by_name(db.connection(), &args.package, args.exact)?
    };

    if verbosity >= Verbosity::Debug {
        eprintln!("[debug] Raw results: {} rows", results.len());
    }

    // Filter by license if specified
    let results = if let Some(ref license) = args.license {
        results
            .into_iter()
            .filter(|p| {
                p.license
                    .as_ref()
                    .is_some_and(|l| l.to_lowercase().contains(&license.to_lowercase()))
            })
            .collect()
    } else {
        results
    };

    // Sort results
    let mut results = results;
    match args.sort {
        cli::SortOrder::Date => {
            results.sort_by(|a, b| b.last_commit_date.cmp(&a.last_commit_date));
        }
        cli::SortOrder::Version => {
            results.sort_by(|a, b| {
                // Semver-aware version comparison
                // Try to parse as semver, fall back to string comparison
                match (semver::Version::parse(&a.version), semver::Version::parse(&b.version)) {
                    (Ok(va), Ok(vb)) => va.cmp(&vb),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less, // Valid semver sorts before invalid
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.version.cmp(&b.version), // Fall back to string comparison
                }
            });
        }
        cli::SortOrder::Name => {
            results.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    if args.reverse {
        results.reverse();
    }

    // Apply limit
    let results: Vec<_> = if args.limit > 0 {
        let total = results.len();
        let limited: Vec<_> = results.into_iter().take(args.limit).collect();
        if total > args.limit && !cli.quiet {
            eprintln!(
                "{} more results. Use --limit 0 for all.",
                total - args.limit
            );
        }
        limited
    } else {
        results
    };

    if results.is_empty() {
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
    print_results(&results, format, options);

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

fn cmd_info(cli: &Cli) -> Result<()> {
    use crate::db::Database;
    use crate::db::queries::get_stats;

    let db = match Database::open_readonly(&cli.db_path) {
        Ok(db) => db,
        Err(error::NxvError::NoIndex) => {
            println!("No index found at {:?}", cli.db_path);
            println!("Run 'nxv update' to download the package index.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let stats = get_stats(db.connection())?;

    // Get meta info
    let last_commit = db.get_meta("last_indexed_commit")?;
    let index_version = db.get_meta("index_version")?;

    println!("Index Information");
    println!("=================");
    println!("Database path: {:?}", cli.db_path);

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

    // File size
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

    Ok(())
}

fn cmd_history(cli: &Cli, args: &cli::HistoryArgs) -> Result<()> {
    use crate::db::Database;
    use crate::db::queries;
    use std::io::{IsTerminal, Write};

    // Open database - handle first-run experience
    let db = match Database::open_readonly(&cli.db_path) {
        Ok(db) => db,
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
                    let update_args = cli::UpdateArgs { force: false, manifest_url: None };
                    cmd_update(cli, &update_args)?;

                    // Try to open the database again
                    Database::open_readonly(&cli.db_path)?
                } else {
                    return Err(error::NxvError::NoIndex.into());
                }
            } else {
                return Err(error::NxvError::NoIndex.into());
            }
        }
        Err(e) => return Err(e.into()),
    };

    if let Some(ref version) = args.version {
        // Show when a specific version was available
        let first = queries::get_first_occurrence(db.connection(), &args.package, version)?;
        let last = queries::get_last_occurrence(db.connection(), &args.package, version)?;

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
    } else {
        // Show all versions
        let history = queries::get_version_history(db.connection(), &args.package)?;

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
                use comfy_table::{Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};

                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec!["Version", "First Seen", "Last Seen"]);

                for (version, first, last) in history {
                    table.add_row(vec![
                        Cell::new(&version).fg(Color::Green),
                        Cell::new(first.format("%Y-%m-%d").to_string()),
                        Cell::new(last.format("%Y-%m-%d").to_string()),
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

    // Build and save bloom filter unless interrupted
    if !result.was_interrupted && result.ranges_created > 0 {
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

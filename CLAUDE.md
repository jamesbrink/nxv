# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

nxv (Nix Versions) is a Rust CLI tool for quickly finding specific versions of Nix packages across nixpkgs history.

## Development Environment

This project uses Nix flakes with rust-overlay for reproducible development:

```bash
nix develop          # Enter devshell
direnv allow         # Or use direnv
```

## Build Commands

```bash
cargo build                      # Debug build
cargo build --release            # Release build
cargo build --features indexer   # Build with indexer feature
cargo run -- <args>              # Run with arguments
cargo test                       # Run unit tests (40 tests)
cargo test --features indexer    # Run all tests including indexer (57 tests)
cargo test --test integration    # Run integration tests (27 tests)
cargo clippy -- -D warnings      # Lint with errors on warnings
cargo fmt                        # Format code
```

## CLI Commands

```bash
# User commands (require downloaded index)
nxv search <package>           # Search for packages by name
nxv search <pkg> --version X   # Filter by version prefix
nxv search <pkg> --exact       # Exact name match only
nxv search --desc "text"       # Search in descriptions (FTS)
nxv search <pkg> --format json # Output as JSON
nxv history <package>          # Show version history
nxv history <pkg> <version>    # Show specific version availability
nxv info                       # Show index statistics
nxv update                     # Download/update the index
nxv update --force             # Force full re-download
nxv completions <shell>        # Generate shell completions

# Indexer commands (feature-gated, for developers)
nxv index --nixpkgs-path <path>        # Incremental index
nxv index --nixpkgs-path <path> --full # Full rebuild
```

## Project Structure

```
src/
├── main.rs              # Entry point, command dispatch
├── cli.rs               # Clap command definitions
├── error.rs             # Error types (thiserror)
├── paths.rs             # XDG paths, default locations
├── bloom.rs             # Bloom filter for fast negative lookups
├── db/
│   ├── mod.rs           # Database connection, schema
│   ├── queries.rs       # Search, stats queries
│   └── import.rs        # Delta pack import (stub)
├── index/               # (feature = "indexer")
│   ├── mod.rs           # Indexing coordinator
│   ├── git.rs           # Git repository traversal
│   ├── extractor.rs     # Nix package extraction
│   └── publisher.rs     # Generate artifacts (stub)
├── remote/
│   ├── mod.rs           # Remote index operations
│   ├── download.rs      # HTTP download with progress
│   ├── manifest.rs      # Manifest parsing
│   └── update.rs        # Update logic
└── output/
    ├── mod.rs           # Output format dispatch
    ├── table.rs         # Colored table output
    ├── json.rs          # JSON output
    └── plain.rs         # Plain text output
tests/
└── integration.rs       # CLI integration tests
```

## Dependency Management

**Always use current versions of dependencies.** Before adding or updating dependencies:

```bash
cargo search <crate>           # Find latest version of a crate
cargo search <crate> --limit 5 # Show multiple results
```

When adding dependencies to Cargo.toml:
- Use `cargo search` to verify the latest version
- Prefer crates with active maintenance and good documentation
- Check crates.io for version info when needed

## Testing

- Unit tests are in each module's `mod tests` section
- Integration tests are in `tests/integration.rs`
- Use `cargo test --features indexer` to run all tests
- Tests create temporary databases using `tempfile` crate

## Key Features

- **Bloom filter**: Fast O(1) negative lookups for exact name searches
- **FTS5**: Full-text search on package descriptions
- **Delta updates**: Incremental index updates (infrastructure ready)
- **Checkpointing**: Resumable indexing with Ctrl+C handling
- **Multiple output formats**: Table, JSON, plain text

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

nxv (Nix Versions) is a Rust CLI tool for quickly finding specific versions of Nix packages across nixpkgs history. It uses a pre-built SQLite index with bloom filters for fast lookups.

## Development Environment

This project uses Nix flakes with crane for reproducible builds:

```bash
nix develop          # Enter devshell with Rust toolchain
direnv allow         # Or use direnv for automatic shell activation
```

## Build Commands

```bash
cargo build                      # Debug build
cargo build --release            # Release build
cargo build --features indexer   # Build with indexer feature (requires libgit2)
cargo run -- <args>              # Run with arguments
cargo test                       # Run tests (~56 tests)
cargo test --features indexer    # Run all tests including indexer (~82 tests)
cargo test <test_name>           # Run a single test by name
cargo test db::                  # Run tests in a specific module
cargo clippy -- -D warnings      # Lint with errors on warnings
cargo fmt                        # Format code
nix flake check                  # Run all Nix checks (build, clippy, fmt, tests)
```

## Architecture

### Data Flow

1. **Index Download** (`remote/`): User runs `nxv update` → downloads compressed SQLite DB + bloom filter from remote manifest
2. **Search** (`db/queries.rs`): Queries go through bloom filter first (fast negative lookup), then SQLite with FTS5
3. **Output** (`output/`): Results formatted as table (default), JSON, or plain text

### Indexer (feature-gated)

The `indexer` feature enables building indexes from a local nixpkgs clone:
- `git.rs`: Walks nixpkgs git history (commits from 2017+)
- `extractor.rs`: Runs `nix eval` to extract package metadata per commit
- `mod.rs`: Coordinates indexing with checkpointing for Ctrl+C resilience

### Database Schema (`db/mod.rs`)

- `package_versions`: Main table with version ranges (first/last commit dates)
- `package_versions_fts`: FTS5 virtual table for description search
- `meta`: Key-value store for index metadata (last_indexed_commit, schema_version)

### Key Design Decisions

- **Version ranges**: Instead of storing every commit where a package exists, stores (first_commit, last_commit) ranges to minimize DB size
- **Bloom filter**: Serialized to separate file, loaded at search time for instant "not found" responses
- **Feature gates**: Indexer code (git2, ctrlc) only compiled with `--features indexer` to keep user binary small

## Dependency Management

**Always use current versions.** Before adding dependencies:

```bash
cargo search <crate>           # Find latest version
```

## Testing

- Unit tests are in each module's `mod tests` section
- Integration tests in `tests/integration.rs` use `assert_cmd` to test CLI behavior
- Tests create temporary databases using `tempfile`
- Some indexer tests require `nix` to be installed (marked `#[ignore]`)

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

nxv (Nix Versions) is a Rust CLI tool for quickly finding specific versions of Nix packages across nixpkgs history. It uses a pre-built SQLite index with bloom filters for fast lookups. Also provides an HTTP API server with web frontend.

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
cargo build --features indexer   # Build with indexer feature (requires libgit2 + nix C libs)
cargo run -- <args>              # Run with arguments
cargo test                       # Run tests (~59 tests)
cargo test --features indexer    # Run all tests including indexer (~64 tests)
cargo test <test_name>           # Run a single test by name
cargo test db::                  # Run tests in a specific module
cargo clippy -- -D warnings      # Lint with errors on warnings
cargo fmt                        # Format code
nix flake check                  # Run all Nix checks (build, clippy, fmt, tests)
```

## Nix Flake Outputs

```bash
nix build                        # Build nxv (user binary)
nix build .#nxv-indexer          # Build with indexer feature
nix build .#nxv-static           # Static musl build (Linux only)
nix build .#nxv-docker           # Docker image (Linux only, use --system x86_64-linux on macOS)
nix run                          # Run nxv directly
nix run .#nxv-indexer            # Run with indexer feature
```

## Architecture

### Data Flow

1. **Index Download** (`remote/`): User runs `nxv update` → downloads compressed SQLite DB + bloom filter from remote manifest
2. **Search** (`db/queries.rs`): Queries go through bloom filter first (fast negative lookup), then SQLite with FTS5
3. **Output** (`output/`): Results formatted as table (default), JSON, or plain text

### Backend Abstraction (`backend.rs`)

The `Backend` enum provides a unified interface for local database or remote API access:
- `Backend::Local(Database)` - Direct SQLite queries
- `Backend::Remote(ApiClient)` - HTTP requests to remote nxv server (via `NXV_API_URL`)

### API Server (`server/`)

The `nxv serve` command runs an HTTP API server with:
- REST API at `/api/v1/*` (search, package info, version history, stats)
- Web frontend at `/` (embedded HTML/JS from `frontend/index.html`)
- OpenAPI documentation at `/docs`
- Configurable CORS support
- Rate limiting and concurrency control via semaphore

### Indexer (feature-gated)

The `indexer` feature enables building indexes from a local nixpkgs clone:
- `git.rs`: Walks nixpkgs git history (commits from 2017+, controlled by `MIN_INDEXABLE_DATE`)
- `extractor.rs`: Extracts package metadata (version, license, homepage, maintainers, platforms)
- `nix_ffi.rs`: Persistent Nix evaluator using FFI bindings (worker thread to prevent stack overflow)
- `worker/`: Worker pool for parallel Nix evaluation (IPC-based subprocess architecture)
- `mod.rs`: Coordinates indexing with checkpointing for Ctrl+C resilience
- `backfill.rs`: Updates missing metadata (source_path, homepage) for existing records
  - HEAD mode: Fast extraction from current nixpkgs (may miss renamed/removed packages)
  - Historical mode (`--history`): Traverses git to original commits for accuracy
- `gc.rs`: Nix store garbage collection to manage disk space during indexing
- `publisher.rs`: Generates compressed index files, delta packs, and signed manifest for distribution

### Database Schema (`db/mod.rs`)

- `package_versions`: Main table with version ranges (first/last commit dates), unique constraint on (attribute_path, version, first_commit_hash)
- `package_versions_fts`: FTS5 virtual table for description search (auto-synced via triggers)
- `meta`: Key-value store for index metadata (last_indexed_commit, schema_version)
- `checkpoint_open_ranges`: Indexer checkpoint table for persistence

Schema version is `SCHEMA_VERSION = 7`. Database uses WAL mode with 5-second busy timeout.

### Key Design Decisions

- **Version ranges**: Instead of storing every commit where a package exists, stores (first_commit, last_commit) ranges to minimize DB size
- **Bloom filter**: Serialized to separate file, loaded at search time for instant "not found" responses (1% false positive rate)
- **Feature gates**: Indexer code (git2, ctrlc, nix-bindings) only compiled with `--features indexer` to keep user binary small (~7MB vs ~15MB)
- **Store path cutoff**: Only extracted for commits >= 2020-01-01 (cache.nixos.org availability)

## Environment Variables

| Variable | Description |
|----------|-------------|
| `NXV_DB_PATH` | Path to index database |
| `NXV_API_URL` | Remote API URL (CLI uses remote instead of local DB) |
| `NXV_MANIFEST_URL` | Custom manifest URL for index downloads |
| `NXV_PUBLIC_KEY` | Custom public key for manifest verification |
| `NXV_SECRET_KEY` | Secret key for manifest signing |
| `NXV_SKIP_VERIFY` | Skip manifest signature verification |
| `NXV_API_TIMEOUT` | API request timeout in seconds (default: 30) |
| `NO_COLOR` | Disable colored output |

## Dependency Management

**Always use current versions.** Before adding dependencies:

```bash
cargo search <crate>           # Find latest version
```

## Data Paths

The database and bloom filter are stored in platform-specific data directories:
- **macOS**: `~/Library/Application Support/nxv/`
- **Linux**: `~/.local/share/nxv/`

Files:
- `index.db` - SQLite database with package versions
- `bloom.bin` - Bloom filter for fast negative lookups

## Testing

- Unit tests are in each module's `mod tests` section
- Integration tests in `tests/integration.rs` use `assert_cmd` to test CLI behavior
- Tests create temporary databases using `tempfile`
- Some indexer tests require `nix` to be installed (marked `#[ignore]`)
- Benchmarks in `benches/` for search, bloom filter, FFI evaluation

## NixOS Module

A NixOS module is provided for running nxv as a systemd service:

```nix
{
  imports = [ inputs.nxv.nixosModules.default ];
  services.nxv = {
    enable = true;
    port = 8080;
    # indexPath = "/path/to/index.db";  # Optional custom path
  };
}
```

## Releasing

**IMPORTANT: Always run `/release-notes` before creating a release.** This runs all pre-flight checks (fmt, clippy, tests, nix flake check, clean git status) and generates release notes.

## CI/CD & Index Publishing

### GitHub Actions Workflows

- `ci.yml`: Runs on PRs and main - tests (cargo + nix), clippy, fmt, builds Docker latest on main
- `release.yml`: Triggered by `v*` tags - builds static binaries, publishes to crates.io, pushes versioned Docker images
- `publish-index.yml`: Weekly scheduled or manual - builds the package index and uploads to `index-latest` release

### Publishing the Index

The default manifest URL is `https://github.com/jamesbrink/nxv/releases/download/index-latest/manifest.json`.

To publish manually with signing:

```bash
nxv publish --url-prefix "https://github.com/jamesbrink/nxv/releases/download/index-latest" --secret-key keys/nxv.key
```

Or trigger the workflow:

```bash
gh workflow run publish-index.yml
```

### Required Secrets

- `CACHIX_AUTH_TOKEN`: Nix binary cache
- `CARGO_REGISTRY_TOKEN`: crates.io publishing
- `NXV_SIGNING_KEY`: Manifest signing (minisign secret key)

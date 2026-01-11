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
cargo test --features indexer    # Run all tests including indexer (~67 tests)
cargo test <test_name>           # Run a single test by name
cargo test db::                  # Run tests in a specific module
cargo clippy -- -D warnings      # Lint with errors on warnings
cargo fmt                        # Format code
nix flake check                  # Run all Nix checks (build, clippy, fmt, tests)
```

## Website Development

The documentation website uses VitePress + Tailwind CSS 4.1 + Bun:

```bash
nix develop .#website            # Enter website devshell (Bun + bun2nix)
cd website && bun install        # Install dependencies
bun run dev                      # Start dev server (http://localhost:5173/nxv/)
bun run build                    # Production build
bun run preview                  # Preview production build
bun run fmt                      # Format with Prettier
bun run fmt:check                # Check formatting (CI)
```

Website structure:
- `website/.vitepress/config.ts` - VitePress configuration
- `website/.vitepress/theme/` - Custom theme with Tailwind
- `website/*.md` - Content pages (guide, API, advanced)
- `website/public/` - Static assets (favicon, demo.gif)

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
- Request correlation IDs (`X-Request-ID` header) for distributed tracing

### Indexer (feature-gated)

The `indexer` feature enables building indexes from a local nixpkgs clone:

- `git.rs`: Walks nixpkgs git history (commits from 2017+, controlled by `MIN_INDEXABLE_DATE`)
  - `get_file_diff()`: Extracts diff content for specific files (used for all-packages.nix parsing)
- `extractor.rs`: Extracts package metadata (version, license, homepage, maintainers, platforms)
  - `PackageInfo.version_source`: Tracks how version was extracted (direct/unwrapped/passthru/name)
- `nix/extract.nix`: Nix expression with version extraction fallback chain
- `nix_ffi.rs`: Persistent Nix evaluator using FFI bindings (worker thread to prevent stack overflow)
- `worker/`: Worker pool for parallel Nix evaluation (IPC-based subprocess architecture)
- `mod.rs`: Coordinates indexing with checkpointing for Ctrl+C resilience
  - Handles `INFRASTRUCTURE_FILES` (all-packages.nix, aliases.nix) via diff parsing
  - `extract_attrs_from_diff()`: Parses git diffs to find affected attribute names
- `backfill.rs`: Updates missing metadata (source_path, homepage) for existing records
  - HEAD mode: Fast extraction from current nixpkgs (may miss renamed/removed packages)
  - Historical mode (`--history`): Traverses git to original commits for accuracy
- `gc.rs`: Nix store garbage collection to manage disk space during indexing
- `publisher.rs`: Generates compressed index file, bloom filter, and signed manifest for distribution

#### Version Extraction Fallback Chain

The indexer tries multiple sources to extract package versions (in order):

1. **direct**: `pkg.version` - Standard attribute
2. **unwrapped**: `pkg.unwrapped.version` - For wrapper packages (e.g., neovim)
3. **passthru**: `pkg.passthru.unwrapped.version` - For packages with passthru metadata
4. **name**: Extracted from `pkg.name` or `pkg.pname` using regex patterns (e.g., "hello-2.12" → "2.12")

The `version_source` field in the database tracks which method was used, enabling debugging without re-indexing.

#### all-packages.nix Handling

Files in `INFRASTRUCTURE_FILES` (all-packages.nix, aliases.nix) are handled specially:

1. Instead of extracting all ~18,000 packages on every change, parse the git diff
2. Extract affected attribute names from changed lines (assignment patterns, inherit statements)
3. Only extract the specific packages that changed (~7 packages average)
4. Fall back to full extraction for large diffs (>100 lines) or unparseable changes

### Database Schema (`db/mod.rs`)

- `package_versions`: Main table with one row per (attribute_path, version), unique constraint on (attribute_path, version)
  - `version_source`: How version was extracted (direct/unwrapped/passthru/name/NULL)
  - Uses UPSERT to update first_commit (earliest) and last_commit (latest) bounds
- `package_versions_fts`: FTS5 virtual table for description search (auto-synced via triggers)
- `meta`: Key-value store for index metadata (last_indexed_commit, schema_version)

Schema version is `SCHEMA_VERSION = 4`. Database uses WAL mode with 5-second busy timeout.

Schema migrations:
- v1 → v2: Added `source_path` column
- v2 → v3: Added `known_vulnerabilities` column
- v3 → v4: Added `version_source` column

### Key Design Decisions

- **One row per version**: Each (attribute_path, version) pair has exactly one row. UPSERT during indexing updates first_commit (earliest seen) and last_commit (latest seen) bounds.
- **Bloom filter**: Serialized to separate file, loaded at search time for instant "not found" responses (1% false positive rate)
- **Feature gates**: Indexer code (git2, ctrlc, nix-bindings) only compiled with `--features indexer` to keep user binary small (~7MB vs ~15MB)
- **Store path cutoff**: Only extracted for commits >= 2020-01-01 (cache.nixos.org availability)

## Environment Variables

| Variable           | Description                                      |
| ------------------ | ------------------------------------------------ |
| `NXV_DB_PATH`      | Path to index database                           |
| `NXV_API_URL`      | Remote API URL (CLI uses remote instead of local DB) |
| `NXV_MANIFEST_URL` | Custom manifest URL for index downloads          |
| `NXV_PUBLIC_KEY`   | Custom public key for manifest verification      |
| `NXV_SECRET_KEY`   | Secret key for manifest signing                  |
| `NXV_SKIP_VERIFY`  | Skip manifest signature verification             |
| `NXV_API_TIMEOUT`  | API request timeout in seconds (default: 30)     |
| `NO_COLOR`         | Disable colored output                           |
| `NXV_LOG`          | Log filter (overrides RUST_LOG)                  |
| `NXV_LOG_LEVEL`    | Log level: error, warn, info, debug, trace       |
| `NXV_LOG_FORMAT`   | Log format: pretty, compact, json                |
| `NXV_LOG_FILE`     | Path to log file (in addition to stderr)         |
| `NXV_LOG_ROTATION` | Log file rotation: hourly, daily, never          |
| `RUST_LOG`         | Standard Rust log filter (fallback for NXV_LOG)  |

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

- `index.db` - SQLite database with package versions (one row per attribute_path/version pair)
- `bloom.bin` - Bloom filter for fast negative lookups

Published artifacts (generated by `nxv publish`):

- `index.db.zst` - Compressed database (~28MB)
- `bloom.bin` - Bloom filter (96 KB)
- `manifest.json` - Index metadata with signature

## Testing

- Unit tests are in each module's `mod tests` section
- Integration tests in `tests/integration.rs` use `assert_cmd` to test CLI behavior
- Tests create temporary databases using `tempfile`
- Some indexer tests require `nix` to be installed (marked `#[ignore]`)
- Benchmarks in `benches/` for search, bloom filter, FFI evaluation

### Regression Testing

- `tests/fixtures/regression_packages.json`: 50+ known-good packages with expected version patterns
- Run regression test: `NIXPKGS_PATH=/path/to/nixpkgs cargo test --features indexer test_regression_fixture -- --ignored`

### QA Scripts

- `scripts/pre_reindex_qa.sh`: Pre-reindex quality gate (fmt, clippy, tests, nix syntax)
- `scripts/post_reindex_validation.sh`: Post-reindex validation (database stats, NixHub comparison)
- `scripts/validate_against_nixhub.py`: Compare nxv data against NixHub API
  - `--verify-commits`: Check commit hashes exist in nixpkgs
  - `--verify-versions`: Verify package version at recorded commit

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

**Use `/release` to prepare and execute a release.** This skill:

1. Runs pre-flight checks (fmt, clippy, tests, nix flake check, clean git status)
2. Generates release notes from git history
3. Shows a complete summary of what will happen
4. Asks for explicit confirmation with the version number
5. Bumps version, updates Docker timestamp, commits, and tags
6. CI/CD handles the rest (builds, GitHub release, crates.io, Docker, FlakeHub)

## CI/CD & Index Publishing

### GitHub Actions Workflows

- `ci.yml`: Runs on PRs and main - tests (cargo + nix), clippy, fmt, builds Docker latest on main
- `release.yml`: Triggered by `v*` tags - builds static binaries, publishes to crates.io, pushes versioned Docker images
- `publish-index.yml`: Weekly scheduled or manual - builds the package index and uploads to `index-latest` release
- `pages.yml`: Deploys documentation website to GitHub Pages on changes to `website/`

> **TODO:** After merging `feat/native-nix-indexer` to main, remove the branch from GitHub Pages deployment policy:
> ```bash
> gh api repos/jamesbrink/nxv/environments/github-pages/deployment-branch-policies
> # Find the ID for feat/native-nix-indexer, then delete it:
> gh api repos/jamesbrink/nxv/environments/github-pages/deployment-branch-policies/<ID> -X DELETE
> ```
> Also revert `.github/workflows/pages.yml` to only trigger on `main`.

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

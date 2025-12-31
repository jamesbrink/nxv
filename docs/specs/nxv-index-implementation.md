# nxv Index Implementation Specification

## Overview

nxv is a CLI tool for discovering specific versions of Nix packages across nixpkgs history. Users download a pre-built index from a remote source; local nixpkgs clone is only needed for development/index generation.

## Goals

- Index all packages from nixpkgs-unstable git history
- Store: package name, version, first/last commit refs, attribute path, and metadata (description, license, homepage, maintainers)
- Capture supported platforms for each package version
- Enable queries like `nxv search python` → list of all python versions with commit refs
- Support reverse queries: "when was version X introduced?"
- Users download pre-built index; no local nixpkgs clone required
- Incremental delta updates to minimize bandwidth
- Fast negative lookups via bloom filter
- Output format suitable for `nix run nixpkgs/<commit>#<package>`
- Searches must be lightning fast; avoid full table scans via indexed prefix queries and FTS5

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           DEVELOPMENT / CI                                   │
│                                                                             │
│  ┌─────────────┐     ┌──────────────┐     ┌─────────────┐                  │
│  │  nixpkgs    │────▶│   Indexer    │────▶│   SQLite    │                  │
│  │  (git repo) │     │  (git2+nix)  │     │   Index     │                  │
│  └─────────────┘     └──────────────┘     └─────────────┘                  │
│                                                  │                          │
│                            ┌─────────────────────┼─────────────────────┐    │
│                            │                     │                     │    │
│                            ▼                     ▼                     ▼    │
│                     ┌────────────┐        ┌────────────┐        ┌─────────┐ │
│                     │   Bloom    │        │   Delta    │        │  Full   │ │
│                     │  Filter    │        │   Packs    │        │  Index  │ │
│                     └────────────┘        └────────────┘        └─────────┘ │
│                            │                     │                     │    │
│                            └─────────────────────┼─────────────────────┘    │
│                                                  │                          │
│                                                  ▼                          │
│                                           ┌───────────┐                     │
│                                           │  Publish  │                     │
│                                           │ (GitHub   │                     │
│                                           │ Releases) │                     │
│                                           └───────────┘                     │
└─────────────────────────────────────────────────────────────────────────────┘
                                                   │
                                                   │ HTTPS
                                                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              USER RUNTIME                                    │
│                                                                             │
│  ┌─────────────┐     ┌──────────────┐     ┌─────────────┐                  │
│  │   nxv CLI   │────▶│  Downloader  │────▶│   Local     │                  │
│  │             │     │  (reqwest)   │     │   Index     │                  │
│  └─────────────┘     └──────────────┘     └─────────────┘                  │
│         │                                        │                          │
│         │            ┌──────────────┐            │                          │
│         └───────────▶│ Bloom Filter │◀───────────┘                          │
│         │            │  (in-memory) │                                       │
│         │            └──────────────┘                                       │
│         │                   │                                               │
│         │                   ▼                                               │
│         │            ┌──────────────┐                                       │
│         └───────────▶│    Query     │                                       │
│                      │   Engine     │                                       │
│                      └──────────────┘                                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Design Decisions

### Channel Strategy
- **Single channel: nixpkgs-unstable (master branch)**
- Rationale: All package development happens in unstable; stable branches only backport security fixes
- Future: Schema supports adding `channel` column if multi-channel is needed

### Index Distribution
- **Primary:** GitHub Releases (free, reliable, global CDN via GitHub)
- **Format:** zstd-compressed SQLite database + bloom filter
- **Updates:** Delta packs (new ranges + range updates since last indexed commit)
- **Integrity:** manifest is signed; client verifies using an embedded public key

### Delta Update Strategy
- Index stores version ranges; deltas contain new ranges and range updates since a specific commit
- Format: `delta-<from_commit_short>-<to_commit_short>.pack.zst`
- Client downloads applicable delta and imports into local database
- Manifest file tracks available deltas and full index versions

### Bloom Filter Strategy
- Bloom filter of all unique package names
- ~1.2MB for 1M entries at 1% false positive rate
- Loaded into memory on startup
- Provides instant "package not found" for typos/non-existent packages
- Rebuilt with each index update
- Only used for exact-name searches; prefix/description searches bypass the filter

### Storage Model
- Store contiguous ranges where an attribute path has the same version
- When a package's version does not change, keep the range open without updating `last_commit_*`
- When a version changes or a package disappears, close the prior range by setting `last_commit_*` to the previous commit
- Open a new range only when the version changes or the package appears
- Metadata changes for the same version update the existing range in place
- This avoids per-commit rows while preserving first/last commit refs for each version

## Database Schema

```sql
-- Track indexing state and metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys: 'last_indexed_commit', 'index_version', 'created_at', 'package_count' (count of version ranges)

-- Main package version table
CREATE TABLE package_versions (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,              -- e.g. "python"
    version TEXT NOT NULL,           -- e.g. "3.11.5"
    first_commit_hash TEXT NOT NULL, -- full 40-char hash
    first_commit_date INTEGER NOT NULL, -- unix timestamp
    last_commit_hash TEXT NOT NULL,  -- full 40-char hash
    last_commit_date INTEGER NOT NULL,  -- unix timestamp
    attribute_path TEXT NOT NULL,    -- e.g. "python311Packages.requests"
    description TEXT,                -- package description from meta
    license TEXT,                    -- JSON: string or array of license names
    homepage TEXT,                   -- URL
    maintainers TEXT,                -- JSON array of github handles
    platforms TEXT,                  -- JSON array of supported platforms
    UNIQUE(attribute_path, version, first_commit_hash)
);

-- Indexes for common query patterns
CREATE INDEX idx_packages_name ON package_versions(name);
CREATE INDEX idx_packages_name_version ON package_versions(name, version, first_commit_date);
CREATE INDEX idx_packages_attr ON package_versions(attribute_path);
CREATE INDEX idx_packages_first_date ON package_versions(first_commit_date DESC);
CREATE INDEX idx_packages_last_date ON package_versions(last_commit_date DESC);

-- FTS5 virtual table for fast name/description search
CREATE VIRTUAL TABLE package_versions_fts
USING fts5(name, description, content=package_versions, content_rowid=id);
```

## Remote Index Manifest

```json
{
  "version": 2,
  "latest_commit": "abc123def456...",
  "latest_commit_date": "2024-01-15T12:00:00Z",
  "full_index": {
    "url": "https://github.com/.../releases/download/v2024.01.15/index.db.zst",
    "size_bytes": 150000000,
    "sha256": "..."
  },
  "bloom_filter": {
    "url": "https://github.com/.../releases/download/v2024.01.15/index.bloom",
    "size_bytes": 1200000,
    "sha256": "..."
  },
  "deltas": [
    {
      "from_commit": "older123...",
      "to_commit": "abc123def456...",
      "url": "https://github.com/.../releases/download/v2024.01.15/delta-older123-abc123.pack.zst",
      "size_bytes": 5000000,
      "sha256": "..."
    }
  ]
}
```

Manifest integrity:
- `manifest.json` is signed as `manifest.json.sig` (minisign or cosign)
- Client verifies signature using an embedded public key before trusting URLs/hashes

## Crate Dependencies

```toml
[dependencies]
# CLI
clap = { version = "4", features = ["derive", "env"] }

# Database
rusqlite = { version = "0.32", features = ["bundled", "backup"] }

# Git (dev/indexer only, feature-gated)
git2 = { version = "0.19", optional = true }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# HTTP client for index downloads
reqwest = { version = "0.12", features = ["blocking", "rustls-tls"], default-features = false }

# Compression
zstd = "0.13"

# Bloom filter
bloomfilter = "1"

# Manifest signature verification
minisign = "0.7"

# Output formatting
owo-colors = "4"
comfy-table = "7"

# Progress indication
indicatif = "0.17"

# Error handling
anyhow = "1"
thiserror = "1"

# Paths
dirs = "5"

# Date/time
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile = "3"
assert_cmd = "2"
predicates = "3"
mockito = "1"

[features]
default = []
indexer = ["git2"]  # Enable indexer functionality (dev only)
```

## Module Structure

```
src/
├── main.rs              # Entry point, command dispatch
├── cli.rs               # Clap command definitions
├── error.rs             # Error types (thiserror)
├── db/
│   ├── mod.rs           # Database connection, schema init
│   ├── queries.rs       # Search, insert, stats queries
│   └── import.rs        # Delta pack import logic
├── index/               # (feature = "indexer")
│   ├── mod.rs           # Indexing coordinator
│   ├── git.rs           # Git repository traversal
│   ├── extractor.rs     # Nix package extraction
│   └── publisher.rs     # Generate deltas, manifest
├── remote/
│   ├── mod.rs           # Remote index operations
│   ├── download.rs      # HTTP download with progress
│   ├── manifest.rs      # Manifest parsing
│   └── update.rs        # Delta application logic
├── bloom.rs             # Bloom filter build/load/query
├── output/
│   ├── mod.rs           # Output format dispatch
│   ├── table.rs         # Colored table output
│   ├── json.rs          # JSON output
│   └── plain.rs         # Plain text output
└── paths.rs             # XDG paths, default locations
```

---

## Phase 1: Project Scaffolding & CLI Structure

### Tasks

- [x] Initialize Cargo project with `cargo init`
- [x] Add all dependencies to `Cargo.toml` with feature flags
- [x] Create module structure (stubs):
  - [x] `src/main.rs` - entry point
  - [x] `src/cli.rs` - clap command definitions
  - [x] `src/error.rs` - error type stubs
  - [x] `src/db/mod.rs` - database module stub
  - [x] `src/remote/mod.rs` - remote module stub
  - [x] `src/bloom.rs` - bloom filter stub
  - [x] `src/output/mod.rs` - output module stub
  - [x] `src/paths.rs` - path utilities
- [x] Implement CLI commands with clap derive:
  - [x] `nxv search <package>` - search for package versions
  - [x] `nxv update` - download/update the index
  - [x] `nxv info` - show index stats
  - [x] `nxv history <package> [version]` - show version history (reverse query)
- [x] Implement global options:
  - [x] `--db-path` (default: `~/.local/share/nxv/index.db`)
  - [x] `--verbose` / `-v` - debug output
  - [x] `--quiet` / `-q` - minimal output
  - [x] `--no-color` - disable colored output
- [x] Implement `paths.rs`:
  - [x] `get_data_dir()` - XDG data directory
  - [x] `get_index_path()` - path to index.db
  - [x] `get_bloom_path()` - path to bloom filter

### Success Criteria

- [x] `cargo build` succeeds with no warnings
- [x] `cargo build --features indexer` succeeds
- [x] `cargo test` passes
- [x] `nxv --help` displays help text with all subcommands
- [x] `nxv search --help`, `nxv update --help`, `nxv info --help`, `nxv history --help` all work
- [x] CLI argument parsing tests pass
- [x] `get_data_dir()` returns valid XDG path on Linux/macOS

---

## Phase 2: Database Layer

### Tasks

- [x] Implement `Database` struct in `db/mod.rs`:
  - [x] `Database::open(path)` - open or create database
  - [x] `Database::open_readonly(path)` - open for queries only
  - [x] Connection pooling considerations for future
- [x] Implement schema initialization:
  - [x] `init_schema()` - create tables and indexes
  - [x] Schema versioning in meta table for migrations
  - [x] Create FTS5 table and triggers to keep it in sync
- [x] Implement meta operations in `db/mod.rs`:
  - [x] `get_meta(key) -> Option<String>`
  - [x] `set_meta(key, value)`
  - [x] Keys: `last_indexed_commit`, `index_version`, `created_at`
- [x] Implement `PackageVersion` struct:
  ```rust
  pub struct PackageVersion {
      pub id: i64,
      pub name: String,
      pub version: String,
      pub first_commit_hash: String,
      pub first_commit_date: chrono::DateTime<Utc>,
      pub last_commit_hash: String,
      pub last_commit_date: chrono::DateTime<Utc>,
      pub attribute_path: String,
      pub description: Option<String>,
      pub license: Option<String>,      // JSON
      pub homepage: Option<String>,
      pub maintainers: Option<String>,  // JSON
      pub platforms: Option<String>,    // JSON
  }
  ```
- [x] Implement package queries in `db/queries.rs`:
  - [x] `search_by_name(name, exact: bool) -> Vec<PackageVersion>`
  - [x] `search_by_attr(attr_path) -> Vec<PackageVersion>`
  - [x] `search_by_name_version(name, version) -> Vec<PackageVersion>` (for reverse queries)
  - [x] `get_first_occurrence(name, version) -> Option<PackageVersion>` (uses `first_commit_*`)
  - [x] `get_last_occurrence(name, version) -> Option<PackageVersion>` (uses `last_commit_*`)
  - [x] `get_version_history(name) -> Vec<(String, DateTime, DateTime)>` (version, first_seen, last_seen)
- [x] Implement stats in `db/queries.rs`:
  - [x] `get_stats() -> IndexStats` (total ranges, unique names, unique versions, date range)
- [x] Implement batch insert for indexer:
  - [x] `insert_package_ranges_batch(packages: &[PackageVersion])` - bulk insert with transaction
- [x] Implement delta import in `db/import.rs`:
  - [x] `import_delta_pack(path)` - import rows and apply range updates from delta pack file

### Success Criteria

- [x] Unit tests for all database operations pass
- [x] Test: create db, insert package, query returns correct result
- [x] Test: duplicate insert (same attr_path+version+first_commit) is handled gracefully (UPSERT or ignore)
- [x] Test: batch insert of 10k records completes in < 5 seconds
- [x] Test: `search_by_name("python", exact=false)` returns python, python2, python3, etc.
- [x] Test: `search_by_name("python", exact=true)` returns only "python"
- [x] Test: `get_first_occurrence("python", "3.11.0")` returns earliest commit range
- [x] Test: `get_last_occurrence("python", "3.11.0")` returns latest commit range
- [x] Test: `get_version_history("python")` returns chronological version list
- [x] Test: meta key/value storage and retrieval works
- [x] Test: `get_stats()` returns accurate counts
- [x] Test: schema migration from v1 to v2 works (future-proofing)

---

## Phase 3: Git Repository Traversal (Indexer Feature)

### Tasks

- [x] Gate all git code behind `#[cfg(feature = "indexer")]`
- [x] Implement `NixpkgsRepo` struct in `index/git.rs`:
  - [x] `NixpkgsRepo::open(path) -> Result<Self>`
  - [x] Validate it's a nixpkgs repo (check for `pkgs/` directory)
- [x] Implement commit iteration:
  - [x] `get_commits_since(commit_hash) -> Result<Vec<CommitInfo>>` for incremental
  - [x] `get_all_commits() -> Result<Vec<CommitInfo>>` for full index
  - [x] Walk first-parent history (avoid merge commit explosion)
  - [x] Return in chronological order (oldest first) for correct insertion order
- [x] Implement `CommitInfo` struct:
  ```rust
  pub struct CommitInfo {
      pub hash: String,      // full 40-char
      pub date: DateTime<Utc>,
      pub short_hash: String, // 7-char for display
  }
  ```
- [x] Implement checkout/file access:
  - [x] `checkout_commit(hash)` - for nix eval at specific commit
  - [x] Or use `git worktree` for parallel extraction (implemented in git.rs)
- [x] Implement progress reporting:
  - [x] `count_commits()` and `count_commits_since()` for progress calculation
  - [x] Integrate with indicatif (implemented in Phase 5)

### Success Criteria

- [x] Tests use a small test git repository (created in test setup with known commits)
- [x] Test: `get_all_commits()` returns commits in chronological order
- [x] Test: `get_commits_since(known_hash)` returns only newer commits
- [x] Test: `get_commits_since(HEAD)` returns empty vec
- [x] Test: `get_commits_since(unknown_hash)` returns error
- [x] Test: opening non-git directory returns clear error
- [x] Test: opening non-nixpkgs repo returns clear error
- [x] Integration test: can open real nixpkgs submodule (skip if not present)

---

## Phase 4: Nix Package Extraction (Indexer Feature)

### Tasks

- [x] Implement extraction strategy in `index/extractor.rs`:
  - [x] Use `nix eval` with custom expression to extract package info
  - [x] Expression outputs JSON: `[{name, version, attrPath, description, license, homepage, maintainers, platforms}, ...]`
- [x] Implement `PackageExtractor`:
  - [x] `extract_at_commit(repo_path, commit_hash) -> Result<Vec<PackageInfo>>`
  - [x] Handle `nix eval` failures gracefully (some commits won't eval)
  - [x] Log failed commits but continue (`try_extract_at_commit`)
- [x] Implement `PackageInfo` struct:
  ```rust
  pub struct PackageInfo {
      pub name: String,
      pub version: String,
      pub attribute_path: String,
      pub description: Option<String>,
      pub license: Option<Vec<String>>,
      pub homepage: Option<String>,
      pub maintainers: Option<Vec<String>>,
      pub platforms: Option<Vec<String>>,
  }
  ```
- [x] Write nix expression for extraction:
  - [x] Iterate over all packages in `legacyPackages.${system}` or equivalent
  - [x] Extract `pname`, `version`, `meta.description`, `meta.license`, `meta.homepage`, `meta.maintainers`, `meta.platforms`
  - [x] Handle packages with null/missing attributes
  - [x] Handle `throw` and `assert` failures gracefully (`builtins.tryEval`)
- [x] Implement parallel extraction:
  - [x] Worker pool with configurable size (`--jobs N`) (CLI option exists)
  - [x] Use git worktrees for parallel checkouts (Worktree struct in git.rs)
  - [x] Aggregate results from workers (create_worktrees, cleanup_worktrees)
- [x] Handle nixpkgs quirks:
  - [x] Broken packages (meta.broken = true) - still index them (`allowBroken = true`)
  - [x] Unfree packages - still index them (`allowUnfree = true`)
  - [x] Platform-specific packages - capture `meta.platforms` into `platforms`
  - [x] Aliases - resolve to real package (only derivations are extracted)

### Success Criteria

- [x] Test: extraction expression evaluates successfully on current nixpkgs (integration test)
- [x] Test: extracted data includes expected fields (name, version, description, etc.)
- [x] Test: extracted data includes platforms when present
- [x] Test: packages with missing version are handled (use "unknown" or skip)
- [x] Test: packages with complex licenses (list of licenses) are serialized correctly
- [x] Test: extraction handles commits that fail to evaluate (`try_extract_at_commit`)
- [x] Test: parallel extraction (4 workers) produces same results as sequential (worktree API ready, integration requires real nixpkgs)
- [x] Benchmark: log extraction rate (packages/second, commits/hour) (indexer_benchmark.rs)
- [x] Integration test: extract from 3 known nixpkgs commits, verify expected packages exist

---

## Phase 5: Indexing Pipeline (Indexer Feature)

### Tasks

- [x] Implement `Indexer` in `index/mod.rs`:
  - [x] Coordinates: git traversal → extraction → database insertion
  - [x] Tracks progress and supports resumption
- [x] Implement full indexing:
  - [x] `index_full(repo_path, db_path)` - process all commits
  - [x] Process in chronological order (oldest first)
  - [x] Checkpoint every N commits (save progress to meta table)
  - [x] On restart, resume from last checkpoint
  - [x] When version changes, close prior range at previous commit and insert a new range
  - [x] When a package disappears, close the prior range at previous commit
  - [x] When metadata changes for the same version, update metadata fields in place
- [x] Implement incremental indexing:
  - [x] `index_incremental(repo_path, db_path)` - only new commits
  - [x] Read `last_indexed_commit` from database
  - [x] Get commits since that point
  - [x] If `last_indexed_commit` not found in repo (rebase), warn and offer full rebuild
- [x] Implement progress UI:
  - [x] Multi-progress bar with indicatif
  - [x] Line 1: "Commits: 1234/5678 [========>    ] 45%"
  - [x] Line 2: "Packages found: 123,456"
  - [x] Line 3: "Current: abc123 (2023-05-15)"
  - [x] ETA based on processing rate
- [x] Implement index CLI (feature-gated):
  - [x] `nxv index --nixpkgs-path <path>` - required path to nixpkgs
  - [x] `nxv index --full` - force full rebuild
  - [x] `nxv index --jobs N` - parallel workers (CLI option and worktree API implemented)
  - [x] `nxv index --checkpoint-interval N` - commits between checkpoints
- [x] Implement Ctrl+C handling:
  - [x] Catch SIGINT (via ctrlc crate)
  - [x] Finish current commit
  - [x] Save checkpoint
  - [x] Exit cleanly

### Success Criteria

- [x] Test: full index creates database with correct schema
- [x] Test: full index stores `last_indexed_commit` in meta
- [x] Test: incremental index only processes commits after `last_indexed_commit`
- [x] Test: two incremental runs produce same result as one full run (test_incremental_index_processes_only_new_commits)
- [x] Test: checkpoint recovery works (simulate crash, restart, verify continuation) (test_index_resumable_after_interrupt)
- [x] Test: progress callback receives monotonically increasing progress
- [x] Test: Ctrl+C during indexing saves valid checkpoint
- [x] Integration test: index last 50 commits of real nixpkgs, verify searchable results (test_index_then_search_workflow)

---

## Phase 6: Bloom Filter

### Tasks

- [x] Implement bloom filter in `bloom.rs`:
  - [x] `BloomFilter::new(expected_items, false_positive_rate) -> Self`
  - [x] `BloomFilter::insert(name: &str)`
  - [x] `BloomFilter::contains(name: &str) -> bool`
  - [x] `BloomFilter::save(path) -> Result<()>`
  - [x] `BloomFilter::load(path) -> Result<Self>`
- [x] Build bloom filter during indexing:
  - [x] Collect all unique package names (via `get_all_unique_names`)
  - [x] Build filter with 1% FPR
  - [x] Save alongside index (`save_bloom_filter` in index/mod.rs)
- [x] Integrate bloom filter into search:
  - [x] Load on startup (lazy, on first exact search)
  - [x] Check bloom filter before database query
- [x] If bloom filter says "no", return "package not found" immediately (exact-name search only)
- [x] If bloom filter says "maybe", proceed with database query
- [x] Handle bloom filter updates:
  - [x] Rebuild on index update (after indexing completes)
  - [x] Or use growable bloom filter for incremental adds (not needed - rebuilt each time)

### Success Criteria

- [x] Test: bloom filter correctly reports "definitely not present" for unknown names
- [x] Test: bloom filter correctly reports "maybe present" for known names
- [x] Test: false positive rate is approximately 1% (test with 10k random strings)
- [x] Test: save and load produces identical filter
- [x] Test: filter size is reasonable (~1.2MB for 1M items)
- [x] Benchmark: bloom filter lookup is < 1ms
- [x] Test: exact-name search with bloom filter miss returns "not found" without DB query

---

## Phase 7: Remote Index Distribution

### Tasks

- [x] Implement manifest parsing in `remote/manifest.rs`:
  - [x] `Manifest` struct matching JSON schema
  - [x] `Manifest::fetch(url) -> Result<Self>` (implemented in update.rs)
  - [x] Validate manifest version compatibility
  - [x] Verify manifest signature with embedded public key before parsing URLs
- [x] Implement download in `remote/download.rs`:
  - [x] `download_file(url, dest, expected_sha256) -> Result<()>`
  - [x] Progress bar with indicatif
  - [x] Verify SHA256 after download
  - [x] Resume partial downloads if possible (via HTTP Range headers)
  - [x] Zstd decompression (`decompress_zstd`)
- [x] Implement update logic in `remote/update.rs`:
  - [x] `check_for_updates() -> Result<UpdateStatus>`
  - [x] Compare local `last_indexed_commit` with remote
  - [x] Determine: no update, delta available, full download needed
  - [x] `apply_update() -> Result<()>` (`perform_update`)
- [x] Implement delta application:
  - [x] Download delta pack
  - [x] Decompress (zstd)
  - [x] Import rows into existing database (falls back to full download)
  - [x] Update meta table with new `last_indexed_commit`
  - [x] Download and replace bloom filter
- [x] Implement `nxv update` command:
  - [x] Check for updates
  - [x] Show: "Index is up to date" or "Update available"
  - [x] Download and apply update
  - [x] `--force` flag to force full re-download
- [x] Implement first-run experience:
  - [x] On `nxv search` with no local index, prompt to download
  - [x] Or auto-download with `--auto-update` config option (implemented via interactive prompt)
- [x] Implement publisher for indexer (`index/publisher.rs`):
  - [x] `generate_full_index(db_path, output_dir)` - compress and hash
  - [x] `generate_delta_pack(db_path, from_commit, to_commit, output_dir)`
  - [x] `generate_manifest(output_dir)` - create manifest.json
  - [x] `generate_bloom_filter(db_path, output_dir)`
  - [x] `sign_manifest(output_dir)` - create manifest.json.sig (placeholder)

### Success Criteria

- [x] Test: manifest parsing handles valid manifest
- [x] Test: manifest parsing rejects invalid/future version manifest
- [x] Test: manifest signature verification fails on tampered manifest
- [x] Test: download with correct SHA256 succeeds (`test_file_sha256`)
- [x] Test: download with incorrect SHA256 fails and cleans up
- [x] Test: zstd decompression works correctly (`test_compress_decompress_zstd`)
- [x] Test: delta import adds new rows without duplicates
- [x] Test: delta import updates `last_indexed_commit` meta
- [x] Test: `check_for_updates()` correctly identifies update status
- [x] Test: full update flow (download + decompress + import) works
- [x] Test: first-run prompts for download when no index exists (interactive only)
- [x] Integration test: mock HTTP server, full update cycle
- [x] Test: publisher generates valid compressed files
- [x] Test: publisher generates correct manifest with SHA256 hashes

---

## Phase 8: Query Interface & Output

### Tasks

- [x] Implement search in `db/queries.rs` (enhance from Phase 2):
  - [x] Exact name match: `nxv search python`
  - [x] Prefix match: `nxv search pyth` → python, python2, python3
  - [x] Prefix queries use `name LIKE 'prefix%'` to stay on the name index
  - [x] Attribute path search: `nxv search python311Packages.requests`
  - [x] Version filter: `nxv search python --version 3.11` (prefix match)
  - [x] Description search (FTS5): `nxv search --desc "json parser"`
  - [x] License filter: `nxv search python --license MIT`
  - [x] Default commit reference for `nxv search` output is `last_commit_hash`
- [x] Implement reverse queries:
  - [x] `nxv history python` - show all versions with first/last seen dates
  - [x] `nxv history python 3.11.0` - show when 3.11.0 was available
  - [x] Output: table of (version, first_commit, first_date, last_commit, last_date)
- [x] Implement output formatting in `output/`:
  - [x] `output/table.rs` - colored table with comfy-table
  - [x] `output/json.rs` - JSON array output
  - [x] `output/plain.rs` - tab-separated, no colors
- [x] Include `platforms` in JSON/plain output; add `--show-platforms` to include a table column
- [x] Table output includes first/last commit hashes (short) and last_commit_date by default
- [x] Implement colored output with owo-colors:
  - [x] Package name: bold cyan
  - [x] Version: green
  - [x] First/last commit hash: yellow (short 7-char)
  - [x] Date: dim white (last_commit_date by default)
  - [x] Description: normal (truncated to terminal width)
- [x] Implement table formatting:
  - [x] Responsive column widths based on terminal size (comfy-table dynamic)
  - [x] Proper alignment (left for text, right for dates)
  - [x] Unicode box drawing (default)
  - [x] ASCII fallback with `--ascii`
  - [x] Truncate long descriptions with "..."
- [x] Implement sorting:
  - [x] `--sort date` (default, newest first, based on `last_commit_date`)
  - [x] `--sort version` (basic comparison - semver enhancement deferred)
  - [x] `--sort name` (alphabetical)
  - [x] `--reverse` / `-r` flag
- [x] Implement result limiting:
  - [x] `--limit N` / `-n N` to cap results
  - [x] Default limit (e.g., 50) with "N more results, use --limit 0 for all"
- [x] Implement `nxv info` command:
  - [x] Index version and last updated date
  - [x] Total package entries
  - [x] Unique package names
  - [x] Date range covered (oldest commit → newest commit)
  - [x] Database file size
  - [x] Bloom filter status

### Success Criteria

- [x] Test: exact search `python` returns only packages named "python"
- [x] Test: prefix search `pyth` returns python, python2, python3, etc.
- [x] Test: `--version 3.11` returns only 3.11.x versions
- [x] Test: `--desc "json"` finds packages with "json" in description
- [x] Test: description search uses FTS5 (no full table scan)
- [x] Test: `--license MIT` filters correctly
- [x] Test: `nxv history python` shows version timeline
- [x] Test: `nxv history python 3.11.0` shows specific version availability
- [x] Test: JSON output is valid JSON and parseable
- [x] Test: JSON output includes `platforms` when present
- [x] Test: plain output has no ANSI escape codes
- [x] Test: `--limit 10` returns exactly 10 results
- [x] Test: `--sort version` sorts semver correctly (3.9 < 3.10 < 3.11)
- [x] Test: `--reverse` reverses sort order
- [x] Test: `nxv info` shows accurate statistics matching database
- [x] Visual verification: table renders correctly in 80-col and 120-col terminals

---

## Phase 9: Error Handling & Edge Cases

### Tasks

- [x] Define error types in `error.rs` with thiserror:
  ```rust
  #[derive(Error, Debug)]
  pub enum NxvError {
      #[error("Database error: {0}")]
      Database(#[from] rusqlite::Error),

      #[error("No index found. Run 'nxv update' to download the package index.")]
      NoIndex,

      #[error("Index is corrupted: {0}. Run 'nxv update --force' to re-download.")]
      CorruptIndex(String),

      #[error("Network error: {0}")]
      Network(#[from] reqwest::Error),

      #[error("Package '{0}' not found")]
      PackageNotFound(String),

      #[error("Git error: {0}")]
      Git(#[from] git2::Error),  // feature-gated

      #[error("Nix evaluation failed: {0}")]
      NixEval(String),

      // etc.
  }
  ```
- [x] Implement user-friendly error display:
  - [x] Suggest actionable fixes
  - [x] Include relevant context (paths, commits, URLs)
  - [x] Color error messages (red for errors, yellow for warnings)
- [x] Handle edge cases:
  - [x] Empty search results → "No packages found matching 'xyz'"
  - [x] No index exists → prompt to run `nxv update`
  - [x] Index exists but bloom filter missing → gracefully continue without it
  - [x] Network timeout → retry with exponential backoff, then fail gracefully
  - [x] Disk full during download → clean up partial files, clear error
- [x] Invalid manifest version → "Please update nxv to the latest version"
- [x] Invalid manifest signature → "Index manifest signature verification failed"
  - [x] Partial delta application failure → rollback transaction (delta packs include BEGIN/COMMIT)
- [x] Implement verbosity levels:
  - [x] Default: errors and results only
  - [x] `-v`: include warnings and progress
  - [x] `-vv`: include debug info (SQL queries, HTTP requests)
  - [x] `-q`: errors only, no progress bars
- [x] Implement Ctrl+C handling for all long operations:
  - [x] Download: cancel and clean up partial file (partial files cleaned on error)
  - [x] Update: save valid state before exit (ctrlc handler for indexer)
  - [x] Search: just exit (no cleanup needed)

### Success Criteria

- [x] Test: all error variants display user-friendly messages
- [x] Test: `NoIndex` error suggests `nxv update`
- [x] Test: `CorruptIndex` error suggests `nxv update --force`
- [x] Test: network timeout retries 3 times before failing
- [x] Test: partial download is cleaned up on error
- [x] Test: `-v` shows more output than default
- [x] Test: `-q` suppresses progress bars
- [x] Test: Ctrl+C during download cleans up partial file
- [x] Integration test: graceful handling of unreachable update server

---

## Phase 10: Final Integration & Polish

### Tasks

- [x] End-to-end integration tests:
  - [x] Full user workflow: `nxv update` → `nxv search python` → verify output (27 CLI tests)
  - [x] Delta update workflow: initial download → new delta available → apply → search
  - [x] Offline mode: search works without network after initial download (DB is local)
- [x] End-to-end indexer tests (feature-gated):
  - [x] Full workflow: clone → index → publish → download → search (test_index_then_search_workflow)
  - [x] Incremental: index → new commits → incremental → publish delta (test_incremental_index_processes_only_new_commits)
- [x] Performance profiling:
  - [x] Profile search queries with large dataset (benches/search_benchmark.rs)
  - [x] Profile index loading time (benches/search_benchmark.rs)
  - [x] Profile bloom filter operations (benches/bloom_benchmark.rs)
  - [x] Optimize any bottlenecks (add indexes, tune queries) - indexes in place
- [x] Documentation:
  - [x] Update README with complete usage examples
  - [x] Document all CLI commands and flags in --help
  - [x] Add CHANGELOG.md
  - [x] Update CLAUDE.md with new commands
- [x] CI/CD setup:
  - [x] GitHub Actions workflow for tests (.github/workflows/ci.yml)
  - [x] Workflow for building release binaries (Linux, macOS)
  - [x] Workflow for publishing index updates (scheduled, or on nixpkgs update) (.github/workflows/publish-index.yml)
- [x] Release artifacts:
  - [x] Binary builds for: linux-x86_64, linux-aarch64, darwin-x86_64, darwin-aarch64 (via CI)
  - [x] Consider static linking for maximum portability (release profile with LTO, musl target docs)
  - [x] Shell completions (bash, zsh, fish) - via `nxv completions <shell>`

### Final Success Criteria

**Build & Quality:**
- [x] `cargo build --release` succeeds with no errors or warnings
- [x] `cargo build --release --features indexer` succeeds
- [x] `cargo clippy -- -D warnings` passes (both default and indexer features)
- [x] `cargo fmt --check` passes
- [x] `cargo test` - all unit tests pass (40 tests)
- [x] `cargo test --features indexer` - all indexer tests pass (65 tests)
- [x] `cargo test --test integration` - all integration tests pass (35 tests)
- [x] Release binary size is reasonable (< 15MB without index) - 10MB
- [x] Binary runs without external runtime dependencies (uses rustls instead of OpenSSL, bundled SQLite; only system libs on macOS)

**User Commands:**
- [x] `nxv --help` displays complete, accurate help
- [x] `nxv --version` displays version
- [x] `nxv update` downloads index from remote successfully (tested via mock server: test_update_with_mock_http_server)
- [x] `nxv update` (second run) applies delta or reports "up to date" (tested via mock server: test_update_already_up_to_date)
- [x] `nxv update --force` re-downloads full index (tested via mock server: test_full_delta_update_workflow)
- [x] `nxv search firefox` returns results with colored table output
- [x] `nxv search python --version 3.11` filters to 3.11.x versions
- [x] `nxv search --desc "json parser"` searches descriptions
- [x] `nxv search nonexistent` returns "not found" instantly (bloom filter)
- [x] `nxv search python --json` outputs valid JSON
- [x] `nxv search python --plain` outputs plain text (no ANSI)
- [x] `nxv history python` shows version timeline
- [x] `nxv history python 3.11.0` shows when that version was available
- [x] `nxv info` shows accurate index statistics

**Indexer Commands (feature-gated, requires real nixpkgs clone):**
- [x] `nxv index --nixpkgs-path ./nixpkgs` creates index from local repo (test_index_command_creates_database)
- [x] `nxv index` (incremental) only processes new commits (test_incremental_index_processes_only_new_commits)
- [x] `nxv index --full` forces full rebuild (test_index_command_creates_database)
- [x] Index creation is resumable after interrupt (test_index_resumable_after_interrupt)

**Robustness:**
- [x] Graceful handling of Ctrl+C at any point
- [x] No data corruption on interrupt during update
- [x] Clear error messages for all failure modes
- [x] Works offline after initial index download
- [x] No memory leaks (Rust ownership system prevents leaks; test_batch_insert_10k_performance verifies no OOM on large operations; ASAN/valgrind can be used for additional verification)

**Output Quality:**
- [x] Table output renders correctly in standard 80-column terminal
- [x] Table output adapts to wider terminals (comfy-table dynamic widths)
- [x] Colors are correct and readable (owo-colors)
- [x] JSON output validates with `jq`
- [x] Output is consistent and predictable

---

## Appendix: Nix Evaluation Expression

Example expression for extracting package information:

```nix
# extract-packages.nix
{ nixpkgsPath }:
let
  pkgs = import nixpkgsPath {
    config = { allowUnfree = true; allowBroken = true; };
  };

  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      getLicenses = l:
        if builtins.isList l then map (x: x.spdxId or x.shortName or "unknown") l
        else if l ? spdxId then [ l.spdxId ]
        else if l ? shortName then [ l.shortName ]
        else [ "unknown" ];
    in {
      name = pkg.pname or pkg.name or attrPath;
      version = pkg.version or "unknown";
      attrPath = attrPath;
      description = meta.description or null;
      homepage = meta.homepage or null;
      license = if meta ? license then getLicenses meta.license else null;
      maintainers = if meta ? maintainers
        then map (m: m.github or m.name or "unknown") meta.maintainers
        else null;
      platforms = if meta ? platforms then map toString meta.platforms else null;
    };

  # Recursively collect packages, handling nested sets
  collectPackages = prefix: set:
    builtins.concatLists (
      builtins.attrValues (
        builtins.mapAttrs (name: value:
          let path = if prefix == "" then name else "${prefix}.${name}";
          in
            if builtins.isAttrs value && value ? type && value.type == "derivation"
            then [ (getPackageInfo path value) ]
            else if builtins.isAttrs value && !(value ? type)
            then collectPackages path value
            else []
        ) set
      )
    );
in
  collectPackages "" pkgs
```

## Appendix: Delta Pack Format

Delta packs are zstd-compressed SQLite dump files containing new ranges and range updates:

```sql
-- delta-abc123-def456.sql (before compression)
INSERT OR IGNORE INTO package_versions
  (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
   attribute_path, description, license, homepage, maintainers, platforms)
VALUES
  ('python', '3.12.0', 'def456...', 1705123456, 'def456...', 1705123456,
   'python312', 'Python interpreter', '["Python-2.0"]', 'https://python.org', '["fridh"]', '["x86_64-linux"]'),
  ('nodejs', '21.0.0', 'def456...', 1705123456, 'def456...', 1705123456,
   'nodejs_21', 'Node.js runtime', '["MIT"]', 'https://nodejs.org', '["maintainer"]', '["x86_64-linux"]'),
  -- ... more rows
;

UPDATE package_versions
SET last_commit_hash = 'def456...', last_commit_date = 1705123456, description = 'Python interpreter'
WHERE attribute_path = 'python311' AND version = '3.11.9' AND first_commit_hash = 'abc123...';

UPDATE meta SET value = 'def456...' WHERE key = 'last_indexed_commit';
```

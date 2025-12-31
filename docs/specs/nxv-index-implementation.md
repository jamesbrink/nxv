# nxv Index Implementation Specification

## Overview

nxv is a CLI tool for discovering specific versions of Nix packages across nixpkgs history. Users download a pre-built index from a remote source; local nixpkgs clone is only needed for development/index generation.

## Goals

- Index all packages from nixpkgs-unstable git history
- Store: package name, version, commit hash, commit date, attribute path, and metadata (description, license, homepage, maintainers)
- Enable queries like `nxv search python` → list of all python versions with commit refs
- Support reverse queries: "when was version X introduced?"
- Users download pre-built index; no local nixpkgs clone required
- Incremental delta updates to minimize bandwidth
- Fast negative lookups via bloom filter
- Output format suitable for `nix run nixpkgs/<commit>#<package>`

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
- **Updates:** Delta packs (append-only rows since last indexed commit)

### Delta Update Strategy
- Index is naturally append-only (new commits = new package rows)
- Delta packs contain only rows added since a specific commit
- Format: `delta-<from_commit_short>-<to_commit_short>.pack.zst`
- Client downloads applicable delta and imports into local database
- Manifest file tracks available deltas and full index versions

### Bloom Filter Strategy
- Bloom filter of all unique package names
- ~1.2MB for 1M entries at 1% false positive rate
- Loaded into memory on startup
- Provides instant "package not found" for typos/non-existent packages
- Rebuilt with each index update

## Database Schema

```sql
-- Track indexing state and metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys: 'last_indexed_commit', 'index_version', 'created_at', 'package_count'

-- Main package version table
CREATE TABLE packages (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,              -- e.g. "python"
    version TEXT NOT NULL,           -- e.g. "3.11.5"
    commit_hash TEXT NOT NULL,       -- full 40-char hash
    commit_date INTEGER NOT NULL,    -- unix timestamp
    attribute_path TEXT NOT NULL,    -- e.g. "python311Packages.requests"
    description TEXT,                -- package description from meta
    license TEXT,                    -- JSON: string or array of license names
    homepage TEXT,                   -- URL
    maintainers TEXT,                -- JSON array of github handles
    UNIQUE(attribute_path, commit_hash)
);

-- Indexes for common query patterns
CREATE INDEX idx_packages_name ON packages(name);
CREATE INDEX idx_packages_name_version ON packages(name, version, commit_date);
CREATE INDEX idx_packages_attr ON packages(attribute_path);
CREATE INDEX idx_packages_date ON packages(commit_date DESC);
CREATE INDEX idx_packages_description ON packages(description) WHERE description IS NOT NULL;

-- FTS5 virtual table for full-text search on description (optional, Phase 8)
-- CREATE VIRTUAL TABLE packages_fts USING fts5(name, description, content=packages, content_rowid=id);
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

- [ ] Initialize Cargo project with `cargo init`
- [ ] Add all dependencies to `Cargo.toml` with feature flags
- [ ] Create module structure (stubs):
  - [ ] `src/main.rs` - entry point
  - [ ] `src/cli.rs` - clap command definitions
  - [ ] `src/error.rs` - error type stubs
  - [ ] `src/db/mod.rs` - database module stub
  - [ ] `src/remote/mod.rs` - remote module stub
  - [ ] `src/bloom.rs` - bloom filter stub
  - [ ] `src/output/mod.rs` - output module stub
  - [ ] `src/paths.rs` - path utilities
- [ ] Implement CLI commands with clap derive:
  - [ ] `nxv search <package>` - search for package versions
  - [ ] `nxv update` - download/update the index
  - [ ] `nxv info` - show index stats
  - [ ] `nxv history <package> [version]` - show version history (reverse query)
- [ ] Implement global options:
  - [ ] `--db-path` (default: `~/.local/share/nxv/index.db`)
  - [ ] `--verbose` / `-v` - debug output
  - [ ] `--quiet` / `-q` - minimal output
  - [ ] `--no-color` - disable colored output
- [ ] Implement `paths.rs`:
  - [ ] `get_data_dir()` - XDG data directory
  - [ ] `get_index_path()` - path to index.db
  - [ ] `get_bloom_path()` - path to bloom filter

### Success Criteria

- [ ] `cargo build` succeeds with no warnings
- [ ] `cargo build --features indexer` succeeds
- [ ] `cargo test` passes
- [ ] `nxv --help` displays help text with all subcommands
- [ ] `nxv search --help`, `nxv update --help`, `nxv info --help`, `nxv history --help` all work
- [ ] CLI argument parsing tests pass
- [ ] `get_data_dir()` returns valid XDG path on Linux/macOS

---

## Phase 2: Database Layer

### Tasks

- [ ] Implement `Database` struct in `db/mod.rs`:
  - [ ] `Database::open(path)` - open or create database
  - [ ] `Database::open_readonly(path)` - open for queries only
  - [ ] Connection pooling considerations for future
- [ ] Implement schema initialization:
  - [ ] `init_schema()` - create tables and indexes
  - [ ] Schema versioning in meta table for migrations
- [ ] Implement meta operations in `db/mod.rs`:
  - [ ] `get_meta(key) -> Option<String>`
  - [ ] `set_meta(key, value)`
  - [ ] Keys: `last_indexed_commit`, `index_version`, `created_at`
- [ ] Implement `PackageVersion` struct:
  ```rust
  pub struct PackageVersion {
      pub id: i64,
      pub name: String,
      pub version: String,
      pub commit_hash: String,
      pub commit_date: chrono::DateTime<Utc>,
      pub attribute_path: String,
      pub description: Option<String>,
      pub license: Option<String>,      // JSON
      pub homepage: Option<String>,
      pub maintainers: Option<String>,  // JSON
  }
  ```
- [ ] Implement package queries in `db/queries.rs`:
  - [ ] `search_by_name(name, exact: bool) -> Vec<PackageVersion>`
  - [ ] `search_by_attr(attr_path) -> Vec<PackageVersion>`
  - [ ] `search_by_name_version(name, version) -> Vec<PackageVersion>` (for reverse queries)
  - [ ] `get_first_occurrence(name, version) -> Option<PackageVersion>`
  - [ ] `get_last_occurrence(name, version) -> Option<PackageVersion>`
  - [ ] `get_version_history(name) -> Vec<(String, DateTime, DateTime)>` (version, first_seen, last_seen)
- [ ] Implement stats in `db/queries.rs`:
  - [ ] `get_stats() -> IndexStats` (total rows, unique names, unique versions, date range)
- [ ] Implement batch insert for indexer:
  - [ ] `insert_packages_batch(packages: &[PackageVersion])` - bulk insert with transaction
- [ ] Implement delta import in `db/import.rs`:
  - [ ] `import_delta_pack(path)` - import rows from delta pack file

### Success Criteria

- [ ] Unit tests for all database operations pass
- [ ] Test: create db, insert package, query returns correct result
- [ ] Test: duplicate insert (same attr_path+commit) is handled gracefully (UPSERT or ignore)
- [ ] Test: batch insert of 10k records completes in < 5 seconds
- [ ] Test: `search_by_name("python", exact=false)` returns python, python2, python3, etc.
- [ ] Test: `search_by_name("python", exact=true)` returns only "python"
- [ ] Test: `get_first_occurrence("python", "3.11.0")` returns earliest commit
- [ ] Test: `get_last_occurrence("python", "3.11.0")` returns latest commit with that version
- [ ] Test: `get_version_history("python")` returns chronological version list
- [ ] Test: meta key/value storage and retrieval works
- [ ] Test: `get_stats()` returns accurate counts
- [ ] Test: schema migration from v1 to v2 works (future-proofing)

---

## Phase 3: Git Repository Traversal (Indexer Feature)

### Tasks

- [ ] Gate all git code behind `#[cfg(feature = "indexer")]`
- [ ] Implement `NixpkgsRepo` struct in `index/git.rs`:
  - [ ] `NixpkgsRepo::open(path) -> Result<Self>`
  - [ ] Validate it's a nixpkgs repo (check for `pkgs/` directory)
- [ ] Implement commit iteration:
  - [ ] `get_commits_since(commit_hash) -> Result<Vec<CommitInfo>>` for incremental
  - [ ] `get_all_commits() -> Result<Vec<CommitInfo>>` for full index
  - [ ] Walk first-parent history (avoid merge commit explosion)
  - [ ] Return in chronological order (oldest first) for correct insertion order
- [ ] Implement `CommitInfo` struct:
  ```rust
  pub struct CommitInfo {
      pub hash: String,      // full 40-char
      pub date: DateTime<Utc>,
      pub short_hash: String, // 7-char for display
  }
  ```
- [ ] Implement checkout/file access:
  - [ ] `checkout_commit(hash)` - for nix eval at specific commit
  - [ ] Or use `git worktree` for parallel extraction
- [ ] Implement progress reporting:
  - [ ] Callback/channel for "processing commit N of M"
  - [ ] Integrate with indicatif

### Success Criteria

- [ ] Tests use a small test git repository (created in test setup with known commits)
- [ ] Test: `get_all_commits()` returns commits in chronological order
- [ ] Test: `get_commits_since(known_hash)` returns only newer commits
- [ ] Test: `get_commits_since(HEAD)` returns empty vec
- [ ] Test: `get_commits_since(unknown_hash)` returns error
- [ ] Test: opening non-git directory returns clear error
- [ ] Test: opening non-nixpkgs repo returns clear error
- [ ] Integration test: can open real nixpkgs submodule (skip if not present)

---

## Phase 4: Nix Package Extraction (Indexer Feature)

### Tasks

- [ ] Implement extraction strategy in `index/extractor.rs`:
  - [ ] Use `nix eval` with custom expression to extract package info
  - [ ] Expression outputs JSON: `[{name, version, attrPath, description, license, homepage, maintainers}, ...]`
- [ ] Implement `PackageExtractor`:
  - [ ] `extract_at_commit(repo_path, commit_hash) -> Result<Vec<PackageInfo>>`
  - [ ] Handle `nix eval` failures gracefully (some commits won't eval)
  - [ ] Log failed commits but continue
- [ ] Implement `PackageInfo` struct:
  ```rust
  pub struct PackageInfo {
      pub name: String,
      pub version: String,
      pub attribute_path: String,
      pub description: Option<String>,
      pub license: Option<Vec<String>>,
      pub homepage: Option<String>,
      pub maintainers: Option<Vec<String>>,
  }
  ```
- [ ] Write nix expression for extraction:
  - [ ] Iterate over all packages in `legacyPackages.${system}` or equivalent
  - [ ] Extract `pname`, `version`, `meta.description`, `meta.license`, `meta.homepage`, `meta.maintainers`
  - [ ] Handle packages with null/missing attributes
  - [ ] Handle `throw` and `assert` failures gracefully
- [ ] Implement parallel extraction:
  - [ ] Worker pool with configurable size (`--jobs N`)
  - [ ] Use git worktrees for parallel checkouts
  - [ ] Aggregate results from workers
- [ ] Handle nixpkgs quirks:
  - [ ] Broken packages (meta.broken = true) - still index them
  - [ ] Unfree packages - still index them
  - [ ] Platform-specific packages - index all platforms
  - [ ] Aliases - resolve to real package

### Success Criteria

- [ ] Test: extraction expression evaluates successfully on current nixpkgs
- [ ] Test: extracted data includes expected fields (name, version, description, etc.)
- [ ] Test: packages with missing version are handled (use "unknown" or skip)
- [ ] Test: packages with complex licenses (list of licenses) are serialized correctly
- [ ] Test: extraction handles commits that fail to evaluate
- [ ] Test: parallel extraction (4 workers) produces same results as sequential
- [ ] Benchmark: log extraction rate (packages/second, commits/hour)
- [ ] Integration test: extract from 3 known nixpkgs commits, verify expected packages exist

---

## Phase 5: Indexing Pipeline (Indexer Feature)

### Tasks

- [ ] Implement `Indexer` in `index/mod.rs`:
  - [ ] Coordinates: git traversal → extraction → database insertion
  - [ ] Tracks progress and supports resumption
- [ ] Implement full indexing:
  - [ ] `index_full(repo_path, db_path)` - process all commits
  - [ ] Process in chronological order (oldest first)
  - [ ] Checkpoint every N commits (save progress to meta table)
  - [ ] On restart, resume from last checkpoint
- [ ] Implement incremental indexing:
  - [ ] `index_incremental(repo_path, db_path)` - only new commits
  - [ ] Read `last_indexed_commit` from database
  - [ ] Get commits since that point
  - [ ] If `last_indexed_commit` not found in repo (rebase), warn and offer full rebuild
- [ ] Implement progress UI:
  - [ ] Multi-progress bar with indicatif
  - [ ] Line 1: "Commits: 1234/5678 [========>    ] 45%"
  - [ ] Line 2: "Packages found: 123,456"
  - [ ] Line 3: "Current: abc123 (2023-05-15)"
  - [ ] ETA based on processing rate
- [ ] Implement index CLI (feature-gated):
  - [ ] `nxv index --nixpkgs-path <path>` - required path to nixpkgs
  - [ ] `nxv index --full` - force full rebuild
  - [ ] `nxv index --jobs N` - parallel workers (default: num_cpus)
  - [ ] `nxv index --checkpoint-interval N` - commits between checkpoints
- [ ] Implement Ctrl+C handling:
  - [ ] Catch SIGINT
  - [ ] Finish current commit
  - [ ] Save checkpoint
  - [ ] Exit cleanly

### Success Criteria

- [ ] Test: full index creates database with correct schema
- [ ] Test: full index stores `last_indexed_commit` in meta
- [ ] Test: incremental index only processes commits after `last_indexed_commit`
- [ ] Test: two incremental runs produce same result as one full run
- [ ] Test: checkpoint recovery works (simulate crash, restart, verify continuation)
- [ ] Test: progress callback receives monotonically increasing progress
- [ ] Test: Ctrl+C during indexing saves valid checkpoint
- [ ] Integration test: index last 50 commits of real nixpkgs, verify searchable results

---

## Phase 6: Bloom Filter

### Tasks

- [ ] Implement bloom filter in `bloom.rs`:
  - [ ] `BloomFilter::new(expected_items, false_positive_rate) -> Self`
  - [ ] `BloomFilter::insert(name: &str)`
  - [ ] `BloomFilter::contains(name: &str) -> bool`
  - [ ] `BloomFilter::save(path) -> Result<()>`
  - [ ] `BloomFilter::load(path) -> Result<Self>`
- [ ] Build bloom filter during indexing:
  - [ ] Collect all unique package names
  - [ ] Build filter with 1% FPR
  - [ ] Save alongside index
- [ ] Integrate bloom filter into search:
  - [ ] Load on startup (lazy, on first search)
  - [ ] Check bloom filter before database query
  - [ ] If bloom filter says "no", return "package not found" immediately
  - [ ] If bloom filter says "maybe", proceed with database query
- [ ] Handle bloom filter updates:
  - [ ] Rebuild on index update
  - [ ] Or use growable bloom filter for incremental adds

### Success Criteria

- [ ] Test: bloom filter correctly reports "definitely not present" for unknown names
- [ ] Test: bloom filter correctly reports "maybe present" for known names
- [ ] Test: false positive rate is approximately 1% (test with 10k random strings)
- [ ] Test: save and load produces identical filter
- [ ] Test: filter size is reasonable (~1.2MB for 1M items)
- [ ] Benchmark: bloom filter lookup is < 1ms
- [ ] Test: search with bloom filter miss returns "not found" without DB query

---

## Phase 7: Remote Index Distribution

### Tasks

- [ ] Implement manifest parsing in `remote/manifest.rs`:
  - [ ] `Manifest` struct matching JSON schema
  - [ ] `Manifest::fetch(url) -> Result<Self>`
  - [ ] Validate manifest version compatibility
- [ ] Implement download in `remote/download.rs`:
  - [ ] `download_file(url, dest, expected_sha256) -> Result<()>`
  - [ ] Progress bar with indicatif
  - [ ] Verify SHA256 after download
  - [ ] Resume partial downloads if possible
  - [ ] Zstd decompression
- [ ] Implement update logic in `remote/update.rs`:
  - [ ] `check_for_updates() -> Result<UpdateStatus>`
  - [ ] Compare local `last_indexed_commit` with remote
  - [ ] Determine: no update, delta available, full download needed
  - [ ] `apply_update() -> Result<()>`
- [ ] Implement delta application:
  - [ ] Download delta pack
  - [ ] Decompress (zstd)
  - [ ] Import rows into existing database
  - [ ] Update meta table with new `last_indexed_commit`
  - [ ] Download and replace bloom filter
- [ ] Implement `nxv update` command:
  - [ ] Check for updates
  - [ ] Show: "Index is up to date" or "Update available: 1234 new commits"
  - [ ] Download and apply update
  - [ ] `--force` flag to force full re-download
- [ ] Implement first-run experience:
  - [ ] On `nxv search` with no local index, prompt to download
  - [ ] Or auto-download with `--auto-update` config option
- [ ] Implement publisher for indexer (`index/publisher.rs`):
  - [ ] `generate_full_index(db_path, output_dir)` - compress and hash
  - [ ] `generate_delta_pack(db_path, from_commit, to_commit, output_dir)`
  - [ ] `generate_manifest(output_dir)` - create manifest.json
  - [ ] `generate_bloom_filter(db_path, output_dir)`

### Success Criteria

- [ ] Test: manifest parsing handles valid manifest
- [ ] Test: manifest parsing rejects invalid/future version manifest
- [ ] Test: download with correct SHA256 succeeds
- [ ] Test: download with incorrect SHA256 fails and cleans up
- [ ] Test: zstd decompression works correctly
- [ ] Test: delta import adds new rows without duplicates
- [ ] Test: delta import updates `last_indexed_commit` meta
- [ ] Test: `check_for_updates()` correctly identifies update status
- [ ] Test: full update flow (download + decompress + import) works
- [ ] Test: first-run prompts for download when no index exists
- [ ] Integration test: mock HTTP server, full update cycle
- [ ] Test: publisher generates valid compressed files
- [ ] Test: publisher generates correct manifest with SHA256 hashes

---

## Phase 8: Query Interface & Output

### Tasks

- [ ] Implement search in `db/queries.rs` (enhance from Phase 2):
  - [ ] Exact name match: `nxv search python`
  - [ ] Partial/fuzzy match: `nxv search pyth` → python, python2, python3
  - [ ] Attribute path search: `nxv search python311Packages.requests`
  - [ ] Version filter: `nxv search python --version 3.11` (prefix match)
  - [ ] Description search: `nxv search --desc "json parser"`
  - [ ] License filter: `nxv search python --license MIT`
- [ ] Implement reverse queries:
  - [ ] `nxv history python` - show all versions with first/last seen dates
  - [ ] `nxv history python 3.11.0` - show when 3.11.0 was available
  - [ ] Output: table of (version, first_commit, first_date, last_commit, last_date)
- [ ] Implement output formatting in `output/`:
  - [ ] `output/table.rs` - colored table with comfy-table
  - [ ] `output/json.rs` - JSON array output
  - [ ] `output/plain.rs` - tab-separated, no colors
- [ ] Implement colored output with owo-colors:
  - [ ] Package name: bold cyan
  - [ ] Version: green
  - [ ] Commit hash: yellow (short 7-char)
  - [ ] Date: dim white
  - [ ] Description: normal (truncated to terminal width)
- [ ] Implement table formatting:
  - [ ] Responsive column widths based on terminal size
  - [ ] Proper alignment (left for text, right for dates)
  - [ ] Unicode box drawing (default)
  - [ ] ASCII fallback with `--ascii`
  - [ ] Truncate long descriptions with "..."
- [ ] Implement sorting:
  - [ ] `--sort date` (default, newest first)
  - [ ] `--sort version` (semver-aware where possible)
  - [ ] `--sort name` (alphabetical)
  - [ ] `--reverse` / `-r` flag
- [ ] Implement result limiting:
  - [ ] `--limit N` / `-n N` to cap results
  - [ ] Default limit (e.g., 50) with "N more results, use --limit 0 for all"
- [ ] Implement `nxv info` command:
  - [ ] Index version and last updated date
  - [ ] Total package entries
  - [ ] Unique package names
  - [ ] Date range covered (oldest commit → newest commit)
  - [ ] Database file size
  - [ ] Bloom filter status

### Success Criteria

- [ ] Test: exact search `python` returns only packages named "python"
- [ ] Test: partial search `pyth` returns python, python2, python3, etc.
- [ ] Test: `--version 3.11` returns only 3.11.x versions
- [ ] Test: `--desc "json"` finds packages with "json" in description
- [ ] Test: `--license MIT` filters correctly
- [ ] Test: `nxv history python` shows version timeline
- [ ] Test: `nxv history python 3.11.0` shows specific version availability
- [ ] Test: JSON output is valid JSON and parseable
- [ ] Test: plain output has no ANSI escape codes
- [ ] Test: `--limit 10` returns exactly 10 results
- [ ] Test: `--sort version` sorts semver correctly (3.9 < 3.10 < 3.11)
- [ ] Test: `--reverse` reverses sort order
- [ ] Test: `nxv info` shows accurate statistics matching database
- [ ] Visual verification: table renders correctly in 80-col and 120-col terminals

---

## Phase 9: Error Handling & Edge Cases

### Tasks

- [ ] Define error types in `error.rs` with thiserror:
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
- [ ] Implement user-friendly error display:
  - [ ] Suggest actionable fixes
  - [ ] Include relevant context (paths, commits, URLs)
  - [ ] Color error messages (red for errors, yellow for warnings)
- [ ] Handle edge cases:
  - [ ] Empty search results → "No packages found matching 'xyz'"
  - [ ] No index exists → prompt to run `nxv update`
  - [ ] Index exists but bloom filter missing → regenerate or warn
  - [ ] Network timeout → retry with exponential backoff, then fail gracefully
  - [ ] Disk full during download → clean up partial files, clear error
  - [ ] Invalid manifest version → "Please update nxv to the latest version"
  - [ ] Partial delta application failure → rollback transaction
- [ ] Implement verbosity levels:
  - [ ] Default: errors and results only
  - [ ] `-v`: include warnings and progress
  - [ ] `-vv`: include debug info (SQL queries, HTTP requests)
  - [ ] `-q`: errors only, no progress bars
- [ ] Implement Ctrl+C handling for all long operations:
  - [ ] Download: cancel and clean up partial file
  - [ ] Update: save valid state before exit
  - [ ] Search: just exit (no cleanup needed)

### Success Criteria

- [ ] Test: all error variants display user-friendly messages
- [ ] Test: `NoIndex` error suggests `nxv update`
- [ ] Test: `CorruptIndex` error suggests `nxv update --force`
- [ ] Test: network timeout retries 3 times before failing
- [ ] Test: partial download is cleaned up on error
- [ ] Test: `-v` shows more output than default
- [ ] Test: `-q` suppresses progress bars
- [ ] Test: Ctrl+C during download cleans up partial file
- [ ] Integration test: graceful handling of unreachable update server

---

## Phase 10: Final Integration & Polish

### Tasks

- [ ] End-to-end integration tests:
  - [ ] Full user workflow: `nxv update` → `nxv search python` → verify output
  - [ ] Delta update workflow: initial download → new delta available → apply → search
  - [ ] Offline mode: search works without network after initial download
- [ ] End-to-end indexer tests (feature-gated):
  - [ ] Full workflow: clone → index → publish → download → search
  - [ ] Incremental: index → new commits → incremental → publish delta
- [ ] Performance profiling:
  - [ ] Profile search queries with large dataset
  - [ ] Profile index loading time
  - [ ] Profile bloom filter operations
  - [ ] Optimize any bottlenecks (add indexes, tune queries)
- [ ] Documentation:
  - [ ] Update README with complete usage examples
  - [ ] Document all CLI commands and flags in --help
  - [ ] Add CHANGELOG.md
  - [ ] Update CLAUDE.md with new commands
- [ ] CI/CD setup:
  - [ ] GitHub Actions workflow for tests
  - [ ] Workflow for building release binaries (Linux, macOS, Windows)
  - [ ] Workflow for publishing index updates (scheduled, or on nixpkgs update)
- [ ] Release artifacts:
  - [ ] Binary builds for: linux-x86_64, linux-aarch64, darwin-x86_64, darwin-aarch64
  - [ ] Consider static linking for maximum portability
  - [ ] Shell completions (bash, zsh, fish)

### Final Success Criteria

**Build & Quality:**
- [ ] `cargo build --release` succeeds with no errors or warnings
- [ ] `cargo build --release --features indexer` succeeds
- [ ] `cargo clippy -- -D warnings` passes (both default and indexer features)
- [ ] `cargo fmt --check` passes
- [ ] `cargo test` - all unit tests pass
- [ ] `cargo test --features indexer` - all indexer tests pass
- [ ] `cargo test --test integration` - all integration tests pass
- [ ] Release binary size is reasonable (< 15MB without index)
- [ ] Binary runs without external runtime dependencies

**User Commands:**
- [ ] `nxv --help` displays complete, accurate help
- [ ] `nxv --version` displays version
- [ ] `nxv update` downloads index from remote successfully
- [ ] `nxv update` (second run) applies delta or reports "up to date"
- [ ] `nxv update --force` re-downloads full index
- [ ] `nxv search firefox` returns results with colored table output
- [ ] `nxv search python --version 3.11` filters to 3.11.x versions
- [ ] `nxv search --desc "json parser"` searches descriptions
- [ ] `nxv search nonexistent` returns "not found" instantly (bloom filter)
- [ ] `nxv search python --json` outputs valid JSON
- [ ] `nxv search python --plain` outputs plain text (no ANSI)
- [ ] `nxv history python` shows version timeline
- [ ] `nxv history python 3.11.0` shows when that version was available
- [ ] `nxv info` shows accurate index statistics

**Indexer Commands (feature-gated):**
- [ ] `nxv index --nixpkgs-path ./nixpkgs` creates index from local repo
- [ ] `nxv index` (incremental) only processes new commits
- [ ] `nxv index --full` forces full rebuild
- [ ] Index creation is resumable after interrupt

**Robustness:**
- [ ] Graceful handling of Ctrl+C at any point
- [ ] No data corruption on interrupt during update
- [ ] Clear error messages for all failure modes
- [ ] Works offline after initial index download
- [ ] No memory leaks (test with valgrind/heaptrack on large operations)

**Output Quality:**
- [ ] Table output renders correctly in standard 80-column terminal
- [ ] Table output adapts to wider terminals
- [ ] Colors are correct and readable
- [ ] JSON output validates with `jq`
- [ ] Output is consistent and predictable

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

Delta packs are zstd-compressed SQLite dump files containing only new rows:

```sql
-- delta-abc123-def456.sql (before compression)
INSERT OR IGNORE INTO packages (name, version, commit_hash, commit_date, attribute_path, description, license, homepage, maintainers)
VALUES
  ('python', '3.12.0', 'def456...', 1705123456, 'python312', 'Python interpreter', '["Python-2.0"]', 'https://python.org', '["fridh"]'),
  ('nodejs', '21.0.0', 'def456...', 1705123456, 'nodejs_21', 'Node.js runtime', '["MIT"]', 'https://nodejs.org', '["maintainer"]'),
  -- ... more rows
;

UPDATE meta SET value = 'def456...' WHERE key = 'last_indexed_commit';
```

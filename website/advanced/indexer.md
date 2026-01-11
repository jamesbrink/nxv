# Building Indexes

This guide covers building your own nxv index from a local nixpkgs checkout. This is an advanced topic - most users should use `nxv update` to download the pre-built index.

## Prerequisites

- nxv built with the `indexer` feature
- Local nixpkgs clone (~3GB)
- Nix with flakes enabled
- ~50GB free disk space (for Nix store during indexing)

## Building with Indexer Feature

```bash
# Using Nix flakes
nix run github:jamesbrink/nxv#nxv-indexer -- index --help

# From source
cargo build --release --features indexer
```

## Basic Indexing

```bash
# Clone nixpkgs if you haven't
git clone https://github.com/NixOS/nixpkgs.git

# Index from the beginning (slow, ~24 hours)
nxv index --nixpkgs-path ./nixpkgs

# Index a specific date range
nxv index --nixpkgs-path ./nixpkgs --since 2023-01-01 --until 2024-01-01
```

## Incremental Indexing

nxv supports resuming interrupted indexing:

```bash
# Start indexing (Ctrl+C safe)
nxv index --nixpkgs-path ./nixpkgs

# If interrupted, just run again - it resumes from checkpoint
nxv index --nixpkgs-path ./nixpkgs
```

Checkpoints are saved every 100 commits.

## Parallel Year-Range Indexing

For faster indexing on multi-core machines:

```bash
# Index years in parallel (one worker per year)
nxv index --nixpkgs-path ./nixpkgs --parallel-years

# Custom worker count
nxv index --nixpkgs-path ./nixpkgs --parallel-years --workers 8
```

## Memory Management

Indexing requires significant memory for Nix evaluation:

```bash
# Set memory budget (default: 8GB)
nxv index --nixpkgs-path ./nixpkgs --memory-budget 16G

# Per-worker memory limit
nxv index --nixpkgs-path ./nixpkgs --worker-memory 2G
```

## Store Path Extraction

Store paths are only extracted for commits after 2020-01-01 (when cache.nixos.org availability improved):

```bash
# Skip store path extraction (faster, smaller index)
nxv index --nixpkgs-path ./nixpkgs --skip-store-paths

# Force store paths for all commits
nxv index --nixpkgs-path ./nixpkgs --store-paths-since 2017-01-01
```

## Backfilling Metadata

After indexing, fill in missing metadata:

```bash
# Backfill from HEAD (fast, may miss renamed packages)
nxv backfill --nixpkgs-path ./nixpkgs

# Historical backfill (slow, comprehensive)
nxv backfill --nixpkgs-path ./nixpkgs --history
```

## Publishing

Generate compressed artifacts for distribution:

```bash
# Generate manifest, compressed DB, and bloom filter
nxv publish --output-dir ./publish

# Sign with minisign key
nxv publish --output-dir ./publish --secret-key ./nxv.key

# Custom URL prefix for manifest
nxv publish --output-dir ./publish \
  --url-prefix "https://example.com/nxv"
```

Generated files:
- `index.db.zst` - Compressed database (~28MB)
- `bloom.bin` - Bloom filter (~96KB)
- `manifest.json` - Metadata with checksums and signature

## Key Generation

Generate a minisign keypair for signing:

```bash
nxv keygen --output-dir ./keys
```

This creates:
- `nxv.key` - Secret key (keep private!)
- `nxv.pub` - Public key (distribute with your index)

## Database Schema

The index uses SQLite with the following schema:

```sql
package_versions (
    attribute_path TEXT,
    version TEXT,
    version_source TEXT,  -- direct/unwrapped/passthru/name
    first_commit_hash TEXT,
    first_commit_date TEXT,
    last_commit_hash TEXT,
    last_commit_date TEXT,
    description TEXT,
    license TEXT,
    homepage TEXT,
    maintainers TEXT,  -- JSON array
    platforms TEXT,    -- JSON array
    source_path TEXT,
    known_vulnerabilities TEXT,  -- JSON array
    store_paths TEXT,  -- JSON object
    UNIQUE(attribute_path, version)
)
```

## Version Extraction

nxv uses a fallback chain to extract versions:

1. **direct** - `pkg.version`
2. **unwrapped** - `pkg.unwrapped.version` (for wrappers)
3. **passthru** - `pkg.passthru.unwrapped.version`
4. **name** - Parse from `pkg.name` (e.g., "hello-2.12" -> "2.12")

The `version_source` field tracks which method was used.

## Garbage Collection

Long indexing runs accumulate Nix store data. nxv runs garbage collection automatically:

```bash
# Disable automatic GC
nxv index --nixpkgs-path ./nixpkgs --no-gc

# Manual GC threshold (GB of store before GC)
nxv index --nixpkgs-path ./nixpkgs --gc-threshold 100
```

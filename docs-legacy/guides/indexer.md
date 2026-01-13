# Indexer Guide

This guide covers building a local nxv index from a nixpkgs repository. The indexer
traverses git history, extracts package metadata via Nix evaluation, and stores
results in a SQLite database with bloom filter for fast lookups.

## Table of Contents

- [Overview](#overview)
- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [CLI Reference](#cli-reference)
- [Parallel Indexing](#parallel-indexing)
- [Memory Management](#memory-management)
- [Checkpointing and Recovery](#checkpointing-and-recovery)
- [Garbage Collection](#garbage-collection)
- [Validation](#validation)
- [Examples](#examples)
- [Troubleshooting](#troubleshooting)
- [See Also](#see-also)

## Overview

The nxv indexer builds a searchable database of Nix package versions by:

1. Walking nixpkgs git history from 2017 to present
2. Extracting package metadata (version, description, license, platforms) via Nix evaluation
3. Storing results with UPSERT semantics (one row per attribute_path/version pair)
4. Building a bloom filter for instant negative lookups

The indexer is feature-gated and requires building with `--features indexer`:

```bash
cargo build --release --features indexer
# or use the Nix flake
nix build .#nxv-indexer
```

### Why Build Your Own Index?

- Test index changes before publishing
- Index custom nixpkgs forks or branches
- Debug package version extraction issues
- Generate indexes for specific date ranges

## Prerequisites

### Required

- **Nix**: The indexer uses Nix evaluation to extract package metadata
- **nixpkgs clone**: A local checkout of the nixpkgs repository
- **Disk space**: At least 50 GB free for `/nix/store` during indexing
- **Memory**: Minimum 8 GiB RAM (16+ GiB recommended for parallel indexing)

### nixpkgs Setup

Clone nixpkgs if you do not already have a local copy:

```bash
git clone https://github.com/NixOS/nixpkgs.git
cd nixpkgs
git fetch --all
```

Ensure you are on the `nixpkgs-unstable` branch for the latest packages:

```bash
git checkout origin/nixpkgs-unstable
```

## Quick Start

Build a complete index from scratch:

```bash
# Full index (all commits from 2017 to present)
nxv index --nixpkgs-path ./nixpkgs --full

# Incremental update (only new commits since last run)
nxv index --nixpkgs-path ./nixpkgs
```

The index is saved to `~/.local/share/nxv/index.db` (Linux) or
`~/Library/Application Support/nxv/index.db` (macOS).

## CLI Reference

### Required Arguments

| Flag | Description |
|------|-------------|
| `--nixpkgs-path PATH` | Path to local nixpkgs repository clone |

### Indexing Mode

| Flag | Description |
|------|-------------|
| `--full` | Force full rebuild, ignoring any previous checkpoint |
| (default) | Incremental mode: resume from last indexed commit |

### Date Range Filters

| Flag | Description |
|------|-------------|
| `--since DATE` | Only process commits after this date (YYYY-MM-DD or git date string) |
| `--until DATE` | Only process commits before this date (YYYY-MM-DD or git date string) |
| `--max-commits N` | Limit to processing N commits |

### Worker Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--workers N` | Number of systems | Parallel worker processes for Nix evaluation |
| `--max-memory SIZE` | `8G` | Total memory budget for all workers (e.g., `32G`, `16GiB`, `8192M`) |
| `--systems LIST` | `x86_64-linux,aarch64-linux,x86_64-darwin,aarch64-darwin` | Comma-separated systems to evaluate |

### Checkpointing

| Flag | Default | Description |
|------|---------|-------------|
| `--checkpoint-interval N` | `100` | Save checkpoint every N commits |

### Garbage Collection

| Flag | Default | Description |
|------|---------|-------------|
| `--gc-interval N` | `5` | Run GC every N checkpoints (0 to disable) |
| `--gc-min-free-gb N` | `10` | Emergency GC if free space falls below N GB |

### Version Extraction

| Flag | Default | Description |
|------|---------|-------------|
| `--full-extraction-interval N` | `50` | Force full package extraction every N commits |

The `--full-extraction-interval` option is important for catching "wrapper packages"
like Firefox, Chromium, and Thunderbird. These packages define their versions in
separate files (e.g., `pkgs/applications/networking/browsers/firefox/packages.nix`)
but are assigned in `all-packages.nix`. Incremental detection based on changed files
can miss version updates for these packages.

Setting this to `50` (the default) ensures a full extraction happens every 50 commits,
catching any versions that incremental detection might miss. Set to `0` for faster
indexing at the risk of missing some package versions.

### Parallel Range Indexing

| Flag | Default | Description |
|------|---------|-------------|
| `--parallel-ranges SPEC` | (none) | Process year ranges in parallel |
| `--max-range-workers N` | `4` | Maximum concurrent range workers |

### Verbosity

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Show verbose output including extraction warnings |

## Parallel Indexing

For large-scale indexing, the `--parallel-ranges` option processes multiple year
ranges concurrently, each with its own git worktree.

### When to Use Parallel Indexing

- Building a complete index from scratch
- Re-indexing after schema changes
- Machines with many CPU cores and sufficient RAM

### Range Format Specifications

The `--parallel-ranges` flag accepts several formats:

```bash
# Auto-partition: split into N equal ranges
--parallel-ranges 4        # 4 ranges across all years

# Individual years
--parallel-ranges 2017,2018,2019,2020

# Year ranges (end year is inclusive)
--parallel-ranges 2017-2020,2021-2024

# Half-year ranges
--parallel-ranges 2023-H1,2023-H2,2024-H1,2024-H2

# Quarterly ranges
--parallel-ranges 2024-Q1,2024-Q2,2024-Q3,2024-Q4
```

### How Parallel Indexing Works

1. Each range gets its own isolated git worktree
2. Each range worker spawns its own Nix evaluation workers
3. Results merge into the shared database via UPSERT semantics
4. Per-range checkpoints allow resumption if interrupted

### Resource Calculation

Memory is divided among all workers:

```text
total_workers = max_range_workers x systems_count
per_worker_memory = max_memory / total_workers
```

For example, with `--max-memory=32G --max-range-workers=4` and 4 systems:

```text
total_workers = 4 x 4 = 16
per_worker_memory = 32 GiB / 16 = 2 GiB
```

Each worker requires at least 512 MiB. The indexer will error if the memory budget
is insufficient.

## Memory Management

The indexer uses a memory budget system to prevent out-of-memory conditions during
Nix evaluation.

### Memory Budget Model

The `--max-memory` flag sets the total memory budget for all workers combined.
This is automatically divided among workers:

```bash
# 32 GiB total, with 4 systems = 8 GiB per worker
nxv index --nixpkgs-path ./nixpkgs --max-memory 32G

# Plain numbers are treated as MiB for backwards compatibility
nxv index --nixpkgs-path ./nixpkgs --max-memory 8192  # 8 GiB
```

### Supported Size Formats

| Format | Example | Meaning |
|--------|---------|---------|
| Plain number | `8192` | MiB (backwards compatibility) |
| With G suffix | `32G`, `32GB`, `32GiB` | Gibibytes |
| With M suffix | `1024M`, `1024MB`, `1024MiB` | Mebibytes |
| Fractional | `1.5G` | 1.5 GiB = 1536 MiB |

### Minimum Requirements

- **Minimum per worker**: 512 MiB
- **Default budget**: 8 GiB
- **Recommended**: 16-32 GiB for parallel indexing

If the budget is too small for the number of workers, the indexer will exit with
an error explaining the constraint.

## Checkpointing and Recovery

The indexer saves progress at regular intervals, enabling resumption after
interruption.

### How Checkpointing Works

1. Every `--checkpoint-interval` commits (default: 100), the indexer:
   - Flushes pending UPSERT operations to the database
   - Records the last processed commit hash
   - Calls `PRAGMA wal_checkpoint` to persist WAL data

2. On Ctrl+C:
   - The indexer catches the signal gracefully
   - Saves checkpoint before exiting
   - Prints a message indicating safe resume is possible

### Resuming After Interruption

Simply run the same command again:

```bash
# First run (interrupted)
nxv index --nixpkgs-path ./nixpkgs --full
# ... Ctrl+C pressed

# Resume from checkpoint
nxv index --nixpkgs-path ./nixpkgs
```

The indexer automatically detects the last checkpoint and resumes from there.

### Per-Range Checkpoints

When using `--parallel-ranges`, each range maintains its own checkpoint. The indexer
uses the pattern `checkpoint_{range_label}` (e.g., `checkpoint_2023`) to track
progress per range.

## Garbage Collection

During indexing, millions of derivation files accumulate in `/nix/store`. The indexer
manages this through periodic garbage collection.

### Automatic GC

The indexer runs `nix-collect-garbage -d` automatically based on two triggers:

1. **Periodic**: Every `--gc-interval` checkpoints (default: 5, meaning every 500 commits)
2. **Emergency**: When free disk space falls below `--gc-min-free-gb` (default: 10 GB)

### Temp Eval Store Cleanup

The indexer uses a temporary eval store (`/tmp/nxv-eval-store`) to isolate derivations.
This store is cleaned up:

- At indexer startup
- After each garbage collection cycle
- When the indexer exits

### Disabling GC

For faster indexing on systems with ample disk space:

```bash
nxv index --nixpkgs-path ./nixpkgs --gc-interval 0
```

This disables periodic GC but emergency GC still triggers if disk space runs low.

## Validation

After building an index, validate data quality before publishing.

### Quick Validation

```bash
# Check database statistics
nxv stats

# Spot-check specific packages
nxv search firefox
nxv history python3
```

### Comprehensive Validation

Use the post-reindex validation script:

```bash
./scripts/post_reindex_validation.sh --db ~/.local/share/nxv/index.db

# With nixpkgs path for commit verification
./scripts/post_reindex_validation.sh \
  --db ~/.local/share/nxv/index.db \
  --nixpkgs ./nixpkgs

# Quick mode (fewer packages)
./scripts/post_reindex_validation.sh --quick

# Full mode (comprehensive)
./scripts/post_reindex_validation.sh --full
```

### Validation Checks

The script performs these checks:

1. **Database Statistics**: Total records, unique packages, schema version
2. **Version Source Distribution**: Breakdown by extraction method (direct/unwrapped/passthru/name)
3. **Unknown Version Analysis**: Percentage of packages with unknown versions
4. **Post-July 2023 Coverage**: Validates data after a known problematic period
5. **Index Gap Detection**: Checks for missing months in the data
6. **NixHub Comparison**: Compares against NixHub API for completeness
7. **Critical Package Spot Check**: Verifies important packages like Firefox, Python, Node.js
8. **Wrapper Package Coverage**: Ensures packages with versions in separate files are indexed

### Quality Targets

- Completeness vs NixHub: >= 80%
- Unknown version rate: < 10%
- Wrapper packages: >= 10 versions each for Firefox, Chromium, Thunderbird

## Examples

### Full Index from Scratch

Build a complete index with all commits from 2017 to present:

```bash
nxv index --nixpkgs-path ./nixpkgs --full --max-memory 16G
```

### Incremental Update

Update an existing index with new commits:

```bash
nxv index --nixpkgs-path ./nixpkgs
```

### Re-index a Specific Date Range

Index only commits from 2024:

```bash
nxv index --nixpkgs-path ./nixpkgs --full \
  --since 2024-01-01 --until 2025-01-01
```

### Parallel Quarterly Indexing

Index with 4 quarterly ranges using 32 GiB memory:

```bash
nxv index --nixpkgs-path ./nixpkgs --full \
  --parallel-ranges 2023-Q1,2023-Q2,2023-Q3,2023-Q4,2024-Q1,2024-Q2,2024-Q3,2024-Q4 \
  --max-range-workers 4 \
  --max-memory 32G
```

### Index Only Recent Commits

Process only the last 1000 commits:

```bash
nxv index --nixpkgs-path ./nixpkgs --max-commits 1000
```

### Fast Indexing (Skip Full Extraction)

Faster indexing at the risk of missing some wrapper package versions:

```bash
nxv index --nixpkgs-path ./nixpkgs --full \
  --full-extraction-interval 0
```

### Minimal Memory Usage

For systems with limited RAM (8 GiB):

```bash
nxv index --nixpkgs-path ./nixpkgs \
  --workers 2 \
  --max-memory 4G
```

## Troubleshooting

### Out of Memory Errors

**Symptom**: Workers crash with OOM or the system becomes unresponsive.

**Solution**: Reduce workers or increase memory budget:

```bash
# Reduce workers
nxv index --nixpkgs-path ./nixpkgs --workers 2

# Or increase budget
nxv index --nixpkgs-path ./nixpkgs --max-memory 32G
```

### Disk Space Exhaustion

**Symptom**: Indexing fails with "no space left on device" errors.

**Solution**: Increase GC frequency or free space threshold:

```bash
nxv index --nixpkgs-path ./nixpkgs \
  --gc-interval 2 \
  --gc-min-free-gb 20
```

### Repository HEAD Behind Last Indexed

**Symptom**: Error "Repository HEAD is older than last indexed commit".

**Cause**: The nixpkgs checkout was reset or is out of date.

**Solution**: Update nixpkgs or force a full rebuild:

```bash
# Update nixpkgs
cd ./nixpkgs && git fetch origin && git checkout origin/nixpkgs-unstable

# Or force rebuild
nxv index --nixpkgs-path ./nixpkgs --full
```

### Missing Wrapper Package Versions

**Symptom**: Packages like Firefox or Chromium have fewer versions than expected.

**Cause**: Incremental detection missed version changes in separate files.

**Solution**: Re-index with full extraction enabled:

```bash
nxv index --nixpkgs-path ./nixpkgs --full \
  --full-extraction-interval 50
```

### Nix Store Corruption

**Symptom**: Nix evaluation errors or "corrupt store" messages.

**Solution**: Verify and repair the store:

```bash
nix-store --verify --repair
```

### Stale Worktrees

**Symptom**: Git errors about existing worktrees.

**Solution**: Clean up orphaned worktrees:

```bash
nxv reset --nixpkgs-path ./nixpkgs
```

## Indexer Run History

Reference metrics from completed indexing runs.

### 2026-01-12: 2020 Gap Fill Re-index

**Configuration:**
- Mode: Incremental with gap detection
- Date range: 2020-01-01 to 2020-12-31
- Workers: 4 (2048 MiB each)
- Memory budget: 8 GiB
- Full extraction interval: 50 commits
- GC interval: 5 checkpoints

**Results:**

| Metric | Value |
|--------|-------|
| Commits processed | 6,277 |
| Total packages found | 5,601,806 |
| Packages upserted | 5,601,806 |
| Unique package names | 51,374 |
| Duration | ~5.5 hours |
| Final DB records | 205,486 |
| Date coverage | 2017-01-01 to 2021-01-02 |

**Notes:**
- Gap detection identified 6,277 commits with package changes in 2020
- Batching fix (BATCH_SIZE=500) prevented memory exhaustion during full extraction
- Periodic full extraction (every 50 commits) ensured wrapper packages captured
- Firefox captured: 62 total versions (2 new from 2020: 72.0.1, 76.0.1)

## See Also

- [Database Optimization](../database-optimization.md) - SQLite tuning for large indexes
- [Troubleshooting Guide](../troubleshooting.md) - General troubleshooting
- [NixHub API Reference](../nixhub-api.md) - Compare against external data source

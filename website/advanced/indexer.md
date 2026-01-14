# Building Indexes

This guide covers building your own nxv index from a local nixpkgs checkout.
This is an advanced topic for users who want to:

- Create indexes for custom nixpkgs forks
- Build indexes with different date ranges
- Self-host their own nxv infrastructure
- Contribute to the official index

::: tip Most Users If you just want to use nxv, run `nxv update` to download the
pre-built index. Building your own index takes 24+ hours and significant
resources. :::

## Prerequisites

### Software Requirements

- **nxv with indexer feature** - The indexer is feature-gated to keep the main
  binary small
- **Nix** - With flakes enabled (for evaluation)
- **Git** - For cloning and traversing nixpkgs history

### Hardware Requirements

| Resource | Minimum    | Recommended         |
| -------- | ---------- | ------------------- |
| RAM      | 8 GB       | 32 GB               |
| Disk     | 50 GB free | 100 GB free         |
| CPU      | 4 cores    | 8+ cores            |
| Time     | 24 hours   | 12 hours (parallel) |

### Getting the Indexer

```bash
# Using Nix flakes (recommended)
nix run github:jamesbrink/nxv#nxv-indexer -- --help

# From source
cargo build --release --features indexer
./target/release/nxv index --help
```

### Cloning nixpkgs

```bash
# Full clone (~3 GB)
git clone https://github.com/NixOS/nixpkgs.git

# Or shallow clone for faster download (limits --since range)
git clone --depth 10000 https://github.com/NixOS/nixpkgs.git
```

## Indexing Workflow

### Full Index (First Time)

A full index processes all nixpkgs commits since 2017-01-01:

```bash
nxv index --nixpkgs-path ./nixpkgs
```

This takes 24-48 hours depending on hardware. Progress is checkpointed every 100
commits, so you can safely interrupt with Ctrl+C and resume later.

### Resuming Interrupted Indexing

Just run the same command again:

```bash
# Picks up from the last checkpoint automatically
nxv index --nixpkgs-path ./nixpkgs
```

To force a fresh start (ignoring checkpoints):

```bash
nxv index --nixpkgs-path ./nixpkgs --full
```

### Parallel Year-Range Indexing

For faster builds on multi-core machines, process year ranges in parallel:

```bash
# Auto-partition into 4 concurrent ranges
nxv index --nixpkgs-path ./nixpkgs --parallel-ranges 4 --max-memory 32G

# Or specify explicit ranges
nxv index --nixpkgs-path ./nixpkgs \
  --parallel-ranges "2017-2019,2020-2022,2023-2024"
```

This can reduce build time by 2-3x on systems with 8+ cores and sufficient RAM.

### Indexing a Specific Date Range

To index only recent commits:

```bash
# Only 2024 onwards
nxv index --nixpkgs-path ./nixpkgs --since 2024-01-01

# Specific range
nxv index --nixpkgs-path ./nixpkgs --since 2023-01-01 --until 2024-01-01
```

## Memory Management

The indexer spawns worker processes for parallel Nix evaluation. Memory is
managed with a total budget that's divided among workers.

```bash
# Default: 8 GiB total, auto-divided among workers
nxv index --nixpkgs-path ./nixpkgs

# Increase for faster processing
nxv index --nixpkgs-path ./nixpkgs --max-memory 32G
```

### Memory Budget Allocation

| Total Budget | Workers | Per Worker |
| ------------ | ------- | ---------- |
| 8 GiB        | 4       | 2 GiB      |
| 16 GiB       | 4       | 4 GiB      |
| 32 GiB       | 8       | 4 GiB      |

The minimum per-worker allocation is 512 MiB. Indexing will fail if the budget
can't meet this threshold.

### Memory Format

Human-readable sizes are supported:

- `8G`, `8GB`, `8GiB` - 8 gibibytes
- `1024M`, `1024MB` - 1024 mebibytes
- `6144` - 6144 mebibytes (backwards compatibility)
- `1.5G` - 1.5 gibibytes

## Backfilling Metadata

After indexing, some metadata may be missing (source paths, homepages,
vulnerability info). Use `backfill` to update:

### HEAD Mode (Default)

Extracts from the current nixpkgs checkout. Fast but may miss renamed/removed
packages:

```bash
nxv backfill --nixpkgs-path ./nixpkgs
```

### Historical Mode

Traverses git history to find each package's original commit. Slower but
complete:

```bash
nxv backfill --nixpkgs-path ./nixpkgs --history
```

### Selective Backfill

Update only specific fields:

```bash
# Only source paths
nxv backfill --nixpkgs-path ./nixpkgs --fields source-path

# Multiple fields
nxv backfill --nixpkgs-path ./nixpkgs --fields source-path,homepage
```

## Publishing

Generate compressed artifacts for distribution:

```bash
# Basic publish
nxv publish --output ./publish

# With signing (recommended)
nxv keygen  # Creates nxv.key and nxv.pub
nxv publish --output ./publish --sign --secret-key ./nxv.key
```

### Generated Artifacts

| File            | Size   | Description                           |
| --------------- | ------ | ------------------------------------- |
| `index.db.zst`  | ~28 MB | Zstd-compressed SQLite database       |
| `bloom.bin`     | ~96 KB | Bloom filter for fast lookups         |
| `manifest.json` | ~1 KB  | Metadata with checksums and signature |

### Hosting

Upload artifacts to any HTTP server. Update your manifest's `url_prefix`:

```bash
nxv publish --output ./publish \
  --url-prefix "https://example.com/nxv" \
  --sign --secret-key ./nxv.key
```

Users can then configure their nxv to use your index:

```bash
export NXV_MANIFEST_URL="https://example.com/nxv/manifest.json"
export NXV_PUBLIC_KEY="RWTxxxxxxxx..."
nxv update
```

## Architecture Deep Dive

### Nix FFI Evaluation

The indexer uses Nix's C API directly (via `nix-bindings` crate) rather than
spawning `nix eval` processes. This provides:

- **Persistent evaluator** - Single initialization (~2-3s) amortized across all
  evaluations
- **Large stack (64 MB)** - Required for deep nixpkgs evaluation
- **Memory management** - Values managed by Nix's garbage collector

### Worker Pool Subprocess Model

Since the Nix C API is single-threaded, parallelism is achieved through
subprocesses:

```
┌─────────────┐
│   Indexer   │
│  (main)     │
└──────┬──────┘
       │ spawn
       ├──────────┬──────────┬──────────┐
       ▼          ▼          ▼          ▼
┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
│ Worker 1 │ │ Worker 2 │ │ Worker 3 │ │ Worker 4 │
│ x86_64   │ │ aarch64  │ │ x86_64   │ │ aarch64  │
│ linux    │ │ linux    │ │ darwin   │ │ darwin   │
└──────────┘ └──────────┘ └──────────┘ └──────────┘
```

Each worker:

- Has its own Nix evaluator instance
- Processes one system architecture
- Communicates via IPC (serialized work requests/responses)
- Automatically restarts on crash or memory exhaustion

### Version Extraction Fallback Chain

Not all packages expose versions the same way. The indexer tries multiple
sources:

| Priority | Source                           | Example                   |
| -------- | -------------------------------- | ------------------------- |
| 1        | `pkg.version`                    | Most packages             |
| 2        | `pkg.unwrapped.version`          | Wrapper packages (neovim) |
| 3        | `pkg.passthru.unwrapped.version` | Passthru metadata         |
| 4        | Parse from `pkg.name`            | `"hello-2.12"` → `"2.12"` |

The `version_source` field in the database tracks which method was used,
enabling debugging without re-indexing.

### all-packages.nix Optimization

The file `pkgs/top-level/all-packages.nix` changes frequently but usually
affects only a few packages. Instead of extracting all ~18,000 packages on every
commit:

1. Parse the git diff for changed lines
2. Extract affected attribute names (assignment patterns, inherit statements)
3. Evaluate only those specific packages
4. Average: ~7 packages per commit vs 18,000

This optimization provides 100x+ speedup for incremental indexing.

### Checkpointing and Ctrl+C Safety

Progress is saved every 100 commits (configurable via `--checkpoint-interval`):

- **Checkpoint data**: Last indexed commit hash, date, statistics
- **Atomic writes**: Database commits are transactional
- **Signal handling**: Ctrl+C triggers graceful shutdown with checkpoint save
- **Resume**: Next run reads checkpoint and continues from last position

## Database Schema

The index uses SQLite with this schema (version 4):

```sql
CREATE TABLE package_versions (
    attribute_path TEXT,           -- e.g., "python311"
    version TEXT,                  -- e.g., "3.11.4"
    version_source TEXT,           -- direct/unwrapped/passthru/name
    first_commit_hash TEXT,        -- Earliest commit with this version
    first_commit_date TEXT,        -- RFC3339 timestamp
    last_commit_hash TEXT,         -- Latest commit with this version
    last_commit_date TEXT,         -- RFC3339 timestamp
    description TEXT,
    license TEXT,                  -- JSON array
    homepage TEXT,
    maintainers TEXT,              -- JSON array
    platforms TEXT,                -- JSON array
    source_path TEXT,              -- e.g., "pkgs/tools/foo/default.nix"
    known_vulnerabilities TEXT,    -- JSON array of CVEs
    store_path TEXT,               -- Only for commits >= 2020-01-01
    is_insecure BOOLEAN,
    UNIQUE(attribute_path, version)
);

-- Full-text search index (auto-synced via triggers)
CREATE VIRTUAL TABLE package_versions_fts USING fts5(description);

-- Metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
```

### Key Design: One Row Per Version

Each `(attribute_path, version)` pair has exactly one row. When the same version
appears in multiple commits:

- `first_commit_*` tracks the earliest appearance
- `last_commit_*` tracks the latest appearance

This provides version timeline information without row explosion.

### Store Path Extraction

Store paths are only extracted for commits after 2020-01-01, when
cache.nixos.org availability became reliable. Earlier commits may have packages
that aren't in the binary cache.

## Garbage Collection

Long indexing runs accumulate Nix store data (.drv files, build outputs).
Automatic garbage collection prevents disk exhaustion:

```bash
# Default: GC every 5 checkpoints (500 commits)
nxv index --nixpkgs-path ./nixpkgs

# More frequent GC
nxv index --nixpkgs-path ./nixpkgs --gc-interval 2

# Disable automatic GC
nxv index --nixpkgs-path ./nixpkgs --gc-interval 0
```

## Troubleshooting

### "Insufficient memory" Error

The memory budget is too small for the number of workers. Either:

- Increase `--max-memory`
- Reduce `--workers` count
- Reduce `--systems` to fewer architectures

### Stuck on a Commit

Some commits may have evaluation issues. Skip them with `--max-commits`:

```bash
# Process only next 100 commits then stop
nxv index --nixpkgs-path ./nixpkgs --max-commits 100
```

### Database Corruption

If the database becomes corrupted after a crash:

```bash
# Reset to fresh state
rm ~/.local/share/nxv/index.db  # Linux
rm ~/Library/Application\ Support/nxv/index.db  # macOS

# Re-run indexing
nxv index --nixpkgs-path ./nixpkgs
```

### nixpkgs Repository Issues

If the nixpkgs clone is in an inconsistent state:

```bash
# Reset to known good state
nxv reset --nixpkgs-path ./nixpkgs --fetch
```

## CLI Reference

For complete command documentation, see the
[Indexer CLI Reference](/advanced/indexer-cli).

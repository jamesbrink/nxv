# Building Indexes

This guide covers building your own nxv index from nixpkgs channel-release
snapshots. This is an advanced topic for users who want to:

- Build indexes with different date ranges or channels
- Self-host their own nxv infrastructure
- Contribute to the official index

::: tip Most Users

If you just want to use nxv, run `nxv sync` to download the pre-built index. A
full rebuild from scratch streams ~15 GB of snapshots and takes a few hours.

:::

## How It Works

The indexer does **not** walk nixpkgs git history and does **not** need a
nixpkgs checkout. It ingests channel-release snapshots from releases.nixos.org:

- **2020-06 onward**: every channel release ships a `packages.json.br` (~380 MB
  decompressed) that enumerates all ~144k attributes — including nested package
  sets (`python3Packages.*`, `haskellPackages.*`, `nodePackages.*`, ...) — with
  versions and metadata. No Nix evaluation is needed for this era.
- **2020-03-27 → 2020-06**: `packages.json.br` first appears on 2020-03-27, but
  the boundary falls mid-release-name (`20.09pre`), so the source is settled by
  probing each release rather than inferred from its date. Ordinary runs cover
  this window — a probe costs one request and no evaluation.
- **2016-09 → 2020-03-27**: releases predate `packages.json` entirely. Opt in
  with `--backfill-evals` to evaluate each release's `nixexprs.tar.xz` with
  `nix-env` (the only path that requires `nix`).

Every stored commit is a real Hydra-built channel commit: the
`(attribute, version)` pair was verifiably present at both ends of its range.

## Prerequisites

### Software Requirements

- **nxv with indexer feature** - The indexer is feature-gated to keep the main
  binary small
- **Nix** - Only for `--backfill-evals` (pre-2020 era) and `--head-eval`; the
  packages.json era needs no Nix at all
- **Network access** to releases.nixos.org

No git, no nixpkgs clone.

### Hardware Requirements

| Resource | Full rebuild                                                     |
| -------- | ---------------------------------------------------------------- |
| RAM      | ~2 GB (default 4 parallel workers)                               |
| Disk     | ~5 GB free (the raw database is ~2 GB)                           |
| Network  | ~15 GB streamed (snapshots are parsed in flight, never hit disk) |
| Time     | A few hours for 2020→present; +1.5-3 h with `--backfill-evals`   |

Incremental runs (the normal case) take seconds to minutes — a typical 6-hour
window sees 0-2 new releases.

### Getting the Indexer

```bash
# Using Nix flakes (recommended)
nix run github:utensils/nxv#nxv-indexer -- --help

# From source
cargo build --release --features indexer
./target/release/nxv index --help
```

## Indexing Workflow

### Full Index (First Time)

```bash
nxv index
```

This ingests every release of the default channels: `nixpkgs-unstable` (the
historical spine, back to 2016) and `nixos-unstable-small` (the currency
channel, typically hours behind master).

To also cover the pre-2020 era (requires `nix`):

```bash
nxv index --backfill-evals
```

Interrupting with Ctrl+C is safe: each release commits atomically together with
its row in the `releases` ledger, so unfinished releases simply stay `pending`.

### Reading the Pending Count

`nxv stats` and the end-of-run report split `pending` releases into the ones a
run will pick up and the ones it structurally cannot:

```
nixos-unstable-small: 4017 ingested, 3099 pending, 1 skipped (newest: …); 2902 pre-2020, needs --backfill-evals
nixpkgs-unstable: 4191 ingested, 22 failed, 1 skipped (newest: …); 22 pre-2020, needs --backfill-evals
```

The trailing clause counts pending **and** failed releases from the nix-env era.
Neither is a backlog and neither shrinks on its own — no scheduled run includes
them, and `--retry-failed` alone does not reach them (it takes
`--retry-failed --backfill-evals`). It is a standing choice to trade a decade of
coarse historical coverage against hours of `nix-env` evaluation. Everything
outside that clause is genuine queued work the next run will attempt.

### Resuming and Incremental Updates

Just run the same command again — the `releases` ledger tracks what has already
been ingested, and only new (or still-pending) releases are processed:

```bash
nxv index
```

To re-queue every known release:

```bash
nxv index --full
```

To retry releases that previously failed:

```bash
nxv index --retry-failed
```

### Indexing a Specific Date Range

To ingest only releases in a date window:

```bash
# Only 2024 onwards
nxv index --since 2024-01-01

# Specific range
nxv index --since 2023-01-01 --until 2024-01-01
```

### Data-Quality Gates

Monitors check each snapshot **before** anything is written: attribute count
floors, rolling baselines, sentinel packages (firefox, thunderbird, nh,
python3Packages.requests), birth/death anomalies, and head lag. With `--strict`,
warnings become fatal (this is what CI uses). `--report report.json` writes the
end-of-run coverage report.

### Staying Current During Channel Stalls

When the channels lag behind nixpkgs master, `--head-eval` evaluates master HEAD
directly from a GitHub tarball (requires `nix`):

```bash
nxv index --strict --head-eval --report report.json
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

| File                    | Size    | Description                        |
| ----------------------- | ------- | ---------------------------------- |
| `index.db.zst`          | ~220 MB | Zstd-compressed SQLite database    |
| `bloom.bin`             | ~330 KB | Bloom filter for fast lookups      |
| `manifest.json`         | ~1 KB   | Metadata with checksums            |
| `manifest.json.minisig` | ~1 KB   | minisign signature (when `--sign`) |

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
nxv sync
```

## Architecture Deep Dive

### Pipeline

Each run is: plan → parallel fetch/parse → in-order gating → aggregated upserts
→ finish.

1. **Plan**: list the channel's release directories on the releases.nixos.org S3
   bucket and diff against the `releases` ledger; new directories become
   `pending` rows.
2. **Fetch/parse**: workers stream each `packages.json.br`, decompressing brotli
   and parsing JSON in flight — the ~380 MB document is never materialized in
   memory or on disk.
3. **Gate**: monitors validate each snapshot in release order before any write.
4. **Upsert**: widen-only — an existing `(attribute_path, version)` row only
   ever has its `first_commit_*` moved earlier or its `last_commit_*` moved
   later. Each release commits atomically with its ledger row.
5. **Finish**: rebuild the bloom filter, rebuild FTS (bulk runs drop the
   triggers and rebuild from scratch), and update watermarks.

### Range Semantics

A row means "this `(attribute, version)` pair was **observed** at `first_commit`
and at `last_commit`" — both real Hydra-built channel commits. Interior presence
is interpolated, not guaranteed: a version that lives shorter than one channel
advance can be missed entirely.

### Interruption Safety

There is no checkpoint file. Ctrl+C triggers a graceful shutdown; releases that
didn't finish stay `pending` in the ledger and the next run picks them up.
Failed releases are parked with retry backoff instead of aborting the run.

## Database Schema

The index uses SQLite with this schema (version 4):

```sql
CREATE TABLE package_versions (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,            -- Short package name, e.g. "python3"
    version TEXT NOT NULL,         -- e.g., "3.11.4"
    first_commit_hash TEXT NOT NULL,  -- Earliest channel commit observed with this version
    first_commit_date INTEGER NOT NULL,  -- Unix timestamp (seconds)
    last_commit_hash TEXT NOT NULL,      -- Latest channel commit observed with this version
    last_commit_date INTEGER NOT NULL,   -- Unix timestamp (seconds)
    attribute_path TEXT NOT NULL,  -- e.g., "python3Packages.requests"
    description TEXT,
    license TEXT,                  -- JSON array
    homepage TEXT,
    maintainers TEXT,              -- JSON array
    platforms TEXT,                -- JSON array
    source_path TEXT,              -- e.g., "pkgs/tools/foo/default.nix"
    known_vulnerabilities TEXT,    -- JSON array of CVEs
    UNIQUE(attribute_path, version),
    CHECK(first_commit_date <= last_commit_date)
);

-- Channel-release ingestion ledger: which snapshots have been ingested,
-- which are pending/failed, and their retry/backoff state
CREATE TABLE releases (
    id INTEGER PRIMARY KEY,
    channel TEXT NOT NULL,
    release_name TEXT NOT NULL,
    commit_hash TEXT NOT NULL,
    commit_count INTEGER,
    release_date INTEGER NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending/ingested/failed/skipped
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER,
    attr_count INTEGER,
    error TEXT,
    ingested_at INTEGER,
    UNIQUE(channel, release_name)
);

-- Full-text search index (auto-synced via triggers)
CREATE VIRTUAL TABLE package_versions_fts
USING fts5(name, description, content=package_versions, content_rowid=id);

-- Cover prefix and prefix+version candidate ranking without fetching full rows
CREATE INDEX idx_packages_search_nocase ON package_versions(
    attribute_path COLLATE NOCASE,
    version COLLATE NOCASE,
    (LENGTH(attribute_path) - LENGTH(REPLACE(attribute_path, '.', ''))),
    last_commit_date DESC,
    first_commit_date DESC
);

-- Metadata
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

### Key Design: One Row Per Version Range

A given `(attribute_path, version)` pair is exactly one row, enforced by the
`UNIQUE(attribute_path, version)` constraint. Re-observing a pair only widens
its range. `nxv dedupe` exists to repair databases built by pre-0.1.5 indexer
versions that left duplicate rows behind.

Prefix searches first rank compact entries from `idx_packages_search_nocase`,
then fetch at most 5,000 full rows by primary key. Bulk ingestion drops this
covering index to avoid write amplification and rebuilds it transactionally. If
a run is interrupted, the next writable run repairs it; publishing also repairs
the index or refuses to emit a slow artifact.

### Insecure Packages

`is_insecure` is not stored as a column — it's derived at query time from
`known_vulnerabilities` (a non-empty JSON array means the package is flagged
insecure). The HTTP API's version-history endpoint surfaces this as a boolean
alongside the advisory text in `vulnerabilities`.

Version history resolves the advisory by **package name and version**, not by
attribute path and not by version string alone:

- Version string alone would be wrong in the obvious direction — `nxv` 0.7.1 has
  nothing to do with `zeronet` 0.7.1.
- Attribute path alone would be wrong in the subtle direction. Each
  `(attribute_path, version)` row keeps the `known_vulnerabilities` of its newest
  observation and is never backfilled, so an advisory added after an attribute
  stopped shipping a version never reaches that row. `emacs` 28.2 retired in
  2023 holding no advisory while `emacs28` 28.2 carried the identical build until
  2025 and picked up CVE-2024-53920.

Name scoping answers "does this software version have a known advisory". That is
a broader question than "will nix refuse to build this attribute at this
revision" — for the latter, read `known_vulnerabilities` off the version itself
(`/packages/{attr}/versions/{version}/first`), which is what the copy-to-clipboard
commands use to decide on `NIXPKGS_ALLOW_INSECURE`.

## Troubleshooting

### A Release Fails to Ingest

Failed releases don't abort the run — they're parked in the `releases` ledger
with retry backoff. Retry them explicitly:

```bash
nxv index --retry-failed
```

A handful of historical releases are genuinely defective upstream (broken 2018
evaluations, corrupt 16.09/17.03 re-uploads) and will stay parked as skipped —
that's expected.

### Database Corruption

If the database becomes corrupted after a crash:

```bash
# Reset to fresh state
rm ~/.local/share/nxv/index.db  # Linux
rm ~/Library/Application\ Support/nxv/index.db  # macOS

# Re-run indexing (or `nxv sync` to re-download the pre-built index)
nxv index
```

### Pre-2020 Versions Missing

The pre-2020 era is opt-in because it requires `nix` and takes 1.5-3 hours:

```bash
nxv index --backfill-evals
```

### Using a Mirror

Set `NXV_RELEASES_URL` to point the indexer at an alternate releases.nixos.org
S3 endpoint (tests, mirrors).

## CLI Reference

For complete command documentation, see the
[Indexer CLI Reference](/advanced/indexer-cli).

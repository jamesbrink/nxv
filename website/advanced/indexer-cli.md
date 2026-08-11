# Indexer CLI Reference

Complete documentation for nxv indexer commands. These commands are only
available when nxv is built with the `indexer` feature.

::: warning Feature-Gated

These commands require `nxv-indexer` or building with `--features indexer`. Most
users should use `nxv sync` to download the pre-built index instead.

:::

## index

Build the package index from nixpkgs channel-release snapshots
(releases.nixos.org). No nixpkgs checkout is needed; `nix` is only required for
`--backfill-evals` and `--head-eval`.

```bash
nxv index [OPTIONS]
```

::: tip Verbose Logging

Use global `-v` for debug logging (`nxv -v index ...`). The `-v` flag must come
before the subcommand.

:::

### Options

| Flag                   | Default     | Description                                                                     |
| ---------------------- | ----------- | ------------------------------------------------------------------------------- |
| `--channel <CHANNELS>` | see below   | Channels to ingest (repeatable or comma-separated)                              |
| `--since <DATE>`       | -           | Only ingest releases dated on/after this date (YYYY-MM-DD)                      |
| `--until <DATE>`       | -           | Only ingest releases dated on/before this date                                  |
| `--jobs <N>`           | CPUs, max 4 | Parallel snapshot download/parse workers                                        |
| `--strict`             | `false`     | Treat monitor warnings (count floors, sentinels, head lag) as fatal             |
| `--report <PATH>`      | -           | Write the end-of-run coverage report as JSON to this path                       |
| `--retry-failed`       | `false`     | Retry releases that were parked as failed/skipped                               |
| `--backfill-evals`     | `false`     | Also ingest the pre-2020-03-27 era via `nix-env` over `nixexprs.tar.xz`, and allow a `nix-env` fallback for any later release missing `packages.json.br` (needs `nix`) |
| `--head-eval`          | `false`     | Evaluate nixpkgs master HEAD when channel observations lag (needs `nix`)        |
| `--full`               | `false`     | Re-queue every known release instead of only new ones                           |
| `--max-releases <N>`   | -           | Limit the number of releases ingested this run (for testing)                    |

### Default Channels

When `--channel` is not specified, these 2 channels are ingested:

- `nixpkgs-unstable` - the historical spine (S3 history back to 2016)
- `nixos-unstable-small` - the currency channel (typically hours behind master)

Any `nixos-*` channel name is also accepted (e.g. `nixos-24.05`).

### Examples

```bash
# Full index from scratch (packages.json era; a few hours)
nxv index

# Also cover the pre-2020 era (requires nix; ~1.5-3 h extra)
nxv index --backfill-evals

# Incremental update (only new releases; the normal case)
nxv index

# CI-style run: gates fatal, head fallback, coverage report
nxv index --strict --head-eval --report report.json

# Only one channel, only 2024 releases
nxv index --channel nixos-unstable-small --since 2024-01-01

# Retry previously failed releases
nxv index --retry-failed
```

### Environment

`NXV_RELEASES_URL` overrides the releases.nixos.org S3 endpoint (tests,
mirrors).

---

## backfill and reset (retired)

The git-walking indexer's `nxv backfill` and `nxv reset` subcommands are
retired: package metadata now comes from channel snapshots (see `nxv index`),
and the snapshot indexer does not use a local nixpkgs checkout. Both remain as
hidden stubs that print a deprecation notice.

---

## dedupe

Collapse duplicate `(attribute_path, version)` rows in the index. Repairs
databases bloated by the pre-0.1.5 incremental indexer bug; current-schema
databases enforce uniqueness and don't need it. Keeps one row per unique pair
with the earliest `first_commit_*` and the latest `last_commit_*`, then VACUUMs.

```bash
nxv dedupe [OPTIONS]
```

### Options

| Flag          | Default | Description                                        |
| ------------- | ------- | -------------------------------------------------- |
| `--dry-run`   | `false` | Report what would change without modifying the DB  |
| `--no-vacuum` | `false` | Skip the trailing VACUUM (faster, DB won't shrink) |

---

## publish

Generate publishable index artifacts: compressed database, bloom filter, and
signed manifest.

```bash
nxv publish [OPTIONS]
```

### Options

| Flag                 | Default        | Description                                                           |
| -------------------- | -------------- | --------------------------------------------------------------------- |
| `--output <DIR>`     | `./publish`    | Output directory for artifacts                                        |
| `--url-prefix <URL>` | -              | Base URL for manifest download links                                  |
| `--sign`             | `false`        | Sign manifest with minisign                                           |
| `--secret-key <KEY>` | -              | Minisign secret key: file path or raw content (also `NXV_SECRET_KEY`) |
| `--min-version <N>`  | schema version | Minimum schema version required to read this index                    |

`--min-version` defaults to the database's schema version; set it lower only for
backward-compatible schema changes. Publishing a schema-4 index with
`min_version` below 4 is refused.

### Generated Files

| File                    | Size    | Description                            |
| ----------------------- | ------- | -------------------------------------- |
| `index.db.zst`          | ~220 MB | Zstd-compressed SQLite database        |
| `bloom.bin`             | ~330 KB | Bloom filter for fast negative lookups |
| `manifest.json`         | ~1 KB   | Metadata with checksums                |
| `manifest.json.minisig` | ~1 KB   | minisign signature (when `--sign`)     |

### Examples

```bash
# Basic publish (no signing)
nxv publish --output ./publish

# Publish with signing
nxv publish --output ./publish \
  --sign --secret-key ./keys/nxv.key

# Full publish for GitHub releases
nxv publish --output ./publish \
  --sign --secret-key ./keys/nxv.key \
  --url-prefix "https://github.com/utensils/nxv/releases/download/index-latest"
```

### Manifest Format

```json
{
  "version": 1,
  "min_version": 4,
  "latest_commit": "b503dde361500433ca25a32e8f4d218bf58fb659",
  "latest_commit_date": "2026-06-11T20:00:47Z",
  "full_index": {
    "url": "https://example.com/index.db.zst",
    "size_bytes": 194414052,
    "sha256": "abc123..."
  },
  "bloom_filter": {
    "url": "https://example.com/bloom.bin",
    "size_bytes": 338595,
    "sha256": "def456..."
  },
  "deltas": []
}
```

The signature is not embedded in the manifest — it's written alongside it as
`manifest.json.minisig`.

---

## keygen

Generate a minisign keypair for signing index manifests.

```bash
nxv keygen [OPTIONS]
```

### Options

| Flag                  | Default             | Description                   |
| --------------------- | ------------------- | ----------------------------- |
| `--secret-key <PATH>` | `./nxv.key`         | Secret key output path        |
| `--public-key <PATH>` | `./nxv.pub`         | Public key output path        |
| `--comment <TEXT>`    | `"nxv signing key"` | Comment embedded in key files |
| `--force`             | `false`             | Overwrite existing key files  |

### Generated Files

| File      | Description                             |
| --------- | --------------------------------------- |
| `nxv.key` | Secret key (keep private!)              |
| `nxv.pub` | Public key (distribute with your index) |

### Examples

```bash
# Generate keypair in current directory
nxv keygen

# Custom output paths
nxv keygen --secret-key ./keys/nxv.key --public-key ./keys/nxv.pub

# With custom comment
nxv keygen --comment "My nxv index signing key"

# Overwrite existing keys
nxv keygen --force
```

### Security Notes

- Keep the secret key (`nxv.key`) private
- Distribute the public key with your published index
- Users verify signatures using `NXV_PUBLIC_KEY` environment variable
- Signature verification can be skipped with `NXV_SKIP_VERIFY=1`

# nxv — Nix Version Index

[![CI](https://github.com/jamesbrink/nxv/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesbrink/nxv/actions/workflows/ci.yml)
[![Release](https://github.com/jamesbrink/nxv/actions/workflows/release.yml/badge.svg)](https://github.com/jamesbrink/nxv/actions/workflows/release.yml)
[![Crates.io](https://img.shields.io/crates/v/nxv.svg)](https://crates.io/crates/nxv)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Nix Flake](https://img.shields.io/badge/nix-flake-blue?logo=nixos)](https://nixos.wiki/wiki/Flakes)
[![FlakeHub](https://img.shields.io/endpoint?url=https://flakehub.com/f/jamesbrink/nxv/badge)](https://flakehub.com/flake/jamesbrink/nxv)
[![Built with Claude](https://img.shields.io/badge/Built%20with-Claude-D97757?logo=claude&logoColor=white)](https://claude.ai)

**Find any version of any Nix package, instantly.**

nxv indexes the entire nixpkgs git history to help you discover when packages were added, which versions existed, and the exact commit to use with `nix shell nixpkgs/<commit>#package`.

## Why nxv?

Because sometimes you need Python 2.7 for that legacy project nobody wants to touch. Or Ruby 2.6 because the Gemfile hasn't been updated since the Obama administration.
Instead of spending your afternoon spelunking through GitHub commits and praying to the Nix gods, just ask nxv. It's indexed 8+ years of nixpkgs history so you don't have to.

<p align="center">
  <img src="./docs/where-is-it.gif" alt="nxv in action" />
</p>

## Try It Now

No installation required — query the live API directly:

```bash
# Search for Node.js 15.x
NXV_API_URL=https://nxv.urandom.io nix run github:jamesbrink/nxv -- search nodejs 15

# Find Python 2.7
NXV_API_URL=https://nxv.urandom.io nix run github:jamesbrink/nxv -- search python 2.7
```

Or visit **https://nxv.urandom.io** to search in your browser.

## Features

- **Fast search** — Bloom filter for instant "not found" responses, SQLite FTS5 for full-text search
- **Version history** — See when each version was introduced and when it was superseded
- **Multiple interfaces** — CLI tool, HTTP API server with web UI, or use as a library
- **NixOS module** — Run as a systemd service with automatic index updates
- **Lightweight** — ~10MB static binary, ~100MB compressed index

## How It Works

```text
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│    nixpkgs      │────▶│     Indexer     │────▶│  SQLite Index   │
│   git history   │     │  (nix eval per  │     │  + Bloom Filter │
│                 │     │    commit)      │     │                 │
└─────────────────┘     └─────────────────┘     └─────────────────┘
                                                        │
                                                        ▼
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   nxv search    │◀────│   Query Engine  │◀────│  Download from  │
│   nxv serve     │     │  (bloom + FTS5) │     │  remote/local   │
└─────────────────┘     └─────────────────┘     └─────────────────┘
```

The indexer walks nixpkgs commits (from 2017+), runs `nix eval` to extract package metadata, and stores version ranges in SQLite.
Users download a pre-built compressed index (~100MB) and query it locally or via the API server.

## Installation

```bash
# Run directly with Nix
nix run github:jamesbrink/nxv -- search python

# Install to profile
nix profile install github:jamesbrink/nxv
```

Or add to your flake:

```nix
{
  inputs.nxv.url = "github:jamesbrink/nxv";

  outputs = { nixpkgs, nxv, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [{
        nixpkgs.overlays = [ nxv.overlays.default ];
        environment.systemPackages = [ pkgs.nxv ];
      }];
    };
  };
}
```

### Quick Install (curl)

```bash
curl -sSfL https://raw.githubusercontent.com/jamesbrink/nxv/main/install.sh | sh
```

For extra safety, download the script first, review it, and verify release checksums from GitHub Releases. You can also set `NXV_VERIFY=1` to enforce checksum verification.

### Cargo

```bash
cargo install nxv
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/jamesbrink/nxv/releases):

| Platform | Binary |
| -------- | ------ |
| Linux x86_64 | `nxv-x86_64-linux-musl` (static) |
| Linux aarch64 | `nxv-aarch64-linux-musl` (static) |
| macOS x86_64 | `nxv-x86_64-apple-darwin` |
| macOS Apple Silicon | `nxv-aarch64-apple-darwin` |

Shell completions for bash, zsh, and fish are included via `nxv completions <shell>`.

## Usage

### Search for Packages

```bash
nxv search python                    # Find all python packages
nxv search python 3.11               # Filter by version prefix
nxv search python --exact            # Exact name match only
nxv search --desc "json parser"      # Search descriptions (FTS)
nxv search python --format json      # JSON output
```

### Package Info & History

```bash
nxv info python              # Detailed package information
nxv info python 3.11.0       # Info for specific version
nxv history python           # Version timeline
nxv history python 3.11.0    # When was 3.11.0 available?
```

### Use a Found Version

```bash
# After finding a commit hash from search results:
nix shell nixpkgs/e4a45f9#python
nix run nixpkgs/e4a45f9#python
```

### Index Management

```bash
nxv update           # Download/update the index
nxv update --force   # Force full re-download
nxv stats            # Show index statistics
```

## API Server

Run nxv as an HTTP API server with a built-in web interface:

```bash
nxv serve                                    # localhost:8080
nxv serve --host 0.0.0.0 --port 3000 --cors  # Public with CORS
```

**Endpoints:**

| Endpoint | Description |
| -------- | ----------- |
| `GET /` | Web UI |
| `GET /docs` | OpenAPI documentation (Scalar) |
| `GET /api/v1/search?q=python` | Search packages |
| `GET /api/v1/search/description?q=json` | Search descriptions |
| `GET /api/v1/packages/{attr}` | Package details |
| `GET /api/v1/packages/{attr}/history` | Version history |
| `GET /api/v1/stats` | Index statistics |
| `GET /api/v1/health` | Health check |

### Remote API Mode

Point the CLI at a remote server instead of using a local database:

```bash
export NXV_API_URL=http://your-server:8080
nxv search python  # Uses remote API transparently
```

## NixOS Module

Run the API server as a systemd service with automatic updates:

```nix
{
  inputs.nxv.url = "github:jamesbrink/nxv";

  outputs = { nixpkgs, nxv, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        nxv.nixosModules.default
        {
          services.nxv = {
            enable = true;
            host = "0.0.0.0";
            port = 8080;
            dataDir = "/var/lib/nxv";
            cors.enable = true;
            openFirewall = true;
            autoUpdate.enable = true;   # Daily index updates
          };
        }
      ];
    };
  };
}
```

<details>
<summary>All module options</summary>

| Option | Default | Description |
| ------ | ------- | ----------- |
| `enable` | `false` | Enable the nxv API service |
| `host` | `127.0.0.1` | Address to bind to |
| `port` | `8080` | Port to listen on |
| `dataDir` | `/var/lib/nxv` | Directory for `index.db` |
| `manifestUrl` | `null` | Custom manifest URL for self-hosted index |
| `publicKey` | `null` | Custom public key for signature verification (path or raw key) |
| `skipVerify` | `false` | Skip signature verification (insecure) |
| `cors.enable` | `false` | Enable CORS for all origins |
| `cors.origins` | `null` | Specific allowed origins |
| `openFirewall` | `false` | Open firewall port |
| `autoUpdate.enable` | `false` | Enable automatic index updates |
| `autoUpdate.interval` | `daily` | Update frequency (systemd calendar syntax) |

</details>

## Docker

Run nxv as a container using the official image from GitHub Container Registry:

```bash
# Run the API server (default command)
docker run -p 8080:8080 ghcr.io/jamesbrink/nxv:latest

# With persistent index storage
docker run -p 8080:8080 -v nxv-data:/root/.local/share/nxv ghcr.io/jamesbrink/nxv:latest

# Run other commands
docker run ghcr.io/jamesbrink/nxv:latest search python
docker run ghcr.io/jamesbrink/nxv:latest --help

# Build an index (requires nixpkgs mount)
docker run -v ./nixpkgs:/nixpkgs -v nxv-data:/root/.local/share/nxv \
  ghcr.io/jamesbrink/nxv:latest index --nixpkgs-path /nixpkgs
```

**Tags:**
- `latest` — Latest build from main branch
- `x.y.z` — Specific version (e.g., `0.1.0`)

The image includes the indexer feature, git, and CA certificates. By default it runs `nxv serve` on port 8080.

### Build from Source with Nix

```bash
# Build the Docker image (Linux only)
nix build .#packages.x86_64-linux.nxv-docker

# Load into Docker
docker load < result
```

## Building Your Own Index

The self-hosting workflow is:

1. **Build** — Run `nxv index` against a nixpkgs clone to create the SQLite database
2. **Publish** — Run `nxv publish` to generate compressed artifacts with a manifest
3. **Host** — Upload artifacts to any static file host (S3, GitHub Releases, etc.)
4. **Configure** — Point clients at your manifest via `NXV_MANIFEST_URL`

### Indexing

Requires the `indexer` feature and a local nixpkgs clone:

```bash
# Build with indexer support
nix build .#nxv-indexer
# or: cargo build --release --features indexer

# Clone nixpkgs
git clone --depth 1000 https://github.com/NixOS/nixpkgs.git

# Build the index (takes hours for full history)
nxv index --nixpkgs-path ./nixpkgs --full

# Incremental update (much faster)
nxv index --nixpkgs-path ./nixpkgs
```

### Backfilling Metadata

Update missing fields without full rebuild:

```bash
# Fast: extract from current nixpkgs HEAD
nxv backfill --nixpkgs-path ./nixpkgs

# Accurate: traverse git history to original commits
nxv backfill --nixpkgs-path ./nixpkgs --history
```

### Publishing Your Index

Generate distribution-ready artifacts with the `publish` command:

```bash
# Generate compressed index, bloom filter, and manifest
nxv publish --output ./publish --url-prefix https://your-server.com/nxv

# Files created:
#   publish/index.db.zst   - Compressed SQLite database (~100MB)
#   publish/bloom.bin      - Bloom filter for fast lookups (~26KB)
#   publish/manifest.json  - Manifest with URLs and checksums
```

The `--url-prefix` sets the base URL that will appear in the manifest. This should match where you'll host the files.

### Integrity and Rollback

- `manifest.json` includes SHA256 checksums for all artifacts
- Manifests are cryptographically signed with [minisign](https://jedisct1.github.io/minisign/) and verified on download
- To roll back a bad index, re-upload a previous `index.db.zst`, `bloom.bin`, and `manifest.json` to the same location or point `NXV_MANIFEST_URL` at a known-good manifest

### Signing Your Manifest (Self-Hosted)

For self-hosted indexes, sign your manifest to enable signature verification.

```bash
# Generate a keypair (one-time, requires indexer feature)
nxv keygen --secret-key nxv.key --public-key nxv.pub

# Publish with signing in one step
nxv publish --output ./publish --url-prefix https://your-server/nxv --sign --secret-key ./nxv.key

# Or use NXV_SECRET_KEY environment variable (useful for CI/CD)
export NXV_SECRET_KEY=/path/to/nxv.key  # Can also be raw key content
nxv publish --output ./publish --url-prefix https://your-server/nxv --sign

# Files created:
#   publish/index.db.zst       - Compressed database
#   publish/bloom.bin          - Bloom filter
#   publish/manifest.json      - Manifest with URLs and checksums
#   publish/manifest.json.minisig - Signature file
```

Clients can then verify using your public key:

```bash
# Via CLI flag
nxv update --manifest-url https://your-server/manifest.json \
           --public-key /path/to/nxv.pub

# Or via environment variable
export NXV_PUBLIC_KEY=/path/to/nxv.pub
nxv update --manifest-url https://your-server/manifest.json
```

To skip verification (not recommended for production):

```bash
nxv update --manifest-url https://your-server/manifest.json --skip-verify
```

### Hosting Your Own Index

You can host the published artifacts anywhere that serves static files:

<details>
<summary>GitHub Releases</summary>

```bash
# Generate artifacts
nxv publish --output ./publish \
  --url-prefix https://github.com/YOUR_USER/YOUR_REPO/releases/download/index-latest

# Create release and upload
gh release create index-latest \
  --title "Package Index" \
  --notes "nxv package index" \
  --latest=false

gh release upload index-latest publish/* --clobber
```

</details>

<details>
<summary>Amazon S3</summary>

```bash
# Generate artifacts
nxv publish --output ./publish \
  --url-prefix https://your-bucket.s3.amazonaws.com/nxv

# Upload to S3
aws s3 sync ./publish s3://your-bucket/nxv/ --acl public-read
```

</details>

<details>
<summary>Cloudflare R2</summary>

```bash
# Generate artifacts
nxv publish --output ./publish \
  --url-prefix https://your-bucket.r2.cloudflarestorage.com/nxv

# Upload using rclone or wrangler
rclone sync ./publish r2:your-bucket/nxv/
```

</details>

<details>
<summary>Any static file server</summary>

```bash
# Generate artifacts
nxv publish --output ./publish \
  --url-prefix https://your-server.com/nxv

# Copy to your web server
rsync -av ./publish/ your-server:/var/www/nxv/
```

</details>

### Using a Custom Index

There are two ways clients can consume your index:

#### Option A: Static files (recommended)

Clients download the index once and query locally. Low server load, works offline after initial download.

```bash
# One-time download
nxv update --manifest-url https://your-server.com/nxv/manifest.json

# Or set permanently
export NXV_MANIFEST_URL=https://your-server.com/nxv/manifest.json
```

#### Option B: API server

Run `nxv serve` to provide a web UI and REST API. Clients query remotely without downloading the index. Good for shared/team environments or the web UI.

```bash
# Server side
nxv serve --host 0.0.0.0 --port 8080

# Client side
export NXV_API_URL=https://your-server.com:8080
nxv search python  # Queries remote API
```

#### NixOS module with custom manifest

Runs the API server with auto-updates from your manifest:

```nix
services.nxv = {
  enable = true;
  manifestUrl = "https://your-server.com/nxv/manifest.json";
  host = "0.0.0.0";
  autoUpdate.enable = true;
};
```

<details>
<summary>Manifest format reference</summary>

The `manifest.json` format:

```json
{
  "version": 1,
  "latest_commit": "abc123def456789...",
  "latest_commit_date": "2024-01-15T12:00:00Z",
  "full_index": {
    "url": "https://your-server.com/nxv/index.db.zst",
    "size_bytes": 150000000,
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  },
  "bloom_filter": {
    "url": "https://your-server.com/nxv/bloom.bin",
    "size_bytes": 150000,
    "sha256": "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
  },
  "deltas": []
}
```

</details>

## Environment Variables

| Variable | Description |
| -------- | ----------- |
| `NXV_DB_PATH` | Path to index database (bloom filter stored as sibling file) |
| `NXV_API_URL` | Remote API URL (CLI uses remote instead of local DB when set) |
| `NXV_MANIFEST_URL` | Custom manifest URL for index downloads |
| `NXV_PUBLIC_KEY` | Custom public key for manifest verification (path or raw key) |
| `NXV_SECRET_KEY` | Secret key for manifest signing (path or raw key content) |
| `NXV_SKIP_VERIFY` | Skip manifest signature verification (set to any value) |
| `NXV_API_TIMEOUT` | API request timeout in seconds (default: 30) |
| `NO_COLOR` | Disable colored output |
| `NXV_INSTALL_DIR` | Custom install directory for curl installer (default: `~/.local/bin`) |
| `NXV_VERSION` | Specific version for curl installer (default: latest) |

## Development

```bash
nix develop                         # Enter dev shell
cargo build                         # Debug build
cargo build --features indexer      # With indexer
cargo test                          # Run tests
cargo test --features indexer       # All tests including indexer
cargo clippy -- -D warnings         # Lint
```

### Project Structure

```text
src/
├── main.rs          # Entry point, command dispatch
├── cli.rs           # Clap command definitions
├── db/              # SQLite database layer
├── remote/          # Index download/update
├── server/          # HTTP API (axum)
├── output/          # Table/JSON/plain formatters
├── bloom.rs         # Bloom filter
└── index/           # Indexer (feature-gated)
    ├── git.rs       # Git history traversal
    ├── extractor.rs # Nix evaluation
    ├── backfill.rs  # Metadata backfill
    └── publisher.rs # Index publishing
```

## License

MIT

# nxv — Nix Version Index

[![CI](https://github.com/utensils/nxv/actions/workflows/ci.yml/badge.svg)](https://github.com/utensils/nxv/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/nxv.svg)](https://crates.io/crates/nxv)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org/)
[![Nix Flake](https://img.shields.io/badge/nix-flake-blue?logo=nixos)](https://nixos.wiki/wiki/Flakes)
[![FlakeHub](https://img.shields.io/endpoint?url=https://flakehub.com/f/utensils/nxv/badge)](https://flakehub.com/flake/utensils/nxv)
[![Built with Claude](https://img.shields.io/badge/Built%20with-Claude-D97757?logo=claude&logoColor=white)](https://claude.ai)
[![Docs](https://img.shields.io/badge/docs-utensils.io%2Fnxv-blue)](https://utensils.io/nxv/)

**Find any version of any Nix package, instantly.**

nxv indexes nearly a decade of nixpkgs channel releases to help you discover when packages were added, which versions existed, and the exact commit to use with `nix shell nixpkgs/<commit>#package`.

## Why nxv?

Because sometimes you need Python 2.7 for that legacy project nobody wants to touch. Or Ruby 2.6 because the Gemfile hasn't been updated since the Obama administration.
Instead of spending your afternoon spelunking through GitHub commits and praying to the Nix gods, just ask nxv. It's indexed 9+ years of nixpkgs history so you don't have to.

<p align="center">
  <img src="./docs/where-is-it.gif" alt="nxv in action" />
</p>

## Try It Now

No installation required — query the live API directly:

```bash
# Search for Node.js 15.x
NXV_API_URL=https://nxv.urandom.io nix run github:utensils/nxv -- search nodejs 15

# Find Python 2.7
NXV_API_URL=https://nxv.urandom.io nix run github:utensils/nxv -- search python 2.7
```

Or visit **<https://nxv.urandom.io>** to search in your browser.

## Features

- **Fast search** — Bloom filter for instant "not found" responses, SQLite FTS5 for full-text search
- **Instant environments** — Resolve historical versions and jump straight into a pinned shell
- **Version history** — See when each version was introduced and when it was superseded
- **Multiple interfaces** — CLI tool, HTTP API server with web UI, or query via remote API
- **NixOS module** — Run as a systemd service with automatic index updates
- **Lightweight** — ~10MB static binary, ~220MB compressed index

## How It Works

```text
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│ channel-release │────▶│     Indexer     │────▶│  SQLite Index   │
│ snapshots from  │     │ (packages.json  │     │  + Bloom Filter │
│ nixos.org (S3)  │     │  per release)   │     │                 │
└─────────────────┘     └─────────────────┘     └─────────────────┘
                                                        │
                                                        ▼
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   nxv search    │◀────│   Query Engine  │◀────│  Download from  │
│   nxv serve     │     │ bloom + indexes │     │  remote/local   │
└─────────────────┘     └─────────────────┘     └─────────────────┘
```

The indexer ingests Hydra-built channel-release snapshots from releases.nixos.org (back to 2016): `packages.json.br` for releases from 2020 onward, and `nix-env` evaluation of `nixexprs.tar.xz` for the earlier era. Every recorded commit is one where the package version verifiably existed — including nested package sets like `python3Packages.*` and `haskellPackages.*`.
Users download a pre-built compressed index (~220MB) and query it locally or via the API server.

## Installation

### Quick Install

```bash
curl -sSfL https://raw.githubusercontent.com/utensils/nxv/main/install.sh | sh
```

This installs a pre-built binary to `/usr/local/bin` (if writable) or `~/.local/bin` otherwise; set `NXV_INSTALL_DIR` to override. For extra safety, download and review the script first. Set `NXV_VERIFY=1` to enforce checksum verification against GitHub Releases.

### Cargo

```bash
cargo install nxv
```

### Nix

```bash
# Run directly
nix run github:utensils/nxv -- search python

# Install to profile
nix profile install github:utensils/nxv
```

Or add to your flake:

```nix
{
  inputs.nxv.url = "github:utensils/nxv";

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

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/utensils/nxv/releases):

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
nxv search python 3.11               # Search the closest attribute tier
nxv search python 2.7.3 --all-depths # Include nested package-set members
nxv search python --exact            # Exact name match only
nxv search "json parser" --desc      # Search descriptions (FTS)
nxv search python --format json      # JSON output
```

### Jump Into a Package Shell

```bash
nxv run python 2.7                   # Resolve Python 2.7 and open a shell
nxv run python 3.11 --with jq        # Add the latest jq
nxv run python 3.11 --with nodejs@20 # Pin multiple package versions
```

`nxv run` prefers an exact attribute match, then falls back to the same
package/version scope and relevance rules as `search`. It opens one shell with
every resolved package. Modern revisions use `nix shell`; environments
containing a pre-flake revision automatically use a compatible pinned
`nix-shell` expression. On Apple Silicon, that fallback evaluates packages as
`x86_64-darwin` and requires Rosetta.

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

### Keeping nxv up to date

```bash
nxv update                    # Update the nxv application
nxv sync                      # Download or refresh the package index
nxv sync --force              # Force a full re-download of the index
nxv stats                     # Show index statistics
```

`nxv update` only checks GitHub for the latest nxv release and behaves
according to how nxv was installed:

- **Local install** (from `install.sh` or a manual download) — downloads
  the platform binary, verifies its SHA-256 against `SHA256SUMS.txt`, and
  atomically swaps the running executable.
- **Nix / cargo / Homebrew** — leaves the binary alone and prints the
  matching upgrade command (e.g. `brew upgrade nxv`,
  `cargo install --locked nxv`).

`nxv sync` independently refreshes the local SQLite index and bloom filter.
It never checks for or replaces the nxv application, so it is the command to
use in CI and systemd timers.

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
| `GET /api/v1/search?q=python` | Search packages (`all_depths=true` opts into nested version matches) |
| `GET /api/v1/search/description?q=json` | Search descriptions |
| `GET /api/v1/packages/{attr}` | Package details |
| `GET /api/v1/packages/{attr}/history` | Version history |
| `GET /api/v1/packages/{attr}/versions/{version}` | Specific version info |
| `GET /api/v1/packages/{attr}/versions/{version}/first` | First occurrence commit |
| `GET /api/v1/packages/{attr}/versions/{version}/last` | Last occurrence commit |
| `GET /api/v1/stats` | Index statistics |
| `GET /api/v1/health` | Health check |
| `GET /api/v1/metrics` | Runtime metrics (latency, activity, uptime) |

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
  inputs.nxv.url = "github:utensils/nxv";

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
| `package` | `pkgs.nxv` | The nxv package to use |
| `host` | `127.0.0.1` | Address to bind to |
| `port` | `8080` | Port to listen on |
| `dataDir` | `/var/lib/nxv` | Directory for `index.db` |
| `manifestUrl` | `null` | Custom manifest URL for self-hosted index |
| `publicKey` | `null` | Custom public key for signature verification (path or raw key) |
| `skipVerify` | `false` | Skip signature verification (insecure) |
| `cors.enable` | `false` | Enable CORS for all origins |
| `cors.origins` | `null` | Specific allowed origins |
| `openFirewall` | `false` | Open firewall port |
| `user` | `nxv` | User account the service runs as |
| `group` | `nxv` | Group the service runs as |
| `autoUpdate.enable` | `false` | Enable automatic index updates |
| `autoUpdate.interval` | `daily` | Update frequency (systemd calendar syntax) |
| `logging.level` | `nxv=info,tower_http=info,warn` | Log level (RUST_LOG syntax) |
| `logging.format` | `text` | Log output format (`text` or `json`) |
| `database.maxConnections` | `32` | Max concurrent database operations |
| `database.timeoutSeconds` | `30` | Database operation timeout |
| `rateLimit.enable` | `false` | Enable IP-based rate limiting |
| `rateLimit.requestsPerSecond` | `10` | Max requests per second per IP |
| `rateLimit.burst` | `null` | Burst size (defaults to 2x rate) |

</details>

## Docker

Run nxv as a container using the official image from GitHub Container Registry:

```bash
# Run the API server (default command)
docker run -p 8080:8080 ghcr.io/utensils/nxv:latest

# With persistent index storage
docker run -p 8080:8080 -v nxv-data:/root/.local/share/nxv ghcr.io/utensils/nxv:latest

# Run other commands
docker run ghcr.io/utensils/nxv:latest search python
docker run ghcr.io/utensils/nxv:latest --help

# Build an index (downloads channel snapshots from releases.nixos.org)
docker run -v nxv-data:/root/.local/share/nxv \
  ghcr.io/utensils/nxv:latest index
```

**Tags:**

- `latest` — Latest build from main branch
- `x.y.z` — Specific version (e.g., `0.3.0`)

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

1. **Build** — Run `nxv index` to ingest channel-release snapshots into the SQLite database
2. **Publish** — Run `nxv publish` to generate compressed artifacts with a manifest
3. **Host** — Upload artifacts to any static file host (S3, GitHub Releases, etc.)
4. **Configure** — Point clients at your manifest via `NXV_MANIFEST_URL`

### Indexing

Requires the `indexer` feature. The indexer downloads channel-release snapshots from releases.nixos.org — no nixpkgs checkout is needed:

```bash
# Build with indexer support
nix build .#nxv-indexer
# or: cargo build --release --features indexer

# Build the index from channel-release snapshots (2020+ era, no nix required)
nxv index

# Also ingest the pre-2020 era via nix-env evaluation (one-time, requires nix)
nxv index --backfill-evals

# Subsequent runs are incremental — only new releases are ingested
nxv index
```

### Publishing Your Index

Generate distribution-ready artifacts with the `publish` command:

```bash
# Generate compressed index, bloom filter, and manifest
nxv publish --output ./publish --url-prefix https://your-server.com/nxv

# Files created:
#   publish/index.db.zst   - Compressed SQLite database (~220MB)
#   publish/bloom.bin      - Bloom filter for fast lookups (~330KB)
#   publish/manifest.json  - Manifest with URLs and checksums
```

The `--url-prefix` sets the base URL that will appear in the manifest. This should match where you'll host the files.
For GitHub Releases or other mutable asset stores, `--artifact-name-prefix` can
put run-specific payload names in the manifest while keeping the local output
files unchanged. Upload those payload assets first, then replace `manifest.json`
last so clients keep using the old index until the new one is fully available.

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
nxv sync --manifest-url https://your-server/manifest.json \
         --public-key /path/to/nxv.pub

# Or via environment variable
export NXV_PUBLIC_KEY=/path/to/nxv.pub
nxv sync --manifest-url https://your-server/manifest.json
```

To skip verification (not recommended for production):

```bash
nxv sync --manifest-url https://your-server/manifest.json --skip-verify
```

### Hosting Your Own Index

You can host the published artifacts anywhere that serves static files:

<details>
<summary>GitHub Releases</summary>

```bash
RUN_ID="$(date +%Y%m%d%H%M%S)-"

# Generate artifacts
nxv publish --output ./publish \
  --url-prefix https://github.com/YOUR_USER/YOUR_REPO/releases/download/index-latest \
  --artifact-name-prefix "$RUN_ID" \
  --sign \
  --secret-key ./nxv.key

# Create release and upload
gh release create index-latest \
  --title "Package Index" \
  --notes "nxv package index" \
  --latest=false

upload_dir="$(mktemp -d)"
cp publish/index.db.zst "${upload_dir}/${RUN_ID}index.db.zst"
cp publish/bloom.bin "${upload_dir}/${RUN_ID}bloom.bin"

gh release upload index-latest \
  "${upload_dir}/${RUN_ID}index.db.zst" \
  "${upload_dir}/${RUN_ID}bloom.bin"

# Replace the stable pointer only after the payload assets are present.
gh release upload index-latest publish/manifest.json.minisig --clobber
gh release upload index-latest publish/manifest.json --clobber
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
nxv sync --manifest-url https://your-server.com/nxv/manifest.json

# Or set permanently
export NXV_MANIFEST_URL=https://your-server.com/nxv/manifest.json
```

#### Option B: API server

Run `nxv serve` to provide a web UI and REST API. Clients query remotely without downloading the index. Good for shared/team environments or the web UI.

```bash
# Server side
nxv serve --host 0.0.0.0 --port 8080

# Client side
export NXV_API_URL=http://your-server:8080
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
  "min_version": 4,
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
| `NXV_HOST` | `nxv serve` bind address (default: `127.0.0.1`) |
| `NXV_PORT` | `nxv serve` port (default: `8080`) |
| `NXV_RATE_LIMIT` | `nxv serve` max requests per second per IP |
| `NXV_RATE_LIMIT_BURST` | `nxv serve` rate-limit burst size |
| `NXV_MAX_DB_CONNECTIONS` | `nxv serve` max concurrent DB operations (default: 32) |
| `NXV_DB_TIMEOUT_SECS` | `nxv serve` DB operation timeout in seconds (default: 30) |
| `NXV_LOG_FORMAT` | `nxv serve` log format, `text` or `json` |
| `NXV_RELEASES_URL` | Override the releases.nixos.org S3 endpoint for `nxv index` |
| `NXV_VERIFY` | Set to `1` to verify curl-installer download against `SHA256SUMS.txt` |
| `NXV_INSTALL_DIR` | Custom install directory for curl installer (default: `/usr/local/bin` if writable, else `~/.local/bin`) |
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
├── backend.rs       # Backend abstraction (local/remote)
├── client.rs        # HTTP client for remote API
├── completions.rs   # Shell completion generation
├── db/              # SQLite database layer
├── remote/          # Index download/update
├── server/          # HTTP API (axum)
├── output/          # Table/JSON/plain formatters
├── bloom.rs         # Bloom filter
├── search.rs        # Search/filter/sort logic
├── self_update.rs   # Application update used by nxv update
├── skill/           # Agent skill install/list/uninstall (nxv skill ...)
├── version.rs       # Version/long-version string helpers
├── paths.rs         # Platform-specific paths
├── error.rs         # Error types
└── index/           # Indexer (feature-gated)
    ├── mod.rs       # Indexer orchestration
    ├── releases.rs  # releases.nixos.org listing + planning
    ├── snapshot.rs  # Streaming packages.json.br parsing
    ├── eval.rs      # nix-env backfill + head eval
    ├── monitor.rs   # Data-quality gates + coverage report
    └── publisher.rs # Index publishing
```

## Agent Skill

nxv ships an [Agent Skills](https://agentskills.io)-standard skill so AI
coding agents — Claude Code, OpenAI Codex CLI, Pi, OpenClaw, GitHub Copilot
CLI, Cursor, Gemini CLI, Amp, Goose — can run nxv commands and call the HTTP
API on your behalf without extra setup. The binary embeds the skill and
installs it where each agent looks:

```bash
nxv skill install codex        # Install user-wide for one agent
nxv skill install --detected   # Explicitly target detected agents
nxv skill install codex --project # Install for Codex in this project
nxv skill list                 # Show agents, paths, install status
```

Then ask your agent things like *"which nixpkgs commit shipped python 2.7?"*
or *"give me the `nix shell` command for nodejs 15.14"*. See the
[skill guide](https://utensils.io/nxv/guide/skill) for the supported agents,
agent patterns, and JSON shapes.

## Related Projects

There are several other great tools in this space:

| Tool | Approach | Link |
| ---- | -------- | ---- |
| [nix-community/nix-index](https://github.com/nix-community/nix-index) | Indexes files in nixpkgs for `nix-locate` — find which package has a file | [github.com/nix-community/nix-index](https://github.com/nix-community/nix-index) |
| [lazamar/nix-package-versions](https://github.com/lazamar/nix-package-versions) | Web service that samples nixpkgs every ~5 weeks | [lazamar.co.uk/nix-versions](https://lazamar.co.uk/nix-versions/) |
| [NixHub.io](https://www.nixhub.io/) | Parses Hydra build outputs, provides web UI and API | [nixhub.io](https://www.nixhub.io/) |
| [vic/nix-versions](https://github.com/vic/nix-versions) | CLI that aggregates lazamar, nixhub, and history APIs | [github.com/vic/nix-versions](https://github.com/vic/nix-versions) |
| [history.nix-packages.com](https://history.nix-packages.com/) | Web-based package history browser | [history.nix-packages.com](https://history.nix-packages.com/) |

nxv takes a different approach: it ingests every Hydra-built channel release into a local index, giving you a complete offline-capable database covering nested package sets back to 2016. Choose what works best for your use case!

## License

[MIT](LICENSE)

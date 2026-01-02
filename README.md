# nxv

CLI tool to find specific versions of Nix packages across nixpkgs history.

> **Early Development** - No public index available yet. Building your own index requires a local nixpkgs clone.

## Installation

```bash
# Run directly
nix run github:jamesbrink/nxv -- search python

# Install to profile
nix profile install github:jamesbrink/nxv

# Or use the overlay in your flake
{
  inputs.nxv.url = "github:jamesbrink/nxv";

  outputs = { nixpkgs, nxv, ... }: {
    # NixOS
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [{
        nixpkgs.overlays = [ nxv.overlays.default ];
        environment.systemPackages = [ pkgs.nxv ];
      }];
    };
  };
}
```

Shell completions for bash, zsh, and fish are included.

## Usage

```bash
# Search for packages
nxv search python
nxv search python --version 3.11
nxv search --desc "json parser"

# Package info and history
nxv info python
nxv history python

# Index stats
nxv stats

# Output formats
nxv search python --format json
```

Use a found version with Nix:
```bash
nix shell nixpkgs/<commit>#python
```

## API Server

```bash
nxv serve                          # localhost:8080
nxv serve --host 0.0.0.0 --port 3000 --cors
```

Endpoints: `/api/v1/search`, `/api/v1/packages/{attr}`, `/api/v1/stats`, `/docs`

### NixOS Module

Run the API server as a systemd service:

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
            dataDir = "/var/lib/nxv";   # database location (index.db)
            cors.enable = true;
            openFirewall = true;
            autoUpdate.enable = true;   # daily index updates
          };
        }
      ];
    };
  };
}
```

| Option | Default | Description |
|--------|---------|-------------|
| `enable` | `false` | Enable the nxv API service |
| `host` | `127.0.0.1` | Address to bind to |
| `port` | `8080` | Port to listen on |
| `dataDir` | `/var/lib/nxv` | Directory for `index.db` |
| `manifestUrl` | `null` | Custom manifest URL for self-hosted index |
| `cors.enable` | `false` | Enable CORS for all origins |
| `cors.origins` | `null` | Specific allowed origins |
| `openFirewall` | `false` | Open firewall port |
| `autoUpdate.enable` | `false` | Enable automatic index updates |
| `autoUpdate.interval` | `daily` | Update frequency (systemd calendar syntax) |

The module creates a dedicated user, applies systemd hardening, downloads the index on first start, and optionally manages automatic updates.

## Remote API

Point the CLI at a remote server instead of local database:

```bash
export NXV_API_URL=http://your-server:8080
nxv search python  # uses remote API transparently
```

## Building an Index

Requires the `indexer` feature and a local nixpkgs clone:

```bash
nix build .#nxv-indexer
# or
cargo build --release --features indexer

nxv index --nixpkgs-path ./nixpkgs --full
```

### Backfilling Metadata

Update missing `source_path` and `homepage` fields without rebuilding:

```bash
# Fast: extract from current nixpkgs HEAD
nxv backfill --nixpkgs-path ./nixpkgs

# Accurate: traverse git history to original commits
nxv backfill --nixpkgs-path ./nixpkgs --history
```

### Resetting nixpkgs Repository

If the nixpkgs repository gets into a messy state (e.g., after an interrupted operation), reset it:

```bash
# Reset to origin/master
nxv reset --nixpkgs-path ./nixpkgs

# Fetch and reset to latest
nxv reset --nixpkgs-path ./nixpkgs --fetch

# Reset to a specific commit
nxv reset --nixpkgs-path ./nixpkgs --to abc1234
```

### Hosting Your Own Index

To host your own index, create a `manifest.json` with the following format:

```json
{
  "version": 2,
  "latest_commit": "abc123def456789...",
  "latest_commit_date": "2024-01-15T12:00:00Z",
  "full_index": {
    "url": "https://your-server.com/index.db.zst",
    "size_bytes": 150000000,
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  },
  "bloom_filter": {
    "url": "https://your-server.com/bloom.bin",
    "size_bytes": 150000,
    "sha256": "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
  },
  "deltas": [
    {
      "from_commit": "previouscommit123...",
      "to_commit": "abc123def456789...",
      "url": "https://your-server.com/delta-prev-to-current.sql.zst",
      "size_bytes": 50000,
      "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    }
  ]
}
```

| Field | Description |
|-------|-------------|
| `version` | Manifest format version (currently `2`) |
| `latest_commit` | Full nixpkgs commit hash of the index |
| `latest_commit_date` | ISO 8601 timestamp of the commit |
| `full_index.url` | URL to zstd-compressed SQLite database |
| `full_index.size_bytes` | Compressed file size |
| `full_index.sha256` | SHA-256 hash of compressed file |
| `bloom_filter.*` | Same structure for the bloom filter file |
| `deltas` | Optional array of incremental updates |

Point CLI or module at your manifest:

```bash
nxv update --manifest-url https://your-server.com/manifest.json
```

```nix
services.nxv.manifestUrl = "https://your-server.com/manifest.json";
```

## Development

```bash
nix develop
cargo test
nix flake check
```

## License

MIT

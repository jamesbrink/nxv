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
            cors.enable = true;
            openFirewall = true;
            autoUpdate.enable = true;  # daily index updates
          };
        }
      ];
    };
  };
}
```

The module creates a dedicated user, applies systemd hardening, and optionally manages automatic index updates.

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

## Development

```bash
nix develop
cargo test
nix flake check
```

## License

MIT

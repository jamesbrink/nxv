# Installation

There are several ways to install nxv depending on your needs.

## Nix Flakes (Recommended)

The easiest way to install nxv:

```bash
# Install to your profile
nix profile install github:jamesbrink/nxv

# Or run directly without installing
nix run github:jamesbrink/nxv -- search python
```

## NixOS Module

Add nxv as a service to your NixOS configuration:

```nix
# flake.nix
{
  inputs.nxv.url = "github:jamesbrink/nxv";

  outputs = { nixpkgs, nxv, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        nxv.nixosModules.default
        {
          services.nxv = {
            enable = true;
            port = 8080;
          };
        }
      ];
    };
  };
}
```

## Cargo

If you have Rust installed:

```bash
cargo install nxv
```

## Docker

Run the HTTP server with Docker:

```bash
docker run -p 8080:8080 ghcr.io/jamesbrink/nxv:latest
```

## From Source

Clone and build:

```bash
git clone https://github.com/jamesbrink/nxv
cd nxv
nix develop
cargo build --release
```

## First Run

After installation, download the package index:

```bash
nxv update
```

This downloads ~28MB of compressed data to your local data directory:
- **Linux**: `~/.local/share/nxv/`
- **macOS**: `~/Library/Application Support/nxv/`

The index is updated weekly. Run `nxv update` periodically to get the latest packages.

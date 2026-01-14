# Installation

There are several ways to install nxv depending on your needs.

## Run Without Installing

Try nxv instantly without any installation:

```bash
# Run directly via Nix flakes - nothing persisted
nix run github:jamesbrink/nxv -- search python

# Or use the web interface at https://nxv.urandom.io
```

## Shell Script

One-liner install that downloads a static binary:

```bash
curl -fsSL https://raw.githubusercontent.com/jamesbrink/nxv/main/install.sh | sh
```

This installs to `~/.local/bin/nxv` (or `/usr/local/bin` with sudo).

## Nix Flakes

Install to your Nix profile:

```bash
nix profile install github:jamesbrink/nxv
```

## NixOS / Home Manager (Recommended)

Add nxv declaratively to your system or user packages:

```nix
# flake.nix
{
  inputs.nxv.url = "github:jamesbrink/nxv";

  outputs = { nixpkgs, nxv, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [{
        # Add the overlay
        nixpkgs.overlays = [ nxv.overlays.default ];
        # Install the package
        environment.systemPackages = [ pkgs.nxv ];
      }];
    };
  };
}
```

For Home Manager:

```nix
{
  nixpkgs.overlays = [ inputs.nxv.overlays.default ];
  home.packages = [ pkgs.nxv ];
}
```

## NixOS Module (Server)

Run nxv as a systemd service:

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

The index is updated weekly. Run `nxv update` periodically to get the latest
packages.

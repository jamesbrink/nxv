# nxv

A CLI tool to quickly find specific versions of Nix packages across nixpkgs history.

## Features

- Search for packages by name, version, or description
- View version history for any package
- Instant negative lookups via bloom filter
- Pre-built index - no local nixpkgs clone needed
- Delta updates for fast index synchronization
- Multiple output formats (table, JSON, plain text)

## Installation

### From source

```bash
# Clone the repository
git clone https://github.com/jamesbrink/nxv
cd nxv

# Build with Nix flakes
nix build

# Or with Cargo
cargo build --release
```

## Usage

### Download/update the package index

```bash
# Download or update the package index
nxv update

# Force full re-download
nxv update --force
```

### Search for packages

```bash
# Search by package name (prefix match)
nxv search python

# Exact name match
nxv search python --exact

# Filter by version
nxv search python --version 3.11

# Search in descriptions
nxv search --desc "json parser"

# Filter by license
nxv search python --license MIT

# Different output formats
nxv search python --format json
nxv search python --format plain

# Show platforms
nxv search python --show-platforms

# Sort and limit
nxv search python --sort version --reverse
nxv search python --limit 100
```

### View package history

```bash
# Show all versions of a package
nxv history python

# Show when a specific version was available
nxv history python 3.11.0
```

### Show index information

```bash
nxv info
```

### Use a specific version

Once you find the version you need, use it with Nix:

```bash
# Run a specific version
nix run nixpkgs/<commit>#python

# Install temporarily
nix shell nixpkgs/<commit>#python

# Add to your flake.nix inputs
inputs.nixpkgs-python311.url = "github:NixOS/nixpkgs/<commit>";
```

### Shell completions

```bash
# Generate completions for your shell
nxv completions bash > ~/.local/share/bash-completion/completions/nxv
nxv completions zsh > ~/.zsh/completions/_nxv
nxv completions fish > ~/.config/fish/completions/nxv.fish
```

## Development

This project uses Nix flakes for development environment management.

```bash
# Enter the development shell
nix develop

# Or with direnv
direnv allow

# Build
cargo build

# Run tests
cargo test

# Build with indexer feature (for creating new indexes)
cargo build --features indexer
cargo test --features indexer
```

### Building static binaries

For maximum portability, you can build fully static binaries:

```bash
# Linux (musl) - fully static binary
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl

# macOS - already mostly static (uses rustls, bundled SQLite)
cargo build --release

# Check dependencies
ldd target/release/nxv  # Linux
otool -L target/release/nxv  # macOS
```

The release profile is optimized for size with LTO and symbol stripping.

### Building the index (developers only)

If you need to build your own index from a local nixpkgs clone:

```bash
# Enable the indexer feature
cargo build --release --features indexer

# Create a full index
nxv index --nixpkgs-path /path/to/nixpkgs --full

# Incremental update
nxv index --nixpkgs-path /path/to/nixpkgs
```

## License

MIT

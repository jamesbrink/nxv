# nxv

CLI tool to find specific versions of Nix packages across nixpkgs history.

> **Early Development** - No public index available yet. Building your own index requires a local nixpkgs clone.

## Installation

```bash
nix build github:jamesbrink/nxv
# or
cargo build --release
```

## Usage

```bash
# Search for packages
nxv search python
nxv search python --version 3.11
nxv search --desc "json parser"

# View version history
nxv history python

# Output formats
nxv search python --format json
nxv search python --format plain
```

Use a found version with Nix:
```bash
nix shell nixpkgs/<commit>#python
```

## Building an Index

Requires the `indexer` feature and a local nixpkgs clone:

```bash
cargo build --release --features indexer
nxv index --nixpkgs-path ./nixpkgs --full
```

## Development

```bash
nix develop
cargo test --features indexer
nix flake check
```

## License

MIT

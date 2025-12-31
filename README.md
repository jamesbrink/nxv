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

## Development

```bash
nix develop
cargo test
nix flake check
```

## License

MIT

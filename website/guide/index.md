# Getting Started

nxv helps you find specific versions of Nix packages across nixpkgs history.
Whether you need to pin a dependency to an older version or find when a package
was introduced, nxv makes it fast and easy.

::: tip Try Without Installing
Use the public web interface at [nxv.urandom.io](https://nxv.urandom.io/) - no installation required.
:::

## Quick Start

### Run Directly (No Install)

```bash
# Run directly via Nix flakes - nothing persisted
nix run github:jamesbrink/nxv -- search python
```

### Install via Shell Script

```bash
# One-liner install (downloads static binary)
curl -fsSL https://raw.githubusercontent.com/jamesbrink/nxv/main/install.sh | sh

# Update the package index (downloads ~28MB)
nxv update

# Search for a package
nxv search python

# Find a specific version
nxv search python --version 3.11
```

## What You Get

For each package version, nxv provides:

- **Version history** - When each version was first and last available
- **Commit hashes** - Exact nixpkgs commits for reproducibility
- **Store paths** - Pre-built binary paths from cache.nixos.org
- **Flake references** - Copy-paste flake refs for any version
- **Security info** - CVE warnings and insecure package markers

## How It Works

nxv uses a pre-built SQLite index containing:

- ~2.8 million package version records
- Package metadata (description, license, homepage)
- Bloom filter for instant "not found" responses

The index is downloaded once and searched locally, so queries are fast and work
offline.

## Next Steps

- [Installation](/guide/installation) - Different ways to install nxv
- [Configuration](/guide/configuration) - Environment variables and options
- [CLI Reference](/guide/cli-reference) - Complete command documentation

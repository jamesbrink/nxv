---
layout: home
hero:
  name: nxv
  text: Nix Version Search
  tagline: Find any version of any Nix package instantly
  image:
    src: /demo.gif
    alt: nxv demo
  actions:
    - theme: brand
      text: Get Started
      link: /guide/
    - theme: alt
      text: View on GitHub
      link: https://github.com/jamesbrink/nxv
features:
  - icon: "\u26A1"
    title: Fast
    details:
      Bloom filter + SQLite FTS5 for instant results across millions of package
      versions
  - icon: "\uD83D\uDCE6"
    title: Complete
    details:
      Every nixpkgs package version since 2017, with store paths and flake
      references
  - icon: "\uD83D\uDD12"
    title: Offline-first
    details: Download the index once, search locally without network requests
  - icon: "\uD83D\uDEE1\uFE0F"
    title: Security Aware
    details: CVE warnings for vulnerable packages, insecure package indicators
---

## Quick Example

```bash
# Search for a package
nxv search python

# Find a specific version
nxv search python --version 3.11

# Get package history
nxv history python311

# Use in a flake
nix shell nixpkgs/abc123#python311
```

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2024-12-30

### Added

- Initial release of nxv (Nix Versions)
- **Search functionality**
  - Search packages by name (prefix match)
  - Exact name matching with `--exact` flag
  - Version filtering with `--version` flag
  - Description search using FTS5 with `--desc` flag
  - License filtering with `--license` flag
  - Multiple output formats: table (default), JSON, plain text
  - Sort options: date, version, name
  - Result limiting with `--limit`
  - Platform display with `--show-platforms`
- **Version history**
  - View all versions of a package with `nxv history <package>`
  - Show specific version availability with `nxv history <package> <version>`
- **Index management**
  - Download pre-built index with `nxv update`
  - Force full re-download with `nxv update --force`
  - Delta update support (infrastructure ready)
- **Index statistics**
  - View index info with `nxv info`
  - Shows database size, commit range, package counts
- **Shell completions**
  - Generate completions with `nxv completions <shell>`
  - Supports bash, zsh, fish, powershell, elvish
- **Bloom filter**
  - Fast O(1) negative lookups for exact name searches
  - Instant "package not found" for typos
- **Indexer** (feature-gated)
  - Build index from local nixpkgs clone with `nxv index`
  - Incremental indexing from last indexed commit
  - Full rebuild with `--full` flag
  - Checkpoint/resume support with Ctrl+C handling
  - Progress bars during indexing

### Technical Details

- SQLite database with FTS5 for full-text search
- Zstd compression for index distribution
- SHA256 verification for downloads
- Rust 2024 edition
- 10MB release binary size

[unreleased]: https://github.com/jamesbrink/nxv/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jamesbrink/nxv/releases/tag/v0.1.0

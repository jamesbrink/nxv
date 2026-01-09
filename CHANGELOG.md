# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Schema forward compatibility with `min_version` field in manifest
- MIT LICENSE file

### Changed

- `/release` skill replaces `/release-notes` with full release automation

### Fixed

- `--force` flag now properly bypasses schema validation during update
- Incremental indexing now handles merge commits and `pkgs/by-name` paths correctly

## [0.1.3] - 2026-01-04

### Added

- Structured JSON logging for API server with `--log-format json`
- Request tracing with unique request IDs
- Graceful shutdown handling for API server

### Changed

- Improved error handling throughout API server
- Better error messages for database and network failures

### Fixed

- Blocking database calls now use `spawn_blocking` to prevent async runtime starvation
- Rate limiting and security hardening for API endpoints

## [0.1.2] - 2026-01-03

### Added

- Dynamic tab completion for package names in bash/zsh/fish
- `/release-notes` command for pre-release checks
- `last_indexed_date` field in stats output

### Changed

- Improved stats output clarity and formatting
- README reorganized with better installation instructions

### Fixed

- DevShell now uses `bashInteractive` for proper readline/completion support

## [0.1.1] - 2026-01-03

### Added

- Cross-platform release builds (Linux x86_64/aarch64, macOS x86_64/ARM64)
- Static musl binaries for Linux

### Fixed

- crates.io version check for first-time publish
- Excluded large files (nixpkgs, publish artifacts) from crate package

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

[unreleased]: https://github.com/jamesbrink/nxv/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/jamesbrink/nxv/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jamesbrink/nxv/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jamesbrink/nxv/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jamesbrink/nxv/releases/tag/v0.1.0

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

nxv (Nix Versions) is a Rust CLI tool for quickly finding specific versions of Nix packages.

## Development Environment

This project uses Nix flakes with rust-overlay for reproducible development:

```bash
nix develop          # Enter devshell
direnv allow         # Or use direnv
```

## Build Commands

```bash
cargo build          # Debug build
cargo build --release # Release build
cargo run -- <args>  # Run with arguments
cargo test           # Run all tests
cargo test <name>    # Run specific test
cargo clippy         # Lint
cargo fmt            # Format code
```

## Dependency Management

**Always use current versions of dependencies.** Before adding or updating dependencies:

```bash
cargo search <crate>           # Find latest version of a crate
cargo search <crate> --limit 5 # Show multiple results
```

When adding dependencies to Cargo.toml:
- Use `cargo search` to verify the latest version
- Prefer crates with active maintenance and good documentation
- Check crates.io for version info when needed

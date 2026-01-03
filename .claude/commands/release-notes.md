---
description: Generate release notes from git history
allowed-tools: Bash, Read, Grep
---

# Release Notes

Generate release notes for changes since the last git tag. Execute all steps.

## Pre-flight checks (stop if any fail)

1. `cargo fmt --check` - verify formatting
2. `cargo clippy -- -D warnings` - lint with errors on warnings
3. `cargo test` - run all tests
4. `nix flake check` - run nix checks
5. `git status` - verify working tree is clean (no uncommitted changes)

## Release info

1. `git log --oneline $(git describe --tags --abbrev=0)..HEAD` - commits since last tag
2. Verify `Cargo.toml` version matches intended release (Nix flake inherits via `craneLib.crateNameFromCargoToml`)
3. Check `created` timestamp in `flake.nix` Docker config against `date -u +%Y-%m-%dT%H:%M:%SZ`.
   If the date differs from today, note that `flake.nix` needs updating before release.

Output markdown release notes with Features, Fixes, and Other Changes sections. Do not perform the release.

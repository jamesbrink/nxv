---
description: Prepare and execute a release with pre-flight checks and user confirmation
allowed-tools: Bash, Read, Grep, Edit, AskUserQuestion
---

# Release

Prepare and execute a release for nxv. Run pre-flight checks, generate release
notes, and require explicit user confirmation before making any changes.

## Phase 1: Pre-flight Checks

Run these checks and stop immediately if any fail:

```bash
cargo fmt --check
cargo clippy --features indexer -- -D warnings
cargo test --features indexer
nix flake check
git status --porcelain
```

If `git status` shows uncommitted changes, stop and inform the user.

## Phase 2: Gather Release Information

1. Get the current version from `Cargo.toml`.
2. Get the last tag with `git describe --tags --abbrev=0`.
3. Get commits since last tag with `git log --oneline <last-tag>..HEAD`.
4. Get today's UTC timestamp with `date -u +%Y-%m-%dT%H:%M:%SZ`.
5. Check the `created` timestamp in `flake.nix` Docker config.

## Phase 3: Generate Release Notes

Create markdown release notes with these sections:

- **Features** - New functionality (commits starting with `feat:`)
- **Fixes** - Bug fixes (commits starting with `fix:`)
- **Other** - Everything else (refactor, chore, docs, etc.)

## Phase 4: User Confirmation

Present a summary and ask for confirmation before proceeding:

```text
=== RELEASE SUMMARY ===

Current version: X.Y.Z
Proposed version: (user to confirm)

Commits to be released:
  <commit list>

Release Notes:
  <generated notes>

Actions to perform:
  1. Bump version in Cargo.toml
  2. Update Docker timestamp in flake.nix
  3. Commit with message "chore: bump version to X.Y.Z for release"
  4. Create and push tag vX.Y.Z

CI/CD will then:
  - Build static binaries (Linux x86_64/aarch64, macOS x86_64/ARM64)
  - Create GitHub Release with binaries and checksums
  - Publish to crates.io
  - Push Docker image to ghcr.io/jamesbrink/nxv
  - Publish to FlakeHub

Proceed? Enter version number to confirm, or "abort" to cancel.
```

Use `AskUserQuestion` to get confirmation. The user must provide the version
number (e.g., `0.1.4` or `0.2.0`).

## Phase 5: Execute Release

Only proceed after explicit user confirmation with a version number.

1. Update `Cargo.toml` version field.
2. Update `flake.nix` Docker `created` timestamp.
3. Commit the changes:

   ```bash
   git add Cargo.toml flake.nix
   git commit -m "chore: bump version to X.Y.Z for release"
   ```

4. Create and push the tag:

   ```bash
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```

5. Report completion with link to [GitHub Actions][actions].

[actions]: https://github.com/jamesbrink/nxv/actions

## Important Notes

- Never proceed past Phase 4 without explicit user confirmation.
- Stop immediately if any pre-flight check fails.
- Version must follow semver: `MAJOR.MINOR.PATCH`.
- Pre-release versions use hyphen suffix (e.g., `0.2.0-rc1`).
- The `created` timestamp in `flake.nix` only affects Docker image metadata.

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
markdownlint '**/*.md' --ignore node_modules
nix flake check
git status --porcelain
```

If `git status` shows uncommitted changes, stop and inform the user.

## Phase 2: Gather Release Information

1. Get the current version from `Cargo.toml`.
2. Get the last tag with `git describe --tags --abbrev=0`.
3. Get commits since last tag with `git log --oneline <last-tag>..HEAD`.
4. Read current `CHANGELOG.md` to see unreleased section.

## Phase 3: Determine Version

Follow semantic versioning (`MAJOR.MINOR.PATCH`):

- **MAJOR** - Breaking changes (API incompatibility, removed features)
- **MINOR** - New features (backward compatible additions)
- **PATCH** - Bug fixes (backward compatible fixes)

Pre-release versions use hyphen suffix (e.g., `0.2.0-rc1`).

Review the commits and suggest an appropriate version bump based on the changes.

## Phase 4: Generate Release Notes

Create markdown release notes with these sections:

- **Features** - New functionality (commits starting with `feat:`)
- **Fixes** - Bug fixes (commits starting with `fix:`)
- **Other** - Everything else (refactor, chore, docs, etc.)

## Phase 5: User Confirmation

Present a summary and ask for confirmation before proceeding:

```text
=== RELEASE SUMMARY ===

Current version: X.Y.Z
Suggested version: A.B.C (reason: <major/minor/patch bump rationale>)

Commits to be released:
  <commit list>

Release Notes:
  <generated notes>

Files to modify:
  1. Cargo.toml - bump version to A.B.C
  2. CHANGELOG.md - move Unreleased items to [A.B.C] section

Actions after commit:
  3. Commit with message "chore: release A.B.C"
  4. Create and push tag vA.B.C

CI/CD will then:
  - Build static binaries (Linux x86_64/aarch64, macOS x86_64/ARM64)
  - Create GitHub Release with binaries and checksums
  - Publish to crates.io
  - Push Docker image to ghcr.io/jamesbrink/nxv (timestamp derived from commit)
  - Publish to FlakeHub

Proceed? Enter version number to confirm, or "abort" to cancel.
```

Use `AskUserQuestion` to get confirmation. The user must provide the version
number (e.g., `0.1.4` or `0.2.0`).

## Phase 6: Execute Release

Only proceed after explicit user confirmation with a version number.

### Step 1: Update Cargo.toml

Edit the `version` field to the new version.

### Step 2: Update CHANGELOG.md

Move the `[Unreleased]` section contents to a new version section:

1. Create new section `## [A.B.C] - YYYY-MM-DD` below `[Unreleased]`.
2. Move all content from `[Unreleased]` to the new section.
3. Leave `[Unreleased]` empty (keep the heading).
4. Update the comparison links at the bottom:
   - Change `[unreleased]` link to compare against new tag.
   - Add new version link.

### Step 3: Commit changes

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "chore: release A.B.C"
```

### Step 4: Create and push tag

```bash
git tag vA.B.C
git push origin main
git push origin vA.B.C
```

### Step 5: Report completion

Provide link to [GitHub Actions][actions] and remind user to monitor the
release workflow.

[actions]: https://github.com/jamesbrink/nxv/actions

## Important Notes

- Never proceed past Phase 5 without explicit user confirmation.
- Stop immediately if any pre-flight check fails.
- The changelog must be updated as part of the release.
- Version bumps must follow semver conventions.
- Docker image timestamp is derived automatically from git commit date.

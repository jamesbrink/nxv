---
description: Prepare and execute a release with pre-flight checks and user confirmation
allowed-tools: Bash, Read, Grep, Edit, AskUserQuestion
---

# Release

Prepare and execute a release for nxv. This skill runs pre-flight checks, generates release notes, and requires explicit user confirmation before making any changes.

## Phase 1: Pre-flight Checks

Run these checks and **STOP if any fail**:

```bash
cargo fmt --check
cargo clippy --features indexer -- -D warnings
cargo test --features indexer
nix flake check
git status --porcelain
```

If `git status` shows uncommitted changes, **STOP** and inform the user.

## Phase 2: Gather Release Information

1. Get the current version from `Cargo.toml`
2. Get the last tag: `git describe --tags --abbrev=0`
3. Get commits since last tag: `git log --oneline $(git describe --tags --abbrev=0)..HEAD`
4. Get today's UTC timestamp: `date -u +%Y-%m-%dT%H:%M:%SZ`
5. Check the `created` timestamp in `flake.nix` Docker config

## Phase 3: Generate Release Notes

Create markdown release notes with sections:
- **Features**: New functionality (commits starting with `feat:`)
- **Fixes**: Bug fixes (commits starting with `fix:`)
- **Other Changes**: Everything else (refactor, chore, docs, etc.)

## Phase 4: User Confirmation

Present the following to the user and **ASK FOR CONFIRMATION** before proceeding:

```
=== RELEASE SUMMARY ===

Current version: <current>
New version: <to be confirmed>

Commits to be released:
<commit list>

Release Notes:
<generated notes>

Actions that will be performed:
1. Bump version in Cargo.toml to <new version>
2. Update Docker 'created' timestamp in flake.nix to <today>
3. Commit changes with message: "chore: bump version to <new version> for release"
4. Create and push tag: v<new version>

CI/CD will then automatically:
- Build static binaries (Linux x86_64/aarch64, macOS x86_64/ARM64)
- Create GitHub Release with binaries and checksums
- Publish to crates.io
- Push Docker image to ghcr.io/jamesbrink/nxv:<version> (and :latest if stable)
- Publish to FlakeHub

Proceed with release? [Provide version number to confirm, or 'abort' to cancel]
```

Use **AskUserQuestion** to get confirmation. The user must provide the version number they want to release (e.g., "0.1.4" or "0.2.0").

## Phase 5: Execute Release (only after confirmation)

Only proceed if the user explicitly confirms with a version number.

1. **Update Cargo.toml version**:
   ```bash
   # Edit the version line in Cargo.toml
   ```

2. **Update flake.nix Docker timestamp**:
   ```bash
   # Edit the created timestamp in flake.nix
   ```

3. **Commit the changes**:
   ```bash
   git add Cargo.toml flake.nix
   git commit -m "chore: bump version to <version> for release"
   ```

4. **Create and push the tag**:
   ```bash
   git tag v<version>
   git push origin main
   git push origin v<version>
   ```

5. **Report completion**:
   - Provide link to GitHub Actions: `https://github.com/jamesbrink/nxv/actions`
   - Remind user to monitor the release workflow

## Important Notes

- **NEVER** proceed past Phase 4 without explicit user confirmation
- If any pre-flight check fails, **STOP** and report the failure
- Version must follow semver (MAJOR.MINOR.PATCH)
- Pre-release versions use hyphen suffix (e.g., 0.2.0-rc1)
- The `created` timestamp in flake.nix affects Docker image metadata only

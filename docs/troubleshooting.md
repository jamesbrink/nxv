# Troubleshooting Guide

This guide covers common issues with nxv indexing and how to diagnose them.

## Table of Contents

- [Missing Package Versions](#missing-package-versions)
- [Unknown Version Values](#unknown-version-values)
- [Version Source Analysis](#version-source-analysis)
- [all-packages.nix Issues](#all-packagesnix-issues)
- [Validation and Debugging](#validation-and-debugging)

## Missing Package Versions

### Symptoms

- Package exists in nixpkgs but not in nxv database
- Fewer versions than expected for a package
- Gap in version history (e.g., versions 1.0-1.5 exist, then 1.6-2.0 missing)

### Diagnosis

1. **Check if package was indexed:**

   ```bash
   nxv search <package_name>
   ```

2. **Check version count:**

   ```bash
   sqlite3 ~/.local/share/nxv/index.db \
     "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '<package>';"
   ```

3. **Check for gaps in version history:**

   ```bash
   sqlite3 ~/.local/share/nxv/index.db \
     "SELECT version, datetime(first_commit_date, 'unixepoch') as date
      FROM package_versions
      WHERE attribute_path = '<package>'
      ORDER BY first_commit_date;"
   ```

### Common Causes

1. **all-packages.nix changes not captured** (pre-fix issue)
   - Versions after July 2023 may be missing due to the INFRASTRUCTURE_FILES exclusion bug
   - Solution: Re-index with the fixed indexer

2. **Package defined in excluded file**
   - Some packages are defined in infrastructure files
   - Check if package is in `pkgs/top-level/all-packages.nix` or `aliases.nix`

3. **Wrapper package without version passthrough**
   - See [Unknown Version Values](#unknown-version-values)

## Unknown Version Values

### Symptoms

- Package shows version as "unknown" or empty
- `version_source` is NULL

### Diagnosis

1. **Check version_source distribution for a package:**

   ```bash
   sqlite3 ~/.local/share/nxv/index.db \
     "SELECT version, version_source FROM package_versions
      WHERE attribute_path = '<package>'
      ORDER BY first_commit_date DESC LIMIT 10;"
   ```

2. **Check overall unknown rate:**

   ```bash
   sqlite3 ~/.local/share/nxv/index.db \
     "SELECT ROUND(COUNT(*) * 100.0 / (SELECT COUNT(*) FROM package_versions), 2)
      FROM package_versions
      WHERE version = 'unknown' OR version IS NULL OR version = '';"
   ```

### Common Causes

1. **Wrapper packages** (e.g., neovim, weechat)
   - Wrapper doesn't expose underlying `.version` attribute
   - Solution: The indexer now tries `unwrapped.version` and `passthru.unwrapped.version`

2. **Build hooks and infrastructure packages**
   - Packages like `wrapQtAppsHook`, `addOpenGLRunpath` have no version
   - This is expected behavior - these are legitimate versionless packages

3. **Language package sets**
   - Some packages in `pythonPackages`, `nodePackages`, etc. don't expose versions
   - Check if `-unwrapped` variant exists with version

### Version Extraction Fallback Chain

The indexer tries these sources in order:

| Order | Source     | Example                          | When Used                    |
| ----- | ---------- | -------------------------------- | ---------------------------- |
| 1     | direct     | `pkg.version`                    | Standard packages            |
| 2     | unwrapped  | `pkg.unwrapped.version`          | Wrapper packages             |
| 3     | passthru   | `pkg.passthru.unwrapped.version` | Complex wrappers             |
| 4     | name       | Extract from `pkg.name`          | When no version attr exists  |

## Version Source Analysis

### Check version_source distribution

```bash
sqlite3 ~/.local/share/nxv/index.db "
SELECT
    COALESCE(version_source, 'NULL') as source,
    COUNT(*) as count,
    ROUND(COUNT(*) * 100.0 / (SELECT COUNT(*) FROM package_versions), 1) as pct
FROM package_versions
GROUP BY version_source
ORDER BY count DESC;
"
```

### Expected Distribution

| Source   | Expected % | Notes                                  |
| -------- | ---------- | -------------------------------------- |
| direct   | 70-80%     | Most packages have standard `.version` |
| name     | 10-20%     | Packages with version in name          |
| unwrapped| 2-5%       | Wrapper packages                       |
| passthru | 1-2%       | Complex wrapper chains                 |
| NULL     | 5-10%      | Build hooks, infrastructure            |

### Investigate specific version_source

```bash
# Find packages using name-based extraction
sqlite3 ~/.local/share/nxv/index.db "
SELECT DISTINCT attribute_path, name, version
FROM package_versions
WHERE version_source = 'name'
LIMIT 20;
"
```

## all-packages.nix Issues

### Background

The file `pkgs/top-level/all-packages.nix` is special:

- Contains ~18,000 package definitions
- Changed on almost every nixpkgs commit
- Full extraction takes ~91 seconds

### How It's Handled

1. When `all-packages.nix` changes, parse the git diff
2. Extract affected attribute names from changed lines
3. Only extract those specific packages (~7 on average)
4. Fall back to full extraction for large diffs (>100 lines)

### Debugging all-packages.nix Extraction

1. **Check if diff parsing is working:**

   ```bash
   # Look at recent commits that touched all-packages.nix
   git -C /path/to/nixpkgs log --oneline -- pkgs/top-level/all-packages.nix | head -10
   ```

2. **Test diff extraction manually:**

   ```bash
   git -C /path/to/nixpkgs diff <commit>^..<commit> -- pkgs/top-level/all-packages.nix
   ```

3. **Check for patterns the parser handles:**
   - `  attrName = ...` (assignment)
   - `  attrName = callPackage ...` (callPackage)
   - `  inherit (scope) attr1 attr2;` (inherit)

### Known Limitations

- Helper function changes (e.g., `wrapThunderbird` modified) won't trigger package re-extraction
- Very large diffs (>100 lines) trigger full extraction which is slow
- Multi-line attribute definitions may not be fully captured

## Validation and Debugging

### Pre-Reindex QA

Run before starting a full reindex:

```bash
./scripts/pre_reindex_qa.sh --nixpkgs /path/to/nixpkgs
```

Checks:

- Code formatting (cargo fmt)
- Clippy lints
- Unit tests
- Nix syntax validation
- Git status

### Post-Reindex Validation

Run after completing a reindex:

```bash
./scripts/post_reindex_validation.sh --nixpkgs /path/to/nixpkgs
```

Checks:

- Database statistics
- Version source distribution
- Unknown version rate
- Critical package coverage
- NixHub comparison

### Compare Against NixHub

```bash
# Basic comparison
python scripts/validate_against_nixhub.py --packages firefox,chromium,python3

# With git verification
python scripts/validate_against_nixhub.py \
  --packages firefox,chromium \
  --nixpkgs /path/to/nixpkgs \
  --verify-commits

# Comprehensive validation
python scripts/validate_against_nixhub.py --comprehensive --output report.json
```

### Debug Specific Package

```bash
# Full package history
sqlite3 ~/.local/share/nxv/index.db "
SELECT
    version,
    version_source,
    datetime(first_commit_date, 'unixepoch') as first_seen,
    datetime(last_commit_date, 'unixepoch') as last_seen,
    first_commit_hash
FROM package_versions
WHERE attribute_path = 'firefox'
ORDER BY first_commit_date DESC;
"

# Check if commit exists in nixpkgs
git -C /path/to/nixpkgs cat-file -t <commit_hash>

# Check package version at specific commit
git -C /path/to/nixpkgs checkout <commit_hash>
nix-instantiate --eval -E '(import <nixpkgs> {}).firefox.version' -I nixpkgs=/path/to/nixpkgs
git -C /path/to/nixpkgs checkout -
```

## Getting Help

1. Check the [GitHub Issues](https://github.com/jamesbrink/nxv/issues)
2. Run with verbose logging: `RUST_LOG=debug nxv ...`
3. For indexer issues: `RUST_LOG=nxv::index=trace cargo run --features indexer ...`

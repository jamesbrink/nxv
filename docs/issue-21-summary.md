# Issue #21 Resolution Summary

**Issue:** [#21 - Incomplete set and wrong hashes?](https://github.com/jamesbrink/nxv/issues/21)

## Problem

Users reported that nxv was missing package versions, particularly for packages like `thunderbird`, `firefox`, and `chromium`. Investigation revealed two separate issues:

### Issue 1: all-packages.nix Exclusion (Primary)

When `pkgs/top-level/all-packages.nix` changed, the indexer couldn't determine which packages were affected. This file contains ~18,000 package definitions and changes on almost every commit.

The `INFRASTRUCTURE_FILES` exclusion was added to avoid slow full extractions (~91 seconds), but it caused the indexer to miss package updates.

**Evidence:**
- After July 24, 2023: only ~8% of expected packages captured
- thunderbird: only versions up to 115.0.1 (July 2023) vs NixHub's 146.0.1 (Dec 2025)
- ~27 months of versions missing for major packages

### Issue 2: Wrapper Version Extraction (Secondary)

Some packages (neovim, weechat) are wrappers that don't expose `.version`:

```nix
# all-packages.nix
neovim = wrapNeovim neovim-unwrapped { };
```

The wrapper doesn't preserve the version attribute, resulting in "unknown" versions for ~39% of packages.

## Solution

### Phase 1: Version Extraction Improvements

Added fallback chain in `extract.nix`:

1. **direct**: `pkg.version` (standard)
2. **unwrapped**: `pkg.unwrapped.version` (wrapper packages)
3. **passthru**: `pkg.passthru.unwrapped.version` (complex wrappers)
4. **name**: Extract from package name using regex (e.g., "hello-2.12" → "2.12")

Added `version_source` field to track extraction method.

### Phase 2: all-packages.nix Diff Parsing

Instead of extracting all 18,000 packages or none:

1. Parse git diff for `all-packages.nix` changes
2. Extract affected attribute names from changed lines
3. Only extract those specific packages (~7 average)
4. Fall back to full extraction for large diffs (>100 lines)

**Performance:**
- Normal commits: ~7 packages, ~2 seconds
- Large updates: ~18,000 packages, ~91 seconds (fallback)
- Average: ~6 seconds per commit

### Phase 3: Validation Infrastructure

- Enhanced `validate_against_nixhub.py` with `--verify-commits` and `--verify-versions`
- Created regression test suite with 50+ known-good packages
- Added QA scripts: `pre_reindex_qa.sh`, `post_reindex_validation.sh`
- Added CI quality gate in `publish-index.yml`

### Phase 4: Reindex Required

A full reindex from 2017 is required to apply the fixes. The current database was truncated to July 24, 2023 (last known-good date).

## Files Changed

- `src/index/nix/extract.nix` - Version fallback chain
- `src/index/extractor.rs` - PackageInfo.version_source field
- `src/index/git.rs` - get_file_diff() for diff parsing
- `src/index/mod.rs` - extract_attrs_from_diff(), infrastructure file handling
- `src/db/mod.rs` - version_source column in schema
- `scripts/validate_against_nixhub.py` - Enhanced validation
- `scripts/pre_reindex_qa.sh` - Pre-reindex checks
- `scripts/post_reindex_validation.sh` - Post-reindex validation
- `.github/workflows/publish-index.yml` - CI quality gate
- `tests/fixtures/regression_packages.json` - Regression test data

## Validation Results (Pre-Reindex)

Research on 500 post-July 2023 commits showed:
- 95.2% of commits have extractable attribute names via diff parsing
- Average 7 attributes per commit (vs 18,000 for full extraction)
- Only 0.8% of commits require full extraction fallback

## Next Steps

1. **User performs full reindex** with fixed indexer
2. Run post-reindex validation: `./scripts/post_reindex_validation.sh`
3. Target: >80% completeness vs NixHub for major packages
4. Publish new index to `index-latest` release

## Closing Notes

This issue highlighted the importance of:
- Handling infrastructure files specially (not just excluding them)
- Tracking extraction metadata (version_source) for debugging
- Automated validation against known-good data sources (NixHub)
- Pre/post-reindex QA gates to catch regressions

The fix maintains the performance benefits of the original INFRASTRUCTURE_FILES exclusion while correctly capturing package updates.

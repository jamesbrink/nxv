# Indexer Issue: Missing Package Updates from all-packages.nix

**Status:** Phases 1-3 complete, pending reindex (Phase 4)
**Related Issue:** [#21 - Incomplete set and wrong hashes?](https://github.com/jamesbrink/nxv/issues/21)
**Date:** 2026-01-09
**Last Updated:** 2026-01-09 (Phase 3 completed)

## Executive Summary

Investigation revealed **two separate issues** affecting the nxv indexer:

1. **all-packages.nix issue** (primary): When `all-packages.nix` changes, the indexer can't determine which packages were affected due to the `INFRASTRUCTURE_FILES` exclusion. This caused the post-July 2023 data gap.

2. **Wrapper version issue** (secondary): Some packages (neovim, weechat) are wrappers that don't expose `.version`, resulting in "unknown" versions. This affects 33,760 packages (39%) but is a separate issue.

**Pre-July 2023 data quality**: 95-99% extraction success rate for most months. Data is largely usable.

**Selected solution**: Option 2 - Parse diffs to extract affected attribute names (95.2% success rate in testing).

## Problem Summary

The nxv indexer is missing package version updates for packages defined via `callPackage` in `pkgs/top-level/all-packages.nix`. This affects major packages like `thunderbird`, `firefox`, `chromium`, and many others.

### Evidence

Database analysis shows a dramatic drop in package capture rate:

| Date | New Packages/Day |
|------|------------------|
| July 24, 2023 | 34,562 |
| July 25, 2023 | 151 |

After July 24, 2023, only ~8% of expected packages are being captured.

**Thunderbird example:**
- NixHub shows versions up to 146.0.1 (Dec 2025)
- nxv database only has up to 115.0.1 (July 2023)
- ~27 months of versions missing

## Root Cause Analysis

### The Architecture

The indexer uses a file-to-attribute mapping approach:

1. **Build position map:** Use `builtins.unsafeGetAttrPos` to map attribute names to file locations
2. **On each commit:** Get changed file paths via `git diff`
3. **Map files to attrs:** Look up which attributes to extract based on changed files
4. **Fallback heuristics:** If file not in map, try to extract attr name from path

### The Problem

`builtins.unsafeGetAttrPos` returns where an attribute is **assigned**, not where its value is **defined**.

```nix
# all-packages.nix line 11930
thunderbird = wrapThunderbird thunderbird-unwrapped { };
```

When we call `builtins.unsafeGetAttrPos "thunderbird" pkgs`, it returns:
```json
{"column":3,"file":".../pkgs/top-level/all-packages.nix","line":11930}
```

But the actual thunderbird code lives in:
```
pkgs/applications/networking/mailreaders/thunderbird/packages.nix
```

### The INFRASTRUCTURE_FILES Exclusion

To avoid triggering 18,000+ package extractions on every commit that touches `all-packages.nix`, this exclusion was added:

```rust
const INFRASTRUCTURE_FILES: &[&str] = &[
    "pkgs/top-level/all-packages.nix",
    "pkgs/top-level/aliases.nix",
];
```

**Result:** When `all-packages.nix` changes (which happens on most version bumps), the indexer:
1. Doesn't use the file-to-attribute map (file is excluded)
2. Falls back to path heuristics
3. Path heuristics fail for non-standard structures
4. Package update is missed

### Fallback Heuristic Failure

The fallback tries to extract attribute names from paths:

```rust
// For pkgs/by-name/XX/pkgname/package.nix -> "pkgname" (works)
// For pkgs/.../thunderbird/default.nix -> "thunderbird" (works)
// For pkgs/.../thunderbird/packages.nix -> "packages" (FAILS!)
```

Thunderbird has no `default.nix`, only `packages.nix`, so the heuristic extracts "packages" which isn't a valid attribute.

## Proposed Solutions

### Option 1: Remove INFRASTRUCTURE_FILES Exclusion (Rejected)

**Pros:**
- Simple, guaranteed correct
- No complex diff parsing

**Cons:**
- ~91 seconds per commit touching all-packages.nix
- Full reindex would take days/weeks
- Wasteful: most all-packages.nix changes only affect 1-2 packages

**Status:** Rejected - too slow for practical use.

### Option 2: Parse Diffs to Extract Affected Attributes (Selected)

When `all-packages.nix` or `aliases.nix` changes:

1. Get the diff: `git diff <prev>..<curr> -- pkgs/top-level/all-packages.nix`
2. Parse changed lines to extract attribute names
3. Only extract those specific attributes

**Example diff:**
```diff
-  thunderbird = wrapThunderbird thunderbird-unwrapped { };
+  thunderbird = wrapThunderbird thunderbird-unwrapped { enableFoo = true; };
```

**Extraction:** Line starts with `thunderbird =` -> extract `thunderbird`

**Pros:**
- Fast: only extract affected packages (~7 attrs avg vs 18,000)
- Accurate for version bumps (always touch the package line)
- 95.2% success rate in testing

**Cons:**
- May miss indirect changes (helper function modifications)
- Requires robust diff parsing

**Status:** Selected as primary approach.

## Selected Implementation: Option 2 with Fallback

### Algorithm

```
for each commit:
    changed_files = git_diff(commit)
    target_attrs = []

    for file in changed_files:
        if file in INFRASTRUCTURE_FILES:
            # Parse diff to extract affected attributes
            diff_text = git_diff_file(commit, file)
            attrs = extract_attrs_from_diff(diff_text)

            if len(attrs) == 0 or diff_is_too_large(diff_text):
                # FALLBACK: Large structural change, extract all
                # This handles ~5% of commits
                attrs = get_all_package_attrs()

            target_attrs.extend(attrs)
        else:
            # Normal file-to-attr mapping
            attrs = file_attr_map.get(file, [])
            target_attrs.extend(attrs)

    # Extract packages for all target attributes
    extract_packages(target_attrs)
```

### Attribute Extraction Patterns

The following regex patterns extract attribute names from diff lines:

| Pattern | Example | Extracts |
|---------|---------|----------|
| Assignment | `  thunderbird = ...` | `thunderbird` |
| callPackage | `  hello = callPackage ...` | `hello` |
| Override | `  pkg = prev.pkg.override ...` | `pkg` |
| Inherit | `  inherit (foo) bar baz;` | `bar`, `baz` |

### Fallback Triggers

Fall back to full extraction when:

1. **No attrs extracted** - Diff contains only comments, whitespace, or unparseable lines
2. **Large diff** - More than 100 lines changed (indicates bulk update)
3. **Known problematic patterns** - `let`/`in` blocks, function definitions

### Performance Expectations

| Scenario | Attrs to Extract | Time |
|----------|------------------|------|
| Normal commit (95%) | ~7 | ~2s |
| Large diff fallback (5%) | ~18,000 | ~91s |
| Average | ~900 | ~6s |

### Edge Cases

1. **Helper function changes**: If someone modifies a helper like `wrapThunderbird`, we won't detect which packages use it. These are rare and typically accompanied by package-level changes anyway.

2. **Bulk updates**: Mass version bumps (e.g., Python package updates) will trigger fallback. This is correct behavior.

3. **Deleted packages**: Detected via `-` lines in diff, used to close version ranges.

## Additional Issue: Wrapper Version Extraction

During investigation, a **separate issue** was discovered affecting certain wrapped packages.

### The Problem

Some packages are defined as wrappers around an "unwrapped" base:

```nix
# all-packages.nix
neovim = wrapNeovim neovim-unwrapped { };
```

The wrapper (`neovim`) often doesn't preserve the `.version` attribute from the unwrapped package (`neovim-unwrapped`).

### Evidence

| Attribute | Versions Extracted | Notes |
|-----------|-------------------|-------|
| `neovim` | 0.1.7 only (2017-2019), then **unknown** | Wrapper doesn't expose version |
| `neovim-unwrapped` | 0.5.1 → 0.9.1 (2021-2023) | All versions correct |
| `weechat` | 1.6 only (2017-2019), then **unknown** | Same wrapper pattern |
| `firefox` | All versions correct (94.0.2+) | Wrapper preserves version |
| `chromium` | All versions correct (100.0+) | Wrapper preserves version |

### Impact

- **33,760 packages** (39% of all) have at least one "unknown" version entry
- Most affected: wrapped packages, language-specific package sets (rPackages, pythonPackages)
- Top affected source paths are builder/wrapper files, not package definitions

### Recommendation

This is a **separate issue** from the all-packages.nix problem and should be addressed independently:

1. **Short-term**: Index both wrapped and unwrapped variants
2. **Long-term**: Improve extraction to follow wrapper chains to find version

## Pre-July 2023 Data Quality

Analysis of extraction success rate by month shows the indexer has worked well overall.

### Monthly Extraction Success Rate

| Period | Success Rate | Notes |
|--------|--------------|-------|
| 2017-2019 | 45-92% (variable) | Early indexer, some issues |
| 2020 | **98-99%** | Excellent |
| 2021 | **96-99%** | Excellent |
| 2022 | **86-97%** | Good, with periodic spikes |
| Jan-Jul 2023 | **50-99%** | Good, March 2023 anomaly |

### Periodic Failure Spikes

Certain months show elevated failure rates:

| Month | Known Rate | Likely Cause |
|-------|------------|--------------|
| Jan 2022 | 86.5% | Mass package set update |
| Aug 2022 | 89.2% | Structural changes |
| Mar 2023 | 50.9% | Nixpkgs reorganization |

### Assessment

Pre-July 2023 data is **largely usable** with these caveats:

1. ~95-99% of versions extracted correctly for standard packages
2. Wrapped packages (neovim, weechat, etc.) missing versions
3. Language package sets (rPackages, pythonPackages) have gaps
4. Some historical months have lower success rates

## Unknown Version Recovery Analysis

Investigation of the 560,986 "unknown" version records revealed that **99% can be recovered** from the package name.

### Statistics

| Metric | Count |
|--------|-------|
| Total unknown version records | 560,986 |
| Unique attr+name combinations | 64,873 |
| **Recoverable from name** | 64,251 (99.0%) |
| Truly unrecoverable | 622 (1.0%) |

### Version Extraction Patterns

The following patterns can extract versions from package names:

| Pattern | Example | Extracts |
|---------|---------|----------|
| Semver | `hello-2.12.1` | `2.12.1` |
| Semver with suffix | `app-1.01b` | `1.01b` |
| Date (ISO) | `pkg-2021-07-29` | `2021-07-29` |
| Date (compact) | `OVMF-202202` | `202202` |
| Single number | `boost-1.75` | `1.75` |

### Unrecoverable Categories

The ~1% truly unrecoverable are legitimate versionless packages:

| Category | Count | Examples |
|----------|-------|----------|
| Build hooks | 50 | `breakpointHook`, `wrapQtAppsHook` |
| Stdenv/bootstrap | 32 | `gcc6Stdenv`, `bootstrapTools` |
| Infrastructure | 431 | `addOpenGLRunpath`, `steam-run` |
| Misc | 109 | `jre8`, `certbot-full` |

**Recommendation:** Skip storing these versionless packages rather than storing "unknown".

## Research Results

### Diff Analysis (500 commits since July 2023)

Run: `python scripts/research_all_packages_diffs.py --nixpkgs ./nixpkgs --limit 500`

| Metric | Value |
|--------|-------|
| Commits analyzed | 500 |
| Touching all-packages.nix | 346 (69%) |
| Touching aliases.nix | 254 (51%) |
| Success rate (>=1 attr extracted) | **95.2%** |
| Avg attrs per commit | 7.0 |
| Avg lines changed per commit | 13.9 |
| Large diffs (>100 lines) | 4 (0.8%) |

**Extraction Methods:**
- `assignment`: 3,134 (89%)
- `callPackage`: 342 (10%)
- `inherit`: 32 (1%)
- `override`: 1 (<1%)

**Assessment:** Diff parsing is viable for 95%+ of commits.

### NixHub Validation (15 packages)

Run: `python scripts/validate_against_nixhub.py`

| Package | nxv | NixHub | Completeness |
|---------|-----|--------|--------------|
| hello | 4 | 4 | 75.0% |
| git | 43 | 50 | 48.0% |
| thunderbird | 68 | 134 | 42.5% |
| rustc | 25 | 55 | 38.2% |
| vim | 28 | 56 | 32.1% |
| chromium | 74 | 217 | 29.0% |
| go | 44 | 167 | 25.7% |
| nodejs | 72 | 257 | 24.9% |
| curl | 15 | 46 | 23.9% |
| firefox | 78 | 178 | 23.0% |
| neovim | 1 | 28 | **0.0%** |

**Overall:**
- Average completeness: **39.4%**
- Total missing versions: **870** (out of 1,251)
- Commit hash mismatches: 377 (different first-seen commits)

### Conclusions

1. **Option 2 is viable** - 95.2% of commits have extractable attribute names
2. **Post-July 2023 gap is severe** - Only 39.4% completeness vs NixHub for this period
3. **Pre-July 2023 data is good** - 95-99% extraction success rate for most months
4. **Two separate issues identified**:
   - **all-packages.nix issue**: Causes post-July 2023 gaps (affects all packages)
   - **Wrapper version issue**: Causes "unknown" versions for specific packages (neovim, weechat, etc.)
5. **Large packages hit hardest** - nodejs, chromium, firefox at ~25% complete post-July 2023
6. **Some packages need unwrapped variants** - neovim at 0% because wrapper doesn't expose version

## Research Scripts

- `scripts/research_all_packages_diffs.py` - Analyze historical diffs
- `scripts/validate_against_nixhub.py` - Compare nxv data with NixHub

## Database State

After truncation (2026-01-09):
- **Last indexed commit:** `c4a065f6c30efacc79a635274a9d765503bb91be`
- **Last indexed date:** July 24, 2023 23:01:34 UTC
- **Total records:** 7,941,464
- **Status:** Ready for reindex with fixed logic

## Files Involved

- `src/index/mod.rs` - Main indexer logic, `INFRASTRUCTURE_FILES` constant, diff parsing
- `src/index/nix/positions.nix` - Position extraction using `unsafeGetAttrPos`
- `src/index/nix/extract.nix` - Package metadata extraction, version fallback chain
- `src/index/git.rs` - Git operations including `get_file_diff()` for diff content
- `src/index/extractor.rs` - PackageInfo struct with `version_source` field
- `src/db/mod.rs` - Schema with `version_source` column (schema v4)
- `src/db/queries.rs` - PackageVersion struct with `version_source` field

## Next Steps

### Research & Analysis (Completed)

- [x] Run research script to analyze all-packages.nix diff patterns
- [x] Run validation script to quantify data gaps vs NixHub
- [x] Decide on solution based on research findings (Option 2 selected)
- [x] Analyze pre-July 2023 data quality (95-99% success rate)
- [x] Identify wrapper version issue as separate problem
- [x] Analyze unknown version patterns (99% recoverable from name)

### Phase 1: Version Extraction Improvements (Completed)

- [x] **1.1** Add wrapper version fallback in `extract.nix`
  - Try `pkg.version` (direct)
  - Try `pkg.unwrapped.version`
  - Try `pkg.passthru.unwrapped.version`

- [x] **1.2** Add name-based version extraction fallback
  - Parse version from package name (e.g., `hello-2.12` → `2.12`)
  - Handles 99% of previously "unknown" versions
  - Patterns: semver, dates (YYYY-MM-DD), YYYYMMDD, single numbers

- [x] **1.3** Keep packages with no extractable version
  - Store with NULL version rather than skipping
  - Ensures all packages are tracked even without version info

- [x] **1.4** Add `version_source` field to track extraction method
  - Values: `direct`, `unwrapped`, `passthru`, `name`, or NULL
  - Helps debug issues without re-indexing

### Phase 2: all-packages.nix Diff Parsing (Completed)

- [x] **2.1** Implement diff parsing for INFRASTRUCTURE_FILES
  - Added `get_file_diff()` method to NixpkgsRepo
  - Parse git diff output to extract affected attribute names
  - String parsing for: assignment (`attr =`), inherit patterns

- [x] **2.2** Add fallback for large/unparseable diffs
  - If >100 lines changed, triggers full extraction
  - Handles bulk updates and structural changes

- [x] **2.3** Add metrics logging for diff parsing
  - Log attrs extracted per commit at trace level
  - Log fallback triggers at debug level

### Phase 3: Validation & Testing (Completed)

- [x] **3.1** Enhance validation script
  - Added `--nixpkgs PATH` option for git-based validation
  - Added `--verify-commits` to check commits exist in nixpkgs
  - Added `--verify-versions` to verify version at recorded commit
  - Check store paths against cache.nixos.org (already exists)

- [x] **3.2** Create regression test suite
  - Created `tests/fixtures/regression_packages.json` with 50+ packages
  - Covers stable, browsers, languages, compilers, databases, etc.
  - Includes edge cases: wrappers, nested packages, date versions
  - Added `test_regression_fixture` Rust test (run with `--ignored`)

- [x] **3.3** Add spot-check validation against actual nixpkgs git log
  - `--verify-commits` validates commit hashes exist
  - `--verify-versions` confirms package version at commit
  - Results tracked in validation reports

### Phase 4: Database & Reindex

- [ ] **4.1** Nuke existing database
  - Fresh start with improved extraction logic

- [ ] **4.2** Full reindex from 2017
  - With all extraction improvements in place
  - Monitor metrics during indexing

- [ ] **4.3** Post-reindex validation
  - Run validation script against NixHub
  - Target: >95% completeness for major packages
  - Verify no regressions from known-good test set

### Phase 5: Documentation & Cleanup

- [ ] **5.1** Update CLAUDE.md with new extraction logic
- [ ] **5.2** Document version extraction fallback chain
- [ ] **5.3** Add troubleshooting guide for common issues
- [ ] **5.4** Close GitHub issue #21 with summary

## References

- [NixHub API Documentation](./nixhub-api.md)
- [GitHub Issue #21](https://github.com/jamesbrink/nxv/issues/21)
- Commit `3921ddf` - Added INFRASTRUCTURE_FILES exclusion

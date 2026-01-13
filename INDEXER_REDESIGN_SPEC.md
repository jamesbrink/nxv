# nxv Indexer Redesign Specification

**Status**: Draft
**Branch**: `experiment/smart-indexer`
**Date**: 2026-01-13
**Authors**: Claude + Codex (Peer Review)

## Executive Summary

This specification proposes a hybrid static + dynamic approach to achieve complete package coverage in the nxv indexer. The current implementation misses ~10-15% of packages due to a fundamental limitation in how `builtins.unsafeGetAttrPos` works.

## Problem Statement

### The Core Issue

The current implementation uses `builtins.unsafeGetAttrPos` to build a file-to-attribute map (`file_attr_map`). This function returns the **assignment location**, not the **definition location**.

**Example:**
```nix
# In pkgs/top-level/all-packages.nix:
firefox = callPackage ../applications/networking/browsers/firefox/packages.nix { };
```

- `unsafeGetAttrPos "firefox" pkgs` returns `all-packages.nix:line_number`
- NOT `pkgs/applications/networking/browsers/firefox/packages.nix`

**Consequence:** When `firefox/packages.nix` changes, the indexer cannot determine that the `firefox` attribute is affected because `file_attr_map` only maps to `all-packages.nix`.

### Packages Affected

This affects all "wrapper packages" where:
- The attribute is assigned in `all-packages.nix` via `callPackage`
- The actual package definition lives in a separate file

Examples: firefox, chromium, vscode, many KDE/GNOME packages, etc.

## Constraints

| Constraint | Rationale |
|------------|-----------|
| NO sampling (every X days/weeks) | Must capture every commit for complete history |
| NO complex CLI arguments | User experience |
| NO shell-based nix eval | Nix C API is faster |
| NO reliance on NixHub/NixOS CI | Self-contained solution |
| YES lower-level optimizations | Performance critical |
| YES raw git file reading | Avoid expensive checkouts |

## Proposed Solution: Hybrid Static + Dynamic Approach

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Indexer Flow                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐  │
│  │  Git Commit  │───▶│  Blob Cache  │───▶│  Static Parser   │  │
│  │   Walker     │    │  (by hash)   │    │  (rnix-parser)   │  │
│  └──────────────┘    └──────────────┘    └──────────────────┘  │
│                                                   │             │
│                                                   ▼             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐  │
│  │   Package    │◀───│   Dynamic    │◀───│  Static File     │  │
│  │  Extractor   │    │   Fallback   │    │  Attr Map        │  │
│  └──────────────┘    └──────────────┘    └──────────────────┘  │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Phase 1: Static Analysis Module

**File:** `src/index/static_analysis.rs`

**Dependency:** `rnix-parser` crate

**Purpose:** Parse `all-packages.nix` into AST and build a **reverse** file-to-attribute map.

#### Supported Patterns

```nix
# Pattern 1: Direct callPackage with literal path
firefox = callPackage ../path/to/package.nix { };

# Pattern 2: callPackages (plural)
firefoxPackages = callPackages ../path/to/packages.nix { };

# Pattern 3: Simple scope
qt6 = recurseIntoAttrs (callPackage ../path { });
```

#### Unresolved Patterns (Fallback Required)

```nix
# Non-literal paths
pkg = callPackage (import ./path {}) { };
pkg = callPackage (./path + "/package.nix") { };

# Computed attribute names
"${name}" = callPackage ./path { };

# Generated attributes
inherit (lib.genAttrs names (n: callPackage ./path { }));
```

#### Data Structure

```rust
/// Result of static analysis of all-packages.nix
pub struct StaticFileMap {
    /// file_path -> [attribute_names]
    /// Example: "pkgs/browsers/firefox/packages.nix" -> ["firefox", "firefox-esr"]
    pub file_to_attrs: HashMap<String, Vec<String>>,

    /// Attributes that couldn't be statically resolved
    pub unresolved_attrs: Vec<String>,

    /// Coverage ratio (resolved / total)
    pub coverage_ratio: f64,
}

/// Parse all-packages.nix content and extract file mappings
pub fn parse_all_packages(content: &str) -> Result<StaticFileMap>;
```

### Phase 2: Blob Hash Caching

**Critical Optimization** (from Codex review)

Cache static maps by blob hash, not commit hash. This reduces unique parses from ~500K commits to ~tens of thousands unique blob revisions.

#### Data Structure

```rust
/// Cache for static file maps, keyed by blob hash
pub struct FileMapCache {
    /// blob_sha -> StaticFileMap
    cache: HashMap<String, StaticFileMap>,

    /// Persistence path
    cache_file: PathBuf,
}

impl FileMapCache {
    /// Get or compute static map for a blob
    pub fn get_or_parse(&mut self, blob_hash: &str, content: &str) -> &StaticFileMap;

    /// Save cache to disk (compressed)
    pub fn save(&self) -> Result<()>;

    /// Load cache from disk
    pub fn load(path: &Path) -> Result<Self>;
}
```

#### Blob Reading Without Checkout

```rust
/// Read file content directly from git object database
pub fn read_blob(repo: &Repository, commit: &str, path: &str) -> Result<String> {
    // Use: git show commit:path
    // Or libgit2: repo.revparse_single(&format!("{}:{}", commit, path))
}
```

### Phase 3: Hybrid Extraction Strategy

#### Decision Flow

```
For each commit:
1. Get changed files from diff
2. For each changed file:
   a. If file in static_map:
      → Extract specific attrs from static_map[file]
   b. Else if file is all-packages.nix:
      → Parse diff to find changed attrs (AST diff)
   c. Else if file is unknown:
      → Log warning, add to unresolved list
3. If unresolved_count > threshold OR first commit of range:
   → Trigger dynamic baseline extraction
4. Else:
   → Extract only resolved attrs (incremental)
```

#### Coverage Monitoring

```rust
/// Track extraction coverage per commit
pub struct CoverageMetrics {
    pub total_changed_files: usize,
    pub resolved_files: usize,
    pub unresolved_files: usize,
    pub coverage_ratio: f64,
}

/// Threshold below which we trigger dynamic fallback
const MIN_COVERAGE_THRESHOLD: f64 = 0.80;
```

### Phase 4: Memory-Safe Baseline Extraction

For first commit of each range (baseline), we must do full `builtins.attrNames` to capture all packages.

#### Strategy

1. **Serialize by system**: Process one system at a time (not parallel)
2. **Partition by attr groups**: Pre-split by top-level attribute prefixes
3. **Subprocess isolation**: Worker with 4GB memory limit
4. **Progress tracking**: Deterministic resume on OOM

#### Attr Group Partitioning

```rust
/// Top-level attribute groups for partitioned extraction
const ATTR_GROUPS: &[&str] = &[
    "a-d",      // Packages starting with a-d
    "e-h",      // Packages starting with e-h
    "i-l",      // ...
    "m-p",
    "q-t",
    "u-z",
    "python",   // pythonPackages, python3Packages, etc.
    "haskell",  // haskellPackages
    "node",     // nodePackages
    "perl",     // perlPackages
    "ruby",     // rubyPackages
    "lua",      // luaPackages
    "qt",       // qt5, qt6, libsForQt5
    "kde",      // kdePackages
    "gnome",    // gnome, pantheon, etc.
];

/// Extract baseline in partitions to avoid OOM
pub fn extract_baseline_partitioned(
    worktree: &Path,
    system: &str,
    groups: &[&str],
) -> Result<Vec<PackageInfo>>;
```

#### OOM Recovery

```rust
/// If a group OOMs, subdivide and retry
pub fn extract_with_subdivision(
    worktree: &Path,
    system: &str,
    prefix: &str,
    max_depth: usize,
) -> Result<Vec<PackageInfo>> {
    match extract_group(worktree, system, prefix) {
        Ok(packages) => Ok(packages),
        Err(e) if e.is_oom() && max_depth > 0 => {
            // Subdivide: "a" -> "aa", "ab", "ac", ...
            let subgroups = subdivide_prefix(prefix);
            let mut all = Vec::new();
            for sub in subgroups {
                all.extend(extract_with_subdivision(
                    worktree, system, &sub, max_depth - 1
                )?);
            }
            Ok(all)
        }
        Err(e) => Err(e),
    }
}
```

### Phase 5: AST Diff for all-packages.nix

When `all-packages.nix` itself changes, parse the diff to find affected attributes.

```rust
/// Parse git diff of all-packages.nix to find changed attributes
pub fn parse_all_packages_diff(diff: &str) -> Vec<String> {
    // For each changed line, extract:
    // - New attribute assignments: `foo = callPackage ...`
    // - Modified assignments
    // - Removed attributes
    // - Changed inherit statements
}
```

## Edge Cases and Blind Spots

### Known Limitations (from Codex Review)

| Edge Case | Impact | Mitigation |
|-----------|--------|------------|
| Non-literal paths | Can't resolve statically | Dynamic fallback |
| Computed attr names | Can't resolve statically | Track as unresolved |
| Inline definitions | No file mapping exists | Parse all-packages.nix directly |
| Conditional variants | Over-include both branches | Accept false positives |
| Overlays | Separate file mappings | Parse overlay files too |
| rec/inherit expansion | Can't trace origin | Dynamic fallback |
| recurseIntoAttrs | Scope changes | Best-effort mapping |

### Coverage Expectations

- **Expected static coverage**: 80-90% of packages
- **Dynamic fallback**: Required for remaining 10-20%
- **False positive rate**: ~5% (extracting unchanged packages)

## Implementation Plan

### Phase 1: Static Analysis (Week 1)

- [ ] Add `rnix-parser` dependency to Cargo.toml
- [ ] Implement `src/index/static_analysis.rs`
- [ ] Unit tests for callPackage pattern matching
- [ ] Benchmark parsing time on current all-packages.nix

### Phase 2: Blob Caching (Week 2)

- [ ] Implement blob reading via libgit2
- [ ] Implement FileMapCache with persistence
- [ ] Add cache warming command: `nxv index --warm-cache`
- [ ] Benchmark unique blob count across history

### Phase 3: Hybrid Extraction (Week 3)

- [ ] Integrate static map into incremental indexing
- [ ] Implement coverage monitoring
- [ ] Implement dynamic fallback trigger
- [ ] Integration tests

### Phase 4: Memory-Safe Baseline (Week 4)

- [ ] Implement attr group partitioning
- [ ] Implement OOM recovery with subdivision
- [ ] Progress tracking and resume
- [ ] Stress testing on large nixpkgs

### Phase 5: Polish (Week 5)

- [ ] AST diff parsing for all-packages.nix
- [ ] End-to-end testing on historical commits
- [ ] Performance optimization
- [ ] Documentation

## Performance Estimates

### Blob Cache Impact

| Metric | Without Cache | With Cache |
|--------|---------------|------------|
| Unique parses | ~500,000 | ~30,000 |
| Parse time/blob | ~50ms | ~50ms |
| Total parse time | ~7 hours | ~25 min |

### Full Index Time Estimate

| Phase | Time Estimate |
|-------|---------------|
| Cache warming | 1-2 hours |
| Commit traversal | 2-4 hours |
| Nix evaluation | 20-40 hours |
| **Total** | **1-2 days** |

*Note: Nix evaluation dominates. Static analysis optimizes the "which packages changed" detection, not the evaluation itself.*

## Dependencies

### New Crates

```toml
[dependencies]
rnix = "0.11"  # Nix parser
```

### Existing (No Changes)

- `git2` - Already used for git operations
- `serde` - Already used for serialization

## Testing Strategy

### Unit Tests

- Pattern matching for all callPackage variations
- Blob cache operations
- Coverage calculation
- OOM recovery logic

### Integration Tests

- End-to-end indexing of small commit range
- Cache persistence across runs
- Dynamic fallback triggering

### Regression Tests

- Known packages that were previously missed
- Coverage ratio stability across nixpkgs versions

## Open Questions

1. **Coverage threshold**: What % triggers dynamic fallback? (Proposed: 80%)
2. **Cache location**: Same data dir as index.db? Separate?
3. **Parallel blob parsing**: How many threads?
4. **Historical cache**: Pre-compute for all history or on-demand?

## References

- [rnix-parser](https://github.com/nix-community/rnix-parser) - Nix parser in Rust
- [nix-eval-jobs](https://github.com/nix-community/nix-eval-jobs) - Parallel Nix evaluation with memory limits
- [ofborg](https://github.com/NixOS/ofborg) - nixpkgs CI with output path comparison
- [tree-sitter-nix](https://github.com/nix-community/tree-sitter-nix) - Alternative parser (incremental)
- [Hydra](https://github.com/NixOS/hydra) - NixOS CI system

## Appendix: Codex Peer Review

> **Feasibility Assessment:** This is feasible, but there are meaningful blind spots in the static mapping step and the baseline extraction plan needs tighter guardrails.

> **Key Recommendation:** Cache file map by blob hash of `all-packages.nix`, not by commit. This reduces unique parses dramatically and is critical for performance.

> **Risk Assessment:**
> - Coverage risk: medium-high (plan fallback)
> - False positives: medium (cap affected attrs)
> - Baseline OOM: medium (group partitioning)
> - Performance: medium (blob hash caching critical)

*Full review available in project conversation history.*

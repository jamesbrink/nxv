# Recursive Package Indexing and Parallel Extraction

**Tracking Issue**: [#5 - Add support for nested package sets](https://github.com/jamesbrink/nxv/issues/5)

## Overview

This spec addresses two critical limitations in the current nxv indexer:

1. **Missing nested packages**: Packages under scopes like `qt6.qtwebengine`,
   `python3Packages.numpy`, `haskellPackages.pandoc` are not indexed because
   the extractor only iterates top-level attributes.

2. **Sequential processing**: The indexer processes commits one-at-a-time, modifying the nixpkgs checkout directly, which is slow and prevents parallelization.

### Goals

- Index all derivations in nixpkgs, including those nested under package scopes
- Enable parallel extraction using git worktrees in a temporary directory
- Never modify the user's nixpkgs checkout
- Maintain backward compatibility with existing indexes
- Support graceful shutdown and checkpointing
- Ensure backfill works with nested packages

---

## Phase 1: Worktree Pool Infrastructure

Create a robust worktree management system for parallel extraction.

**Existing code**: `git.rs:722-817` already has `Worktree`, `create_worktree()`,
`create_worktrees()`, and `cleanup_worktrees()` (prunes `nxv-worktree-*` entries).
This phase builds on that foundation.

### 1.1 WorktreePool Implementation

- [ ] Create `src/index/worktree_pool.rs` module (separate from `git.rs` worktree primitives)
- [ ] Implement `WorktreePool` struct with configurable worker count
- [ ] **Naming convention**: Use `nxv-worktree-{pool_id}-{worker_id}` format
  - `pool_id`: Unique identifier per pool instance (e.g., timestamp or random)
  - Aligns with existing `cleanup_worktrees()` prefix matching (`git.rs:809`)
- [ ] Pool creates worktrees in system temp directory: `/tmp/nxv-worktrees-{pool_id}/`
- [ ] Use existing `NixpkgsRepo::create_worktree()` internally (`git.rs:726-751`)
- [ ] Each worktree is created with `git worktree add --detach`
- [ ] Implement `acquire()` method that returns a `WorktreeHandle`
- [ ] Implement `release()` method to return worktree to pool
- [ ] Worktrees are reused across commits (checkout new commit, don't recreate)
- [ ] Implement `Drop` for `WorktreePool` that:
  - Calls `git worktree remove` for each managed worktree
  - Removes the temp directory
  - Calls `git worktree prune` on the main repo

### 1.2 WorktreeHandle Implementation

- [ ] `WorktreeHandle` wraps a worktree path and provides safe access
- [ ] Implements `checkout(&self, commit_hash: &str) -> Result<()>`
- [ ] Tracks current commit to avoid redundant checkouts
- [ ] Returns to pool on `Drop` (or marks as dirty if checkout failed)

### 1.3 Safety Guarantees

- [ ] Original nixpkgs path is **never** modified (read-only after initial clone check)
- [ ] All git operations happen on worktrees in temp directory
- [ ] Worktree cleanup on panic via `Drop` implementation
- [ ] **Startup cleanup**: On pool creation, call existing `cleanup_worktrees()` (`git.rs:802-817`)
  to prune stale `nxv-worktree-*` refs from previous crashed runs
- [ ] **Orphan detection**: Scan `/tmp/nxv-worktrees-*/` for directories not associated with
  running processes (check PID if encoded in pool_id)
- [ ] Register signal handlers (SIGTERM, SIGINT) to trigger cleanup before exit

### 1.4 Success Criteria

- [ ] Unit tests for `WorktreePool` creation and cleanup
- [ ] Test that original nixpkgs is never modified during extraction
- [ ] Test worktree reuse across multiple checkouts
- [ ] Test cleanup happens on normal exit and panic
- [ ] Integration test: create pool, checkout 10 different commits, verify all worktrees cleaned up

---

## Phase 2: Recursive Package Discovery

Modify the Nix extractor to discover and extract nested package scopes.

### 2.1 Identify Recursable Package Sets

- [ ] Create list of known package scope patterns to recurse into:

  ```text
  - *Packages (python3Packages, haskellPackages, perlPackages, etc.)
  - qt5, qt6
  - libsForQt5
  - gnome, xfce, mate, pantheon, plasma5Packages
  - nodePackages, nodePackages_latest
  - ocamlPackages
  - beam.packages.* (erlang)
  - rPackages
  - vimPlugins, emacsPackages
  - fishPlugins, zshPlugins
  - linuxPackages_*
  ```

- [ ] Detect `recurseForDerivations = true` attribute dynamically
- [ ] Configure maximum recursion depth (default: 2) to prevent infinite loops

### 2.2 Update EXTRACT_NIX Expression

- [ ] Modify `EXTRACT_NIX` in `extractor.rs` to accept a `recurse` parameter
- [ ] When `recurse = true`:
  - Check each attribute for `recurseForDerivations = true`
  - If found, recursively extract derivations with prefixed attribute path
  - Store full attribute path (e.g., `qt6.qtwebengine` not `qtwebengine`)
- [ ] Add recursion depth tracking to prevent stack overflow
- [ ] Handle cycles in package sets (some sets reference each other)

### 2.3 Dotted Attribute Path API (Critical)

**Problem**: Current `EXTRACT_NIX` only accepts top-level attribute names. The expression
uses `builtins.attrNames pkgs` and `pkgs ? name` checks (`extractor.rs:245-265`), which
fail for dotted paths like `python3Packages.numpy`. Incremental indexing and backfill
pass attribute lists directly to the extractor, so they cannot selectively update nested
packages without this change.

**Solution**: Extend the extractor API to support scope+path filtering:

- [ ] Add new `extract_packages_for_scoped_attrs()` function signature:

  ```rust
  pub fn extract_packages_for_scoped_attrs<P: AsRef<Path>>(
      repo_path: P,
      system: &str,
      scoped_attrs: &[ScopedAttr],  // e.g., [("python3Packages", "numpy"), ("qt6", "qtwebengine")]
  ) -> Result<Vec<PackageInfo>>
  ```

- [ ] Create `ScopedAttr` type: `(Option<String>, String)` where `None` scope = top-level
- [ ] Update `EXTRACT_NIX` to accept `scopedAttrs` parameter alongside `attrNames`:

  ```nix
  # When scopedAttrs is provided, navigate to scope first
  # e.g., for ("python3Packages", "numpy"): pkgs.python3Packages.numpy
  ```

- [ ] `hasAttr` check becomes: `scope ? name` instead of `pkgs ? name`
- [ ] Keep `extract_packages_for_attrs()` as convenience wrapper for top-level only
- [ ] Update call sites in `mod.rs:985` and `backfill.rs:423` to use scoped API

### 2.4 Change Detection for Nested Packages (Critical)

**Problem**: The file→attr map is built from `unsafeGetAttrPos` over top-level names only
(`extractor.rs:269-322`). Edits to `pkgs/development/python-modules/numpy/default.nix`
won't map to `python3Packages.numpy`, so incremental indexing skips nested packages entirely.

**Solution**: Extend position extraction to cover nested scopes:

- [ ] Create `POSITIONS_RECURSIVE_NIX` expression that:
  - Iterates top-level attrs as before
  - For whitelisted scopes, also iterates their children
  - Returns `{ attrPath = "python3Packages.numpy"; file = "/path/to/numpy/default.nix"; }`
- [ ] Update `build_file_attr_map()` to use recursive positions when recursion is enabled
- [ ] File→attr map keys remain file paths; values become full dotted attr paths
- [ ] Target path resolution in `mod.rs:942-966` works unchanged (file lookup returns dotted path)
- [ ] Add `--no-recurse-positions` flag to skip nested position extraction (faster but less accurate)

**Performance consideration**: Recursive position extraction adds evaluation time. Consider:

- Cache position maps per-commit in memory
- Only rebuild when `all-packages.nix` or scope definition files change

### 2.5 Attribute Path Handling

- [ ] Store full dotted attribute path in database (already supported by schema)
- [ ] Ensure `name` field remains the package name (pname), not the full path
- [ ] Update bloom filter to include full attribute paths
- [ ] Verify search still works for both name and attribute path queries

### 2.6 Extraction Performance

- [ ] Profile extraction time with recursion enabled
- [ ] Add `--no-recurse` flag to skip nested packages (for faster incremental updates)
- [ ] Consider caching which scopes exist at each commit to avoid re-evaluation

### 2.7 Success Criteria

- [ ] Unit test: extract from mock nixpkgs with nested derivation, verify full path stored
- [ ] Integration test: extract `qt6.qtwebengine` from real nixpkgs HEAD
- [ ] Integration test: extract `python3Packages.numpy` from real nixpkgs HEAD
- [ ] Test that deeply nested packages (>2 levels) are handled correctly
- [ ] Benchmark: extraction time increase is <3x compared to top-level only
- [ ] Test: `extract_packages_for_scoped_attrs()` correctly extracts `("python3Packages", "numpy")`
- [ ] Test: file→attr map includes `python3Packages.numpy` for `pkgs/development/python-modules/numpy/default.nix`
- [ ] Test: incremental index detects change to nested package file and updates correctly

---

## Phase 3: Parallel Indexer Architecture

Refactor the indexer to use worker threads with worktree pool.

**Call-site migration required**: Currently `process_commits()` (`mod.rs:837-934`) and
`run_backfill_historical()` (`backfill.rs:331-444`) directly call `repo.checkout_commit()`
on the user's nixpkgs. These must be refactored to use `WorktreeHandle` exclusively.

### 3.1 Indexer Refactoring

- [ ] Extract commit processing logic into standalone `process_single_commit()` function
- [ ] Create `IndexerWorker` struct that owns a `WorktreeHandle`
- [ ] Worker receives commits via channel, processes them, sends results back
- [ ] Main thread coordinates workers and handles database writes
- [ ] **Remove** all `repo.checkout_commit()` calls from main indexer code path
- [ ] **Migrate call sites**:
  - `mod.rs:840` - initial checkout → use first worker's worktree
  - `mod.rs:902` - per-commit checkout → worker handles via `WorktreeHandle`
  - `mod.rs:931` - file map rebuild → worker provides changed paths
  - `backfill.rs:412` - historical checkout → use worktree pool

### 3.2 Responsibility Contract

**Main thread responsibilities**:

- Maintains the commit queue (ordered by date)
- Dispatches commit+target_attrs tuples to workers
- Receives `ExtractionResult` from workers
- Buffers out-of-order results in `pending_results: BTreeMap<CommitSeq, ExtractionResult>`
- Processes results in sequence order for open-range bookkeeping (`mod.rs:1008-1044`)
- Writes to database (single writer, no contention)
- Handles checkpointing and graceful shutdown

**Worker thread responsibilities**:

- Owns a `WorktreeHandle` from the pool
- Receives `(commit_hash, target_attrs, commit_seq)` from channel
- Calls `handle.checkout(commit_hash)`
- Computes `changed_paths` via `git diff-tree` in worktree
- Builds file→attr map if needed (in worktree, not main repo)
- Resolves `target_attr_paths` from changed files
- Runs extraction via `extract_packages_for_scoped_attrs()`
- Sends `ExtractionResult { commit_seq, packages, changed_paths }` back

### 3.3 Parallel Commit Processing

- [ ] Add `--workers N` CLI option (default: number of CPUs / 2, min 1)
- [ ] Distribute commits across workers using work-stealing queue
- [ ] Each worker:
  1. Acquires worktree from pool
  2. Checks out assigned commit
  3. Computes changed paths and target attrs (in worker, not main thread)
  4. Runs extraction (with recursion if enabled)
  5. Sends `ExtractionResult` back via channel
- [ ] Main thread:
  1. Receives extraction results from workers
  2. Buffers out-of-order results until predecessor commits complete
  3. Processes results in commit-order for open-range state
  4. Batches database writes

### 3.4 Ordering Guarantees

- [ ] Commits must be processed in chronological order for range tracking
- [ ] Use commit sequence numbers to ensure correct ordering
- [ ] Buffer out-of-order results until preceding commits complete
- [ ] Checkpoint only after all preceding commits are processed

### 3.5 Graceful Shutdown

- [ ] Signal workers to stop on Ctrl+C
- [ ] Wait for in-flight extractions to complete (with timeout)
- [ ] Checkpoint at last fully-processed commit
- [ ] Clean up all worktrees before exit

### 3.6 Progress Reporting

- [ ] Per-worker progress display
- [ ] Overall ETA based on commits remaining
- [ ] Show current commit being processed by each worker

### 3.7 Success Criteria

- [ ] Test parallel extraction with 4 workers processes commits correctly
- [ ] Test ordering is maintained when workers complete out-of-order
- [ ] Test graceful shutdown saves valid checkpoint
- [ ] Test that database is consistent after parallel indexing
- [ ] Benchmark: 4 workers should be ~3x faster than single-threaded (I/O bound)
- [ ] Test: no `checkout_commit()` calls remain in main indexer code path
- [ ] Test: original nixpkgs working directory unchanged after parallel indexing

---

## Phase 4: Backfill Enhancement

Update backfill to work with nested packages and use worktrees.

**Current issue**: `run_backfill_historical()` (`backfill.rs:411-444`) calls
`repo.checkout_commit()` directly, modifying the user's nixpkgs. It also passes
attr_paths to `extract_packages_for_attrs()` which doesn't support dotted paths.

### 4.1 Nested Package Support

- [ ] Update `get_attrs_needing_backfill()` to return full attribute paths
- [ ] Parse dotted paths into `ScopedAttr` tuples for extraction
- [ ] Use `extract_packages_for_scoped_attrs()` instead of `extract_packages_for_attrs()`
- [ ] Update `backfill.rs:423` call site to use scoped API
- [ ] Verify backfill correctly updates nested package records

### 4.2 Worktree-Based Backfill

- [ ] Refactor `run_backfill_historical()` to use `WorktreePool`
- [ ] **Remove** `repo.checkout_commit()` call at `backfill.rs:412`
- [ ] **Remove** `repo.restore_ref()` call at `backfill.rs:404` (no longer needed)
- [ ] Acquire `WorktreeHandle` at start of backfill, release on completion
- [ ] Never modify the main nixpkgs checkout
- [ ] Support parallel backfill with multiple worktrees for different commits

### 4.3 Historical Mode Improvements

- [ ] Group commits by proximity to minimize checkout thrashing
- [ ] Use worktree pool for parallel commit processing
- [ ] Add progress indicator for commit traversal

### 4.4 Success Criteria

- [ ] Test backfill populates metadata for `qt6.qtwebengine`
- [ ] Test backfill with `--history` mode uses worktrees
- [ ] Test original nixpkgs is unmodified after backfill
- [ ] Integration test: backfill missing source_path for nested packages
- [ ] Test: no `checkout_commit()` or `restore_ref()` calls in backfill code path
- [ ] Test: `ScopedAttr` parsing handles `python3Packages.numpy` → `("python3Packages", "numpy")`

---

## Phase 5: CLI and Configuration Updates

### 5.1 New CLI Options

- [ ] `--workers N` - Number of parallel workers (default: auto-detect)
- [ ] `--no-recurse` - Skip nested package scopes (faster for updates)
- [ ] `--recurse-all` - Use dynamic detection instead of whitelist
- [ ] `--recurse-depth N` - Maximum recursion depth (default: 2)
- [ ] `--worktree-dir PATH` - Custom worktree directory (default: system temp)

### 5.2 Configuration Validation

- [ ] Validate nixpkgs path is a valid git repository
- [ ] Check available disk space for worktrees (warn if <10GB)
- [ ] Verify git version supports worktrees (>= 2.5)

### 5.3 Status and Diagnostics

- [ ] Add `nxv index status` command showing:
  - Last indexed commit
  - Number of packages (top-level vs nested)
  - Worktree status (if any exist)
- [ ] Add `--verbose` flag for detailed extraction logging

### 5.4 Success Criteria

- [ ] CLI help text documents new options
- [ ] Invalid options produce clear error messages
- [ ] Status command shows accurate nested package counts

---

## Phase 6: Database and Compatibility

### 6.1 Schema Compatibility

- [ ] Existing schema already supports dotted `attribute_path` - verify no changes needed
- [ ] Add database migration if schema version bump is required
- [ ] Ensure old indexes can be read (backward compatibility)

### 6.2 Index Metadata Updates

- [ ] Store `recursive_extraction = true/false` in meta table
- [ ] Store `recursion_depth` in meta table
- [ ] Store `worktree_parallel = true/false` in meta table

### 6.3 Bloom Filter Updates

- [ ] Include full attribute paths in bloom filter
- [ ] Verify search performance with larger bloom filter
- [ ] Consider separate bloom filter for nested packages (optimization)

### 6.4 Success Criteria

- [ ] Old index files can be read by new code
- [ ] New index files can be read by old code (graceful degradation)
- [ ] Bloom filter correctly rejects non-existent nested packages

---

## Phase 7: Testing and Validation

### 7.1 Unit Tests

- [ ] `worktree.rs`: Pool creation, acquire/release, cleanup
- [ ] `extractor.rs`: Recursive extraction Nix expression
- [ ] `mod.rs`: Parallel worker coordination, ordering

### 7.2 Integration Tests

- [ ] Full index of test nixpkgs subset with nested packages
- [ ] Incremental index adds new nested packages correctly
- [ ] Backfill updates nested package metadata
- [ ] Search finds nested packages by name and attribute path

### 7.3 End-to-End Tests

- [ ] Index real nixpkgs HEAD with recursion, verify qt6.qtwebengine exists
- [ ] Index with 4 workers, verify identical results to single-threaded
- [ ] Simulate Ctrl+C during indexing, verify clean recovery

### 7.4 Performance Tests

- [ ] Benchmark: single-threaded vs 2/4/8 workers
- [ ] Benchmark: with recursion vs without
- [ ] Memory usage profiling with large worktree pool
- [ ] Disk space usage with multiple worktrees

### 7.5 Success Criteria

- [ ] All existing tests pass
- [ ] New tests achieve >80% coverage of new code
- [ ] No regressions in search performance
- [ ] CI pipeline runs full test suite

---

## Implementation Order

1. **Phase 1** (Worktree Pool) - Foundation for all other changes
2. **Phase 2** (Recursive Discovery) - Core feature, can test with single thread
3. **Phase 6** (Database) - Ensure schema works before parallel writes
4. **Phase 3** (Parallel Indexer) - Biggest refactor, depends on 1 & 2
5. **Phase 4** (Backfill) - Uses infrastructure from 1 & 3
6. **Phase 5** (CLI) - Polish and configuration
7. **Phase 7** (Testing) - Ongoing throughout, final validation

---

## Risks and Mitigations

| Risk                                         | Mitigation                                      |
| -------------------------------------------- | ----------------------------------------------- |
| Worktree creation is slow                    | Reuse worktrees, create pool upfront            |
| Disk space exhaustion                        | Monitor usage, configurable pool size, cleanup  |
| Nix evaluation hangs                         | Timeout per extraction, kill worker process     |
| Database contention                          | Single writer thread, batch inserts             |
| Memory pressure from large results           | Stream results, limit batch sizes               |
| Inconsistent results from parallel execution | Strict ordering, deterministic merge            |

---

## Design Decisions

### 1. Whitelist vs Dynamic Detection

**Decision**: Use a curated whitelist by default, with `--recurse-all` flag for dynamic detection.

**Rationale**: Whitelist is safer and covers 99% of use cases. Dynamic detection via
`recurseForDerivations = true` can hit edge cases (infinite recursion, broken evaluations).
Power users can opt into full dynamic detection.

**Default whitelist**:

```text
*Packages (python3Packages, haskellPackages, perlPackages, rubyPackages, etc.)
qt5, qt6, libsForQt5
gnome, xfce, mate, pantheon, cinnamon
plasma5Packages, kdePackages
nodePackages, nodePackages_latest
ocamlPackages, coqPackages
beam.packages.erlang*
rPackages
vimPlugins, emacsPackages
fishPlugins, zshPlugins, tmuxPlugins
linuxPackages, linuxPackages_*
```

### 2. Recursion Depth

**Decision**: Default depth of 2, configurable via `--recurse-depth N`.

**Rationale**: Most ecosystem packages are 1 level deep (`python3Packages.numpy`).
Some like `linuxPackages_6_6.nvidia_x11` are 2 levels. Depth 3+ is rare and risks
performance issues. Users needing deeper can specify explicitly.

### 3. Search Ranking

**Decision**: No special ranking for nested vs top-level packages.

**Rationale**: This matches [NixOS search](https://github.com/NixOS/nixos-search) behavior.
They use Elasticsearch with field boosting:

| Field              | Boost |
| ------------------ | ----- |
| `attr_name`        | 9.0   |
| `pname`            | 6.0   |
| `description`      | 1.3   |

Searching "numpy" matches `pname` (6.0 boost), while "python3Packages.numpy" matches
`attr_name` (9.0 boost). This naturally handles both use cases without special logic.

### 4. Incremental Recursion

**Decision**: Yes, support adding nested packages to existing non-recursive indexes.

**Implementation**: When `--recurse` is enabled on an existing index:

1. Extract nested packages from HEAD (or specified commit range)
2. Insert new records with `first_commit = last_commit = extraction_commit`
3. Future incremental runs will track these packages normally
4. Historical data for nested packages only goes back to when recursion was enabled

This allows gradual adoption without requiring a full re-index of all commits.

---

## Appendix: Review Findings and Resolutions

This section documents issues identified during spec review and how they are addressed.

### High Priority

#### 1. EXTRACT_NIX doesn't support dotted paths for filtered extraction

**Issue**: Current `EXTRACT_NIX` uses `builtins.attrNames pkgs` and `pkgs ? name` checks
(`extractor.rs:245-265`), which fail for dotted paths. Incremental/backfill pass attr
lists directly, so they cannot selectively update nested packages.

**Resolution**: Added **Section 2.3 (Dotted Attribute Path API)** with:

- New `extract_packages_for_scoped_attrs()` function
- `ScopedAttr` type for (scope, name) pairs
- Updated Nix expression to navigate scopes
- Call-site migration plan for `mod.rs:985` and `backfill.rs:423`

#### 2. Change detection is top-level only

**Issue**: File→attr map built from `unsafeGetAttrPos` over top-level names only
(`extractor.rs:269-322`). Edits to nested package files won't trigger updates.

**Resolution**: Added **Section 2.4 (Change Detection for Nested Packages)** with:

- `POSITIONS_RECURSIVE_NIX` expression for nested scopes
- Updated `build_file_attr_map()` to use recursive positions
- File paths correctly map to full dotted attr paths

### Medium Priority

#### 3. "Never modify nixpkgs" needs call-site migration

**Issue**: `process_commits()` and `run_backfill_historical()` call `checkout_commit()`
on user's nixpkgs. Adding worktree pool doesn't help without refactoring these paths.

**Resolution**: Added explicit migration in:

- **Section 3.1**: Lists specific call sites to migrate (`mod.rs:840,902,931`, `backfill.rs:412`)
- **Section 3.2**: Defines responsibility contract (workers own worktrees, main thread never checkouts)
- **Section 4.2**: Backfill-specific migration with `checkout_commit()` and `restore_ref()` removal

#### 4. Worktree lifecycle/cleanup under-specified

**Issue**: Spec proposed `/tmp/nxv-worktrees-{pid}/` without naming rules or startup
pruning. Misalignment with existing `cleanup_worktrees()` at `git.rs:802-817`.

**Resolution**: Updated **Section 1.1** with:

- Naming convention: `nxv-worktree-{pool_id}-{worker_id}` (aligns with existing prefix matching)
- Uses existing `NixpkgsRepo::create_worktree()` internally
- Updated **Section 1.3** with startup cleanup via existing `cleanup_worktrees()`
- Orphan detection for temp directories

#### 5. Parallel architecture responsibilities unclear

**Issue**: Unspecified whether main thread or workers compute `changed_paths`/`target_attr_paths`.
Open-range bookkeeping is order-sensitive.

**Resolution**: Added **Section 3.2 (Responsibility Contract)** with:

- Main thread: commit queue, result buffering, ordering, DB writes
- Workers: checkout, changed_paths, target resolution, extraction
- Explicit `ExtractionResult` type with `commit_seq` for ordering
- `pending_results: BTreeMap<CommitSeq, ExtractionResult>` for buffering

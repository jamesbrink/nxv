# TODO

Tracking known issues, bugs, and planned improvements for nxv.

## Resolved Issues

### Wrapper Package Version Gap (Fixed: 2025-01-10)

**Commit:** `f3bfa0d` feat(index): add periodic full extraction for wrapper packages

**Problem:** Packages like Firefox were missing version updates from 2018-2020.
Firefox had only 13 versions indexed (all from 2017) despite commits existing through 2020+.

**Root Cause:** The indexer's `file_attr_map` is built using `builtins.unsafeGetAttrPos`,
which returns the **assignment location** (where the attribute is defined in all-packages.nix),
NOT the **definition location** (where the package code lives).

| Package | Assignment Location | Definition Location |
|---------|---------------------|---------------------|
| firefox | `all-packages.nix` (`firefox = wrapFirefox...`) | `pkgs/.../firefox/packages.nix` |

When `packages.nix` changed:

1. `file_attr_map.get("firefox/packages.nix")` → `None` (not in map)
2. Fallback `extract_attr_from_path()` returned `"packages"` (filename without .nix)
3. `"packages"` is not a valid package → silently dropped
4. Firefox versions never extracted

**Affected Packages:** All packages with versions defined in separate files:

- Wrapper patterns: `firefox`, `chromium`, `thunderbird`
- Generated packages: vim plugins, python packages via `callPackage`
- Any package using `packages.nix`, `common.nix`, `browser.nix`, etc.

**Fix:**

1. Added `AMBIGUOUS_FILENAMES` constant to reject filenames like "packages", "common"
2. Added `--full-extraction-interval` (default: 50) for periodic full extraction
3. Added fallback trigger when unknown `.nix` files in `pkgs/` change
4. Added wrapper package coverage check to validation script

**Verification:** Re-index affected timeframes, then check:

```bash
sqlite3 ~/.local/share/nxv/index.db \
  "SELECT COUNT(*) FROM package_versions WHERE attribute_path = 'firefox';"
# Should show 50+ versions instead of 13
```

**Re-indexing command used (2025-01-10):**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2018-Q1,2018-Q2,2018-Q3,2018-Q4,2019-Q1,2019-Q2,2019-Q3,2019-Q4,2020-Q1,2020-Q2,2020-Q3,2020-Q4" \
  --max-memory 32G \
  --max-range-workers 4
```

Ranges processed:

- 2018-Q1 (2018-01-01 to 2018-04-01)
- 2018-Q2 (2018-04-01 to 2018-07-01)
- 2018-Q3 (2018-07-01 to 2018-10-01)
- 2018-Q4 (2018-10-01 to 2019-01-01)
- 2019-Q1 (2019-01-01 to 2019-04-01)
- 2019-Q2 (2019-04-01 to 2019-07-01)
- 2019-Q3 (2019-07-01 to 2019-10-01)
- 2019-Q4 (2019-10-01 to 2020-01-01)
- 2020-Q1 (2020-01-01 to 2020-04-01)
- 2020-Q2 (2020-04-01 to 2020-07-01)
- 2020-Q3 (2020-07-01 to 2020-10-01)
- 2020-Q4 (2020-10-01 to 2021-01-01)

### Full Extraction Fallback Gap (Fixed: 2025-01-11)

**Commit:** `163b34f` fix(index): handle empty file_attr_map in full extraction fallback

**Problem:** The periodic full extraction fix (f3bfa0d) had a gap. When `build_file_attr_map`
fails on old commits (common with 2018 nixpkgs + modern Nix), `all_attrs` is `None`, and
the full extraction code does nothing.

**Root Cause:**

```rust
// This condition NEVER runs when all_attrs is None!
if needs_full_extraction && let Some(all_attrs_list) = all_attrs {
    for attr in all_attrs_list { ... }
}
```

When `build_file_attr_map` fails (returns Err), the indexer falls back to an empty map.
With an empty map, `all_attrs` is `None`, making periodic full extraction ineffective.

**Fix:** Added `extract_all_packages` flag that triggers when `needs_full_extraction` is true
but `all_attrs` is `None`. When this flag is set, an empty target list is passed to the
extraction, which triggers `builtins.attrNames pkgs` in the Nix code to get all packages.

---

### Empty attrNames List in extract.nix (Fixed: 2025-01-11)

**Commit:** `41b6c89` fix(index): handle empty attrNames list in extract.nix

**Problem:** Even after the `extract_all_packages` fix above, Firefox versions were still
not being indexed. The Rust code passed an empty list `[]` to Nix, but `extract.nix` didn't
handle empty lists correctly.

**Root Cause:** In `extract.nix` line 423:

```nix
names = if attrNames != null then attrNames else builtins.attrNames pkgs;
```

When `attrNames = []` (empty list), the condition `attrNames != null` is **true** (because
`[]` is not `null`), so Nix used the empty list directly instead of falling back to
`builtins.attrNames pkgs` to discover all packages.

**Fix:** Changed to check both null AND empty list:

```nix
names = if attrNames != null && builtins.length attrNames > 0 then attrNames else builtins.attrNames pkgs;
```

**Test:** Added `test_extract_nix_handles_empty_list_directly` regression test that
verifies packages are discovered when `attrNames = []` is passed directly to Nix.

---

### Parallel Range Checkpoints with --full Flag (Fixed: 2025-01-11)

**Commit:** `33d7957` fix(index): clear range checkpoints when --full flag used with parallel ranges

**Problem:** When using `--full` with `--parallel-ranges`, the indexer was resuming from
existing range checkpoints instead of starting fresh.

**Root Cause:** The `--full` flag only cleared the main checkpoint (`last_indexed_commit`),
not the range-specific checkpoints (e.g., `last_indexed_commit_2018-Q1`).

**Fix:** Added `full: bool` parameter to `index_parallel_ranges()` that calls
`db.clear_range_checkpoints()` when true, removing all range-specific checkpoint keys.

**Test:** Added `test_clear_range_checkpoints_removes_all_range_keys` to verify checkpoint
clearing behavior.

---

### Store Path Extraction Causes Darwin SDK Errors (Fixed: 2025-01-11)

**Commit:** `b10adc2` fix(index): skip store path extraction for pre-2020 commits

**Problem:** When extracting all packages with `attrNames = []`, Nix evaluation fails for
packages that have darwin dependencies (e.g., `emacsMacport`). The error:

```
error: attribute '__propagatedImpureHostDeps' missing
at .../darwin/apple-sdk/default.nix:205:36
```

**Root Cause:** `builtins.tryEval` does NOT catch errors that occur during
`derivationStrict` (derivation instantiation). When we access `pkg.outPath` or
`pkg.storePath`, Nix triggers `derivationStrict` which evaluates ALL build inputs,
including darwin SDK dependencies. On old nixpkgs (2018) + modern Nix, the darwin SDK
evaluation fails, and this error escapes the `tryEval` wrapper.

The key insight: `derivationStrict` is called **lazily** when you first access:
- `pkg.outPath`
- `pkg.drvPath`
- Any store path reference

This means the error doesn't occur when evaluating `pkg.version` or `pkg.meta`, only
when accessing store paths.

**Fix:** Added `extractStorePaths` parameter through the entire extraction pipeline:

1. **extract.nix**: Added `extractStorePaths ? true` parameter. When false, sets
   `storePath = null` without accessing `outPath`.

2. **extractor.rs**: Updated `extract_packages_for_attrs()` signature to accept
   `extract_store_paths: bool`.

3. **worker/protocol.rs**: Added `extract_store_paths` field to `WorkRequest::Extract`
   with serde default for backwards compatibility.

4. **worker/worker_main.rs**: Pass `extract_store_paths` to the handler.

5. **worker/pool.rs**: Updated `Worker::extract()`, `WorkerPool::extract()`, and
   `WorkerPool::extract_parallel()` signatures.

6. **mod.rs**: Use `is_after_store_path_cutoff(commit.date)` to determine whether
   to extract store paths. For commits before 2020-01-01, pass `false`.

**Rationale:** Store paths for commits before 2020 are unlikely to be in
cache.nixos.org anyway (the binary cache didn't retain old builds), so skipping
store path extraction for old commits has no practical downside while fixing the
darwin SDK evaluation errors.

**Verification:**

```bash
# Test with December 2018 range (pre-store-path-cutoff)
NXV_LOG=debug cargo run --features indexer -- index \
  --nixpkgs-path nixpkgs --since 2018-12-01 --until 2019-01-01 --full

# Verify Firefox versions from 2018 are now captured:
sqlite3 ~/.local/share/nxv/index.db \
  "SELECT version, datetime(first_commit_date, 'unixepoch') as first_seen
   FROM package_versions WHERE attribute_path = 'firefox' ORDER BY first_commit_date DESC;"
# Should show Firefox 64.0, 63.0.3, etc. from December 2018
```

**Result:** Firefox versions 64.0 (2018-12-12) and 63.0.3 (2018-12-01) are now
successfully indexed. The logs show `extract_store_paths=false` being correctly
passed for all 2018 commits, and x86_64-linux/aarch64-linux extraction succeeds.

---

## Open Issues

_No open issues currently._

---

## Future Improvements

### Performance

- [ ] Consider caching `file_attr_map` between commits with similar trees
- [ ] Investigate incremental bloom filter updates vs full rebuild
- [ ] **Dynamic memory reallocation between workers** - When running parallel ranges
  (e.g., 16 quarters), early-finishing workers should release their memory budget
  to still-running workers. Currently memory is statically allocated at startup
  (`total_budget / num_workers`). When 14 of 16 ranges complete, only 2 workers
  remain but they're still limited to 1/16th of memory each. Research approaches:
  - Shared memory pool with atomic allocation
  - Worker recycling with increased memory threshold for remaining workers
  - Message-passing to notify workers of available memory
  - Dynamic `MAX_MEMORY_MIB` adjustment in running workers via IPC

### Data Quality

- [ ] Add more packages to regression test fixture
- [ ] Consider tracking `version_source` distribution over time
- [ ] Add automated NixHub comparison in CI

### Documentation

- [ ] Add architecture diagram to docs
- [ ] Document the Nix evaluation fallback chain in detail
- [ ] Add troubleshooting guide for common indexing errors

### Features

- [ ] Delta index updates (only changed packages)
- [ ] Package dependency tracking
- [ ] CVE/vulnerability correlation improvements

---

## Notes

### Test Run Log (2025-01-11)

**18:23 - Full reindex attempt (interrupted):**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2018-Q1,2018-Q2,2018-Q3,2018-Q4,2019-Q1,2019-Q2,2019-Q3,2019-Q4,2020-Q1,2020-Q2,2020-Q3,2020-Q4" \
  --max-memory 32G --max-range-workers 12 --full
```

- 12 ranges running in parallel, 48 workers total (4 per range)
- Progress: ranges at 2-9% when interrupted via Ctrl+C
- Graceful shutdown worked: all ranges reported "interrupted"
- One benign git checkout error during shutdown (timing issue, not a bug)

**20:00 - Verification test with extractStorePaths fix:**

```bash
NXV_LOG=debug cargo run --features indexer -- index \
  --nixpkgs-path nixpkgs --since 2018-12-01 --until 2019-01-01 --full
```

- 1354 commits processed
- `extract_store_paths=false` correctly passed for all 2018 commits
- x86_64-linux and aarch64-linux extraction succeeded
- Darwin systems (x86_64-darwin, aarch64-darwin) failed as expected
- Firefox versions from December 2018 successfully indexed:
  - Firefox 64.0 (2018-12-12)
  - Firefox 63.0.3 (2018-12-01)
- **FIX VERIFIED:** Previously only had Firefox versions from 2017

---

### Dynamic Discovery Fallback Missing (Fixed: 2025-01-12)

**Commit:** `ab72378` fix(index): add dynamic discovery fallback for ambiguous file changes

**Problem:** Even with the wrapper package fix and store path fix, Firefox versions
from 2020 were still missing. Full extraction was being triggered but nothing was
extracted.

**Root Cause:** When an ambiguous file (like `firefox/packages.nix`) triggered full
extraction, AND `all_attrs` was `None` (file_attr_map unavailable for old commits),
the code had no fallback - resulting in an empty `target_attr_paths` and the commit
being silently skipped.

```rust
// BUG: If all_attrs is None, this block does nothing!
if let Some(all_attrs_list) = all_attrs {
    for attr in all_attrs_list {
        target_attr_paths.insert(attr.clone());
    }
}
// Missing: else { extract_all_packages = true; }
```

**Fix:** Added `extract_all_packages = true` fallback when `all_attrs` is `None` in
both sequential and parallel indexing paths. This enables dynamic Nix discovery via
`builtins.attrNames pkgs`.

**Tests Added:** 9 TDD tests for wrapper package detection and fallback logic:
- `test_ambiguous_file_detection_comprehensive`
- `test_specific_package_files_still_work`
- `test_ambiguous_filenames_list_completeness`
- `test_full_extraction_trigger_logic`
- `test_file_attr_map_simulation`
- `test_wrapper_package_scenarios`
- `test_non_package_prefixes_filter`
- `test_ambiguous_file_triggers_dynamic_discovery_when_all_attrs_none`
- `test_full_extraction_fallback_completeness`

**Re-indexing Required:** The 2020 data must be re-indexed with this fix to capture
Firefox and other wrapper package versions.

---

### Database State Analysis (2025-01-11)

**Current Index Coverage:**

| Year/Quarter | Status | Checkpoint Date | Commits Done | Total Commits |
|--------------|--------|-----------------|--------------|---------------|
| 2017 | COMPLETE | (sequential mode) | ~26,466 | 26,466 |
| 2018-Q1 | PARTIAL | 2018-01-07 | ~434 | 10,440 |
| 2018-Q2 | PARTIAL | 2018-04-09 | ~611 | 10,219 |
| 2018-Q3 | PARTIAL | 2018-07-07 | ~446 | 9,367 |
| 2018-Q4 | PARTIAL | 2018-10-08 | ~524 | 10,963 |
| 2019-Q1 | PARTIAL | 2019-01-08 | ~554 | 9,516 |
| 2019-Q2 | PARTIAL | 2019-04-08 | ~663 | 9,809 |
| 2019-Q3 | PARTIAL | 2019-07-09 | ~521 | 11,294 |
| 2019-Q4 | PARTIAL | 2019-10-11 | ~919 | 12,240 |
| 2020-Q1 | PARTIAL | 2020-02-08 | ~4,049 | 11,759 |
| 2020-Q2 | PARTIAL | 2020-04-05 | ~475 | 13,292 |
| 2020-Q3 | PARTIAL | 2020-07-03 | ~90 | 12,664 |
| 2020-Q4 | PARTIAL | 2020-10-09 | ~846 | 15,343 |
| 2021+ | NOT INDEXED | - | 0 | ~200,000+ |

**Critical Note:** The partial checkpoints (2018-2020) were created BEFORE the
`extractStorePaths` fix. This means early data in each quarter may be missing
wrapper packages (firefox, chromium, etc.). To ensure 100% data quality, a fresh
`--full` reindex is required.

**Reindexing Strategy:** Process one year at a time with parallel quarters to
ensure thorough validation between each year.

---

### Reindexing Plan (2025-01-11)

**Phase 1: 2017 (4 parallel quarters)**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2017-Q1,2017-Q2,2017-Q3,2017-Q4" \
  --max-memory 48G \
  --max-range-workers 4 \
  --full
```

Commits: ~26,466 | Estimated time: ~1 hour

**Phase 2: 2018 (4 parallel quarters)**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2018-Q1,2018-Q2,2018-Q3,2018-Q4" \
  --max-memory 48G \
  --max-range-workers 4
```

Commits: ~41,000 | Estimated time: ~2 hours

**Phase 3: 2019 (4 parallel quarters)**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2019-Q1,2019-Q2,2019-Q3,2019-Q4" \
  --max-memory 48G \
  --max-range-workers 4
```

Commits: ~43,000 | Estimated time: ~2 hours

**Phase 4: 2020 (4 parallel quarters)**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2020-Q1,2020-Q2,2020-Q3,2020-Q4" \
  --max-memory 48G \
  --max-range-workers 4
```

Commits: ~53,000 | Estimated time: ~2-3 hours

**Alternative: All 2017-2020 in one command (16 parallel quarters)**

```bash
nxv index --nixpkgs-path nixpkgs \
  --parallel-ranges "2017-Q1,2017-Q2,2017-Q3,2017-Q4,2018-Q1,2018-Q2,2018-Q3,2018-Q4,2019-Q1,2019-Q2,2019-Q3,2019-Q4,2020-Q1,2020-Q2,2020-Q3,2020-Q4" \
  --max-memory 48G \
  --max-range-workers 16 \
  --full
```

Commits: ~163,000 | Estimated time: 4-8 hours

**Validation between phases:**

```bash
# Check database stats
sqlite3 ~/.local/share/nxv/index.db "
SELECT strftime('%Y', datetime(first_commit_date, 'unixepoch')) as year,
       COUNT(*) as rows, COUNT(DISTINCT attribute_path) as packages
FROM package_versions GROUP BY year ORDER BY year;"

# Check Firefox coverage
sqlite3 ~/.local/share/nxv/index.db "
SELECT version, datetime(first_commit_date, 'unixepoch') as first_seen
FROM package_versions WHERE attribute_path = 'firefox'
ORDER BY first_commit_date DESC;"

# Check wrapper packages
for pkg in firefox chromium thunderbird; do
  count=$(sqlite3 ~/.local/share/nxv/index.db \
    "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$pkg'")
  echo "$pkg: $count versions"
done
```

---

### Useful Debugging Commands

```bash
# Check Firefox versions
sqlite3 ~/.local/share/nxv/index.db "
SELECT version, datetime(first_commit_date, 'unixepoch') as first_seen
FROM package_versions WHERE attribute_path = 'firefox'
ORDER BY first_commit_date DESC;"

# Check version_source distribution
sqlite3 ~/.local/share/nxv/index.db "
SELECT version_source, COUNT(*) as count
FROM package_versions GROUP BY version_source ORDER BY count DESC;"

# Compare with NixHub
curl -s "https://search.devbox.sh/v2/pkg?name=firefox" | jq '.releases | length'

# Run validation
./scripts/post_reindex_validation.sh --quick
```

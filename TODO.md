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

---

## Open Issues

### None Currently

---

## Future Improvements

### Performance

- [ ] Consider caching `file_attr_map` between commits with similar trees
- [ ] Investigate incremental bloom filter updates vs full rebuild

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

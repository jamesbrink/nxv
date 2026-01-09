# Database Optimization Working Document

## Current State (2025-01-07)

### Index Status

- **Last indexed commit:** `d1da305`
- **Coverage:** 2017-01-02 → 2023-05-30 (indexing still in progress)
- **Schema version:** 4

### Database Size

- **File size:** 7738.62 MB (7.7 GB)
- **Total version ranges:** 6,234,117
- **Unique package names:** 119,146
- **Unique versions:** 24,156
- **Avg versions per package:** ~52

---

## Baseline Benchmarks (No Optimization)

Measured with `--true-cold` flag (OS cache purged before each cold measurement).

### Full Table (`package_versions`) - Cold/Warm Cache

| Query                   | Cold (ms) | Warm (ms) | Rows    |
| ----------------------- | --------- | --------- | ------- |
| `exact_attr(python3)`   | 27.82     | 0.77      | 253     |
| `exact_attr(firefox)` | 27.06 | 0.86 | 273 |
| `exact_attr(nodejs)` | 29.22 | 0.86 | 277 |
| `exact_attr(gcc)` | 3.04 | 0.15 | 21 |
| `name_prefix(python)` | **12,508** | 3,058 | 299,043 |
| `name_prefix(nodejs)` | 1,774 | 1,266 | 3,201 |
| `name_prefix(firefox)` | 1,256 | 1,258 | 4,128 |
| `name_prefix(gtk)` | 1,296 | 1,275 | 14,248 |
| `name_prefix(qt)` | 1,259 | 1,261 | 8,444 |
| `attr_prefix(python)` | 3,858 | **9,625** | 1,400,305 |
| `attr_prefix(nodejs)` | 4,950 | 1,291 | 3,188 |
| `attr_prefix(firefox)` | 1,282 | 1,268 | 3,863 |
| `attr_prefix(gtk)` | 1,294 | 1,290 | 11,769 |
| `attr_prefix(qt)` | 1,279 | 1,284 | 6,655 |
| `relevance_search(python)` | 1,617 | 1,625 | 100 |
| `relevance_search(nodejs)` | 1,355 | 1,357 | 100 |
| `relevance_search(firefox)` | 1,365 | 1,349 | 100 |
| `relevance_search(gtk)` | 1,376 | 1,376 | 100 |
| `relevance_search(qt)` | 1,362 | 1,360 | 100 |
| `name_version(python,3.11)` | 1,270 | 1,266 | 1,705 |
| `stats_count` | 22.0 | 19.2 | 1 |
| `stats_distinct_attrs` | 388.8 | 87.6 | 1 |
| `version_history(python3)` | 3.96 | 1.19 | 12 |
| `complete_prefix(pyth)` | 155.4 | 95.4 | 20 |

**Key findings:**

- **Worst case:** `python` prefix searches return **1.4M rows** (12.5s cold, 9.6s warm)
- **Problem:** `attr_prefix(python)` is slower warm (9.6s) than cold (3.9s) due to memory pressure
- Warm cache doesn't help much for large scans - still 1-9 seconds
- Exact lookups are fast: ~27ms cold, <1ms warm
- `relevance_search` with LIMIT 100 still takes ~1.3-1.6s (full table scan before sort)
- `stats_distinct_attrs` is expensive: 389ms cold (COUNT DISTINCT requires full scan)

---

## Slim Database Implementation

One row per `(attribute_path, version)` - consolidates multiple version ranges while preserving all unique versions.

**Consolidation Query:**

```sql
WITH ranked AS (
    SELECT *,
        ROW_NUMBER() OVER (
            PARTITION BY attribute_path, version
            ORDER BY last_commit_date DESC
        ) as rn
    FROM package_versions
),
first_commits AS (
    SELECT attribute_path, version,
           MIN(first_commit_date) as first_commit_date,
           first_commit_hash
    FROM package_versions
    GROUP BY attribute_path, version
)
SELECT r.*, fc.first_commit_hash, fc.first_commit_date
FROM ranked r
JOIN first_commits fc ON r.attribute_path = fc.attribute_path AND r.version = fc.version
WHERE r.rn = 1
```

**Stats (from full 6.2M row database):**

- **Slim rows:** 196,063 (vs 6,234,117) - **31.8x reduction**
- **Slim DB size:** 239 MB uncompressed, **28 MB** zstd
- **Full DB size:** 7.56 GB uncompressed, **1.26 GB** zstd (compression level 3)
- All unique versions preserved (important for nxv's purpose)

---

## Slim Database Benchmarks

Measured with `--true-cold` flag (OS cache purged before each cold measurement).

### Slim Table (`package_versions`) - 196K rows

| Query                   | Cold (ms) | Warm (ms) | Rows    |
| ----------------------- | --------- | --------- | ------- |
| `exact_attr(python3)`   | 0.44      | 0.02      | 12      |
| `exact_attr(firefox)` | 1.46 | 0.22 | 39 |
| `exact_attr(nodejs)` | 1.10 | 0.12 | 28 |
| `exact_attr(gcc)` | 0.91 | 0.07 | 10 |
| `name_prefix(python)` | 154.63 | 148.04 | 2,162 |
| `name_prefix(nodejs)` | 152.49 | 145.00 | 236 |
| `name_prefix(firefox)` | 151.30 | 145.82 | 468 |
| `name_prefix(gtk)` | 157.06 | 145.33 | 230 |
| `name_prefix(qt)` | 151.27 | 144.92 | 899 |
| `attr_prefix(python)` | 209.68 | 207.06 | 32,636 |
| `attr_prefix(nodejs)` | 152.58 | 146.16 | 226 |
| `attr_prefix(firefox)` | 162.44 | 145.17 | 429 |
| `attr_prefix(gtk)` | 151.58 | 146.13 | 152 |
| `attr_prefix(qt)` | 157.87 | 146.27 | 582 |
| `relevance_search(python)` | 52.24 | 50.64 | 100 |
| `relevance_search(nodejs)` | 46.61 | 43.94 | 100 |
| `relevance_search(firefox)` | 49.39 | 44.41 | 100 |
| `relevance_search(gtk)` | 54.02 | 44.40 | 100 |
| `relevance_search(qt)` | 47.14 | 45.43 | 100 |
| `name_version(python,3.11)` | 151.88 | 146.19 | 73 |
| `stats_count` | 1.21 | 0.86 | 1 |
| `stats_distinct_attrs` | 13.66 | 9.52 | 1 |
| `version_history(python3)` | 0.13 | 0.04 | 12 |
| `complete_prefix(pyth)` | 6.79 | 3.25 | 20 |

---

## Performance Comparison: Full vs Slim

| Query                      | Full Cold  | Full Warm | Slim Cold | Slim Warm | Cold Speedup | Warm Speedup |
| -------------------------- | ---------- | --------- | --------- | --------- | ------------ | ------------ |
| `exact_attr(python3)`      | 27.82ms    | 0.77ms    | 0.44ms    | 0.02ms    | **63.2x**    | **38.5x**    |
| `name_prefix(python)` | 12,508ms | 3,058ms | 155ms | 148ms | **80.9x** | **20.7x** |
| `attr_prefix(python)` | 3,858ms | 9,625ms | 210ms | 207ms | **18.4x** | **46.5x** |
| `relevance_search(python)` | 1,617ms | 1,625ms | 52ms | 51ms | **31.0x** | **32.1x** |
| `stats_distinct_attrs` | 388.8ms | 87.6ms | 13.7ms | 9.5ms | **28.5x** | **9.2x** |
| `complete_prefix(pyth)` | 155.4ms | 95.4ms | 6.8ms | 3.3ms | **22.9x** | **29.4x** |

**Summary:**

- **Cold speedup:** min 3.3x, max 80.9x, **avg 23.1x**
- **Warm speedup:** min 2.1x, max 46.5x, **avg 18.4x**

**Key improvements:**

- **Worst case fixed:** `python` prefix searches now return 2,162 rows (was 299,043) - **138.3x fewer rows**
- **Memory pressure eliminated:** Warm cache is now consistently fast
- **Consistent sub-second queries:** All queries under 210ms cold, 150ms warm
- **Exact lookups:** Sub-millisecond (0.02-0.22ms warm)
- **Relevance search:** 31-32x faster (51ms vs 1,625ms)

---

## Implementation

### Update Variants (ollama-style)

```bash
nxv update              # Downloads slim (default)
nxv update index:slim   # Explicit slim
nxv update index:full   # Downloads full history
```

### Publish Command

```bash
nxv publish --output ./publish --compression-level 3
```

Generates both variants:

- `index.db.zst` - Slim database (28 MB)
- `index-full.db.zst` - Full history (1.26 GB)
- `bloom.bin` - Bloom filter (96 KB)
- `manifest.json` - With optional `full_history_index` field

---

## Notes

- Slim DB preserves all unique (attribute_path, version) pairs
- Uses `MIN(first_commit_date)` and `MAX(last_commit_date)` for consolidated ranges
- Metadata (description, license, etc.) from most recent commit
- Full history available via `index:full` variant for users who need commit-level granularity

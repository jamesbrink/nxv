---
name: db-query
description: Query and inspect the local nxv SQLite database for package versions, debugging, and validation. Use when asked to check database contents, debug missing packages, verify indexing results, or run custom SQL queries against the nxv index.
allowed-tools: Bash, Read
---

# NXV Database Query Skill

Efficiently query and inspect the local nxv SQLite database.

## Database Location

The database is located at the platform-specific data directory:
- **Linux**: `~/.local/share/nxv/index.db`
- **macOS**: `~/Library/Application Support/nxv/index.db`

Expand the path before querying:

```bash
DB_PATH="${XDG_DATA_HOME:-$HOME/.local/share}/nxv/index.db"
```

## Schema Overview

### Main Table: `package_versions`

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER | Primary key |
| attribute_path | TEXT | Nix attribute path (e.g., `python312Packages.requests`) |
| pname | TEXT | Package name |
| version | TEXT | Package version |
| description | TEXT | Package description |
| license | TEXT | License identifier |
| homepage | TEXT | Project homepage URL |
| first_commit_hash | TEXT | First commit where this version appeared |
| first_commit_date | TEXT | ISO8601 date of first commit |
| last_commit_hash | TEXT | Last commit where this version existed |
| last_commit_date | TEXT | ISO8601 date of last commit |
| maintainers | TEXT | JSON array of maintainer handles |
| platforms | TEXT | JSON array of supported platforms |
| store_path | TEXT | Nix store path (for commits >= 2020) |
| source_path | TEXT | Source file path in nixpkgs |
| version_source | TEXT | How version was extracted (direct/unwrapped/passthru/name) |

### FTS Table: `package_versions_fts`

Full-text search on `attribute_path`, `pname`, and `description`.

### Metadata: `meta`

Key-value store with `last_indexed_commit`, `last_indexed_date`, `schema_version`.

## Common Queries

### Check database stats

```bash
sqlite3 "$DB_PATH" "
SELECT
  COUNT(*) as total_rows,
  COUNT(DISTINCT attribute_path) as unique_packages,
  COUNT(DISTINCT version) as unique_versions,
  MIN(first_commit_date) as earliest_date,
  MAX(last_commit_date) as latest_date
FROM package_versions;
"
```

### Search for a package

```bash
sqlite3 "$DB_PATH" "
SELECT attribute_path, version, first_commit_date, last_commit_date
FROM package_versions
WHERE attribute_path LIKE '%firefox%'
ORDER BY first_commit_date DESC
LIMIT 20;
"
```

### Get all versions of a specific package

```bash
sqlite3 "$DB_PATH" "
SELECT version, first_commit_date, last_commit_date, version_source
FROM package_versions
WHERE attribute_path = 'firefox'
ORDER BY first_commit_date DESC;
"
```

### Check version_source distribution

```bash
sqlite3 "$DB_PATH" "
SELECT version_source, COUNT(*) as count,
       ROUND(100.0 * COUNT(*) / (SELECT COUNT(*) FROM package_versions), 2) as pct
FROM package_versions
GROUP BY version_source
ORDER BY count DESC;
"
```

### Find packages with unknown versions

```bash
sqlite3 "$DB_PATH" "
SELECT attribute_path, version, version_source
FROM package_versions
WHERE version = 'unknown' OR version = ''
LIMIT 50;
"
```

### Check indexing metadata

```bash
sqlite3 "$DB_PATH" "SELECT key, value FROM meta;"
```

### Full-text search on descriptions

```bash
sqlite3 "$DB_PATH" "
SELECT attribute_path, version, snippet(package_versions_fts, 2, '>>>', '<<<', '...', 20) as match
FROM package_versions_fts
WHERE package_versions_fts MATCH 'web browser'
LIMIT 10;
"
```

### Find packages by maintainer

```bash
sqlite3 "$DB_PATH" "
SELECT DISTINCT attribute_path
FROM package_versions
WHERE maintainers LIKE '%\"github\":\"username\"%'
LIMIT 20;
"
```

### Get recent package updates

```bash
sqlite3 "$DB_PATH" "
SELECT attribute_path, version, last_commit_date
FROM package_versions
WHERE last_commit_date > datetime('now', '-30 days')
ORDER BY last_commit_date DESC
LIMIT 50;
"
```

### Compare version coverage for a package

```bash
sqlite3 "$DB_PATH" "
SELECT
  attribute_path,
  COUNT(*) as version_count,
  MIN(first_commit_date) as earliest,
  MAX(last_commit_date) as latest
FROM package_versions
WHERE attribute_path IN ('firefox', 'chromium', 'thunderbird')
GROUP BY attribute_path;
"
```

## Validation Queries

### Check for data quality issues

```bash
sqlite3 "$DB_PATH" "
SELECT
  SUM(CASE WHEN version = '' OR version IS NULL THEN 1 ELSE 0 END) as empty_versions,
  SUM(CASE WHEN version = 'unknown' THEN 1 ELSE 0 END) as unknown_versions,
  SUM(CASE WHEN first_commit_hash IS NULL THEN 1 ELSE 0 END) as missing_first_commit,
  SUM(CASE WHEN last_commit_hash IS NULL THEN 1 ELSE 0 END) as missing_last_commit
FROM package_versions;
"
```

### Check schema version

```bash
sqlite3 "$DB_PATH" "SELECT value FROM meta WHERE key = 'schema_version';"
```

## Output Formatting

For readable output, use column mode:

```bash
sqlite3 -header -column "$DB_PATH" "SELECT ..."
```

For machine-readable output, use CSV or JSON:

```bash
sqlite3 -csv "$DB_PATH" "SELECT ..."
sqlite3 -json "$DB_PATH" "SELECT ..."
```

## Tips

1. Always use the expanded `$DB_PATH` variable
2. Quote attribute paths that contain special characters
3. Use `LIKE '%term%'` for partial matches, `=` for exact matches
4. The FTS table is faster for text searches on descriptions
5. Dates are ISO8601 format, comparable with string operations
6. Use `LIMIT` to avoid overwhelming output

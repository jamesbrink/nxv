# Store Path Indexing & fetchClosure Support

**Status:** Implemented (Phases 1-4)
**Created:** 2026-01-04
**Author:** nxv team

## Overview

This spec outlines adding Nix store path extraction to the nxv indexer, enabling users to leverage `builtins.fetchClosure` for instant binary downloads without evaluating nixpkgs.

## Motivation

### The Problem

Currently, using a specific historical package version requires:

1. Downloading ~300MB nixpkgs tarball for that commit
2. Decompressing and extracting
3. Evaluating the Nix expression (10-30+ seconds)
4. Checking binary cache for the store path
5. Downloading the binary (or building from source)

### The Solution

With store paths indexed, users can:

```nix
# Instant binary fetch (~1-2 seconds total)
builtins.fetchClosure {
  fromStore = "https://cache.nixos.org";
  fromPath = /nix/store/z9ljhwdfsydrdjri5fzg599y78nah34j-hello-2.12.2;
}
```

This skips steps 1-4 entirely, providing a 10-100x speedup for package installation.

## Research Findings

### Store Path Extraction

**Method:** Nix store paths for input-addressed derivations (the standard in nixpkgs) are deterministic and computed during evaluation, not build time. They can be extracted via:

```bash
# From a nixpkgs checkout
nix eval --raw -f . hello.outPath
# Output: /nix/store/z9ljhwdfsydrdjri5fzg599y78nah34j-hello-2.12.2
```

**Key insight:** The hash in the output path is computed from the derivation's inputs, not the build output. This means we can know the path without building.

**FFI approach:** The existing `NixEvaluator` FFI can be extended to extract `outPath`:

```nix
# In extract.nix, add to getPackageInfo:
outPath = let
  result = builtins.tryEval (builtins.toString pkg.outPath);
in if result.success then result.value else null;
```

### Binary Cache Availability

**Research summary:**

| Aspect | Finding |
|--------|---------|
| Historical policy | cache.nixos.org kept packages indefinitely (~220TB) |
| 2024 GC event | Announced January 2024, targeted "ancient store paths" |
| What's preserved | FODs (sources), current channel paths, paths with no references |
| Practical testing | Packages from 2020+ generally available (Node 14, Python 2.7, etc.) |
| Verification method | `curl -I https://cache.nixos.org/{hash}.narinfo` returns HTTP 200 if available |

**Test results (2026-01-04):**

| Package | Version | Era | Cache Status |
|---------|---------|-----|--------------|
| hello | 2.12.2 | 2025 | HTTP 200 |
| hello | 2.10 | ~2018 | HTTP 200 |
| python27 | 2.7.18 | 2022 | HTTP 200 |
| nodejs | 14.21.3 (EOL) | 2023 | HTTP 200 |
| nodejs | 14.18.1 (EOL) | 2021 | HTTP 200 |

### fetchClosure Requirements

**Three invocation modes:**

1. **Content-addressed paths** (simplest, no extra config needed):

   ```nix
   builtins.fetchClosure {
     fromStore = "https://cache.nixos.org";
     fromPath = /nix/store/ldbhlwhh39wha58rm61bkiiwm6j7211j-git-2.33.1;
   }
   ```

2. **Input-addressed with rewrite** (converts to CA, no extra config):

   ```nix
   builtins.fetchClosure {
     fromStore = "https://cache.nixos.org";
     fromPath = /nix/store/r2jd...;
     toPath = /nix/store/ldb...; # CA path
   }
   ```

3. **Input-addressed as-is** (requires trusted config):

   ```nix
   builtins.fetchClosure {
     fromStore = "https://cache.nixos.org";
     fromPath = /nix/store/r2jd...;
     inputAddressed = true;
   }
   ```

**Note:** Most nixpkgs packages are input-addressed. Mode 2 or 3 will be needed.

### Multi-System Considerations

Store paths are **system-specific**. A package will have different paths for:

- `x86_64-linux`
- `aarch64-linux`
- `x86_64-darwin`
- `aarch64-darwin`

This impacts database schema and API design.

## Database Schema Changes

### Option A: Separate Table `store_paths`

```sql
CREATE TABLE store_paths (
    id INTEGER PRIMARY KEY,
    package_version_id INTEGER NOT NULL REFERENCES package_versions(id),
    system TEXT NOT NULL,           -- e.g., "x86_64-linux"
    store_path TEXT NOT NULL,       -- full path: /nix/store/hash-name-version
    store_hash TEXT NOT NULL,       -- just the hash portion for cache lookups
    UNIQUE(package_version_id, system)
);

CREATE INDEX idx_store_paths_hash ON store_paths(store_hash);
CREATE INDEX idx_store_paths_system ON store_paths(system);
```

### Option B (Recommended): Inline in `package_versions`

For simplicity, store paths could be added directly:

```sql
ALTER TABLE package_versions ADD COLUMN store_path_x86_64_linux TEXT;
ALTER TABLE package_versions ADD COLUMN store_path_aarch64_linux TEXT;
ALTER TABLE package_versions ADD COLUMN store_path_x86_64_darwin TEXT;
ALTER TABLE package_versions ADD COLUMN store_path_aarch64_darwin TEXT;
```

**Trade-offs:**

- Separate table: Normalized, extensible, but more complex queries
- Inline columns: Simple queries, fixed systems, sparse data (many NULLs)

**Recommendation:** Start with inline columns for x86_64-linux only (Phase 1), expand later.

## API Changes

### Updated Response Schema

```json
{
  "data": {
    "name": "hello",
    "version": "2.12.2",
    "attribute_path": "hello",
    "first_commit_hash": "ee09932...",
    "store_path": "/nix/store/z9ljhwdfsydrdjri5fzg599y78nah34j-hello-2.12.2"
  }
}
```

**Note:** Cache availability is NOT validated by nxv. Store paths are deterministic and correct regardless of whether the binary is currently in cache.nixos.org. Users can verify availability with:

```bash
curl -I "https://cache.nixos.org/z9ljhwdfsydrdjri5fzg599y78nah34j.narinfo"
```

### New Endpoint: `/api/v1/fetch-closure/{name}/{version}`

Returns a ready-to-use Nix expression:

```bash
curl "http://localhost:8080/api/v1/fetch-closure/hello/2.12.2?system=x86_64-linux"
```

Response:

```json
{
  "data": {
    "expression": "builtins.fetchClosure { fromStore = \"https://cache.nixos.org\"; fromPath = /nix/store/z9ljhwdfsydrdjri5fzg599y78nah34j-hello-2.12.2; inputAddressed = true; }",
    "store_path": "/nix/store/z9ljhwdfsydrdjri5fzg599y78nah34j-hello-2.12.2",
    "store_hash": "z9ljhwdfsydrdjri5fzg599y78nah34j",
    "fallback_commit": "ee09932cedcef15aaf476f9343d1dea2cb77e261"
  }
}
```

## Indexer Changes

### Modified extract.nix

Add `outPath` extraction to `getPackageInfo`:

```nix
getPackageInfo = attrPath: pkg:
  let
    meta = pkg.meta or {};
    # ... existing fields ...

    # Extract store path (safe, won't trigger build)
    outPath = let
      result = builtins.tryEval (
        if pkg ? outPath then builtins.toString pkg.outPath else null
      );
    in if result.success then result.value else null;
  in {
    # ... existing fields ...
    outPath = outPath;
  };
```

### Extended PackageInfo struct

```rust
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub attribute_path: String,
    pub description: Option<String>,
    // ... existing fields ...

    /// Store path for x86_64-linux (e.g., /nix/store/hash-name-version)
    #[serde(rename = "outPath")]
    pub out_path: Option<String>,
}
```

## CLI Changes

### New Output Format

```bash
$ nxv search python --version 3.11 --show-store-path

python3  3.11.8  python311  2024-02-15  /nix/store/abc123...-python3-3.11.8

# With --json
{
  "name": "python3",
  "version": "3.11.8",
  "store_path": "/nix/store/abc123...-python3-3.11.8"
}
```

### New Command: `nxv fetch-closure`

```bash
$ nxv fetch-closure python 3.11.8

# Outputs ready-to-use Nix expression:
builtins.fetchClosure {
  fromStore = "https://cache.nixos.org";
  fromPath = /nix/store/abc123...-python3-3.11.8;
  inputAddressed = true;
}
```

## Implementation Phases

### Phase 1: Store Path Extraction (Core) ✅ COMPLETED

**Goal:** Extract and store output paths during indexing

**Tasks:**

1. ✅ Modify `extract.nix` to include `outPath` in package info (renamed to `storePath` to avoid Nix coercion issues)
2. ✅ Add `out_path` field to `PackageInfo` struct in `extractor.rs`
3. ✅ Add `store_path` column to `package_versions` table
4. ✅ Update schema version to v5 with migration
5. ✅ Update batch insert to include store path
6. ✅ Add 2020-01-01 cutoff for store path extraction (older binaries unlikely in cache)

**Files modified:**

- `src/index/nix/extract.nix`
- `src/index/extractor.rs`
- `src/db/mod.rs` (schema + migration)
- `src/db/queries.rs`
- `src/index/mod.rs` (cutoff logic, checkpoint structs)

### Phase 2: API & CLI Exposure ✅ COMPLETED

**Goal:** Expose store paths through API and CLI

**Tasks:**

1. ✅ Add `store_path` to `PackageVersionSchema` in API types
2. ✅ Add `--show-store-path` flag to CLI search command
3. ✅ Update JSON output format to include store paths
4. ✅ Add `/api/v1/fetch-closure` endpoint (query params: attr, version, cache_url, system)
5. ✅ Generate `fetchClosure` expression from store path
6. ✅ Update OpenAPI documentation

**Files modified:**

- `src/server/types.rs`
- `src/server/handlers.rs`
- `src/server/openapi.rs`
- `src/server/mod.rs` (route)
- `src/cli.rs`
- `src/main.rs`
- `src/output/mod.rs`
- `src/output/table.rs`
- `src/output/plain.rs`

### Phase 3: Multi-System Support ⚡ PARTIALLY IMPLEMENTED

**Goal:** Support store paths for all major platforms

**Tasks:**

1. ⏳ Extract paths for multiple systems in indexer (future work)
2. ⏳ Add columns for aarch64-linux, x86_64-darwin, aarch64-darwin (future work)
3. ✅ Add `?system=` query parameter to API (defaults to x86_64-linux, returns error for unsupported systems)
4. ⏳ Update CLI to accept `--system` flag (future work)
5. ⏳ Handle cross-compilation edge cases (future work)

**Note:** Infrastructure is in place for multi-system support. Currently only x86_64-linux store paths are indexed. The API accepts a `system` parameter and will return an informative error for unsupported systems.

**Files modified:**

- `src/server/types.rs`
- `src/server/handlers.rs`

### Phase 4: Publish & Index Distribution ✅ COMPLETED

**Goal:** Include store paths in published index

**Tasks:**

1. ✅ Publisher includes store path columns in compressed index (automatic - full DB compression)
2. ✅ Backward compatible - older nxv versions will ignore unknown columns
3. ⏳ Add store path statistics to index metadata (future enhancement)
4. ⏳ Document fetchClosure usage in README (future enhancement)

**Note:** The publisher compresses the entire database, so store_path is automatically included. No code changes were required.

## Performance Considerations

### Indexing Impact

- **Evaluation overhead:** Extracting `outPath` adds ~10-20% to evaluation time
- **Database size:** ~50 bytes per store path, ~500KB per 10K packages
- **Mitigation:** Store paths only for packages where extraction succeeds

### Why No Cache Verification

We deliberately do NOT verify cache.nixos.org availability because:

1. **Store paths are deterministic** - The path is always correct, regardless of cache state
2. **Cache state is transient** - Paths can be GC'd or restored at any time
3. **Network overhead** - Verifying millions of paths would be expensive and slow
4. **Rate limiting** - Aggressive checks would trigger cache.nixos.org rate limits
5. **Users can verify** - A simple `curl -I` check is trivial for users who need it
6. **NixHub shows many unavailable** - Their data confirms many store paths aren't cached, but the paths are still valuable metadata

## Security Considerations

### Trust Model

- `fetchClosure` with `inputAddressed = true` requires users to trust cache.nixos.org
- Store paths are deterministic from derivation inputs, but contents could differ if cache is compromised
- Users can verify by building locally and comparing

### Recommendations

- Document trust implications in user-facing output
- Provide fallback instructions (commit hash for full evaluation)
- Consider adding CA path conversion in future

## Alternatives Considered

### 1. Scraping NixHub/Devbox API

**Rejected because:**

- Dependency on third-party service
- API may change or become unavailable
- Limited to their indexed packages
- No control over data freshness

### 2. Storing .drv Files

**Rejected because:**

- Much larger storage requirements
- Complexity of parsing .drv format
- Output paths are simpler and sufficient

### 3. Content-Addressed Only

**Rejected because:**

- Requires `nix store make-content-addressed` for each path
- Expensive to compute at indexing time
- Most users can use `inputAddressed = true`

## Decisions

1. **Packages that fail outPath extraction** - Skip silently, store_path will be NULL
2. **Cachix/other caches** - Out of scope
3. **Store path date cutoff** - Only extract for commits from 2020-01-01 onwards (older binaries unlikely to be in cache.nixos.org)

## References

- [Nix Pills - Our First Derivation](https://nixos.org/guides/nix-pills/06-our-first-derivation)
- [Nix Reference Manual - fetchClosure](https://nixos.org/manual/nix/stable/language/builtins.html#builtins-fetchClosure)
- [Derivation outputs in a content-addressed world](https://www.tweag.io/blog/2021-02-17-derivation-outputs-and-output-paths/)
- [NixOS Discourse - Binary Cache Retention](https://discourse.nixos.org/t/how-long-is-binary-cache-kept-on-cache-nixos-org/11210)
- [NixOS Discourse - 2024 GC Announcement](https://discourse.nixos.org/t/upcoming-garbage-collection-for-cache-nixos-org/39078)
- [Binary Cache Specification](https://fzakaria.com/2021/08/12/a-nix-binary-cache-specification)

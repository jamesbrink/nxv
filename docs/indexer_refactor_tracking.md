# Indexer Refactor Tracking

Branch: `refactor/indexer`
Owner: Codex (GPT-5)

## Goals

- Re-architect the indexer around deterministic file-to-attribute mapping with minimal CLI surface.
- Preserve Nix C API evaluation while improving incremental coverage and speed.
- Ensure every attribute/version is captured without relying on fragile attrpos mapping.

## Proposed Refactor (High-Level)

1. **Indexer Pipeline**
   - Stage 1: DB-derived attribute catalog (attribute -> source_path, complete attr list).
   - Stage 2: Static all-packages parsing (blob-cache keyed) to map file -> attrs.
   - Stage 3: Commit planner (diff -> attr targets) with deterministic fallbacks.
   - Stage 4: Extraction executor (Nix C API, batched by system).
   - Stage 5: Mapping feedback loop (update file->attrs with extracted source_path).

2. **State Model**
   - Persist a stable attribute catalog in the DB; refresh at startup.
   - Use DB state to avoid re-evaluating global attr lists unless rebuilding.

3. **CLI Simplification**
   - Keep `--full`, `--since`, `--until`, `--systems`, and `--memory`.
   - Move advanced knobs to config/env (hidden).

4. **Correctness Guarantees**
   - Always index all attributes for the initial baseline (full rebuild).
   - For incremental runs: diff + static map + DB catalog ensures no missed versions.
   - If a file change cannot be mapped, fall back to attrs with missing source_path (not full scan).

## Implementation Log

- 2025-02-XX: Added DB queries for attribute catalog and source_path coverage.
- 2025-02-XX: Hybrid mapping now merges DB-derived mappings to avoid costly Nix fallback.
- 2025-02-XX: Incremental path targeting now uses DB missing-source fallback to prevent misses.

## Open Questions

- Confirm desired minimal CLI surface (parallel ranges retained or removed?).
- Decide whether full rebuild should drop existing DB rows or continue UPSERT-only.

## Next Steps

- Continue extracting static map only when `all-packages.nix` changes (already cached).
- Evaluate removing parallel range CLI or migrating to config-only.
- Add performance benchmarks for mapping + extraction stages.

# nxv Indexer FFI Integration

## Current State

This experimental branch integrates `nix-bindings` to replace subprocess calls to `nix eval` with direct FFI calls to the Nix C API.

## Completed Phases

- [x] **Phase 1: Add nix-bindings dependency**
  - Added `nix-bindings = "2.28"` to Cargo.toml under `indexer` feature
  - Raw FFI bindings (crates.io version, not high-level API)

- [x] **Phase 2: Create build.rs for linking**
  - Links `nixexprc`, `nixstorec`, `nixutilc` when indexer feature enabled
  - Uses pkg-config to find library paths

- [x] **Phase 3: Update flake.nix with Nix C API dependencies**
  - Added `nixBindingsInputs` with all required C libraries
  - Added env vars: `NIX_CFLAGS_COMPILE`, `PKG_CONFIG_PATH`, `LIBCLANG_PATH`
  - Updated both `nxv-indexer` package and devShell

- [x] **Phase 4: Create nix_ffi.rs wrapper module**
  - Safe wrapper around raw nix-bindings FFI
  - `NixEvaluator` struct with `eval_json()` method
  - Wraps expressions in `builtins.toJSON` for easy JSON output
  - Proper cleanup via Drop impl

- [x] **Phase 5: Update extractor.rs to use FFI**
  - `extract_packages_for_attrs()` tries FFI first, falls back to subprocess
  - `extract_attr_positions()` same pattern
  - Graceful degradation if Nix C API not available

- [x] **Phase 6: Tests and validation**
  - All 285 unit + 64 integration tests pass
  - 4 FFI tests added (marked `#[ignore]` - require Nix C API)
  - Clippy clean, nix flake check passes

- [x] **Phase 7: Reuse NixEvaluator across evaluations**
  - Implemented thread-local storage (`thread_local!`) pattern
  - Added `with_evaluator()` function for easy access
  - Amortizes expensive evaluator creation (~2-3s) across many evaluations
  - Each thread gets its own evaluator instance
  - Added `clear_evaluator()` for memory management

- [x] **Phase 8: Benchmark FFI vs subprocess**
  - Added `benches/ffi_benchmark.rs` with criterion benchmarks
  - Benchmarks compare:
    - Cold start (new evaluator + eval)
    - Warm eval (reused thread-local evaluator)
    - Subprocess baseline
    - Multiple evaluations (cold vs warm vs subprocess)
  - Run with: `nix develop -c cargo bench --features indexer --bench ffi_benchmark`

## Future Work

- [ ] **Phase 9: Parallel evaluation (future)**
  - Consider worker pool pattern from nix-eval-jobs-rs
  - Each worker has own `NixEvaluator` (already supported by thread-local design)
  - Parent distributes work via IPC
  - Memory-based worker recycling

## Architecture

```
With thread-local evaluator reuse:
extract_packages_for_attrs()
    └─► with_evaluator()           # Gets/creates thread-local evaluator
        └─► eval_json(expr)        # Fast - reuses Nix state
    └─► [fallback] subprocess      # If FFI fails

Indexer::run()
    └─► for commit in commits:
            extract_packages_for_attrs()
                └─► with_evaluator()   # Same evaluator reused
                    └─► eval_json(...) # Hundreds of fast evals
```

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` | Added `nix-bindings` dep, `ffi_benchmark` bench |
| `build.rs` | NEW - Nix C library linking |
| `flake.nix` | Nix C API deps for indexer |
| `src/index/mod.rs` | Added `nix_ffi` module |
| `src/index/nix_ffi.rs` | NEW - FFI wrapper with thread-local reuse |
| `src/index/extractor.rs` | Uses FFI via `with_evaluator()` |
| `benches/ffi_benchmark.rs` | NEW - FFI vs subprocess benchmarks |

## Notes

- The crates.io `nix-bindings` (2.28) only has raw FFI bindings
- GitHub version has high-level API but has Nix version compatibility issues
- We wrote our own minimal high-level wrapper in `nix_ffi.rs`
- The `NixEvaluator` is `Send` but not `Sync` (Nix C API is single-threaded)
- Thread-local storage naturally supports future parallel worker pools

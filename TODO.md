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
  - All 64 existing tests pass
  - 2 new FFI tests added (marked `#[ignore]` - require Nix C API)
  - Clippy clean, nix flake check passes

## Pending Phases

- [ ] **Phase 7: Reuse NixEvaluator across evaluations**
  - Currently creates new evaluator per call (expensive)
  - Options:
    - Thread-local storage (`thread_local!`)
    - Pass evaluator as parameter to extraction functions
    - Use `Mutex<Option<NixEvaluator>>` for global singleton
  - Goal: Initialize once, reuse for hundreds of commits

- [ ] **Phase 8: Benchmark FFI vs subprocess**
  - Add criterion benchmarks comparing:
    - Cold start (new evaluator + eval)
    - Warm eval (reused evaluator)
    - Subprocess baseline
  - Measure memory usage

- [ ] **Phase 9: Parallel evaluation (future)**
  - Consider worker pool pattern from nix-eval-jobs-rs
  - Each worker has own `NixEvaluator`
  - Parent distributes work via IPC
  - Memory-based worker recycling

## Architecture

```
Current (per-call evaluator):
extract_packages_for_attrs()
    └─► NixEvaluator::new()     # Expensive (~2-3s)
        └─► eval_json(expr)     # Fast once initialized
    └─► [fallback] subprocess   # If FFI fails

Target (reused evaluator):
Indexer::run()
    └─► NixEvaluator::new()     # Once at start
        for commit in commits:
            └─► eval_json(...)  # Reuse state, fast
```

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` | Added `nix-bindings` dep |
| `build.rs` | NEW - Nix C library linking |
| `flake.nix` | Nix C API deps for indexer |
| `src/index/mod.rs` | Added `nix_ffi` module |
| `src/index/nix_ffi.rs` | NEW - FFI wrapper |
| `src/index/extractor.rs` | Uses FFI with fallback |

## Notes

- The crates.io `nix-bindings` (2.28) only has raw FFI bindings
- GitHub version has high-level API but has Nix version compatibility issues
- We wrote our own minimal high-level wrapper in `nix_ffi.rs`
- The `NixEvaluator` is `Send` but not `Sync` (Nix C API is single-threaded)

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
  - All 294 unit + 64 integration tests pass (358 total, 11 ignored)
  - FFI tests marked `#[ignore]` - require Nix C API at runtime
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

- [x] **Phase 9: Proper checkpoint serialization**
  - Added `checkpoint_open_ranges` table to database schema (v4)
  - Open ranges are now fully serialized during checkpoints
  - Resume capability: loads previous open ranges on restart
  - Graceful shutdown saves checkpoint without closing ranges

- [x] **Phase 10: Nested package extraction**
  - Added support for nested package sets (qt6.*, python3Packages.*, etc.)
  - Known nested sets: qt5, qt6, libsForQt5, kdePackages, python*Packages, perlPackages, nodePackages, haskellPackages, ocamlPackages, elmPackages, rPackages, emacsPackages, vimPlugins, gnome, pantheon, mate, cinnamon, xfce, php*Packages, rustPackages, goPackages, texlive
  - Dotted attribute paths (e.g., `qt6.qtwebengine`) are now properly extracted
  - Position extraction also includes nested packages

## Future Work

- [ ] **Phase 11: Parallel evaluation (future)**
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
| `src/index/mod.rs` | Added `nix_ffi` module, `CheckpointRange` type, checkpoint load/save |
| `src/index/nix_ffi.rs` | NEW - FFI wrapper with thread-local reuse |
| `src/index/extractor.rs` | Uses FFI via `with_evaluator()` |
| `src/index/nix/extract.nix` | Added nested package extraction, dotted path support |
| `src/index/nix/positions.nix` | Added nested package position extraction |
| `src/db/mod.rs` | Schema v4 with `checkpoint_open_ranges` table, save/load methods |
| `benches/ffi_benchmark.rs` | NEW - FFI vs subprocess benchmarks |

## Notes

- The crates.io `nix-bindings` (2.28) only has raw FFI bindings
- GitHub version has high-level API but has Nix version compatibility issues
- We wrote our own minimal high-level wrapper in `nix_ffi.rs`
- The `NixEvaluator` is `Send` but not `Sync` (Nix C API is single-threaded)
- Thread-local storage naturally supports future parallel worker pools

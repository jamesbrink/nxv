//! Benchmarks for FFI vs subprocess Nix evaluation.
//!
//! These benchmarks compare:
//! - Cold start: New evaluator creation + evaluation
//! - Warm eval: Reused evaluator evaluation
//! - Subprocess baseline: `nix eval` command
//!
//! Run with: `nix develop -c cargo bench --features indexer --bench ffi_benchmark`
//!
//! Note: Requires Nix to be installed with C API libraries.
//! If FFI is unavailable, benchmarks will skip gracefully.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::process::Command;

/// Check if nix is available on the system.
fn nix_available() -> bool {
    Command::new("nix")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Evaluate a simple expression using subprocess.
fn eval_subprocess(expr: &str) -> Option<String> {
    let output = Command::new("nix")
        .args(["eval", "--json", "--expr", expr])
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

/// Benchmark expressions of varying complexity.
const BENCHMARK_EXPRESSIONS: &[(&str, &str)] = &[
    ("simple_arithmetic", "1 + 2"),
    ("list_length", "builtins.length [1 2 3 4 5]"),
    ("attrset", r#"{ a = 1; b = 2; c = 3; }"#),
    ("nested_attrset", r#"{ foo = { bar = { baz = 42; }; }; }"#),
    ("list_map", "builtins.map (x: x * 2) [1 2 3 4 5 6 7 8 9 10]"),
    (
        "string_concat",
        r#"builtins.concatStringsSep "-" ["a" "b" "c" "d" "e"]"#,
    ),
];

fn bench_subprocess(c: &mut Criterion) {
    if !nix_available() {
        eprintln!("Skipping subprocess benchmarks: nix not available");
        return;
    }

    let mut group = c.benchmark_group("subprocess");
    group.sample_size(20); // Reduce samples for slower subprocess calls

    for (name, expr) in BENCHMARK_EXPRESSIONS {
        group.bench_with_input(BenchmarkId::new("eval", name), expr, |b, expr| {
            b.iter(|| {
                eval_subprocess(expr).expect("subprocess eval failed");
            });
        });
    }

    group.finish();
}

// FFI benchmarks - only available with indexer feature
#[cfg(feature = "indexer")]
mod ffi {
    use super::*;
    use nix_bindings::{
        EvalState, Store, ValueType_NIX_TYPE_STRING, nix_alloc_value, nix_c_context,
        nix_c_context_create, nix_c_context_free, nix_err_NIX_OK, nix_eval_state_build,
        nix_eval_state_builder_free, nix_eval_state_builder_load, nix_eval_state_builder_new,
        nix_expr_eval_from_string, nix_get_string, nix_get_type, nix_libexpr_init,
        nix_libstore_init, nix_libutil_init, nix_setting_set, nix_state_free, nix_store_free,
        nix_store_open, nix_value_force,
    };
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::ptr;
    use std::sync::Once;

    static NIX_INIT: Once = Once::new();

    fn init_nix_library(ctx: *mut nix_c_context) -> bool {
        let mut success = true;

        NIX_INIT.call_once(|| unsafe {
            if nix_libutil_init(ctx) != nix_err_NIX_OK {
                success = false;
                return;
            }

            let key = CString::new("extra-experimental-features").unwrap();
            let value = CString::new("flakes nix-command").unwrap();
            let _ = nix_setting_set(ctx, key.as_ptr(), value.as_ptr());

            if nix_libstore_init(ctx) != nix_err_NIX_OK {
                success = false;
                return;
            }
            if nix_libexpr_init(ctx) != nix_err_NIX_OK {
                success = false;
            }
        });

        success
    }

    extern "C" fn string_callback(
        start: *const std::os::raw::c_char,
        n: std::os::raw::c_uint,
        user_data: *mut std::os::raw::c_void,
    ) {
        let result = user_data.cast::<Option<String>>();
        let slice = unsafe { std::slice::from_raw_parts(start.cast::<u8>(), n as usize) };
        if let Ok(s) = std::str::from_utf8(slice) {
            unsafe { *result = Some(s.to_string()) };
        }
    }

    /// Minimal FFI evaluator for benchmarking
    struct BenchEvaluator {
        ctx: *mut nix_c_context,
        store: *mut Store,
        state: *mut EvalState,
    }

    impl BenchEvaluator {
        fn new() -> Option<Self> {
            unsafe {
                let ctx = nix_c_context_create();
                if ctx.is_null() {
                    return None;
                }

                if !init_nix_library(ctx) {
                    nix_c_context_free(ctx);
                    return None;
                }

                let store = nix_store_open(ctx, ptr::null(), ptr::null_mut());
                if store.is_null() {
                    nix_c_context_free(ctx);
                    return None;
                }

                let builder = nix_eval_state_builder_new(ctx, store);
                if builder.is_null() {
                    nix_store_free(store);
                    nix_c_context_free(ctx);
                    return None;
                }

                if nix_eval_state_builder_load(ctx, builder) != nix_err_NIX_OK {
                    nix_eval_state_builder_free(builder);
                    nix_store_free(store);
                    nix_c_context_free(ctx);
                    return None;
                }

                let state = nix_eval_state_build(ctx, builder);
                nix_eval_state_builder_free(builder);

                if state.is_null() {
                    nix_store_free(store);
                    nix_c_context_free(ctx);
                    return None;
                }

                Some(Self { ctx, store, state })
            }
        }

        fn eval_json(&self, expr: &str) -> Option<String> {
            let json_expr = format!("builtins.toJSON ({})", expr);
            let expr_cstr = CString::new(json_expr).ok()?;
            let path_cstr = CString::new("<bench>").ok()?;

            unsafe {
                let value = nix_alloc_value(self.ctx, self.state);
                if value.is_null() {
                    return None;
                }

                if nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_cstr.as_ptr(),
                    path_cstr.as_ptr(),
                    value,
                ) != nix_err_NIX_OK
                {
                    return None;
                }

                if nix_value_force(self.ctx, self.state, value) != nix_err_NIX_OK {
                    return None;
                }

                if nix_get_type(self.ctx, value) != ValueType_NIX_TYPE_STRING {
                    return None;
                }

                let mut result_str: Option<String> = None;
                nix_get_string(
                    self.ctx,
                    value,
                    Some(string_callback),
                    (&raw mut result_str).cast(),
                );

                result_str
            }
        }
    }

    impl Drop for BenchEvaluator {
        fn drop(&mut self) {
            unsafe {
                if !self.state.is_null() {
                    nix_state_free(self.state);
                }
                if !self.store.is_null() {
                    nix_store_free(self.store);
                }
                if !self.ctx.is_null() {
                    nix_c_context_free(self.ctx);
                }
            }
        }
    }

    thread_local! {
        static THREAD_EVALUATOR: RefCell<Option<BenchEvaluator>> = const { RefCell::new(None) };
    }

    fn with_evaluator<F, T>(f: F) -> Option<T>
    where
        F: FnOnce(&BenchEvaluator) -> Option<T>,
    {
        THREAD_EVALUATOR.with(|cell| {
            let mut borrow = cell.borrow_mut();
            if borrow.is_none() {
                *borrow = BenchEvaluator::new();
            }
            f(borrow.as_ref()?)
        })
    }

    fn ffi_available() -> bool {
        BenchEvaluator::new().is_some()
    }

    pub fn bench_ffi_cold_start(c: &mut Criterion) {
        if !ffi_available() {
            eprintln!("Skipping FFI benchmarks: Nix C API not available");
            return;
        }

        let mut group = c.benchmark_group("ffi_cold_start");
        group.sample_size(10); // Cold start is slow, reduce samples

        for (name, expr) in BENCHMARK_EXPRESSIONS {
            group.bench_with_input(BenchmarkId::new("eval", name), expr, |b, expr| {
                b.iter(|| {
                    let evaluator = BenchEvaluator::new().expect("Failed to create evaluator");
                    evaluator.eval_json(expr).expect("FFI eval failed");
                });
            });
        }

        group.finish();
    }

    pub fn bench_ffi_warm(c: &mut Criterion) {
        if !ffi_available() {
            eprintln!("Skipping FFI warm benchmarks: Nix C API not available");
            return;
        }

        let mut group = c.benchmark_group("ffi_warm");

        // Warm up the thread-local evaluator
        let _ = with_evaluator(|eval| eval.eval_json("1"));

        for (name, expr) in BENCHMARK_EXPRESSIONS {
            group.bench_with_input(BenchmarkId::new("eval", name), expr, |b, expr| {
                b.iter(|| {
                    with_evaluator(|eval| eval.eval_json(expr)).expect("FFI warm eval failed");
                });
            });
        }

        group.finish();
    }

    pub fn bench_ffi_multiple_evals(c: &mut Criterion) {
        if !ffi_available() {
            eprintln!("Skipping FFI multiple evals benchmarks: Nix C API not available");
            return;
        }

        let mut group = c.benchmark_group("ffi_multiple_evals");
        group.sample_size(10);

        // Compare: creating new evaluator per eval vs reusing thread-local
        for count in [5, 10, 20] {
            group.bench_with_input(
                BenchmarkId::new("cold_start_per_eval", count),
                &count,
                |b, &count| {
                    b.iter(|| {
                        for i in 0..count {
                            let evaluator =
                                BenchEvaluator::new().expect("Failed to create evaluator");
                            evaluator
                                .eval_json(&format!("{} + 1", i))
                                .expect("eval failed");
                        }
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("reused_evaluator", count),
                &count,
                |b, &count| {
                    b.iter(|| {
                        for i in 0..count {
                            with_evaluator(|eval| eval.eval_json(&format!("{} + 1", i)))
                                .expect("eval failed");
                        }
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("subprocess_per_eval", count),
                &count,
                |b, &count| {
                    b.iter(|| {
                        for i in 0..count {
                            eval_subprocess(&format!("{} + 1", i)).expect("subprocess eval failed");
                        }
                    });
                },
            );
        }

        group.finish();
    }
}

#[cfg(feature = "indexer")]
criterion_group!(
    benches,
    bench_subprocess,
    ffi::bench_ffi_cold_start,
    ffi::bench_ffi_warm,
    ffi::bench_ffi_multiple_evals
);

#[cfg(not(feature = "indexer"))]
criterion_group!(benches, bench_subprocess);

criterion_main!(benches);

//! FFI bindings to the Nix C API for expression evaluation.
//!
//! This module provides safe wrappers around the raw nix-bindings FFI
//! to evaluate Nix expressions without spawning external processes.

#![allow(unsafe_code)]

use crate::error::{NxvError, Result};
use std::ffi::CString;
use std::ptr;
use std::sync::Once;

use nix_bindings::{
    EvalState, Store, ValueType_NIX_TYPE_STRING, nix_alloc_value, nix_c_context,
    nix_c_context_create, nix_c_context_free, nix_clear_err, nix_err_NIX_OK, nix_err_msg,
    nix_eval_state_build, nix_eval_state_builder_free, nix_eval_state_builder_load,
    nix_eval_state_builder_new, nix_expr_eval_from_string, nix_get_string, nix_get_type,
    nix_libexpr_init, nix_libstore_init, nix_libutil_init, nix_setting_set, nix_state_free,
    nix_store_free, nix_store_open, nix_value_force,
};

/// Global initialization for the Nix library.
static NIX_INIT: Once = Once::new();

/// Initialize the Nix library (called once).
fn init_nix_library(ctx: *mut nix_c_context) -> Result<()> {
    let mut init_result = Ok(());

    NIX_INIT.call_once(|| {
        unsafe {
            if nix_libutil_init(ctx) != nix_err_NIX_OK {
                init_result = Err(NxvError::NixEval("Failed to initialize libutil".into()));
                return;
            }

            // Enable experimental features needed for some evaluations
            let key = CString::new("extra-experimental-features").unwrap();
            let value = CString::new("flakes nix-command").unwrap();
            // Ignore errors - might already be set
            let _ = nix_setting_set(ctx, key.as_ptr(), value.as_ptr());

            if nix_libstore_init(ctx) != nix_err_NIX_OK {
                init_result = Err(NxvError::NixEval("Failed to initialize libstore".into()));
                return;
            }
            if nix_libexpr_init(ctx) != nix_err_NIX_OK {
                init_result = Err(NxvError::NixEval("Failed to initialize libexpr".into()));
            }
        }
    });

    init_result
}

/// Get error message from context.
///
/// # Safety
/// The caller must ensure `ctx` is a valid pointer.
unsafe fn get_error_message(ctx: *mut nix_c_context) -> Option<String> {
    unsafe {
        let mut len: std::os::raw::c_uint = 0;
        let msg_ptr = nix_err_msg(ctx, ctx, &raw mut len);

        if msg_ptr.is_null() || len == 0 {
            return None;
        }

        let slice = std::slice::from_raw_parts(msg_ptr.cast::<u8>(), len as usize);
        let msg = std::str::from_utf8(slice).ok()?.to_string();

        nix_clear_err(ctx);

        if msg.is_empty() { None } else { Some(msg) }
    }
}

/// Callback for extracting strings from Nix values.
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

/// A Nix expression evaluator using the C API.
pub struct NixEvaluator {
    ctx: *mut nix_c_context,
    store: *mut Store,
    state: *mut EvalState,
}

impl NixEvaluator {
    /// Create a new Nix evaluator.
    pub fn new() -> Result<Self> {
        unsafe {
            let ctx = nix_c_context_create();
            if ctx.is_null() {
                return Err(NxvError::NixEval("Failed to create Nix context".into()));
            }

            init_nix_library(ctx)?;

            let store = nix_store_open(ctx, ptr::null(), ptr::null_mut());
            if store.is_null() {
                let msg = get_error_message(ctx).unwrap_or_else(|| "Failed to open store".into());
                nix_c_context_free(ctx);
                return Err(NxvError::NixEval(msg));
            }

            let builder = nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                let msg = get_error_message(ctx)
                    .unwrap_or_else(|| "Failed to create eval state builder".into());
                nix_store_free(store);
                nix_c_context_free(ctx);
                return Err(NxvError::NixEval(msg));
            }

            if nix_eval_state_builder_load(ctx, builder) != nix_err_NIX_OK {
                let msg = get_error_message(ctx)
                    .unwrap_or_else(|| "Failed to load eval state builder".into());
                nix_eval_state_builder_free(builder);
                nix_store_free(store);
                nix_c_context_free(ctx);
                return Err(NxvError::NixEval(msg));
            }

            let state = nix_eval_state_build(ctx, builder);
            nix_eval_state_builder_free(builder);

            if state.is_null() {
                let msg =
                    get_error_message(ctx).unwrap_or_else(|| "Failed to build eval state".into());
                nix_store_free(store);
                nix_c_context_free(ctx);
                return Err(NxvError::NixEval(msg));
            }

            Ok(Self { ctx, store, state })
        }
    }

    /// Evaluate a Nix expression and return the result as a JSON string.
    ///
    /// The expression is automatically wrapped in `builtins.toJSON`.
    pub fn eval_json(&self, expr: &str, path: &str) -> Result<String> {
        // Wrap expression in builtins.toJSON
        let json_expr = format!("builtins.toJSON ({})", expr);

        let expr_cstr =
            CString::new(json_expr).map_err(|_| NxvError::NixEval("Invalid expression".into()))?;
        let path_cstr = CString::new(path).map_err(|_| NxvError::NixEval("Invalid path".into()))?;

        unsafe {
            // Allocate value
            let value = nix_alloc_value(self.ctx, self.state);
            if value.is_null() {
                return Err(NxvError::NixEval("Failed to allocate value".into()));
            }

            // Evaluate expression
            let result = nix_expr_eval_from_string(
                self.ctx,
                self.state,
                expr_cstr.as_ptr(),
                path_cstr.as_ptr(),
                value,
            );

            if result != nix_err_NIX_OK {
                let msg = get_error_message(self.ctx).unwrap_or_else(|| "Evaluation failed".into());
                return Err(NxvError::NixEval(msg));
            }

            // Force evaluation
            if nix_value_force(self.ctx, self.state, value) != nix_err_NIX_OK {
                let msg =
                    get_error_message(self.ctx).unwrap_or_else(|| "Failed to force value".into());
                return Err(NxvError::NixEval(msg));
            }

            // Check type
            let value_type = nix_get_type(self.ctx, value);
            if value_type != ValueType_NIX_TYPE_STRING {
                return Err(NxvError::NixEval(format!(
                    "Expected string result from builtins.toJSON, got type {}",
                    value_type
                )));
            }

            // Extract string
            let mut result_str: Option<String> = None;
            nix_get_string(
                self.ctx,
                value,
                Some(string_callback),
                (&raw mut result_str).cast(),
            );

            result_str.ok_or_else(|| NxvError::NixEval("Failed to get string value".into()))
        }
    }
}

impl Drop for NixEvaluator {
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

// Safety: NixEvaluator is not Sync (the C API is not thread-safe for concurrent access)
// but it is Send (can be transferred between threads).
unsafe impl Send for NixEvaluator {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires nix to be installed with C API
    fn test_eval_simple_expression() {
        let evaluator = NixEvaluator::new().expect("Failed to create evaluator");
        let result = evaluator
            .eval_json("1 + 2", "<test>")
            .expect("Failed to evaluate");
        assert_eq!(result, "3");
    }

    #[test]
    #[ignore] // Requires nix to be installed with C API
    fn test_eval_attrset() {
        let evaluator = NixEvaluator::new().expect("Failed to create evaluator");
        let result = evaluator
            .eval_json(r#"{ name = "hello"; version = "1.0"; }"#, "<test>")
            .expect("Failed to evaluate");

        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("Failed to parse JSON");
        assert_eq!(parsed["name"], "hello");
        assert_eq!(parsed["version"], "1.0");
    }
}

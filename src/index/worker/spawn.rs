//! Worker subprocess spawning.
//!
//! Uses `posix_spawn` via `std::process::Command` for cross-platform compatibility.
//! This avoids issues with fork() on macOS (Objective-C runtime problems).

#![allow(dead_code)] // Some utilities are for future use

use super::proc::Proc;
use crate::error::{NxvError, Result};
use crate::memory::DEFAULT_MEMORY_BUDGET;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counter for unique worker IDs (used for stderr logging threads).
static WORKER_STDERR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Configuration for worker processes.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Memory threshold (MiB) before worker requests restart.
    pub per_worker_memory_mib: usize,
    /// Custom eval store path (for parallel range isolation).
    /// If None, uses the default TEMP_EVAL_STORE_PATH.
    pub eval_store_path: Option<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        const DEFAULT_WORKERS: usize = 4;
        Self {
            // Default: 8 GiB total / 4 workers = 2 GiB per worker
            per_worker_memory_mib: (DEFAULT_MEMORY_BUDGET.as_mib() / DEFAULT_WORKERS as u64)
                as usize,
            eval_store_path: None,
        }
    }
}

/// One-time initialization for parent process before spawning workers.
static PARENT_INIT: Once = Once::new();

/// Initialize parent process state before spawning any workers.
///
/// This sets environment variables that affect forked/spawned children.
///
/// # Safety Notes
///
/// This function uses `std::env::set_var` which is marked unsafe in Rust 1.66+
/// because modifying environment variables in a multi-threaded program can cause
/// data races if another thread is reading the environment concurrently.
///
/// This usage is safe because:
/// 1. `Once::call_once()` guarantees this code runs exactly once
/// 2. This is called during process initialization in `spawn_worker()`, before
///    any worker threads exist
/// 3. The indexer is single-threaded at the point where workers are first spawned
fn init_parent() {
    PARENT_INIT.call_once(|| {
        // Disable Boehm GC in children to prevent fork compatibility issues.
        // The Nix evaluator uses Boehm GC, which can have issues with fork().
        //
        // SAFETY: See function-level documentation for thread safety justification.
        unsafe {
            std::env::set_var("GC_DONT_GC", "1");
        }
    });
}

/// Spawn a worker subprocess.
///
/// The worker is started in `--internal-worker` mode with the specified configuration.
///
/// # Arguments
/// * `config` - Worker configuration
///
/// # Returns
/// A `Proc` handle for communicating with the worker.
pub fn spawn_worker(config: &WorkerConfig) -> Result<Proc> {
    init_parent();

    let exe_path = std::env::current_exe()
        .map_err(|e| NxvError::Worker(format!("Failed to get current executable: {}", e)))?;

    let mut cmd = Command::new(&exe_path);

    // Worker mode flag
    cmd.arg("index");
    cmd.arg("--internal-worker");
    cmd.arg("--max-memory");
    cmd.arg(config.per_worker_memory_mib.to_string());

    // We need a dummy nixpkgs-path argument since it's required by the CLI
    // The actual path is passed via the work request
    cmd.arg("--nixpkgs-path");
    cmd.arg("/dev/null");

    // Set up IPC pipes
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped()); // Capture stderr to log through tracing

    // Environment variables for Nix
    cmd.env("GC_DONT_GC", "1");

    // Pass custom eval store path if specified (for parallel range isolation)
    if let Some(ref store_path) = config.eval_store_path {
        cmd.env("NXV_EVAL_STORE_PATH", store_path);
    }

    // macOS-specific: disable fork safety check for Objective-C
    #[cfg(target_os = "macos")]
    cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");

    // Nix configuration for evaluation
    // CRITICAL: auto-optimise-store = false prevents store corruption when
    // filesystem limits (disk space or EXT4 htree) are hit during indexing.
    // Without this, the deduplication hard-linking can fail mid-operation.
    cmd.env(
        "NIX_CONFIG",
        "accept-flake-config = true\nallow-import-from-derivation = true\nauto-optimise-store = false",
    );

    let mut child = cmd
        .spawn()
        .map_err(|e| NxvError::Worker(format!("Failed to spawn worker: {}", e)))?;

    // Spawn a thread to read and log stderr
    if let Some(stderr) = child.stderr.take() {
        let worker_id = WORKER_STDERR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::thread::Builder::new()
            .name(format!("worker-{}-stderr", worker_id))
            .spawn(move || {
                log_worker_stderr(worker_id, stderr);
            })
            .ok(); // Ignore thread spawn errors - stderr logging is best-effort
    }

    Proc::from_child(child)
}

/// Log worker stderr output through tracing.
///
/// Filters and categorizes nix output:
/// - `trace:` lines → TRACE level
/// - `warning:` lines → DEBUG level (these are nix evaluation warnings, not errors)
/// - `error:` lines → WARN level
/// - Other lines → DEBUG level
fn log_worker_stderr(worker_id: usize, stderr: std::process::ChildStderr) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // Pipe closed
        };

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Categorize and log based on content
        let line_lower = line.to_lowercase();
        if line_lower.starts_with("trace:") {
            tracing::trace!(worker_id, "{}", line);
        } else if line_lower.contains("warning:") || line_lower.starts_with("warning:") {
            // Nix evaluation warnings - log at debug since they're usually informational
            tracing::debug!(worker_id, nix_warning = true, "{}", line);
        } else if line_lower.contains("error:") || line_lower.starts_with("error:") {
            tracing::warn!(worker_id, nix_error = true, "{}", line);
        } else {
            tracing::debug!(worker_id, "{}", line);
        }
    }
}

/// Stack size for collector threads (64 MiB).
///
/// Collector threads run the IPC loop and may need large stacks for
/// handling deeply nested Nix evaluation results.
pub const COLLECTOR_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Spawn a collector thread with a large stack.
///
/// # Arguments
/// * `name` - Thread name for debugging
/// * `f` - Thread function
pub fn spawn_collector_thread<F, T>(name: &str, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(COLLECTOR_STACK_SIZE)
        .spawn(f)
        .expect("Failed to spawn collector thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_config_default() {
        let config = WorkerConfig::default();
        // 8 GiB total / 4 workers = 2 GiB per worker
        assert_eq!(config.per_worker_memory_mib, 2 * 1024);
    }

    // Note: spawn_worker requires the binary to support --internal-worker,
    // which we add later. This test is commented out until then.
    //
    // #[test]
    // fn test_spawn_worker() {
    //     let config = WorkerConfig::default();
    //     let proc = spawn_worker(&config).expect("Failed to spawn worker");
    //     // Worker should be running
    //     assert!(proc.is_running());
    // }
}

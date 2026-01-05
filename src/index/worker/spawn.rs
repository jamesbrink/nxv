//! Worker subprocess spawning.
//!
//! Uses `posix_spawn` via `std::process::Command` for cross-platform compatibility.
//! This avoids issues with fork() on macOS (Objective-C runtime problems).

#![allow(dead_code)] // Some utilities are for future use

use super::proc::Proc;
use crate::error::{NxvError, Result};
use std::process::{Command, Stdio};
use std::sync::Once;

/// Configuration for worker processes.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Memory threshold (MiB) before worker requests restart.
    pub max_memory_mib: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_memory_mib: 6 * 1024, // 6 GiB
        }
    }
}

/// One-time initialization for parent process before spawning workers.
static PARENT_INIT: Once = Once::new();

/// Initialize parent process state before spawning any workers.
///
/// This sets environment variables that affect forked/spawned children.
fn init_parent() {
    PARENT_INIT.call_once(|| {
        // Disable Boehm GC in children to prevent fork compatibility issues.
        // The Nix evaluator uses Boehm GC, which can have issues with fork().
        // Safety: This is called once at startup before any worker threads,
        // and setting GC_DONT_GC is safe as it only affects the GC behavior.
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
    cmd.arg(config.max_memory_mib.to_string());

    // We need a dummy nixpkgs-path argument since it's required by the CLI
    // The actual path is passed via the work request
    cmd.arg("--nixpkgs-path");
    cmd.arg("/dev/null");

    // Set up IPC pipes
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit()); // Worker errors go to parent's stderr

    // Environment variables for Nix
    cmd.env("GC_DONT_GC", "1");

    // macOS-specific: disable fork safety check for Objective-C
    #[cfg(target_os = "macos")]
    cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");

    // Nix configuration for evaluation
    cmd.env(
        "NIX_CONFIG",
        "accept-flake-config = true\nallow-import-from-derivation = true",
    );

    let child = cmd
        .spawn()
        .map_err(|e| NxvError::Worker(format!("Failed to spawn worker: {}", e)))?;

    Proc::from_child(child)
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
        assert_eq!(config.max_memory_mib, 6 * 1024);
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

//! Worker subprocess main entry point.
//!
//! This module runs when `nxv index --internal-worker` is invoked.
//! It creates a Nix evaluator and processes extraction requests from the parent.

use super::ipc::{LineReader, LineWriter, PipeFd};
use super::protocol::{WorkRequest, WorkResponse};
use crate::index::extractor;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use nix::sys::resource::{Resource, setrlimit};

/// Memory threshold configuration from CLI (in MiB).
/// Default: 6 GiB. Uses atomic for safe concurrent access.
static MAX_MEMORY_MIB: AtomicUsize = AtomicUsize::new(6 * 1024);

/// Set the memory threshold for worker restart.
///
/// This can be called at any time but should typically be set once at startup.
pub fn set_max_memory(mib: usize) {
    MAX_MEMORY_MIB.store(mib, Ordering::Relaxed);
}

/// Set a hard memory limit using setrlimit(RLIMIT_AS).
///
/// This creates an OS-enforced limit on the virtual address space.
/// If the worker tries to allocate beyond this limit:
/// - `malloc()` returns NULL / allocations fail with ENOMEM
/// - Nix evaluation may crash or error out
/// - In worst case, OOM killer terminates the process
///
/// The parent process will detect the death and respawn the worker,
/// potentially with a smaller batch size.
///
/// # Arguments
/// * `limit_mib` - Maximum virtual address space in MiB
///
/// # Returns
/// * `Ok(())` if limit was set successfully
/// * `Err(message)` if setrlimit failed (non-fatal, worker continues)
#[cfg(unix)]
fn set_hard_memory_limit(limit_mib: usize) -> Result<(), String> {
    // Convert MiB to bytes
    let limit_bytes = (limit_mib as u64) * 1024 * 1024;

    // Add 50% headroom for memory fragmentation and overhead.
    // The soft limit (getrusage check) handles the actual threshold,
    // this hard limit is a safety net for runaway allocations.
    let hard_limit = limit_bytes + (limit_bytes / 2);

    // Set both soft and hard limits to the same value
    match setrlimit(Resource::RLIMIT_AS, hard_limit, hard_limit) {
        Ok(()) => {
            // Log via stderr (will be captured by parent's stderr reader)
            eprintln!(
                "trace: Worker set RLIMIT_AS to {} MiB (with {}% headroom)",
                hard_limit / (1024 * 1024),
                50
            );
            Ok(())
        }
        Err(e) => {
            // Non-fatal: some systems may not support RLIMIT_AS or have restrictions
            let msg = format!("Failed to set RLIMIT_AS: {}", e);
            eprintln!("warning: {}", msg);
            Err(msg)
        }
    }
}

/// Get the current memory usage in MiB.
///
/// Uses `getrusage()` to get the maximum resident set size.
fn get_memory_usage_mib() -> usize {
    use nix::sys::resource::{UsageWho, getrusage};

    match getrusage(UsageWho::RUSAGE_SELF) {
        Ok(usage) => {
            let max_rss = usage.max_rss();

            #[cfg(target_os = "macos")]
            {
                // macOS: max_rss is in bytes
                (max_rss as usize) / (1024 * 1024)
            }

            #[cfg(not(target_os = "macos"))]
            {
                // Linux: max_rss is in kilobytes
                (max_rss as usize) / 1024
            }
        }
        Err(_) => 0,
    }
}

/// Check if memory exceeds the threshold.
fn is_over_memory_threshold() -> bool {
    let current = get_memory_usage_mib();
    let threshold = MAX_MEMORY_MIB.load(Ordering::Relaxed);
    current > threshold
}

/// Process a single extraction request.
fn handle_extract(
    system: &str,
    repo_path: &str,
    attrs: &[String],
    extract_store_paths: bool,
) -> WorkResponse {
    let path = Path::new(repo_path);

    match extractor::extract_packages_for_attrs(path, system, attrs, extract_store_paths) {
        Ok(packages) => WorkResponse::result(packages),
        Err(e) => WorkResponse::error(format!("{}", e)),
    }
}

/// Process a positions extraction request.
fn handle_extract_positions(system: &str, repo_path: &str) -> WorkResponse {
    let path = Path::new(repo_path);

    match extractor::extract_attr_positions(path, system) {
        Ok(positions) => WorkResponse::positions_result(positions),
        Err(e) => WorkResponse::error(format!("{}", e)),
    }
}

/// Worker main loop.
///
/// Reads requests from stdin, processes them, and writes responses to stdout.
fn worker_loop(reader: &mut LineReader, writer: &mut LineWriter) -> io::Result<()> {
    // Send ready signal
    writer.write_line(&WorkResponse::Ready.to_line())?;

    loop {
        // Read request
        let line = match reader.read_line()? {
            Some(line) => line.to_string(),
            None => {
                // EOF - parent closed the pipe
                return Ok(());
            }
        };

        // Parse request
        let request = match WorkRequest::from_line(&line) {
            Ok(req) => req,
            Err(e) => {
                // Invalid request - send error and continue
                let resp = WorkResponse::error(format!("Invalid request: {}", e));
                writer.write_line(&resp.to_line())?;
                continue;
            }
        };

        // Handle request
        match request {
            WorkRequest::Exit => {
                // Graceful shutdown
                return Ok(());
            }

            WorkRequest::Extract {
                system,
                repo_path,
                attrs,
                extract_store_paths,
            } => {
                // Process extraction
                let response = handle_extract(&system, &repo_path, &attrs, extract_store_paths);
                writer.write_line(&response.to_line())?;

                // Check memory after extraction
                if is_over_memory_threshold() {
                    // Request restart with memory info
                    let current = get_memory_usage_mib();
                    let threshold = MAX_MEMORY_MIB.load(std::sync::atomic::Ordering::Relaxed);
                    writer.write_line(&WorkResponse::restart(current, threshold).to_line())?;
                    return Ok(());
                }

                // Signal ready for next request
                writer.write_line(&WorkResponse::Ready.to_line())?;
            }

            WorkRequest::ExtractPositions { system, repo_path } => {
                // Process positions extraction
                let response = handle_extract_positions(&system, &repo_path);
                writer.write_line(&response.to_line())?;

                // Check memory after extraction (positions can be memory-intensive)
                if is_over_memory_threshold() {
                    // Request restart with memory info
                    let current = get_memory_usage_mib();
                    let threshold = MAX_MEMORY_MIB.load(std::sync::atomic::Ordering::Relaxed);
                    writer.write_line(&WorkResponse::restart(current, threshold).to_line())?;
                    return Ok(());
                }

                // Signal ready for next request
                writer.write_line(&WorkResponse::Ready.to_line())?;
            }
        }
    }
}

/// Run the worker subprocess main function.
///
/// This function never returns normally - it either exits successfully
/// or panics on unrecoverable errors.
pub fn run_worker_main() -> ! {
    // Set hard memory limit FIRST, before any Nix evaluation.
    // This creates an OS-enforced ceiling on memory usage.
    #[cfg(unix)]
    {
        let threshold_mib = MAX_MEMORY_MIB.load(Ordering::Relaxed);
        if threshold_mib > 0 {
            let _ = set_hard_memory_limit(threshold_mib);
        }
    }

    // Install a custom panic hook to handle broken pipe gracefully.
    // Workers inherit stderr from the parent, so when tee exits on Ctrl+C,
    // stderr is broken and write panics would cause abort().
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_broken_pipe = info
            .payload()
            .downcast_ref::<String>()
            .map(|s| s.contains("Broken pipe") || s.contains("os error 32"))
            .unwrap_or(false)
            || info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.contains("Broken pipe") || s.contains("os error 32"))
                .unwrap_or(false);

        if is_broken_pipe {
            std::process::exit(0);
        }
        default_hook(info);
    }));

    // Set up signal handlers
    // Ignore SIGPIPE - we handle pipe errors via io::Error
    unsafe {
        nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGPIPE,
            nix::sys::signal::SigHandler::SigIgn,
        )
        .ok();
    }

    // Create IPC channels from stdin/stdout
    //
    // SAFETY: File descriptors 0 (stdin) and 1 (stdout) are valid because:
    // 1. This worker process is spawned by spawn_worker() which explicitly sets
    //    stdin(Stdio::piped()) and stdout(Stdio::piped())
    // 2. The parent process (pool) holds the other end of these pipes
    // 3. We take ownership here - these FDs will be closed on Drop
    //
    // If this function is ever called outside the worker subprocess context
    // (e.g., directly from the main process), these FDs may not be pipes
    // and IPC will fail with appropriate errors.
    let stdin_fd = unsafe { PipeFd::from_raw(0) };
    let stdout_fd = unsafe { PipeFd::from_raw(1) };

    let mut reader = LineReader::new(stdin_fd);
    let mut writer = LineWriter::new(stdout_fd);

    // Run the main loop
    match worker_loop(&mut reader, &mut writer) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // Use write_all instead of eprintln! to handle broken pipe gracefully
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("Worker error: {}\n", e).as_bytes(),
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_memory_usage() {
        let usage = get_memory_usage_mib();
        // Should be some reasonable value (at least a few MB for the test process)
        assert!(usage > 0);
        assert!(usage < 10_000); // Less than 10GB
    }

    #[test]
    fn test_is_over_memory_threshold() {
        // Save original value
        let original = MAX_MEMORY_MIB.load(Ordering::Relaxed);

        // Set a very high threshold - should not be over
        MAX_MEMORY_MIB.store(100_000, Ordering::Relaxed);
        assert!(!is_over_memory_threshold());

        // Set a very low threshold - should be over
        MAX_MEMORY_MIB.store(1, Ordering::Relaxed);
        assert!(is_over_memory_threshold());

        // Restore original value
        MAX_MEMORY_MIB.store(original, Ordering::Relaxed);
    }

    /// Test that set_hard_memory_limit works correctly.
    ///
    /// This test is ignored by default because setting RLIMIT_AS affects the
    /// entire test process and can cause subsequent tests to fail with OOM.
    /// Run it in isolation with: cargo test --features indexer test_set_hard_memory_limit -- --ignored
    #[test]
    #[ignore = "affects process-wide RLIMIT_AS, run in isolation"]
    #[cfg(unix)]
    fn test_set_hard_memory_limit() {
        use nix::sys::resource::{Resource, getrlimit};

        // Get current limits to work within allowed bounds
        let (_current_soft, current_hard) =
            getrlimit(Resource::RLIMIT_AS).expect("Failed to get current rlimit");

        // If unlimited, we can set any value
        // If limited, we can only lower it
        if current_hard == u64::MAX {
            // Can set any limit - use 16 GiB (safe headroom for tests)
            let result = set_hard_memory_limit(16 * 1024);
            assert!(result.is_ok(), "Failed to set memory limit: {:?}", result);

            // Verify it was set (16 GiB + 50% headroom = 24 GiB)
            let (soft, _) = getrlimit(Resource::RLIMIT_AS).expect("Failed to get rlimit");
            let expected_bytes = (16 * 1024 * 1024 * 1024_u64) + (16 * 1024 * 1024 * 1024_u64 / 2);
            assert_eq!(soft, expected_bytes);
        } else {
            // Can only lower the limit - just verify the function is callable
            // In restricted environments, setrlimit may fail with EPERM
            let _ = set_hard_memory_limit(1024);
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_hard_limit_function_exists() {
        // Simple test that the function exists and returns Result
        // This doesn't actually call setrlimit (which would affect the process)
        // just verifies the interface compiles correctly
        fn _verify_signature() -> Result<(), String> {
            // Function signature test - not executed
            set_hard_memory_limit(1024)
        }
        // If we get here, the function signature is correct
    }
}

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

/// Memory threshold configuration from CLI (in MiB).
/// Default: 6 GiB. Uses atomic for safe concurrent access.
static MAX_MEMORY_MIB: AtomicUsize = AtomicUsize::new(6 * 1024);

/// Set the memory threshold for worker restart.
///
/// This can be called at any time but should typically be set once at startup.
pub fn set_max_memory(mib: usize) {
    MAX_MEMORY_MIB.store(mib, Ordering::Relaxed);
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
fn handle_extract(system: &str, repo_path: &str, attrs: &[String]) -> WorkResponse {
    let path = Path::new(repo_path);

    match extractor::extract_packages_for_attrs(path, system, attrs) {
        Ok(packages) => WorkResponse::result(packages),
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
            } => {
                // Process extraction
                let response = handle_extract(&system, &repo_path, &attrs);
                writer.write_line(&response.to_line())?;

                // Check memory after extraction
                if is_over_memory_threshold() {
                    // Request restart
                    writer.write_line(&WorkResponse::Restart.to_line())?;
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
    // Safety: file descriptors 0 and 1 are always valid for stdin/stdout
    let stdin_fd = unsafe { PipeFd::from_raw(0) };
    let stdout_fd = unsafe { PipeFd::from_raw(1) };

    let mut reader = LineReader::new(stdin_fd);
    let mut writer = LineWriter::new(stdout_fd);

    // Run the main loop
    match worker_loop(&mut reader, &mut writer) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("Worker error: {}", e);
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
}

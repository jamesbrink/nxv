//! Nix store garbage collection for the indexer.
//!
//! During indexing, millions of derivation files (`.drv`) are created in `/nix/store/`.
//! These accumulate and can fill up the disk or hit EXT4 directory entry limits.
//! This module provides functions to periodically clean up the store.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Run nix-collect-garbage to clean up unused derivations.
///
/// Deletes old generations and runs garbage collection.
/// Returns the duration of the GC operation, or None if GC failed.
///
/// # Example
///
/// ```ignore
/// if let Some(duration) = run_garbage_collection() {
///     println!("GC completed in {:?}", duration);
/// }
/// ```
pub fn run_garbage_collection() -> Option<Duration> {
    let start = Instant::now();

    // First try nix-collect-garbage -d (deletes old generations + GC)
    let result = Command::new("nix-collect-garbage")
        .arg("-d")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            let duration = start.elapsed();
            info!(
                duration_secs = duration.as_secs_f64(),
                "Nix garbage collection completed"
            );
            Some(duration)
        }
        Ok(status) => {
            warn!(
                exit_code = ?status.code(),
                "nix-collect-garbage exited with error, trying nix-store --gc"
            );
            // Fall back to nix-store --gc
            run_store_gc()
        }
        Err(e) => {
            warn!(
                error = %e,
                "Failed to run nix-collect-garbage, trying nix-store --gc"
            );
            run_store_gc()
        }
    }
}

/// Fallback: run nix-store --gc directly.
fn run_store_gc() -> Option<Duration> {
    let start = Instant::now();

    let result = Command::new("nix-store")
        .arg("--gc")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            let duration = start.elapsed();
            info!(
                duration_secs = duration.as_secs_f64(),
                "nix-store --gc completed"
            );
            Some(duration)
        }
        Ok(status) => {
            warn!(exit_code = ?status.code(), "nix-store --gc exited with error");
            None
        }
        Err(e) => {
            warn!(error = %e, "Failed to run nix-store --gc");
            None
        }
    }
}

/// Check if garbage collection commands are available.
#[allow(dead_code)]
pub fn is_gc_available() -> bool {
    Command::new("nix-collect-garbage")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get the current Nix store disk usage in bytes.
///
/// Returns None if the store path doesn't exist or can't be queried.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn get_store_usage_bytes() -> Option<u64> {
    use std::path::Path;

    let store_path = Path::new("/nix/store");
    if !store_path.exists() {
        return None;
    }

    // Use statfs to get filesystem stats
    let output = Command::new("df")
        .args(["--output=used", "-B1", "/nix/store"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Skip header line, parse second line
    stdout.lines().nth(1)?.trim().parse().ok()
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn get_store_usage_bytes() -> Option<u64> {
    // On non-Linux, use du which is slower but portable
    let output = Command::new("du")
        .args(["-s", "-k", "/nix/store"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // du -s -k outputs "SIZE\tPATH"
    let size_kb: u64 = stdout.split_whitespace().next()?.parse().ok()?;
    Some(size_kb * 1024)
}

/// Get available disk space on the Nix store partition in bytes.
#[cfg(target_os = "linux")]
pub fn get_store_available_bytes() -> Option<u64> {
    let output = Command::new("df")
        .args(["--output=avail", "-B1", "/nix/store"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().nth(1)?.trim().parse().ok()
}

#[cfg(not(target_os = "linux"))]
pub fn get_store_available_bytes() -> Option<u64> {
    let output = Command::new("df")
        .args(["-k", "/nix/store"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // df output: Filesystem 1K-blocks Used Available Use% Mounted
    // We want the 4th column (Available)
    let line = stdout.lines().nth(1)?;
    let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(available_kb * 1024)
}

/// Check if the store is running low on disk space.
///
/// Returns true if available space is below the threshold (default: 10 GB).
pub fn is_store_low_on_space(threshold_bytes: u64) -> bool {
    get_store_available_bytes()
        .map(|avail| avail < threshold_bytes)
        .unwrap_or(false)
}

/// Verify the Nix store integrity.
///
/// Runs `nix-store --verify` and returns true if the store is healthy.
/// This is a quick check that doesn't verify file contents.
pub fn verify_store() -> bool {
    let result = Command::new("nix-store")
        .arg("--verify")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) => status.success(),
        Err(e) => {
            warn!(error = %e, "Failed to run nix-store --verify");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gc_available() {
        // This test just verifies the function doesn't panic
        let _ = is_gc_available();
    }

    #[test]
    fn test_get_store_available_bytes() {
        // On systems with /nix/store, this should return Some
        // On systems without, it should return None
        let result = get_store_available_bytes();
        if std::path::Path::new("/nix/store").exists() {
            assert!(result.is_some(), "Should return Some on NixOS");
        }
    }

    #[test]
    fn test_is_store_low_on_space() {
        // With a very high threshold, should return true (if /nix/store exists)
        // With threshold 0, should return false
        if std::path::Path::new("/nix/store").exists() {
            assert!(!is_store_low_on_space(0));
        }
    }
}

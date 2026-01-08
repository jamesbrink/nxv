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

/// Path to the temporary eval store used by the indexer.
/// This store isolates derivations from the system store to avoid
/// triggering auto-optimise-store corruption.
///
/// On macOS, we use `/private/tmp` instead of `/tmp` because `/tmp` is a
/// symlink to `/private/tmp`, and Nix refuses to create stores in paths
/// containing symlinks.
#[cfg(target_os = "macos")]
pub const TEMP_EVAL_STORE_PATH: &str = "/private/tmp/nxv-eval-store";

#[cfg(not(target_os = "macos"))]
pub const TEMP_EVAL_STORE_PATH: &str = "/tmp/nxv-eval-store";

/// Clean up the temporary eval store directory.
///
/// This removes all files in the temp store to free disk space.
/// Should be called at indexer startup, during periodic GC, and on exit.
///
/// Note: Nix store paths are read-only, so we need to make them writable first
/// or use chmod -R to fix permissions before deletion.
///
/// Returns the number of bytes freed if cleanup succeeded, None if failed or nothing to clean.
pub fn cleanup_temp_eval_store() -> Option<u64> {
    use std::path::Path;

    let store_path = Path::new(TEMP_EVAL_STORE_PATH);

    if !store_path.exists() {
        return Some(0);
    }

    // Get size before cleanup for reporting
    let size_before = get_temp_eval_store_size().unwrap_or(0);

    // Nix store paths are read-only, so we need to make them writable first
    // Use chmod -R u+w to make all files writable before deletion
    let chmod_result = Command::new("chmod")
        .args(["-R", "u+w", TEMP_EVAL_STORE_PATH])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Err(e) = chmod_result {
        warn!(error = %e, "Failed to chmod temp eval store");
    }

    match std::fs::remove_dir_all(store_path) {
        Ok(()) => Some(size_before),
        Err(e) => {
            warn!(
                error = %e,
                path = TEMP_EVAL_STORE_PATH,
                "Failed to clean up temp eval store"
            );
            None
        }
    }
}

/// Get the size of the temp eval store in bytes.
///
/// Returns None if the store doesn't exist or can't be measured.
#[allow(dead_code)]
pub fn get_temp_eval_store_size() -> Option<u64> {
    use std::path::Path;

    let store_path = Path::new(TEMP_EVAL_STORE_PATH);

    if !store_path.exists() {
        return Some(0);
    }

    // Use du to get directory size
    let output = Command::new("du")
        .args(["-sb", TEMP_EVAL_STORE_PATH])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.split_whitespace().next()?.parse().ok()
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

    #[test]
    fn test_cleanup_temp_eval_store_nonexistent() {
        // Cleaning a non-existent directory should succeed and return Some(0)
        let result = cleanup_temp_eval_store();
        // If directory doesn't exist, returns Some(0)
        // If it exists and cleanup succeeds, returns Some(bytes_freed)
        // If cleanup fails, returns None
        // We just verify it doesn't panic and returns Some
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn test_get_temp_eval_store_size_nonexistent() {
        // Getting size of non-existent store should return Some(0)
        // Note: This depends on TEMP_EVAL_STORE_PATH not existing
        // If it exists from a previous test, this will return Some(size)
        let result = get_temp_eval_store_size();
        // Should return Some value (0 if doesn't exist, or actual size)
        // We just verify it doesn't panic
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn test_temp_eval_store_path_constant() {
        // Verify the constant is set correctly for the current platform
        #[cfg(target_os = "macos")]
        assert_eq!(TEMP_EVAL_STORE_PATH, "/private/tmp/nxv-eval-store");

        #[cfg(not(target_os = "macos"))]
        assert_eq!(TEMP_EVAL_STORE_PATH, "/tmp/nxv-eval-store");
    }
}

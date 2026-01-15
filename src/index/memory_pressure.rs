//! Memory pressure monitoring for adaptive concurrency control.
//!
//! This module monitors system memory pressure to enable graceful backoff
//! when the system is running low on memory. It uses:
//!
//! 1. **Available memory**: Reads `/proc/meminfo` MemAvailable field
//! 2. **PSI (Pressure Stall Information)**: Reads `/proc/pressure/memory` on Linux 4.20+
//!
//! The indexer can use this to:
//! - Pause spawning new workers when memory is low
//! - Reduce batch sizes when under pressure
//! - Wait for memory to free up before continuing heavy operations

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Minimum available memory (MiB) before we consider the system under pressure.
/// Default: 2 GiB - enough headroom for system processes.
const DEFAULT_MIN_AVAILABLE_MIB: u64 = 2 * 1024;

/// PSI "some" threshold (percentage) for considering memory under pressure.
/// If memory stall percentage exceeds this, we throttle.
const PSI_SOME_THRESHOLD: f64 = 25.0;

/// How often to re-check memory status (to avoid syscall spam).
const CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// Cached memory state to avoid constant syscalls.
static LAST_CHECK: AtomicU64 = AtomicU64::new(0);
static CACHED_AVAILABLE_MIB: AtomicU64 = AtomicU64::new(u64::MAX);
static CACHED_UNDER_PRESSURE: AtomicBool = AtomicBool::new(false);

/// Memory pressure state.
#[derive(Debug, Clone)]
pub struct MemoryPressure {
    /// Available memory in MiB.
    pub available_mib: u64,
    /// Whether the system is under memory pressure.
    pub under_pressure: bool,
    /// PSI "some" percentage (0-100), if available.
    pub psi_some: Option<f64>,
    /// PSI "full" percentage (0-100), if available.
    pub psi_full: Option<f64>,
}

impl MemoryPressure {
    /// Check if memory pressure is critical (should stop all new work).
    pub fn is_critical(&self) -> bool {
        // Critical if available < 1 GiB OR PSI full > 50%
        self.available_mib < 1024 || self.psi_full.is_some_and(|p| p > 50.0)
    }

    /// Check if memory pressure is high (should reduce batch sizes).
    pub fn is_high(&self) -> bool {
        // High if available < 2 GiB OR PSI some > 25%
        self.available_mib < 2048 || self.psi_some.is_some_and(|p| p > PSI_SOME_THRESHOLD)
    }
}

/// Get current memory pressure state.
///
/// This function is relatively cheap to call frequently - it caches results
/// and only re-reads system state every 500ms.
pub fn get_memory_pressure() -> MemoryPressure {
    // Check if we should use cached value
    let last = LAST_CHECK.load(Ordering::Relaxed);

    // Use system time for cache invalidation
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    if now.saturating_sub(last) < CHECK_INTERVAL.as_millis() as u64 {
        // Return cached value
        return MemoryPressure {
            available_mib: CACHED_AVAILABLE_MIB.load(Ordering::Relaxed),
            under_pressure: CACHED_UNDER_PRESSURE.load(Ordering::Relaxed),
            psi_some: None, // Not cached for simplicity
            psi_full: None,
        };
    }

    // Read fresh values
    let available_mib = read_available_memory_mib().unwrap_or(u64::MAX);
    let (psi_some, psi_full) = read_psi_memory();

    let under_pressure = available_mib < DEFAULT_MIN_AVAILABLE_MIB
        || psi_some.is_some_and(|p| p > PSI_SOME_THRESHOLD);

    // Update cache
    LAST_CHECK.store(now, Ordering::Relaxed);
    CACHED_AVAILABLE_MIB.store(available_mib, Ordering::Relaxed);
    CACHED_UNDER_PRESSURE.store(under_pressure, Ordering::Relaxed);

    MemoryPressure {
        available_mib,
        under_pressure,
        psi_some,
        psi_full,
    }
}

/// Quick check if memory is under pressure (uses cache).
#[allow(dead_code)]
pub fn is_under_pressure() -> bool {
    get_memory_pressure().under_pressure
}

/// Get available memory in MiB (uses cache).
#[allow(dead_code)]
pub fn get_available_mib() -> u64 {
    get_memory_pressure().available_mib
}

/// Wait for memory pressure to subside.
///
/// Blocks until available memory exceeds the threshold or timeout is reached.
/// Returns true if memory is now available, false if timeout was reached.
pub fn wait_for_memory(min_available_mib: u64, timeout: Duration) -> bool {
    let start = Instant::now();
    let check_interval = Duration::from_millis(100);

    loop {
        // Force fresh read by clearing cache
        LAST_CHECK.store(0, Ordering::Relaxed);

        let pressure = get_memory_pressure();
        if pressure.available_mib >= min_available_mib {
            return true;
        }

        if start.elapsed() >= timeout {
            return false;
        }

        // Sleep before retrying
        std::thread::sleep(check_interval);
    }
}

/// Read available memory from /proc/meminfo.
///
/// Parses the MemAvailable field which represents memory that can be
/// given to a new process without swapping.
#[cfg(target_os = "linux")]
fn read_available_memory_mib() -> Option<u64> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;

    for line in contents.lines() {
        if line.starts_with("MemAvailable:") {
            // Format: "MemAvailable:    12345678 kB"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let kb: u64 = parts[1].parse().ok()?;
                return Some(kb / 1024); // Convert kB to MiB
            }
        }
    }

    // Fallback: try MemFree + Buffers + Cached
    let mut mem_free = 0u64;
    let mut buffers = 0u64;
    let mut cached = 0u64;

    for line in contents.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            if line.starts_with("MemFree:") {
                mem_free = parts[1].parse().unwrap_or(0);
            } else if line.starts_with("Buffers:") {
                buffers = parts[1].parse().unwrap_or(0);
            } else if line.starts_with("Cached:") && !line.starts_with("SwapCached:") {
                cached = parts[1].parse().unwrap_or(0);
            }
        }
    }

    if mem_free > 0 {
        Some((mem_free + buffers + cached) / 1024)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn read_available_memory_mib() -> Option<u64> {
    // On non-Linux systems, assume unlimited memory (no throttling)
    Some(u64::MAX)
}

/// Read PSI memory metrics from /proc/pressure/memory.
///
/// Returns (some_pct, full_pct) if available, or (None, None) on older kernels.
///
/// PSI format:
/// ```text
/// some avg10=0.00 avg60=0.00 avg300=0.00 total=123456
/// full avg10=0.00 avg60=0.00 avg300=0.00 total=123456
/// ```
#[cfg(target_os = "linux")]
fn read_psi_memory() -> (Option<f64>, Option<f64>) {
    let contents = match fs::read_to_string("/proc/pressure/memory") {
        Ok(c) => c,
        Err(_) => return (None, None), // PSI not available (kernel < 4.20)
    };

    let mut some_pct = None;
    let mut full_pct = None;

    for line in contents.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        // Parse avg10 value (most recent 10-second average)
        let avg10 = parts
            .iter()
            .find(|p| p.starts_with("avg10="))
            .and_then(|p| p.strip_prefix("avg10="))
            .and_then(|v| v.parse::<f64>().ok());

        if line.starts_with("some ") {
            some_pct = avg10;
        } else if line.starts_with("full ") {
            full_pct = avg10;
        }
    }

    (some_pct, full_pct)
}

#[cfg(not(target_os = "linux"))]
fn read_psi_memory() -> (Option<f64>, Option<f64>) {
    // PSI is Linux-specific
    (None, None)
}

/// Memory pressure guard that logs when acquired/released.
///
/// Use this to wrap heavy operations that should wait for memory.
#[allow(dead_code)]
pub struct MemoryGuard {
    operation: String,
    start: Instant,
}

impl MemoryGuard {
    /// Try to acquire a memory guard, waiting if necessary.
    ///
    /// Returns None if memory doesn't become available within timeout.
    #[allow(dead_code)]
    pub fn try_acquire(operation: &str, min_available_mib: u64, timeout: Duration) -> Option<Self> {
        let pressure = get_memory_pressure();

        if pressure.available_mib >= min_available_mib {
            tracing::debug!(
                operation = %operation,
                available_mib = pressure.available_mib,
                "Memory guard acquired (no wait)"
            );
            return Some(Self {
                operation: operation.to_string(),
                start: Instant::now(),
            });
        }

        tracing::info!(
            operation = %operation,
            available_mib = pressure.available_mib,
            required_mib = min_available_mib,
            psi_some = ?pressure.psi_some,
            "Waiting for memory pressure to subside"
        );

        if wait_for_memory(min_available_mib, timeout) {
            let waited = Instant::now();
            tracing::info!(
                operation = %operation,
                waited_ms = waited.elapsed().as_millis(),
                "Memory guard acquired after wait"
            );
            Some(Self {
                operation: operation.to_string(),
                start: waited,
            })
        } else {
            tracing::warn!(
                operation = %operation,
                timeout_secs = timeout.as_secs(),
                "Timeout waiting for memory"
            );
            None
        }
    }
}

impl Drop for MemoryGuard {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        if elapsed > Duration::from_secs(1) {
            tracing::debug!(
                operation = %self.operation,
                duration_secs = elapsed.as_secs(),
                "Memory guard released"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_memory_pressure() {
        let pressure = get_memory_pressure();
        // Should return some value (either real data or MAX on non-Linux)
        assert!(pressure.available_mib > 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_read_available_memory() {
        let available = read_available_memory_mib();
        assert!(available.is_some());
        let mib = available.unwrap();
        // Should be reasonable (at least 100 MiB, less than 1 TiB)
        assert!(mib >= 100, "Available memory too low: {} MiB", mib);
        assert!(mib < 1024 * 1024, "Available memory too high: {} MiB", mib);
    }

    #[test]
    fn test_memory_pressure_critical() {
        let critical = MemoryPressure {
            available_mib: 500,
            under_pressure: true,
            psi_some: Some(30.0),
            psi_full: Some(60.0),
        };
        assert!(critical.is_critical());
        assert!(critical.is_high());

        let normal = MemoryPressure {
            available_mib: 10000,
            under_pressure: false,
            psi_some: Some(5.0),
            psi_full: Some(0.0),
        };
        assert!(!normal.is_critical());
        assert!(!normal.is_high());
    }

    #[test]
    fn test_is_under_pressure() {
        // Just ensure it doesn't panic
        let _ = is_under_pressure();
    }

    #[test]
    fn test_wait_for_memory_immediate() {
        // With high threshold, should return immediately
        let result = wait_for_memory(0, Duration::from_millis(100));
        assert!(result);
    }
}

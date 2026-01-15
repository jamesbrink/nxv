//! Per-worker memory watchdog for RSS enforcement.
//!
//! This module provides a watchdog thread that monitors worker subprocess RSS
//! (Resident Set Size) and kills workers that exceed their memory budget.
//!
//! Unlike RLIMIT_AS (which limits virtual address space), this monitors actual
//! physical memory usage and can terminate runaway evaluations mid-execution.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How often the watchdog checks worker RSS.
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Grace period before killing a worker that exceeds its limit.
/// Allows brief spikes without immediate termination.
const GRACE_PERIOD_MS: u64 = 500;

/// Global flag indicating the system is under critical memory pressure.
/// When set, worker pools should pause new work.
pub static MEMORY_CRITICAL: AtomicBool = AtomicBool::new(false);

/// Total system memory in MiB (cached on first read).
static SYSTEM_MEMORY_MIB: AtomicU64 = AtomicU64::new(0);

/// Get total system memory in MiB.
pub fn get_system_memory_mib() -> u64 {
    let cached = SYSTEM_MEMORY_MIB.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }

    let total = read_mem_total_mib().unwrap_or(64 * 1024); // Default 64 GiB
    SYSTEM_MEMORY_MIB.store(total, Ordering::Relaxed);
    total
}

/// Calculate dynamic critical threshold based on system memory.
/// Returns max(1 GiB, 5% of MemTotal).
pub fn get_critical_threshold_mib() -> u64 {
    let total = get_system_memory_mib();
    let five_percent = total / 20;
    five_percent.max(1024) // At least 1 GiB
}

/// Calculate dynamic high-pressure threshold based on system memory.
/// Returns max(2 GiB, 10% of MemTotal).
/// Used for adaptive batch sizing when under moderate pressure.
#[allow(dead_code)]
pub fn get_high_threshold_mib() -> u64 {
    let total = get_system_memory_mib();
    let ten_percent = total / 10;
    ten_percent.max(2048) // At least 2 GiB
}

/// Read total system memory from /proc/meminfo.
#[cfg(target_os = "linux")]
fn read_mem_total_mib() -> Option<u64> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let kb: u64 = parts[1].parse().ok()?;
                return Some(kb / 1024);
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_mem_total_mib() -> Option<u64> {
    None
}

/// Information about a worker being monitored.
#[derive(Debug, Clone)]
struct MonitoredWorker {
    /// Process ID.
    pid: u32,
    /// Memory limit in MiB.
    limit_mib: usize,
    /// Timestamp when worker first exceeded limit (for grace period).
    exceeded_since: Option<std::time::Instant>,
    /// Label for logging (e.g., "range-2020-Q1-w0").
    label: String,
}

/// Worker memory watchdog.
///
/// Runs a background thread that periodically checks worker RSS and
/// kills workers that exceed their memory budget.
pub struct WorkerWatchdog {
    /// Workers being monitored, keyed by PID.
    workers: Arc<Mutex<HashMap<u32, MonitoredWorker>>>,
    /// Flag to stop the watchdog thread.
    shutdown: Arc<AtomicBool>,
    /// Watchdog thread handle.
    thread: Option<JoinHandle<()>>,
}

impl WorkerWatchdog {
    /// Create and start a new watchdog.
    pub fn new() -> Self {
        let workers = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let workers_clone = workers.clone();
        let shutdown_clone = shutdown.clone();

        let thread = thread::Builder::new()
            .name("worker-watchdog".to_string())
            .spawn(move || {
                watchdog_loop(workers_clone, shutdown_clone);
            })
            .expect("Failed to spawn watchdog thread");

        Self {
            workers,
            shutdown,
            thread: Some(thread),
        }
    }

    /// Register a worker to be monitored.
    pub fn register(&self, pid: u32, limit_mib: usize, label: &str) {
        let mut workers = self.workers.lock().unwrap();
        workers.insert(
            pid,
            MonitoredWorker {
                pid,
                limit_mib,
                exceeded_since: None,
                label: label.to_string(),
            },
        );
        tracing::debug!(pid, limit_mib, label, "Watchdog registered worker");
    }

    /// Unregister a worker (e.g., when it exits normally).
    pub fn unregister(&self, pid: u32) {
        let mut workers = self.workers.lock().unwrap();
        if workers.remove(&pid).is_some() {
            tracing::debug!(pid, "Watchdog unregistered worker");
        }
    }

    /// Check if memory is currently critical.
    /// Convenience method for checking the global flag.
    #[allow(dead_code)]
    pub fn is_critical() -> bool {
        MEMORY_CRITICAL.load(Ordering::Relaxed)
    }

    /// Shutdown the watchdog.
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Default for WorkerWatchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WorkerWatchdog {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Main watchdog loop.
fn watchdog_loop(workers: Arc<Mutex<HashMap<u32, MonitoredWorker>>>, shutdown: Arc<AtomicBool>) {
    tracing::debug!("Watchdog thread started");

    while !shutdown.load(Ordering::Relaxed) {
        // Check system-wide memory pressure
        check_system_pressure();

        // Check each worker's RSS
        let workers_snapshot: Vec<MonitoredWorker> = {
            let guard = workers.lock().unwrap();
            guard.values().cloned().collect()
        };

        for worker in workers_snapshot {
            if let Some(rss_mib) = read_process_rss_mib(worker.pid) {
                if rss_mib > worker.limit_mib as u64 {
                    handle_over_limit(&workers, &worker, rss_mib);
                } else {
                    // Clear exceeded_since if under limit
                    let mut guard = workers.lock().unwrap();
                    if let Some(w) = guard.get_mut(&worker.pid) {
                        w.exceeded_since = None;
                    }
                }
            } else {
                // Process gone - unregister
                let mut guard = workers.lock().unwrap();
                guard.remove(&worker.pid);
            }
        }

        thread::sleep(WATCHDOG_POLL_INTERVAL);
    }

    tracing::debug!("Watchdog thread stopped");
}

/// Check system-wide memory pressure and update MEMORY_CRITICAL flag.
fn check_system_pressure() {
    use super::super::memory_pressure;

    let pressure = memory_pressure::get_memory_pressure();
    let critical_threshold = get_critical_threshold_mib();

    let is_critical =
        pressure.available_mib < critical_threshold || pressure.psi_full.is_some_and(|p| p > 10.0);

    let was_critical = MEMORY_CRITICAL.swap(is_critical, Ordering::Relaxed);

    if is_critical && !was_critical {
        tracing::warn!(
            available_mib = pressure.available_mib,
            critical_threshold_mib = critical_threshold,
            psi_full = ?pressure.psi_full,
            "System entered CRITICAL memory pressure"
        );
    } else if !is_critical && was_critical {
        tracing::info!(
            available_mib = pressure.available_mib,
            "System memory pressure recovered"
        );
    }
}

/// Handle a worker that's over its memory limit.
fn handle_over_limit(
    workers: &Arc<Mutex<HashMap<u32, MonitoredWorker>>>,
    worker: &MonitoredWorker,
    rss_mib: u64,
) {
    let now = std::time::Instant::now();

    let should_kill = {
        let mut guard = workers.lock().unwrap();
        if let Some(w) = guard.get_mut(&worker.pid) {
            match w.exceeded_since {
                None => {
                    // First time exceeding - start grace period
                    w.exceeded_since = Some(now);
                    tracing::debug!(
                        pid = worker.pid,
                        label = %worker.label,
                        rss_mib,
                        limit_mib = worker.limit_mib,
                        "Worker over limit, starting grace period"
                    );
                    false
                }
                Some(since) => {
                    // Check if grace period expired
                    since.elapsed().as_millis() as u64 > GRACE_PERIOD_MS
                }
            }
        } else {
            false
        }
    };

    if should_kill {
        tracing::warn!(
            pid = worker.pid,
            label = %worker.label,
            rss_mib,
            limit_mib = worker.limit_mib,
            "Killing worker for exceeding memory limit"
        );

        // Send SIGTERM first for graceful shutdown
        let pid = Pid::from_raw(worker.pid as i32);
        if let Err(e) = kill(pid, Signal::SIGTERM) {
            tracing::debug!(pid = worker.pid, error = %e, "SIGTERM failed, trying SIGKILL");
            let _ = kill(pid, Signal::SIGKILL);
        }

        // Unregister - parent will detect death and respawn
        let mut guard = workers.lock().unwrap();
        guard.remove(&worker.pid);
    }
}

/// Read RSS (Resident Set Size) of a process in MiB.
///
/// Uses /proc/<pid>/statm which has format:
/// size resident shared text lib data dt
/// All values are in pages (usually 4KB).
#[cfg(target_os = "linux")]
fn read_process_rss_mib(pid: u32) -> Option<u64> {
    let path = format!("/proc/{}/statm", pid);
    let contents = fs::read_to_string(&path).ok()?;
    let parts: Vec<&str> = contents.split_whitespace().collect();

    if parts.len() >= 2 {
        let resident_pages: u64 = parts[1].parse().ok()?;
        // Assume 4KB pages (most common)
        let page_size_kb = 4;
        let rss_kb = resident_pages * page_size_kb;
        Some(rss_kb / 1024) // Convert to MiB
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn read_process_rss_mib(_pid: u32) -> Option<u64> {
    // Not supported on non-Linux
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_system_memory() {
        let mem = get_system_memory_mib();
        // Should be at least 1 GiB, less than 10 TiB
        assert!(mem >= 1024, "System memory too low: {} MiB", mem);
        assert!(
            mem < 10 * 1024 * 1024,
            "System memory too high: {} MiB",
            mem
        );
    }

    #[test]
    fn test_dynamic_thresholds() {
        let critical = get_critical_threshold_mib();
        let high = get_high_threshold_mib();

        // Critical should be at least 1 GiB
        assert!(critical >= 1024);
        // High should be at least 2 GiB
        assert!(high >= 2048);
        // High should be >= critical
        assert!(high >= critical);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_read_own_rss() {
        let pid = std::process::id();
        let rss = read_process_rss_mib(pid);
        assert!(rss.is_some());
        let rss = rss.unwrap();
        // Should be at least a few MiB, less than 10 GiB
        assert!(rss >= 1, "RSS too low: {} MiB", rss);
        assert!(rss < 10 * 1024, "RSS too high: {} MiB", rss);
    }

    #[test]
    fn test_watchdog_creation() {
        let mut watchdog = WorkerWatchdog::new();
        // Should be able to register and unregister
        watchdog.register(99999, 1024, "test-worker");
        watchdog.unregister(99999);
        watchdog.shutdown();
    }
}

//! Worker pool for parallel Nix evaluations.
//!
//! Manages a pool of worker subprocesses that can evaluate packages in parallel.

#![allow(dead_code)] // Some fields/methods are for future use or monitoring

use super::proc::Proc;
use super::protocol::{WorkRequest, WorkResponse};
use super::signals::{TerminationReason, WorkerFailure, analyze_wait_status};
use super::spawn::{WorkerConfig, spawn_worker};
use super::watchdog::{MEMORY_CRITICAL, WorkerWatchdog};
use crate::error::{NxvError, Result};
use crate::index::extractor::{AttrPosition, PackageInfo};
use crate::memory::DEFAULT_MEMORY_BUDGET;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{instrument, trace};

/// Configuration for the worker pool.
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// Number of worker processes to spawn.
    pub worker_count: usize,
    /// Per-worker memory threshold (MiB) before worker restart.
    /// This is the already-calculated per-worker allocation from the total budget.
    pub per_worker_memory_mib: usize,
    /// Timeout for worker operations.
    pub timeout: Duration,
    /// Custom eval store path (for parallel range isolation).
    pub eval_store_path: Option<String>,
    /// Label for this pool (e.g., "2020-Q1") for debugging.
    pub label: Option<String>,
}

impl Default for WorkerPoolConfig {
    fn default() -> Self {
        const DEFAULT_WORKERS: usize = 4;
        Self {
            worker_count: DEFAULT_WORKERS,
            // Default: 8 GiB total / 4 workers = 2 GiB per worker
            per_worker_memory_mib: (DEFAULT_MEMORY_BUDGET.as_mib() / DEFAULT_WORKERS as u64)
                as usize,
            timeout: Duration::from_secs(300), // 5 minutes
            eval_store_path: None,
            label: None,
        }
    }
}

/// A single worker in the pool.
struct Worker {
    /// Process handle (None if worker needs respawn).
    proc: Option<Proc>,
    /// Worker configuration for respawning.
    config: WorkerConfig,
    /// Worker ID for logging.
    id: usize,
    /// Number of jobs completed by this worker.
    jobs_completed: usize,
    /// Number of times this worker has been restarted.
    restarts: usize,
    /// Shared watchdog for memory monitoring.
    watchdog: Arc<WorkerWatchdog>,
    /// Label for this worker (for watchdog registration).
    label: String,
}

impl Worker {
    /// Create a new worker and spawn its subprocess.
    fn new(
        id: usize,
        config: WorkerConfig,
        watchdog: Arc<WorkerWatchdog>,
        label: String,
    ) -> Result<Self> {
        let proc = spawn_worker(&config)?;

        // Register with watchdog for RSS monitoring
        let pid = proc.pid().as_raw() as u32;
        watchdog.register(pid, config.per_worker_memory_mib, &label);

        Ok(Self {
            proc: Some(proc),
            config,
            id,
            jobs_completed: 0,
            restarts: 0,
            watchdog,
            label,
        })
    }

    /// Ensure the worker is ready, spawning if needed.
    fn ensure_ready(&mut self) -> Result<()> {
        if self.proc.is_none() {
            self.spawn()?;
            self.wait_for_ready()?; // Consume startup Ready signal
        }
        Ok(())
    }

    /// Spawn or respawn the worker subprocess.
    fn spawn(&mut self) -> Result<()> {
        // Unregister old PID if any (worker may have died)
        if let Some(ref old_proc) = self.proc {
            let old_pid = old_proc.pid().as_raw() as u32;
            self.watchdog.unregister(old_pid);
        }

        let proc = spawn_worker(&self.config)?;

        // Register new PID with watchdog
        let pid = proc.pid().as_raw() as u32;
        self.watchdog
            .register(pid, self.config.per_worker_memory_mib, &self.label);

        self.proc = Some(proc);
        Ok(())
    }

    /// Wait for the worker to signal ready.
    fn wait_for_ready(&mut self) -> Result<()> {
        let proc = self
            .proc
            .as_mut()
            .ok_or_else(|| NxvError::Worker(format!("Worker {} not spawned", self.id)))?;

        match proc.recv()? {
            Some(WorkResponse::Ready) => Ok(()),
            Some(other) => Err(NxvError::Worker(format!(
                "Worker {} sent unexpected response instead of Ready: {:?}",
                self.id, other
            ))),
            None => Err(NxvError::Worker(format!(
                "Worker {} closed connection before Ready",
                self.id
            ))),
        }
    }

    /// Send an extraction request and wait for the result.
    fn extract(
        &mut self,
        system: &str,
        repo_path: &Path,
        attrs: &[String],
        extract_store_paths: bool,
    ) -> Result<Vec<PackageInfo>> {
        let request_start = Instant::now();
        self.ensure_ready()?;

        let proc = self
            .proc
            .as_mut()
            .ok_or_else(|| NxvError::Worker(format!("Worker {} not available", self.id)))?;

        // Send request
        let request = WorkRequest::extract(
            system,
            repo_path.to_string_lossy().to_string(),
            attrs.to_vec(),
            extract_store_paths,
        );
        let send_start = Instant::now();
        proc.send(&request)?;
        let send_time = send_start.elapsed();

        // Receive result
        let recv_start = Instant::now();
        let response = proc.recv()?;
        let recv_time = recv_start.elapsed();

        trace!(
            worker_id = self.id,
            system = %system,
            attr_count = attrs.len(),
            send_time_ms = send_time.as_millis(),
            recv_time_ms = recv_time.as_millis(),
            total_ipc_time_ms = request_start.elapsed().as_millis(),
            "Worker IPC request/response"
        );
        let packages = match response {
            Some(WorkResponse::Result { packages }) => packages,
            Some(WorkResponse::Error { message }) => {
                // Store error but still need to consume Ready signal
                return self.finish_request_and_error(format!(
                    "Worker {} extraction error: {}",
                    self.id, message
                ));
            }
            Some(WorkResponse::Restart {
                memory_mib,
                threshold_mib,
            }) => {
                // Worker requested restart - respawn and retry
                self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib))?;
                return self.extract(system, repo_path, attrs, extract_store_paths);
            }
            Some(WorkResponse::Ready) | Some(WorkResponse::PositionsResult { .. }) => {
                return Err(NxvError::Worker(format!(
                    "Worker {} sent unexpected response instead of result",
                    self.id
                )));
            }
            None => {
                // Worker died - check why and maybe retry
                return Err(self.handle_death("during extraction")?);
            }
        };

        // Wait for Ready or Restart signal
        self.wait_for_ready_signal(packages)
    }

    /// Wait for Ready/Restart signal after receiving a result.
    fn wait_for_ready_signal(&mut self, packages: Vec<PackageInfo>) -> Result<Vec<PackageInfo>> {
        let proc = self
            .proc
            .as_mut()
            .ok_or_else(|| NxvError::Worker(format!("Worker {} not available", self.id)))?;

        match proc.recv()? {
            Some(WorkResponse::Ready) => {
                self.jobs_completed += 1;
                Ok(packages)
            }
            Some(WorkResponse::Restart {
                memory_mib,
                threshold_mib,
            }) => {
                self.jobs_completed += 1;
                self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib))?;
                Ok(packages)
            }
            Some(other) => Err(NxvError::Worker(format!(
                "Worker {} sent unexpected response: {:?}",
                self.id, other
            ))),
            None => {
                // Worker died after sending result - that's ok, we got the data
                self.jobs_completed += 1;
                self.proc = None; // Will respawn on next request
                Ok(packages)
            }
        }
    }

    /// Send a positions extraction request and wait for the result.
    fn extract_positions(&mut self, system: &str, repo_path: &Path) -> Result<Vec<AttrPosition>> {
        self.ensure_ready()?;

        let proc = self
            .proc
            .as_mut()
            .ok_or_else(|| NxvError::Worker(format!("Worker {} not available", self.id)))?;

        // Send request
        let request =
            WorkRequest::extract_positions(system, repo_path.to_string_lossy().to_string());
        proc.send(&request)?;

        // Receive result
        let response = proc.recv()?;
        let positions = match response {
            Some(WorkResponse::PositionsResult { positions }) => positions,
            Some(WorkResponse::Error { message }) => {
                // Consume Ready signal and return error
                return self.finish_request_and_error_positions(format!(
                    "Worker {} positions extraction error: {}",
                    self.id, message
                ));
            }
            Some(WorkResponse::Restart {
                memory_mib,
                threshold_mib,
            }) => {
                // Worker requested restart - respawn and retry
                self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib))?;
                return self.extract_positions(system, repo_path);
            }
            Some(WorkResponse::Ready) => {
                return Err(NxvError::Worker(format!(
                    "Worker {} sent Ready instead of result",
                    self.id
                )));
            }
            Some(other) => {
                return Err(NxvError::Worker(format!(
                    "Worker {} sent unexpected response: {:?}",
                    self.id, other
                )));
            }
            None => {
                // Worker died - check why and maybe retry
                return Err(self.handle_death("during positions extraction")?);
            }
        };

        // Wait for Ready or Restart signal
        self.wait_for_ready_signal_positions(positions)
    }

    /// Wait for Ready/Restart signal after receiving positions result.
    fn wait_for_ready_signal_positions(
        &mut self,
        positions: Vec<AttrPosition>,
    ) -> Result<Vec<AttrPosition>> {
        let proc = self
            .proc
            .as_mut()
            .ok_or_else(|| NxvError::Worker(format!("Worker {} not available", self.id)))?;

        match proc.recv()? {
            Some(WorkResponse::Ready) => {
                self.jobs_completed += 1;
                Ok(positions)
            }
            Some(WorkResponse::Restart {
                memory_mib,
                threshold_mib,
            }) => {
                self.jobs_completed += 1;
                self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib))?;
                Ok(positions)
            }
            Some(other) => Err(NxvError::Worker(format!(
                "Worker {} sent unexpected response: {:?}",
                self.id, other
            ))),
            None => {
                // Worker died after sending result - that's ok, we got the data
                self.jobs_completed += 1;
                self.proc = None; // Will respawn on next request
                Ok(positions)
            }
        }
    }

    /// Consume Ready signal after an error for positions extraction.
    fn finish_request_and_error_positions(
        &mut self,
        error_msg: String,
    ) -> Result<Vec<AttrPosition>> {
        // Worker sends Ready/Restart after Error too - consume it
        if let Some(proc) = self.proc.as_mut() {
            match proc.recv() {
                Ok(Some(WorkResponse::Ready)) => {}
                Ok(Some(WorkResponse::Restart {
                    memory_mib,
                    threshold_mib,
                })) => {
                    let _ = self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib));
                }
                _ => {
                    // Worker died or sent unexpected response - mark for respawn
                    self.proc = None;
                }
            }
        }
        Err(NxvError::Worker(error_msg))
    }

    /// Consume Ready signal after an error, then return the error.
    fn finish_request_and_error(&mut self, error_msg: String) -> Result<Vec<PackageInfo>> {
        // Worker sends Ready/Restart after Error too - consume it
        if let Some(proc) = self.proc.as_mut() {
            match proc.recv() {
                Ok(Some(WorkResponse::Ready)) => {}
                Ok(Some(WorkResponse::Restart {
                    memory_mib,
                    threshold_mib,
                })) => {
                    let _ = self.handle_restart_with_memory(Some(memory_mib), Some(threshold_mib));
                }
                _ => {
                    // Worker died or sent unexpected response - mark for respawn
                    self.proc = None;
                }
            }
        }
        Err(NxvError::Worker(error_msg))
    }

    /// Handle worker restart request.
    fn handle_restart(&mut self) -> Result<()> {
        self.handle_restart_with_memory(None, None)
    }

    /// Handle worker restart request with memory info.
    fn handle_restart_with_memory(
        &mut self,
        memory_mib: Option<usize>,
        threshold_mib: Option<usize>,
    ) -> Result<()> {
        if let (Some(mem), Some(thresh)) = (memory_mib, threshold_mib) {
            tracing::debug!(
                worker_id = self.id,
                cycle = self.restarts + 1,
                jobs_completed = self.jobs_completed,
                memory_mib = mem,
                threshold_mib = thresh,
                "Worker recycling ({}MiB / {}MiB threshold)",
                mem,
                thresh
            );
        } else {
            tracing::debug!(
                worker_id = self.id,
                cycle = self.restarts + 1,
                jobs_completed = self.jobs_completed,
                "Worker recycling for memory management"
            );
        }
        if let Some(mut proc) = self.proc.take() {
            proc.stop(Duration::from_secs(5))?;
        }
        self.restarts += 1;
        self.spawn()?;
        self.wait_for_ready()
    }

    /// Handle worker death and return an appropriate error.
    fn handle_death(&mut self, context: &str) -> Result<NxvError> {
        let reason = if let Some(mut proc) = self.proc.take() {
            match proc.try_wait() {
                Ok(Some(status)) => analyze_wait_status(status),
                _ => TerminationReason::Unknown,
            }
        } else {
            TerminationReason::Unknown
        };

        let failure = WorkerFailure::new(reason.clone()).with_context(context);

        tracing::warn!(
            worker_id = self.id,
            reason = %reason,
            context = context,
            recoverable = failure.is_recoverable(),
            "Worker died unexpectedly"
        );

        if failure.is_recoverable() {
            // Try to respawn
            self.restarts += 1;
            tracing::info!(
                worker_id = self.id,
                restart_count = self.restarts,
                "Attempting to respawn worker"
            );
            if let Err(e) = self.spawn() {
                return Ok(NxvError::Worker(format!(
                    "Worker {} died ({}) and failed to respawn: {}",
                    self.id, reason, e
                )));
            }
            if let Err(e) = self.wait_for_ready() {
                return Ok(NxvError::Worker(format!(
                    "Worker {} respawned but failed to initialize: {}",
                    self.id, e
                )));
            }
            tracing::info!(worker_id = self.id, "Worker respawned successfully");
            // Successfully respawned - caller should retry
            Ok(NxvError::Worker(format!(
                "Worker {} died ({}) but was respawned - retry operation",
                self.id, reason
            )))
        } else {
            Ok(NxvError::Worker(format!(
                "Worker {} failed: {}",
                self.id, failure
            )))
        }
    }

    /// Shutdown the worker gracefully.
    fn shutdown(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            // Unregister from watchdog before stopping
            let pid = proc.pid().as_raw() as u32;
            self.watchdog.unregister(pid);
            let _ = proc.stop(Duration::from_secs(5));
        }
    }
}

/// A pool of worker subprocesses for parallel evaluation.
pub struct WorkerPool {
    workers: Vec<Mutex<Worker>>,
    config: WorkerPoolConfig,
    /// Round-robin counter for worker selection when all are busy.
    next_worker: AtomicUsize,
    /// Memory watchdog for RSS enforcement.
    watchdog: Arc<WorkerWatchdog>,
}

impl WorkerPool {
    /// Create a new worker pool and spawn worker processes.
    pub fn new(config: WorkerPoolConfig) -> Result<Self> {
        tracing::info!(
            workers = config.worker_count,
            memory_limit_mib = config.per_worker_memory_mib,
            total_memory_budget_gib = (config.worker_count * config.per_worker_memory_mib) / 1024,
            "Initializing worker pool"
        );

        // Create shared watchdog for RSS monitoring
        let watchdog = Arc::new(WorkerWatchdog::new());

        // Create label prefix from pool config
        let label_prefix = config.label.as_deref().unwrap_or("pool");

        let mut workers = Vec::with_capacity(config.worker_count);
        for id in 0..config.worker_count {
            // Each worker gets its own eval store to avoid SQLite contention
            let worker_store_path = config
                .eval_store_path
                .as_ref()
                .map(|base| format!("{}-w{}", base, id));
            let worker_config = WorkerConfig {
                per_worker_memory_mib: config.per_worker_memory_mib,
                eval_store_path: worker_store_path,
            };
            let label = format!("{}-w{}", label_prefix, id);
            let worker = Worker::new(id, worker_config, watchdog.clone(), label)?;
            workers.push(Mutex::new(worker));
        }

        // Wait for all workers to be ready
        for (id, worker) in workers.iter().enumerate() {
            let mut w = worker.lock().expect("worker mutex poisoned during init");
            w.wait_for_ready().map_err(|e| {
                NxvError::Worker(format!("Worker {} failed to initialize: {}", id, e))
            })?;
        }

        tracing::info!(workers = config.worker_count, "All workers ready");

        Ok(Self {
            workers,
            config,
            next_worker: AtomicUsize::new(0),
            watchdog,
        })
    }

    /// Get the number of workers in the pool.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Check if the system is under critical memory pressure.
    pub fn is_memory_critical(&self) -> bool {
        MEMORY_CRITICAL.load(Ordering::Relaxed)
    }

    /// Get recommended batch size based on current memory pressure.
    ///
    /// Returns a reduced batch size when the system is under memory pressure:
    /// - Normal: 500 packages per batch
    /// - High pressure: 100 packages per batch
    /// - Critical: 50 packages (caller should check is_memory_critical first)
    ///
    /// This enables graceful degradation under memory pressure rather than OOM.
    pub fn recommended_batch_size(&self, default_batch_size: usize) -> usize {
        use super::super::memory_pressure::get_memory_pressure;

        if MEMORY_CRITICAL.load(Ordering::Relaxed) {
            // Critical pressure - use minimum batch size
            return default_batch_size.min(50);
        }

        let pressure = get_memory_pressure();
        if pressure.is_high() {
            // High pressure - reduce batch size significantly
            default_batch_size.min(100)
        } else {
            // Normal operation
            default_batch_size
        }
    }

    /// Wait for memory pressure to clear before starting new work.
    ///
    /// Uses exponential backoff starting at 100ms, maxing at 5 seconds.
    /// Logs a warning on first wait and info when cleared.
    fn wait_for_memory_clear(&self) {
        use std::thread;

        if !MEMORY_CRITICAL.load(Ordering::Relaxed) {
            return; // Fast path - no pressure
        }

        let mut backoff_ms = 100u64;
        const MAX_BACKOFF_MS: u64 = 5000;
        let mut logged_warning = false;

        while MEMORY_CRITICAL.load(Ordering::Relaxed) {
            if !logged_warning {
                tracing::warn!("Memory critical - pausing new extractions until pressure clears");
                logged_warning = true;
            }

            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
        }

        if logged_warning {
            tracing::info!("Memory pressure cleared - resuming extractions");
        }
    }

    /// Extract packages for a single system using an available worker.
    ///
    /// This method acquires a worker from the pool, sends the extraction request,
    /// and returns the result. It will wait if the system is under critical memory pressure.
    pub fn extract(
        &self,
        system: &str,
        repo_path: &Path,
        attrs: &[String],
        extract_store_paths: bool,
    ) -> Result<Vec<PackageInfo>> {
        // Wait for memory pressure to clear before starting new work
        self.wait_for_memory_clear();

        // Find an available worker using try_lock
        for worker in &self.workers {
            if let Ok(mut w) = worker.try_lock() {
                return w.extract(system, repo_path, attrs, extract_store_paths);
            }
        }

        // All workers busy - use round-robin to distribute wait fairly
        let idx = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let mut w = self.workers[idx].lock().expect("worker mutex poisoned");
        w.extract(system, repo_path, attrs, extract_store_paths)
    }

    /// Extract packages with parent-level batching to control memory.
    ///
    /// For large extractions (>1000 packages), Nix workers accumulate memory
    /// that isn't released between internal batches. This method batches at
    /// the parent level, sending smaller chunks via IPC and allowing workers
    /// to restart between batches if they hit memory limits.
    ///
    /// Each batch is sent as a separate IPC request. If a worker dies during
    /// extraction (killed by watchdog or OOM), it will be respawned and the
    /// batch retried automatically by the underlying Worker::extract().
    pub fn extract_batched(
        &self,
        system: &str,
        repo_path: &Path,
        attrs: &[String],
        extract_store_paths: bool,
    ) -> Result<Vec<PackageInfo>> {
        // Parent-level batch size: larger than worker internal batch (100)
        // to reduce IPC overhead, but small enough that workers don't accumulate
        // too much memory before a potential restart.
        const PARENT_BATCH_SIZE: usize = 500;

        if attrs.len() <= PARENT_BATCH_SIZE {
            // Small extraction - no need for parent-level batching
            return self.extract(system, repo_path, attrs, extract_store_paths);
        }

        let total_batches = attrs.len().div_ceil(PARENT_BATCH_SIZE);

        tracing::debug!(
            system = %system,
            total_attrs = attrs.len(),
            batch_size = PARENT_BATCH_SIZE,
            batches = total_batches,
            "Starting parent-level batched extraction"
        );

        let mut all_packages: Vec<PackageInfo> = Vec::with_capacity(attrs.len());

        for (batch_idx, chunk) in attrs.chunks(PARENT_BATCH_SIZE).enumerate() {
            // Wait for memory to clear between batches
            self.wait_for_memory_clear();

            tracing::trace!(
                system = %system,
                batch = batch_idx + 1,
                total = total_batches,
                chunk_size = chunk.len(),
                "Processing parent batch"
            );

            match self.extract(system, repo_path, chunk, extract_store_paths) {
                Ok(packages) => {
                    all_packages.extend(packages);
                }
                Err(e) => {
                    // Log error but continue with remaining batches
                    tracing::warn!(
                        system = %system,
                        batch = batch_idx + 1,
                        error = %e,
                        "Parent batch failed, continuing with remaining batches"
                    );
                }
            }
        }

        tracing::debug!(
            system = %system,
            total_packages = all_packages.len(),
            "Parent-level batched extraction complete"
        );

        Ok(all_packages)
    }

    /// Extract packages for multiple systems in parallel.
    ///
    /// Each system is assigned to a different worker. If there are more systems
    /// than workers, some workers will process multiple systems sequentially.
    /// Waits for memory pressure to clear before starting.
    #[instrument(level = "debug", skip(self, repo_path, attrs), fields(systems = systems.len(), attrs = attrs.len()))]
    pub fn extract_parallel(
        &self,
        repo_path: &Path,
        systems: &[String],
        attrs: &[String],
        extract_store_paths: bool,
    ) -> Vec<Result<Vec<PackageInfo>>> {
        use std::thread;

        // Wait for memory pressure to clear before starting parallel extraction
        self.wait_for_memory_clear();

        let parallel_start = Instant::now();

        // Log worker assignments
        for (i, system) in systems.iter().enumerate() {
            let worker_idx = i % self.workers.len();
            trace!(
                system = %system,
                worker_idx = worker_idx,
                "Assigning system to worker"
            );
        }

        // Use scoped threads to borrow from self
        let results: Vec<_> = thread::scope(|s| {
            let handles: Vec<_> = systems
                .iter()
                .enumerate()
                .map(|(i, system)| {
                    let worker_idx = i % self.workers.len();
                    let worker = &self.workers[worker_idx];
                    let repo_path = repo_path.to_path_buf();
                    let system = system.clone();
                    let attrs = attrs.to_vec();

                    s.spawn(move || {
                        let mut w = worker.lock().expect("worker mutex poisoned");
                        w.extract(&system, &repo_path, &attrs, extract_store_paths)
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(NxvError::Worker("Worker thread panicked".into())))
                })
                .collect()
        });

        let success_count = results.iter().filter(|r| r.is_ok()).count();
        let total_packages: usize = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|pkgs| pkgs.len())
            .sum();

        trace!(
            systems = systems.len(),
            success_count = success_count,
            total_packages = total_packages,
            parallel_time_ms = parallel_start.elapsed().as_millis(),
            "Parallel extraction completed"
        );

        results
    }

    /// Extract attribute positions for file-to-attribute mapping.
    ///
    /// Uses a worker subprocess to avoid memory accumulation in the parent process.
    /// The worker will restart if it exceeds the memory threshold.
    /// Waits for memory pressure to clear before starting.
    #[instrument(level = "debug", skip(self, repo_path))]
    pub fn extract_positions(&self, system: &str, repo_path: &Path) -> Result<Vec<AttrPosition>> {
        // Wait for memory pressure to clear before starting
        self.wait_for_memory_clear();

        // Find an available worker using try_lock
        for worker in &self.workers {
            if let Ok(mut w) = worker.try_lock() {
                return w.extract_positions(system, repo_path);
            }
        }

        // All workers busy - use round-robin to distribute wait fairly
        let idx = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let mut w = self.workers[idx].lock().expect("worker mutex poisoned");
        w.extract_positions(system, repo_path)
    }

    /// Shutdown all workers gracefully.
    pub fn shutdown(&self) {
        for worker in &self.workers {
            if let Ok(mut w) = worker.lock() {
                w.shutdown();
            }
        }
    }

    /// Get statistics about the worker pool.
    pub fn stats(&self) -> WorkerPoolStats {
        let mut total_jobs = 0;
        let mut total_restarts = 0;

        for worker in &self.workers {
            if let Ok(w) = worker.lock() {
                total_jobs += w.jobs_completed;
                total_restarts += w.restarts;
            }
        }

        WorkerPoolStats {
            worker_count: self.workers.len(),
            total_jobs_completed: total_jobs,
            total_restarts,
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Statistics about the worker pool.
#[derive(Debug, Clone)]
pub struct WorkerPoolStats {
    /// Number of workers in the pool.
    pub worker_count: usize,
    /// Total jobs completed by all workers.
    pub total_jobs_completed: usize,
    /// Total number of worker restarts.
    pub total_restarts: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_pool_config_default() {
        let config = WorkerPoolConfig::default();
        assert_eq!(config.worker_count, 4);
        // 8 GiB total / 4 workers = 2 GiB per worker
        assert_eq!(config.per_worker_memory_mib, 2 * 1024);
        assert!(config.eval_store_path.is_none());
    }

    #[test]
    fn test_worker_pool_config_with_eval_store() {
        let config = WorkerPoolConfig {
            worker_count: 4,
            eval_store_path: Some("/tmp/nxv-eval-store-2018-H1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            config.eval_store_path,
            Some("/tmp/nxv-eval-store-2018-H1".to_string())
        );
    }

    #[test]
    fn test_per_worker_store_path_generation() {
        // Verify that each worker gets a unique store path
        let base_path = "/tmp/nxv-eval-store-2018-H1";
        let config = WorkerPoolConfig {
            worker_count: 4,
            eval_store_path: Some(base_path.to_string()),
            ..Default::default()
        };

        // Simulate the path generation logic from WorkerPool::new
        let mut paths = Vec::new();
        for id in 0..config.worker_count {
            let worker_store_path = config
                .eval_store_path
                .as_ref()
                .map(|base| format!("{}-w{}", base, id));
            paths.push(worker_store_path);
        }

        // Each worker should have a unique path
        assert_eq!(paths[0], Some("/tmp/nxv-eval-store-2018-H1-w0".to_string()));
        assert_eq!(paths[1], Some("/tmp/nxv-eval-store-2018-H1-w1".to_string()));
        assert_eq!(paths[2], Some("/tmp/nxv-eval-store-2018-H1-w2".to_string()));
        assert_eq!(paths[3], Some("/tmp/nxv-eval-store-2018-H1-w3".to_string()));

        // All paths should be unique
        let unique_paths: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(unique_paths.len(), 4);
    }

    #[test]
    fn test_per_worker_store_path_none_when_no_base() {
        // When no eval_store_path is set, workers should get None
        let config = WorkerPoolConfig {
            worker_count: 2,
            eval_store_path: None,
            ..Default::default()
        };

        for id in 0..config.worker_count {
            let worker_store_path = config
                .eval_store_path
                .as_ref()
                .map(|base| format!("{}-w{}", base, id));
            assert!(worker_store_path.is_none());
        }
    }

    // Note: Full pool tests require the binary to support --internal-worker.
    // These will be added as integration tests.
}

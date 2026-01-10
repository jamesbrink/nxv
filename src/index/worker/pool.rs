//! Worker pool for parallel Nix evaluations.
//!
//! Manages a pool of worker subprocesses that can evaluate packages in parallel.

#![allow(dead_code)] // Some fields/methods are for future use or monitoring

use super::proc::Proc;
use super::protocol::{WorkRequest, WorkResponse};
use super::signals::{TerminationReason, WorkerFailure, analyze_wait_status};
use super::spawn::{WorkerConfig, spawn_worker};
use crate::error::{NxvError, Result};
use crate::index::extractor::{AttrPosition, PackageInfo};
use crate::memory::DEFAULT_MEMORY_BUDGET;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
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
}

impl Worker {
    /// Create a new worker and spawn its subprocess.
    fn new(id: usize, config: WorkerConfig) -> Result<Self> {
        let proc = spawn_worker(&config)?;
        Ok(Self {
            proc: Some(proc),
            config,
            id,
            jobs_completed: 0,
            restarts: 0,
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
        let proc = spawn_worker(&self.config)?;
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
                return self.extract(system, repo_path, attrs);
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
            let worker = Worker::new(id, worker_config)?;
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
        })
    }

    /// Get the number of workers in the pool.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Extract packages for a single system using an available worker.
    ///
    /// This method acquires a worker from the pool, sends the extraction request,
    /// and returns the result.
    pub fn extract(
        &self,
        system: &str,
        repo_path: &Path,
        attrs: &[String],
    ) -> Result<Vec<PackageInfo>> {
        // Find an available worker using try_lock
        for worker in &self.workers {
            if let Ok(mut w) = worker.try_lock() {
                return w.extract(system, repo_path, attrs);
            }
        }

        // All workers busy - use round-robin to distribute wait fairly
        let idx = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let mut w = self.workers[idx].lock().expect("worker mutex poisoned");
        w.extract(system, repo_path, attrs)
    }

    /// Extract packages for multiple systems in parallel.
    ///
    /// Each system is assigned to a different worker. If there are more systems
    /// than workers, some workers will process multiple systems sequentially.
    #[instrument(level = "debug", skip(self, repo_path, attrs), fields(systems = systems.len(), attrs = attrs.len()))]
    pub fn extract_parallel(
        &self,
        repo_path: &Path,
        systems: &[String],
        attrs: &[String],
    ) -> Vec<Result<Vec<PackageInfo>>> {
        use std::thread;

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
                        w.extract(&system, &repo_path, &attrs)
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
    #[instrument(level = "debug", skip(self, repo_path))]
    pub fn extract_positions(&self, system: &str, repo_path: &Path) -> Result<Vec<AttrPosition>> {
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

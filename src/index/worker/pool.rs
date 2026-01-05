//! Worker pool for parallel Nix evaluations.
//!
//! Manages a pool of worker subprocesses that can evaluate packages in parallel.

#![allow(dead_code)] // Some fields/methods are for future use or monitoring

use super::proc::Proc;
use super::protocol::{WorkRequest, WorkResponse};
use super::signals::{TerminationReason, WorkerFailure, analyze_wait_status};
use super::spawn::{WorkerConfig, spawn_worker};
use crate::error::{NxvError, Result};
use crate::index::extractor::PackageInfo;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// Configuration for the worker pool.
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// Number of worker processes to spawn.
    pub worker_count: usize,
    /// Memory threshold (MiB) before worker restart.
    pub max_memory_mib: usize,
    /// Timeout for worker operations.
    pub timeout: Duration,
}

impl Default for WorkerPoolConfig {
    fn default() -> Self {
        Self {
            worker_count: 4,
            max_memory_mib: 6 * 1024,          // 6 GiB
            timeout: Duration::from_secs(300), // 5 minutes
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
        proc.send(&request)?;

        // Receive result
        let response = proc.recv()?;
        let packages = match response {
            Some(WorkResponse::Result { packages }) => packages,
            Some(WorkResponse::Error { message }) => {
                // Store error but still need to consume Ready signal
                return self.finish_request_and_error(format!(
                    "Worker {} extraction error: {}",
                    self.id, message
                ));
            }
            Some(WorkResponse::Restart) => {
                // Worker requested restart - respawn and retry
                self.handle_restart()?;
                return self.extract(system, repo_path, attrs);
            }
            Some(WorkResponse::Ready) => {
                return Err(NxvError::Worker(format!(
                    "Worker {} sent Ready instead of result",
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
            Some(WorkResponse::Restart) => {
                self.jobs_completed += 1;
                self.handle_restart()?;
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

    /// Consume Ready signal after an error, then return the error.
    fn finish_request_and_error(&mut self, error_msg: String) -> Result<Vec<PackageInfo>> {
        // Worker sends Ready/Restart after Error too - consume it
        if let Some(proc) = self.proc.as_mut() {
            match proc.recv() {
                Ok(Some(WorkResponse::Ready)) => {}
                Ok(Some(WorkResponse::Restart)) => {
                    let _ = self.handle_restart();
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

        if failure.is_recoverable() {
            // Try to respawn
            self.restarts += 1;
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
}

impl WorkerPool {
    /// Create a new worker pool and spawn worker processes.
    pub fn new(config: WorkerPoolConfig) -> Result<Self> {
        let worker_config = WorkerConfig {
            max_memory_mib: config.max_memory_mib,
        };

        let mut workers = Vec::with_capacity(config.worker_count);
        for id in 0..config.worker_count {
            let worker = Worker::new(id, worker_config.clone())?;
            workers.push(Mutex::new(worker));
        }

        // Wait for all workers to be ready
        for (id, worker) in workers.iter().enumerate() {
            let mut w = worker.lock().unwrap();
            w.wait_for_ready().map_err(|e| {
                NxvError::Worker(format!("Worker {} failed to initialize: {}", id, e))
            })?;
        }

        Ok(Self { workers, config })
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
        // Find an available worker (simple round-robin for now)
        // In practice, workers are used by different threads via extract_parallel
        for worker in &self.workers {
            if let Ok(mut w) = worker.try_lock() {
                return w.extract(system, repo_path, attrs);
            }
        }

        // All workers busy - wait for the first one
        let mut w = self.workers[0].lock().unwrap();
        w.extract(system, repo_path, attrs)
    }

    /// Extract packages for multiple systems in parallel.
    ///
    /// Each system is assigned to a different worker. If there are more systems
    /// than workers, some workers will process multiple systems sequentially.
    pub fn extract_parallel(
        &self,
        repo_path: &Path,
        systems: &[String],
        attrs: &[String],
    ) -> Vec<Result<Vec<PackageInfo>>> {
        use std::thread;

        // Use scoped threads to borrow from self
        thread::scope(|s| {
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
                        let mut w = worker.lock().unwrap();
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
        })
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
        assert_eq!(config.max_memory_mib, 6 * 1024);
    }

    // Note: Full pool tests require the binary to support --internal-worker.
    // These will be added as integration tests.
}
